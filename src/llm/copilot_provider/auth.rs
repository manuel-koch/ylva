// Copyright © 2026 Manuel Koch
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! GitHub Copilot credentials: validation of the long-lived GitHub token and
//! lazy exchange/caching of the short-lived Copilot session token.

use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::agent::{Message, UserMessageKind};
use crate::llm::copilot_provider::github_protocol::{
    COPILOT_FALLBACK_API_BASE, COPILOT_TOKEN_EXCHANGE_URL, EDITOR_PLUGIN_VERSION, EDITOR_VERSION,
    GITHUB_API_VERSION, INTEGRATION_ID, OPENAI_INTENT, USER_AGENT,
};
use crate::llm::openai_compatible_provider::{RequestAuthorization, RequestAuthorizer};
use crate::unix_time::unix_now;

/// A session token that expires within this many seconds is refreshed instead
/// of reused, so a request does not expire in flight. Ylva's own choice (not
/// specified by GitHub); session tokens live roughly 25-30 minutes.
const SESSION_TOKEN_REFRESH_BEFORE_EXPIRY_SECONDS: i64 = 300;

/// Minimum spacing between outbound Copilot requests. Community reports
/// suggest faster request rates can get Copilot access suspended.
pub const COPILOT_MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(2);

/// Value of the `X-Initiator` header: `user` when the request answers a
/// message the user wrote, `agent` for system-driven continuations such as
/// tool-result follow-ups. The header is undocumented; this is the rule VS Code
/// Copilot Chat, Zed and LiteLLM follow.
pub(crate) fn copilot_initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        Some(Message::User {
            kind: UserMessageKind::Explicit,
            ..
        }) => "user",
        _ => "agent",
    }
}

/// Client identification headers; the only ones the token exchange needs.
fn copilot_identification_headers() -> [(&'static str, &'static str); 3] {
    [
        ("editor-version", EDITOR_VERSION),
        ("editor-plugin-version", EDITOR_PLUGIN_VERSION),
        ("user-agent", USER_AGENT),
    ]
}

/// Headers every Copilot chat and model-listing request carries.
pub(crate) fn copilot_request_headers() -> Vec<(&'static str, &'static str)> {
    let mut headers = vec![("copilot-integration-id", INTEGRATION_ID)];
    headers.extend(copilot_identification_headers());
    headers.push(("openai-intent", OPENAI_INTENT));
    headers.push(("x-github-api-version", GITHUB_API_VERSION));
    headers
}

/// Reject GitHub personal access tokens: they cannot be exchanged for a
/// Copilot session token. Only OAuth device-flow tokens work.
pub fn validate_copilot_github_token(github_token: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !github_token.is_empty(),
        "the Copilot GitHub token is empty, run `ylva connect copilot`"
    );
    anyhow::ensure!(
        !(github_token.starts_with("github_pat_") || github_token.starts_with("ghp_")),
        "PAT unsupported, run onboarding (`ylva connect copilot`)"
    );
    Ok(())
}

/// A short-lived Copilot API credential.
#[derive(Clone, PartialEq, Eq)]
pub struct CopilotSessionToken {
    token: String,
    /// Unix timestamp in seconds.
    expires_at: i64,
    api_base: String,
}

impl CopilotSessionToken {
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    fn is_fresh(&self, now: i64) -> bool {
        self.expires_at > now + SESSION_TOKEN_REFRESH_BEFORE_EXPIRY_SECONDS
    }
}

impl std::fmt::Debug for CopilotSessionToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CopilotSessionToken")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("api_base", &self.api_base)
            .finish()
    }
}

#[derive(Deserialize)]
struct TokenExchangeResponse {
    token: String,
    expires_at: i64,
    #[serde(default)]
    endpoints: Option<TokenExchangeEndpoints>,
}

#[derive(Deserialize)]
struct TokenExchangeEndpoints {
    #[serde(default)]
    api: Option<String>,
}

/// Exchanges the long-lived GitHub token for session tokens on demand and
/// caches the current one in memory only.
pub struct CopilotAuth {
    github_token: String,
    client: reqwest::Client,
    token_exchange_url: String,
    fallback_api_base: String,
    cached_session_token: Mutex<Option<CopilotSessionToken>>,
    min_request_interval: Duration,
    last_request_at: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for CopilotAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CopilotAuth")
            .field("github_token", &"<redacted>")
            .field("token_exchange_url", &self.token_exchange_url)
            .finish()
    }
}

impl CopilotAuth {
    /// Create the auth state; fails for PAT-looking or empty tokens.
    pub fn new(github_token: String, client: reqwest::Client) -> anyhow::Result<Self> {
        Self::with_urls(
            github_token,
            client,
            COPILOT_TOKEN_EXCHANGE_URL.to_string(),
            COPILOT_FALLBACK_API_BASE.to_string(),
            COPILOT_MIN_REQUEST_INTERVAL,
        )
    }

    pub(crate) fn with_urls(
        github_token: String,
        client: reqwest::Client,
        token_exchange_url: String,
        fallback_api_base: String,
        min_request_interval: Duration,
    ) -> anyhow::Result<Self> {
        validate_copilot_github_token(&github_token)?;
        Ok(Self {
            github_token,
            client,
            token_exchange_url,
            fallback_api_base,
            cached_session_token: Mutex::new(None),
            min_request_interval,
            last_request_at: Mutex::new(None),
        })
    }

    /// Wait until at least the minimum interval has passed since the previous
    /// request started. Concurrent callers queue behind each other.
    async fn wait_for_request_slot(&self) {
        let mut last_request_at = self.last_request_at.lock().await;
        if let Some(previous) = *last_request_at {
            let ready_at = previous + self.min_request_interval;
            tokio::time::sleep_until(tokio::time::Instant::from_std(ready_at)).await;
        }
        *last_request_at = Some(Instant::now());
    }

    /// Return a session token that does not expire within the refresh-before-expiry window,
    /// exchanging a new one when needed. Concurrent callers share one
    /// exchange.
    pub async fn session_token(&self) -> anyhow::Result<CopilotSessionToken> {
        let mut cached = self.cached_session_token.lock().await;
        if let Some(session_token) = cached.as_ref().filter(|token| token.is_fresh(unix_now())) {
            return Ok(session_token.clone());
        }
        let session_token = self.exchange().await?;
        *cached = Some(session_token.clone());
        Ok(session_token)
    }

    async fn exchange(&self) -> anyhow::Result<CopilotSessionToken> {
        let mut request = self
            .client
            .get(&self.token_exchange_url)
            .header("authorization", format!("token {}", self.github_token))
            .header("accept", "application/json");
        for (name, value) in copilot_identification_headers() {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|error| anyhow::anyhow!("Copilot token exchange failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 | 404 => anyhow::anyhow!(
                    "Copilot token exchange rejected (HTTP {status}): the token is not a valid \
                     GitHub OAuth (ghu_) token or the account has no Copilot access; re-run \
                     `ylva connect copilot`"
                ),
                _ => anyhow::anyhow!("Copilot token exchange failed (HTTP {status})"),
            });
        }
        let body: TokenExchangeResponse = response
            .json()
            .await
            .map_err(|error| anyhow::anyhow!("invalid Copilot token exchange response: {error}"))?;
        let api_base = body
            .endpoints
            .and_then(|endpoints| endpoints.api)
            .filter(|api| !api.is_empty())
            .unwrap_or_else(|| self.fallback_api_base.clone());
        Ok(CopilotSessionToken {
            token: body.token,
            expires_at: body.expires_at,
            api_base,
        })
    }
}

impl RequestAuthorizer for CopilotAuth {
    fn authorize(&self) -> BoxFuture<'_, anyhow::Result<RequestAuthorization>> {
        Box::pin(async move {
            self.wait_for_request_slot().await;
            let session_token = self.session_token().await?;
            Ok(RequestAuthorization {
                base_url: session_token.api_base,
                bearer_token: session_token.token,
            })
        })
    }

    fn chat_request_headers(&self, messages: &[Message]) -> Vec<(&'static str, &'static str)> {
        vec![("x-initiator", copilot_initiator(messages))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serve `responses` (status line, body) in order and record raw requests.
    async fn exchange_server(
        responses: Vec<(&'static str, String)>,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let (recorded, counter) = (requests.clone(), count.clone());
        tokio::spawn(async move {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).await.expect("read");
                recorded
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buffer[..read]).to_string());
                counter.fetch_add(1, Ordering::SeqCst);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.expect("write");
            }
        });
        (format!("http://{address}/token"), requests, count)
    }

    fn token_body(token: &str, expires_in: i64, api: Option<&str>) -> String {
        let endpoints = api
            .map(|api| format!(r#","endpoints":{{"api":"{api}"}}"#))
            .unwrap_or_default();
        format!(
            r#"{{"token":"{token}","expires_at":{}{endpoints}}}"#,
            unix_now() + expires_in
        )
    }

    fn auth_for(url: String) -> CopilotAuth {
        CopilotAuth::with_urls(
            "ghu_secret".to_string(),
            reqwest::Client::new(),
            url,
            "http://fallback.invalid".to_string(),
            Duration::ZERO,
        )
        .expect("auth")
    }

    #[test]
    fn initiator_is_user_only_when_the_last_message_is_an_explicit_user_message() {
        let user = |kind| Message::User {
            content: "hi".to_string(),
            kind,
        };
        let assistant = Message::Assistant {
            content: String::new(),
            status: crate::agent::AssistantStatus::Complete,
            tool_calls: Vec::new(),
        };
        assert_eq!(
            copilot_initiator(&[user(UserMessageKind::Explicit)]),
            "user"
        );
        assert_eq!(
            copilot_initiator(&[
                user(UserMessageKind::Explicit),
                user(UserMessageKind::ImplicitToolNote)
            ]),
            "agent"
        );
        assert_eq!(
            copilot_initiator(&[user(UserMessageKind::Explicit), assistant]),
            "agent"
        );
        assert_eq!(copilot_initiator(&[]), "agent");
    }

    #[tokio::test]
    async fn request_slots_are_spaced_by_the_minimum_interval() {
        let interval = Duration::from_millis(150);
        let auth = CopilotAuth::with_urls(
            "ghu_secret".to_string(),
            reqwest::Client::new(),
            "http://unused.invalid".to_string(),
            "http://fallback.invalid".to_string(),
            interval,
        )
        .expect("auth");

        let started = Instant::now();
        auth.wait_for_request_slot().await;
        assert!(started.elapsed() < interval, "first request is not delayed");
        tokio::join!(auth.wait_for_request_slot(), auth.wait_for_request_slot());

        assert!(started.elapsed() >= interval * 2);
    }

    #[test]
    fn rejects_personal_access_tokens_and_empty_tokens() {
        for token in ["github_pat_abc", "ghp_abc", ""] {
            assert!(validate_copilot_github_token(token).is_err(), "{token}");
        }
        assert!(validate_copilot_github_token("ghu_abc").is_ok());
        let error = CopilotAuth::new("ghp_abc".to_string(), reqwest::Client::new())
            .unwrap_err()
            .to_string();
        assert!(error.contains("PAT unsupported"));
    }

    #[test]
    fn debug_output_never_contains_tokens() {
        let auth = auth_for("http://example.invalid".to_string());
        let session = CopilotSessionToken {
            token: "session-secret".to_string(),
            expires_at: 1,
            api_base: "https://x".to_string(),
        };
        let rendered = format!("{auth:?} {session:?}");
        assert!(!rendered.contains("ghu_secret"));
        assert!(!rendered.contains("session-secret"));
    }

    #[test]
    fn exchanges_lazily_caches_and_sends_required_headers() {
        crate::test_support::run_locked(async {
            let (url, requests, count) = exchange_server(vec![(
                "200 OK",
                token_body("session-1", 1800, Some("https://api.example")),
            )])
            .await;
            let auth = auth_for(url);
            assert_eq!(count.load(Ordering::SeqCst), 0);

            let first = auth.session_token().await.expect("first");
            let second = auth.session_token().await.expect("cached");

            assert_eq!(first, second);
            assert_eq!(first.api_base(), "https://api.example");
            assert_eq!(count.load(Ordering::SeqCst), 1);
            let request = requests.lock().unwrap()[0].to_lowercase();
            assert!(request.contains("authorization: token ghu_secret"));
            assert!(request.contains("editor-version: vscode/"));
            assert!(request.contains("editor-plugin-version: copilot-chat/"));
            assert!(request.contains("user-agent: githubcopilotchat/"));
        });
    }

    #[test]
    fn refreshes_a_token_expiring_within_the_refresh_window_and_falls_back_to_default_api_base() {
        crate::test_support::run_locked(async {
            let (url, _requests, count) = exchange_server(vec![
                ("200 OK", token_body("soon-expiring", 200, None)),
                ("200 OK", token_body("renewed", 1800, None)),
            ])
            .await;
            let auth = auth_for(url);

            let first = auth.session_token().await.expect("first");
            let second = auth.session_token().await.expect("refreshed");

            assert_eq!(count.load(Ordering::SeqCst), 2);
            assert_eq!(first.api_base(), "http://fallback.invalid");
            assert_eq!(second.token, "renewed");
        });
    }

    #[test]
    fn maps_exchange_failures_to_clear_errors() {
        crate::test_support::run_locked(async {
            let (url, _requests, _count) = exchange_server(vec![
                ("403 Forbidden", "{}".to_string()),
                ("500 Internal Server Error", "{}".to_string()),
            ])
            .await;
            let auth = auth_for(url);

            let forbidden = auth.session_token().await.unwrap_err().to_string();
            let server_error = auth.session_token().await.unwrap_err().to_string();

            assert!(forbidden.contains("ylva connect copilot"));
            assert!(!forbidden.contains("ghu_secret"));
            assert!(server_error.contains("HTTP 500"));
        });
    }
}
