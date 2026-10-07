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
//! Brave Search backend for the [`WebSearchBackend`] seam: a JSON `GET`
//! against Brave's documented search API, authenticated with the
//! subscription token and parsed with serde.
//!
//! **Why Brave.** Unlike DuckDuckGo's bot-detected HTML endpoint this backend
//! needs no scraping, and unlike SearXNG it needs no self-hosted instance —
//! the endpoint is fixed and the token comes from
//! `[web_search.backends.brave] api_key`.
//!
//! **Authentication.** The token travels in the `X-Subscription-Token`
//! header, never as an `api_key` query parameter: query strings are written
//! verbatim into `YLVA_REQUEST_DUMP_PATH` dumps, and the request-dump
//! redaction masks every `*-token` header from the dumped header projection.
//!
//! **Query pass-through.** `terms` is sent unchanged — Brave's own `4xx`
//! error envelope diagnoses an empty or over-limit query, so this backend
//! neither rejects nor truncates it locally.
//!
//! Every request fails fast: a transport error, a non-success HTTP status,
//! or an unparseable body is a [`WebSearchError`] — never a retry and never
//! a silently-empty result set on a failed response. An absent or empty
//! `web.results` on HTTP `200` *is* a genuine empty answer: Brave signals
//! failures through the status code, so an empty payload means no matches.

use std::path::Path;
use std::time::Duration;

use futures::future::BoxFuture;
use serde::Deserialize;

use super::{WebSearchBackend, WebSearchError, WebSearchHit, WEB_SEARCH_MAX_RESULT_LIMIT};
use crate::http::OUTBOUND_USER_AGENT;

/// Name of the authentication header carrying the Brave subscription token.
const SUBSCRIPTION_TOKEN_HEADER: &str = "X-Subscription-Token";

// ── Brave Search backend ─────────────────────────────────────────────────────

/// The Brave Search backend: a JSON `GET` against Brave's fixed search
/// endpoint, parsed from its JSON response shape.
pub struct BraveBackend {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    dump_source: crate::http::RequestDumpSource,
}

impl std::fmt::Debug for BraveBackend {
    /// Keep the type name while never formatting the subscription token —
    /// a `Debug` output must not leak the credential.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BraveBackend")
            .field("base_url", &self.base_url)
            .field("api_key", &"<REDACTED>")
            .finish()
    }
}

impl BraveBackend {
    /// Production endpoint root. Brave's API contract fixes this authority,
    /// so there is no configurable URL.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.search.brave.com";

    /// Path of the web-search endpoint below [`DEFAULT_BASE_URL`].
    const SEARCH_PATH: &'static str = "/res/v1/web/search";

    /// Build a backend against [`DEFAULT_BASE_URL`]. `http_timeout` bounds a
    /// single request (the tool's outer timeout must exceed it).
    pub fn with_api_key(api_key: &str, http_timeout: Duration) -> Self {
        Self::with_api_key_and_base_url_and_dump_source(
            api_key,
            Self::DEFAULT_BASE_URL,
            http_timeout,
            crate::http::RequestDumpSource::Environment,
        )
    }

    /// Build a backend against an explicit endpoint root and dump-path
    /// template.
    ///
    /// This keeps tests on the actual request/dump path — a local mock server
    /// stands in for the fixed production authority — without mutating the
    /// process environment; production callers use [`with_api_key`](Self::with_api_key),
    /// which resolves `YLVA_REQUEST_DUMP_PATH` when each request starts.
    pub fn with_api_key_and_base_url_and_dump_path(
        api_key: &str,
        base_url: &str,
        http_timeout: Duration,
        dump_path: &Path,
    ) -> Self {
        Self::with_api_key_and_base_url_and_dump_source(
            api_key,
            base_url,
            http_timeout,
            crate::http::RequestDumpSource::from_template(dump_path),
        )
    }

    fn with_api_key_and_base_url_and_dump_source(
        api_key: &str,
        base_url: &str,
        http_timeout: Duration,
        dump_source: crate::http::RequestDumpSource,
    ) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(OUTBOUND_USER_AGENT)
            .timeout(http_timeout)
            .build()
            .expect("shared HTTP client construction must succeed");
        BraveBackend {
            client,
            api_key: api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            dump_source,
        }
    }
}

impl WebSearchBackend for BraveBackend {
    fn network_target(&self) -> String {
        crate::http::network_target(Self::DEFAULT_BASE_URL)
            .unwrap_or_else(|| Self::DEFAULT_BASE_URL.to_string())
    }

    fn search<'a>(
        &'a self,
        terms: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<WebSearchHit>, WebSearchError>> {
        Box::pin(async move {
            if limit == 0 {
                return Ok(Vec::new());
            }
            let url = format!("{}{}", self.base_url, Self::SEARCH_PATH);
            tracing::debug!(backend = "brave", terms = %terms, limit, "web search request");
            // `terms` passes through unchanged; Brave's error envelope
            // diagnoses an empty or over-limit query (see the module docs).
            let count = limit.min(WEB_SEARCH_MAX_RESULT_LIMIT).to_string();
            let request = self
                .client
                .get(&url)
                .header(reqwest::header::ACCEPT, "application/json")
                .header(reqwest::header::ACCEPT_ENCODING, "gzip")
                .header(SUBSCRIPTION_TOKEN_HEADER, &self.api_key)
                .query(&[
                    ("q", terms),
                    ("count", count.as_str()),
                    ("result_filter", "web"),
                    ("text_decorations", "false"),
                ])
                .build()
                .map_err(|error| WebSearchError::Request(error.to_string()))?;
            let request_url = request.url().to_string();
            let dump_context = crate::http::RequestDumpContext::new(&self.dump_source);
            let request_dump_path = dump_context.request_path();
            if let Some(path) = &request_dump_path {
                crate::http::dump_request(
                    &request_url,
                    &crate::http::redacted_headers_json(request.headers()),
                    &serde_json::Value::Null,
                    path,
                );
            }
            // No retry: every failure is terminal (see the module docs).
            let response = self.client.execute(request).await.map_err(|error| {
                let response_dump_path = dump_context.response_path_for_content_type(None, "jsonl");
                crate::http::dump_response_transport_error(&response_dump_path, &error);
                WebSearchError::Request(error.to_string())
            })?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok());
            let response_dump_path =
                dump_context.response_path_for_content_type(content_type, "bin");
            let body = response.bytes().await.map_err(|error| {
                crate::http::dump_response_transport_error(&response_dump_path, &error);
                WebSearchError::Request(error.to_string())
            })?;
            crate::http::dump_buffered_response(&body, response_dump_path.as_deref());
            if !status.is_success() {
                return Err(status_error(status.as_u16(), &body));
            }
            parse_brave_results(&body, limit)
        })
    }
}

// ── Response parsing ──────────────────────────────────────────────────────────

/// The JSON body Brave answers a web search with.
#[derive(Debug, Deserialize)]
struct BraveSearchResponse {
    /// Web results; absent or `null` when the query matched nothing (Brave
    /// signals failures through the status code instead).
    #[serde(default)]
    web: Option<BraveWebResults>,
}

/// The `web` object of [`BraveSearchResponse`].
#[derive(Debug, Deserialize)]
struct BraveWebResults {
    /// Ranked web results.
    #[serde(default)]
    results: Vec<BraveWebResult>,
}

/// One entry of [`BraveWebResults::results`].
#[derive(Debug, Deserialize)]
struct BraveWebResult {
    /// Title of the result page.
    #[serde(default)]
    title: String,
    /// Absolute URL of the result page.
    #[serde(default)]
    url: String,
    /// Excerpt; absent when Brave provided none.
    #[serde(default)]
    description: Option<String>,
}

/// The error envelope Brave answers a non-success status with
/// (`400`/`401`/`403`/`404`/`422`/`429`).
#[derive(Debug, Deserialize)]
struct BraveErrorResponse {
    /// Diagnosis carried in `detail`/`code`.
    error: BraveErrorDetail,
}

/// The `error` object of [`BraveErrorResponse`].
#[derive(Debug, Deserialize)]
struct BraveErrorDetail {
    /// Human-readable diagnosis, e.g. an over-limit query.
    #[serde(default)]
    detail: Option<String>,
    /// Machine-readable error code, used when `detail` is empty.
    #[serde(default)]
    code: Option<String>,
}

/// Parse a Brave web-search response into at most `limit` hits.
///
/// Anything that is not the expected JSON shape is a parse failure, never an
/// empty result set — a silently-empty answer would tell the model "nothing
/// was found" when the backend actually failed. An absent `web` object or an
/// empty `results` array on HTTP `200` is a genuine empty answer: Brave has
/// no "engine failed" signal on success.
fn parse_brave_results(body: &[u8], limit: usize) -> Result<Vec<WebSearchHit>, WebSearchError> {
    let response: BraveSearchResponse = serde_json::from_slice(body).map_err(|error| {
        WebSearchError::Parse(format!(
            "the response body is not a Brave Search JSON response: {error}"
        ))
    })?;
    let results = response.web.map(|web| web.results).unwrap_or_default();
    let hits = results
        .iter()
        .filter_map(|result| {
            let title = normalize_whitespace(&result.title);
            let url = result.url.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = normalize_whitespace(result.description.as_deref().unwrap_or_default());
            Some(WebSearchHit {
                title,
                url: url.to_string(),
                snippet,
            })
        })
        .take(limit)
        .collect();
    Ok(hits)
}

/// Build the terminal error for a non-success Brave status.
///
/// A body shaped like Brave's error envelope carries the diagnosis in
/// `detail` (falling back to `code`) and becomes
/// [`WebSearchError::StatusWithHint`]; anything else — a proxy answering
/// plain text, for instance — stays a plain [`WebSearchError::Status`].
fn status_error(status: u16, body: &[u8]) -> WebSearchError {
    let hint = serde_json::from_slice::<BraveErrorResponse>(body)
        .ok()
        .and_then(|envelope| {
            non_blank(envelope.error.detail.as_deref())
                .or_else(|| non_blank(envelope.error.code.as_deref()))
                .map(str::to_owned)
        });
    match hint {
        Some(hint) => WebSearchError::StatusWithHint { status, hint },
        None => WebSearchError::Status { status },
    }
}

/// The trimmed value when `value` holds text, `None` when it is absent or
/// blank.
fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Collapse every run of whitespace into a single space and trim the ends, so
/// Brave-provided text (titles, descriptions) is single-line and
/// comparison-stable.
fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sentinel token: no test may ever let it reach an assertion output,
    /// a dump file, or the live API.
    const SENTINEL_API_KEY: &str = "brave-sentinel-api-key";

    /// Run `handler` as a one-shot HTTP server and point a backend at it.
    ///
    /// `handler` receives the raw request bytes and returns the raw response
    /// bytes, so every test asserts the exact wire shape the backend produces.
    async fn with_mock_server<F>(handler: F, dump_path: &Path) -> BraveBackend
    where
        F: FnOnce(String) -> Vec<u8> + Send + 'static,
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind server");
        let address = listener.local_addr().expect("server address");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let bytes_read = stream.read(&mut buffer).await.expect("read request");
                assert!(bytes_read > 0, "request must contain complete headers");
                request.extend_from_slice(&buffer[..bytes_read]);
                // GET carries no body, so the header terminator ends the request.
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request_text = String::from_utf8_lossy(&request).into_owned();
            let response = handler(request_text);
            stream.write_all(&response).await.expect("write response");
        });

        BraveBackend::with_api_key_and_base_url_and_dump_path(
            SENTINEL_API_KEY,
            &format!("http://{address}"),
            Duration::from_secs(1),
            dump_path,
        )
    }

    fn json_response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build Tokio runtime")
    }

    const RESULTS_JSON: &str = r#"{
  "type": "web",
  "query": {"original": "rust async", "show_strict_warning": false},
  "web": {
    "results": [
      {"title": "Rust Programming Language", "url": "https://rust-lang.org/", "description": "A   fast,\nreliable language.", "page_age": "2026-01-01"},
      {"title": "The Rust Book", "url": "https://doc.rust-lang.org/book/"},
      {"title": "Users forum", "url": "https://users.rust-lang.org/", "description": ""},
      {"title": "Skipped: empty url", "url": ""},
      {"title": "   ", "url": "https://no-title.example/"}
    ]
  },
  "news": null,
  "videos": null
}"#;

    /// A `200` body that is not the Brave JSON shape.
    const HTML_BODY: &str = r#"<html><body><p>Service temporarily unavailable.</p></body></html>"#;

    fn error_envelope(detail: &str, code: &str) -> String {
        format!(
            r#"{{"type": "error", "error": {{"id": "err-1", "status": 401, "detail": "{detail}", "code": "{code}"}}, "time": "2026-10-01"}}"#
        )
    }

    #[test]
    fn brave_gets_json_search_and_dumps_the_exchange() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dump_path = temp_dir.path().join("requests.jsonl");
        let observed = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let observed_for_server = observed.clone();

        runtime().block_on(async {
            let backend = with_mock_server(
                move |request| {
                    *observed_for_server.lock().expect("observed request") = request.clone();
                    json_response("200 OK", "application/json", RESULTS_JSON)
                },
                &dump_path,
            )
            .await;
            let hits = backend
                .search("rust async", 10)
                .await
                .expect("search response");
            assert_eq!(hits.len(), 3);
        });

        let request = observed.lock().expect("observed request").clone();
        let request_line = request.lines().next().expect("request line").to_string();
        let request_line_lowercase = request_line.to_ascii_lowercase();
        assert!(
            request_line_lowercase.starts_with("get /res/v1/web/search?"),
            "Brave must receive a GET to /res/v1/web/search: {request_line}"
        );
        assert!(
            request_line.contains("q=rust+async"),
            "the query terms pass through as `q`: {request_line}"
        );
        assert!(
            request_line.contains("count=10"),
            "count carries the requested limit: {request_line}"
        );
        assert!(
            request_line.contains("result_filter=web"),
            "only web results are requested: {request_line}"
        );
        assert!(
            request_line.contains("text_decorations=false"),
            "highlight decorations are suppressed: {request_line}"
        );
        let request_lowercase = request.to_ascii_lowercase();
        assert!(
            request_lowercase.contains(&format!("x-subscription-token: {SENTINEL_API_KEY}")),
            "the subscription token authenticates the request: {request}"
        );
        assert!(
            request_lowercase.contains("accept: application/json"),
            "{request}"
        );
        assert!(
            request_lowercase.contains("user-agent: ylva/"),
            "the shared user agent is sent: {request}"
        );

        let mut request_paths = std::fs::read_dir(temp_dir.path())
            .expect("dump directory")
            .map(|entry| entry.expect("dump entry").path())
            .filter(|path| path.to_string_lossy().ends_with("_request.jsonl"))
            .collect::<Vec<_>>();
        request_paths.sort();
        assert_eq!(request_paths.len(), 1);
        let request_contents = std::fs::read_to_string(&request_paths[0]).expect("request dump");
        let mut request_lines = request_contents.lines();
        let request_metadata: serde_json::Value =
            serde_json::from_str(request_lines.next().expect("request metadata"))
                .expect("metadata");
        assert_eq!(request_metadata["path"], "/res/v1/web/search");
        let query = request_metadata["query"]
            .as_str()
            .expect("the GET query string is dumped")
            .to_string();
        assert!(query.contains("count=10"), "{query}");
        assert!(
            request_metadata["headers"]["x-subscription-token"] == "<REDACTED>",
            "{request_metadata}"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(request_lines.next().expect("request body"))
                .expect("body"),
            serde_json::Value::Null,
            "a GET carries no body"
        );
        assert!(request_lines.next().is_none());
        assert!(
            !request_contents.contains(SENTINEL_API_KEY),
            "the token must never reach a dump artifact: {request_contents:?}"
        );

        let mut response_paths = std::fs::read_dir(temp_dir.path())
            .expect("dump directory")
            .map(|entry| entry.expect("dump entry").path())
            .filter(|path| path.to_string_lossy().ends_with("_response.json"))
            .collect::<Vec<_>>();
        response_paths.sort();
        assert_eq!(response_paths.len(), 1, "a JSON response dumps as .json");
        let response_contents = std::fs::read_to_string(&response_paths[0]).expect("response dump");
        assert_eq!(response_contents, RESULTS_JSON);
    }

    #[test]
    fn parses_hits_from_a_json_response() {
        let hits =
            parse_brave_results(RESULTS_JSON.as_bytes(), 10).expect("parses the JSON response");
        assert_eq!(hits.len(), 3, "entries without url/title are skipped");
        assert_eq!(hits[0].title, "Rust Programming Language");
        assert_eq!(hits[0].url, "https://rust-lang.org/");
        assert_eq!(
            hits[0].snippet, "A fast, reliable language.",
            "whitespace runs collapse to single spaces"
        );
        assert_eq!(
            hits[1].snippet, "",
            "a result without a description key yields an empty snippet"
        );
        assert_eq!(
            hits[2].snippet, "",
            "an empty description yields an empty snippet"
        );
    }

    #[test]
    fn result_order_follows_the_response_order() {
        let hits = parse_brave_results(RESULTS_JSON.as_bytes(), 10).expect("parses");
        let titles: Vec<&str> = hits.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Rust Programming Language", "The Rust Book", "Users forum"]
        );
    }

    #[test]
    fn parse_caps_results_at_the_requested_limit() {
        let hits = parse_brave_results(RESULTS_JSON.as_bytes(), 2).expect("parses");
        assert_eq!(hits.len(), 2, "limit must cap the parsed results");
    }

    #[test]
    fn absent_web_object_is_an_empty_answer() {
        let hits = parse_brave_results(br#"{"type": "web", "query": {"original": "x"}}"#, 10)
            .expect("an absent web object is a valid empty result");
        assert!(hits.is_empty());
    }

    #[test]
    fn valid_empty_results_are_an_empty_answer() {
        let hits = parse_brave_results(br#"{"web": {"results": []}}"#, 10)
            .expect("a genuine empty answer parses");
        assert!(hits.is_empty());
    }

    #[test]
    fn html_body_is_a_parse_error() {
        let error = parse_brave_results(HTML_BODY.as_bytes(), 10)
            .expect_err("a non-JSON body must fail fast");
        assert!(error.to_string().contains("not a Brave Search"), "{error}");
    }

    #[test]
    fn http_401_with_an_error_envelope_carries_the_detail() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dump_path = temp_dir.path().join("requests.jsonl");
        runtime().block_on(async {
            let backend = with_mock_server(
                |_| {
                    json_response(
                        "401 Unauthorized",
                        "application/json",
                        &error_envelope("invalid or revoked subscription token", "UNAUTHORIZED"),
                    )
                },
                &dump_path,
            )
            .await;
            let error = backend
                .search("anything", 5)
                .await
                .expect_err("401 must fail fast");
            assert!(
                matches!(&error, WebSearchError::StatusWithHint { status: 401, .. }),
                "a 401 must use the hint-carrying variant: {error}"
            );
            let message = error.to_string();
            assert!(
                message.contains("invalid or revoked subscription token"),
                "the hint must carry Brave's detail: {message}"
            );
        });
    }

    #[test]
    fn http_429_with_an_error_envelope_carries_the_detail() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dump_path = temp_dir.path().join("requests.jsonl");
        runtime().block_on(async {
            let backend = with_mock_server(
                |_| {
                    json_response(
                        "429 Too Many Requests",
                        "application/json",
                        &error_envelope("rate limit exceeded", "RATE_LIMITED"),
                    )
                },
                &dump_path,
            )
            .await;
            let error = backend
                .search("anything", 5)
                .await
                .expect_err("429 must fail fast");
            assert!(
                matches!(&error, WebSearchError::StatusWithHint { status: 429, .. }),
                "{error}"
            );
            let message = error.to_string();
            assert!(message.contains("rate limit exceeded"), "{message}");
        });
    }

    #[test]
    fn non_success_with_a_plain_text_body_is_a_status_error() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dump_path = temp_dir.path().join("requests.jsonl");
        runtime().block_on(async {
            let backend = with_mock_server(
                |_| {
                    json_response(
                        "503 Service Unavailable",
                        "text/plain",
                        "upstream proxy down",
                    )
                },
                &dump_path,
            )
            .await;
            let error = backend
                .search("anything", 5)
                .await
                .expect_err("503 must fail fast");
            assert!(
                matches!(&error, WebSearchError::Status { status: 503 }),
                "no envelope means no hint: {error}"
            );
        });
    }

    #[test]
    fn html_body_on_success_is_a_parse_error() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dump_path = temp_dir.path().join("requests.jsonl");
        runtime().block_on(async {
            let backend = with_mock_server(
                |_| json_response("200 OK", "text/html", HTML_BODY),
                &dump_path,
            )
            .await;
            let error = backend
                .search("anything", 5)
                .await
                .expect_err("an unparseable body must fail fast");
            assert!(
                matches!(&error, WebSearchError::Parse(_)),
                "a non-JSON 200 body is a parse error: {error}"
            );
        });
    }

    #[test]
    fn over_limit_query_is_passed_unchanged_and_surfaces_braves_detail() {
        let overlong_query = "a".repeat(601);
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dump_path = temp_dir.path().join("requests.jsonl");
        let observed = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let observed_for_server = observed.clone();

        runtime().block_on(async {
            let backend = with_mock_server(
                move |request| {
                    *observed_for_server.lock().expect("observed request") = request.clone();
                    json_response(
                        "422 Unprocessable Entity",
                        "application/json",
                        &error_envelope("q must be under 600 characters", "INVALID_QUERY"),
                    )
                },
                &dump_path,
            )
            .await;
            let error = backend
                .search(&overlong_query, 5)
                .await
                .expect_err("422 must fail fast");
            assert!(
                matches!(&error, WebSearchError::StatusWithHint { status: 422, .. }),
                "{error}"
            );
            let message = error.to_string();
            assert!(
                message.contains("q must be under 600 characters"),
                "the hint must carry Brave's detail: {message}"
            );
        });

        let request = observed.lock().expect("observed request").clone();
        assert!(
            request.contains(&format!("q={overlong_query}")),
            "the query must reach Brave unchanged, without local truncation: {request}"
        );
    }

    #[test]
    fn network_target_is_the_fixed_brave_authority() {
        let backend = BraveBackend::with_api_key(SENTINEL_API_KEY, Duration::from_secs(1));
        assert_eq!(backend.network_target(), "https://api.search.brave.com");
    }

    #[test]
    fn zero_limit_short_circuits_without_requesting() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dump_path = temp_dir.path().join("requests.jsonl");
        let backend = BraveBackend::with_api_key_and_base_url_and_dump_path(
            SENTINEL_API_KEY,
            "http://127.0.0.1:1",
            Duration::from_secs(1),
            &dump_path,
        );
        let hits = runtime()
            .block_on(backend.search("anything", 0))
            .expect("a zero limit must not hit the network");
        assert!(hits.is_empty());
    }

    #[test]
    fn debug_output_never_formats_the_api_key() {
        let backend = BraveBackend::with_api_key(SENTINEL_API_KEY, Duration::from_secs(1));
        let rendered = format!("{backend:?}");
        assert!(
            rendered.contains("BraveBackend"),
            "the type name must stay identifiable: {rendered}"
        );
        assert!(
            !rendered.contains(SENTINEL_API_KEY),
            "the API key must never appear in Debug output: {rendered}"
        );
    }
}
