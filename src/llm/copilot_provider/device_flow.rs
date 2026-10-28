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

//! GitHub OAuth device flow used to obtain the long-lived Copilot token.

use std::io::Write;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::llm::copilot_provider::github_protocol::{
    COPILOT_CLIENT_ID, DEVICE_GRANT_TYPE, GITHUB_ACCESS_TOKEN_URL, GITHUB_DEVICE_CODE_URL,
};

const SLOW_DOWN_INCREMENT: Duration = Duration::from_secs(5);

/// Endpoints of the device flow; injectable for tests.
#[derive(Debug, Clone)]
pub struct DeviceFlowEndpoints {
    pub device_code_url: String,
    pub access_token_url: String,
}

impl Default for DeviceFlowEndpoints {
    fn default() -> Self {
        Self {
            device_code_url: GITHUB_DEVICE_CODE_URL.to_string(),
            access_token_url: GITHUB_ACCESS_TOKEN_URL.to_string(),
        }
    }
}

/// What the user must do to authorize, plus the polling parameters.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: Duration,
    pub interval: Duration,
}

impl std::fmt::Debug for DeviceCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceCode")
            .field("device_code", &"<redacted>")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .finish()
    }
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct AccessTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
}

/// Start the device flow and return the code the user must enter.
pub async fn request_device_code(
    client: &reqwest::Client,
    endpoints: &DeviceFlowEndpoints,
) -> anyhow::Result<DeviceCode> {
    let response = client
        .post(&endpoints.device_code_url)
        .header("accept", "application/json")
        .json(&serde_json::json!({"client_id": COPILOT_CLIENT_ID, "scope": "read:user"}))
        .send()
        .await
        .map_err(|error| anyhow::anyhow!("device code request failed: {error}"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "device code request failed (HTTP {})",
        response.status()
    );
    let body: DeviceCodeResponse = response
        .json()
        .await
        .map_err(|error| anyhow::anyhow!("invalid device code response: {error}"))?;
    Ok(DeviceCode {
        device_code: body.device_code,
        user_code: body.user_code,
        verification_uri: body.verification_uri,
        expires_in: Duration::from_secs(body.expires_in),
        interval: Duration::from_secs(body.interval.unwrap_or(5)),
    })
}

/// Poll until the user authorizes, denies, the code expires or `cancel`
/// fires. Returns the long-lived GitHub token.
pub async fn poll_for_access_token(
    client: &reqwest::Client,
    endpoints: &DeviceFlowEndpoints,
    device_code: &DeviceCode,
    cancel: &CancellationToken,
) -> anyhow::Result<String> {
    let deadline = Instant::now() + device_code.expires_in;
    let mut interval = device_code.interval;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => anyhow::bail!("device authorization cancelled"),
            _ = tokio::time::sleep(interval) => {}
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "the device code expired, run the command again"
        );
        let response = client
            .post(&endpoints.access_token_url)
            .header("accept", "application/json")
            .json(&serde_json::json!({
                "client_id": COPILOT_CLIENT_ID,
                "device_code": device_code.device_code,
                "grant_type": DEVICE_GRANT_TYPE,
            }))
            .send()
            .await
            .map_err(|error| anyhow::anyhow!("access token request failed: {error}"))?;
        let body: AccessTokenResponse = response
            .json()
            .await
            .map_err(|error| anyhow::anyhow!("invalid access token response: {error}"))?;
        if let Some(access_token) = body.access_token.filter(|token| !token.is_empty()) {
            return Ok(access_token);
        }
        match body.error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => {
                interval = body
                    .interval
                    .map(Duration::from_secs)
                    .unwrap_or(interval + SLOW_DOWN_INCREMENT);
            }
            Some("expired_token") => {
                anyhow::bail!("the device code expired, run the command again")
            }
            Some("access_denied") => anyhow::bail!("authorization was denied"),
            Some(other) => anyhow::bail!("device authorization failed: {other}"),
            None => anyhow::bail!("device authorization returned neither a token nor an error"),
        }
    }
}

/// Run the device flow interactively: print the verification instructions to
/// `output`, then poll until authorized. ctrl-c cancels the wait.
pub async fn authorize_with_copilot_device_flow(output: &mut dyn Write) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let endpoints = DeviceFlowEndpoints::default();
    let device_code = request_device_code(&client, &endpoints).await?;
    writeln!(
        output,
        "Open {} and enter the code {}\nWaiting for authorization (ctrl-c to cancel)...",
        device_code.verification_uri, device_code.user_code
    )?;
    output.flush()?;
    let cancel = CancellationToken::new();
    let interrupt_cancel = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            interrupt_cancel.cancel();
        }
    });
    poll_for_access_token(&client, &endpoints, &device_code, &cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn serve(bodies: Vec<&'static str>) -> DeviceFlowEndpoints {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            for body in bodies {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut buffer = [0_u8; 4096];
                let _ = stream.read(&mut buffer).await.expect("read");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.expect("write");
            }
        });
        DeviceFlowEndpoints {
            device_code_url: format!("http://{address}/device"),
            access_token_url: format!("http://{address}/token"),
        }
    }

    fn short_code(expires_in: Duration) -> DeviceCode {
        DeviceCode {
            device_code: "device".to_string(),
            user_code: "ABCD-1234".to_string(),
            verification_uri: "https://github.com/login/device".to_string(),
            expires_in,
            interval: Duration::from_millis(5),
        }
    }

    #[test]
    fn requests_device_code() {
        crate::test_support::run_locked(async {
            let endpoints = serve(vec![
                r#"{"device_code":"d","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#,
            ])
            .await;

            let code = request_device_code(&reqwest::Client::new(), &endpoints)
                .await
                .expect("device code");

            assert_eq!(code.user_code, "ABCD-1234");
            assert_eq!(code.interval, Duration::from_secs(5));
            assert!(!format!("{code:?}").contains("\"d\""));
        });
    }

    #[test]
    fn polls_through_pending_and_slow_down_until_token() {
        crate::test_support::run_locked(async {
            let endpoints = serve(vec![
                r#"{"error":"authorization_pending"}"#,
                r#"{"error":"slow_down","interval":0}"#,
                r#"{"access_token":"ghu_token"}"#,
            ])
            .await;

            let token = poll_for_access_token(
                &reqwest::Client::new(),
                &endpoints,
                &short_code(Duration::from_secs(30)),
                &CancellationToken::new(),
            )
            .await
            .expect("token");

            assert_eq!(token, "ghu_token");
        });
    }

    #[test]
    fn terminal_errors_cancellation_and_deadline_fail() {
        crate::test_support::run_locked(async {
            for (body, expected) in [
                (r#"{"error":"access_denied"}"#, "denied"),
                (r#"{"error":"expired_token"}"#, "expired"),
                (
                    r#"{"error":"unsupported_grant_type"}"#,
                    "unsupported_grant_type",
                ),
            ] {
                let endpoints = serve(vec![body]).await;
                let error = poll_for_access_token(
                    &reqwest::Client::new(),
                    &endpoints,
                    &short_code(Duration::from_secs(30)),
                    &CancellationToken::new(),
                )
                .await
                .unwrap_err()
                .to_string();
                assert!(error.contains(expected), "{error}");
            }

            let cancel = CancellationToken::new();
            cancel.cancel();
            let error = poll_for_access_token(
                &reqwest::Client::new(),
                &DeviceFlowEndpoints::default(),
                &short_code(Duration::from_secs(30)),
                &cancel,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("cancelled"));

            let error = poll_for_access_token(
                &reqwest::Client::new(),
                &DeviceFlowEndpoints::default(),
                &short_code(Duration::ZERO),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("expired"));
        });
    }
}
