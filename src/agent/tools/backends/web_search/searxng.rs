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
//! SearXNG backend for the [`WebSearchBackend`] seam: a form `POST` against
//! the instance's `/search` endpoint asking for `format=json`, parsed with
//! serde.
//!
//! **Why POST.** SearXNG's limiter counts requests whose `format` appears in
//! the *query string* against a tiny per-hour API budget (`API_MAX`), while a
//! form-encoded `format=json` in the body is not counted. Sending the same
//! parameters as a form body therefore keeps the backend inside the instance's
//! default rate budgets.
//!
//! **Required headers.** The instance's bot detection rejects requests that
//! lack them: `Accept: */*` (must cover `text/html`), `Accept-Encoding:
//! gzip`, a non-empty `Accept-Language`, and a non-`curl`/`python-requests`
//! user agent (the client's [`OUTBOUND_USER_AGENT`](crate::http::OUTBOUND_USER_AGENT)).
//!
//! Every request fails fast: a transport error, a non-success HTTP status
//! (including `403`, which means the instance disabled JSON output), an
//! unparseable body, or an empty result set blamed on unresponsive engines is
//! a [`WebSearchError`] — never a retry and never a silently-empty result set
//! on a failed response.

use std::path::Path;
use std::time::Duration;

use futures::future::BoxFuture;
use serde::Deserialize;

use super::{WebSearchBackend, WebSearchError, WebSearchHit};
use crate::http::OUTBOUND_USER_AGENT;

/// Hint attached to an HTTP `403`: SearXNG only answers `format=json` when
/// the instance lists it in `search.formats` of its `settings.yml`, and the
/// shipped default does not.
const JSON_FORMAT_DISABLED_HINT: &str =
    "the SearXNG instance likely has format=json disabled in its settings.yml (search.formats)";

// ── SearXNG backend ───────────────────────────────────────────────────────────

/// The SearXNG backend: a form `POST` against the configured instance's
/// `/search` endpoint, parsed from its JSON response shape.
#[derive(Debug)]
pub struct SearXngBackend {
    client: reqwest::Client,
    base_url: String,
    dump_source: crate::http::RequestDumpSource,
}

impl SearXngBackend {
    /// Build a backend against an explicit instance root (trailing slashes
    /// ignored). There is no default instance: the root comes from
    /// `[web_search.backends.searxng] base_url`.
    pub fn with_base_url(base_url: &str, http_timeout: Duration) -> Self {
        Self::with_base_url_and_dump_source(
            base_url,
            http_timeout,
            crate::http::RequestDumpSource::Environment,
        )
    }

    /// Build a backend against an explicit instance root and dump-path
    /// template.
    ///
    /// This keeps tests on the actual request/dump path without mutating the
    /// process environment; production callers use [`with_base_url`], which
    /// resolves `YLVA_REQUEST_DUMP_PATH` when each request starts.
    pub fn with_base_url_and_dump_path(
        base_url: &str,
        http_timeout: Duration,
        dump_path: &Path,
    ) -> Self {
        Self::with_base_url_and_dump_source(
            base_url,
            http_timeout,
            crate::http::RequestDumpSource::from_template(dump_path),
        )
    }

    fn with_base_url_and_dump_source(
        base_url: &str,
        http_timeout: Duration,
        dump_source: crate::http::RequestDumpSource,
    ) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(OUTBOUND_USER_AGENT)
            .timeout(http_timeout)
            .build()
            .expect("shared HTTP client construction must succeed");
        SearXngBackend {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            dump_source,
        }
    }
}

impl WebSearchBackend for SearXngBackend {
    fn network_target(&self) -> String {
        crate::http::network_target(&self.base_url).unwrap_or_else(|| self.base_url.clone())
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
            let url = format!("{}/search", self.base_url);
            tracing::debug!(backend = "searxng", terms = %terms, limit, "web search request");
            // Form body, not query string: see the module docs on the limiter.
            let form = [("q", terms), ("format", "json")];
            let request = self
                .client
                .post(&url)
                .header(reqwest::header::ACCEPT, "*/*")
                .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
                .header(reqwest::header::ACCEPT_ENCODING, "gzip")
                .form(&form)
                .build()
                .map_err(|error| WebSearchError::Request(error.to_string()))?;
            let request_url = request.url().to_string();
            let dump_context = crate::http::RequestDumpContext::new(&self.dump_source);
            let request_dump_path = dump_context.request_path();
            if let Some(path) = &request_dump_path {
                crate::http::dump_request(
                    &request_url,
                    &crate::http::redacted_headers_json(request.headers()),
                    &serde_json::json!({
                        "q": terms,
                        "format": "json",
                    }),
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
                let status_code = status.as_u16();
                return Err(if status == reqwest::StatusCode::FORBIDDEN {
                    WebSearchError::StatusWithHint {
                        status: status_code,
                        hint: JSON_FORMAT_DISABLED_HINT.to_string(),
                    }
                } else {
                    WebSearchError::Status {
                        status: status_code,
                    }
                });
            }
            parse_searxng_results(&body, limit)
        })
    }
}

// ── Response parsing ──────────────────────────────────────────────────────────

/// The JSON body SearXNG answers a `format=json` search with.
#[derive(Debug, Deserialize)]
struct SearXngSearchResponse {
    /// Ranked results; empty when the instance found nothing or every engine
    /// failed.
    #[serde(default)]
    results: Vec<SearXngResult>,
    /// `[engine, reason]` pairs for engines that failed this query.
    #[serde(default)]
    unresponsive_engines: Vec<(String, String)>,
    /// Present instead of `results` when the instance rejects the request
    /// (e.g. an unknown parameter).
    #[serde(default)]
    error: Option<String>,
}

/// One entry of [`SearXngSearchResponse::results`].
#[derive(Debug, Deserialize)]
struct SearXngResult {
    /// Absolute URL of the result page.
    #[serde(default)]
    url: String,
    /// Title of the result page.
    #[serde(default)]
    title: String,
    /// Excerpt; absent when the engine provided none.
    #[serde(default)]
    content: Option<String>,
}

/// Parse a SearXNG JSON search response into at most `limit` hits.
///
/// Anything that is not the expected JSON shape, an instance-level `error`,
/// or an empty result set explained by unresponsive engines is a parse
/// failure, never an empty result set — a silently-empty answer would tell
/// the model "nothing was found" when the backend actually failed. Only an
/// empty `results` array *without* engine failures is a genuine empty answer.
fn parse_searxng_results(body: &[u8], limit: usize) -> Result<Vec<WebSearchHit>, WebSearchError> {
    let response: SearXngSearchResponse = serde_json::from_slice(body).map_err(|error| {
        WebSearchError::Parse(format!(
            "the response body is not a SearXNG JSON search response: {error}"
        ))
    })?;
    if let Some(message) = response
        .error
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Err(WebSearchError::Parse(format!(
            "the SearXNG instance reported an error: {message}"
        )));
    }
    if response.results.is_empty() && !response.unresponsive_engines.is_empty() {
        let engines = response
            .unresponsive_engines
            .iter()
            .map(|(engine, reason)| format!("{engine}: {reason}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(WebSearchError::Parse(format!(
            "the SearXNG instance returned no results; unresponsive engines — {engines}"
        )));
    }
    let hits = response
        .results
        .iter()
        .filter_map(|result| {
            let title = normalize_whitespace(&result.title);
            let url = result.url.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = normalize_whitespace(result.content.as_deref().unwrap_or_default());
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

/// Collapse every run of whitespace into a single space and trim the ends, so
/// engine-provided text (titles, snippets) is single-line and
/// comparison-stable.
fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `handler` as a one-shot HTTP server and point a backend at it.
    ///
    /// `handler` receives the raw request bytes and returns the raw response
    /// bytes, so every test asserts the exact wire shape the backend produces.
    async fn with_mock_server<F>(handler: F, dump_path: &Path) -> SearXngBackend
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
                assert!(bytes_read > 0, "request must contain a complete body");
                request.extend_from_slice(&buffer[..bytes_read]);
                let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = std::str::from_utf8(&request[..header_end])
                    .expect("request headers are UTF-8")
                    .to_ascii_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .expect("form request must have a content length");
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let request_text = String::from_utf8_lossy(&request).into_owned();
            let response = handler(request_text);
            stream.write_all(&response).await.expect("write response");
        });

        SearXngBackend::with_base_url_and_dump_path(
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

    #[test]
    fn searxng_posts_form_json_to_search_and_dumps_the_exchange() {
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
        let request_lowercase = request.to_ascii_lowercase();
        assert!(
            request_lowercase.starts_with("post /search http/1.1"),
            "SearXNG must receive a form POST to /search without a query string: {request}"
        );
        assert!(request_lowercase.contains("accept: */*"), "{request}");
        assert!(
            request_lowercase.contains("accept-language: en-us,en;q=0.9"),
            "{request}"
        );
        assert!(
            request_lowercase.contains("accept-encoding: gzip"),
            "{request}"
        );
        assert!(
            request_lowercase.contains("user-agent: ylva/"),
            "the bot detection rejects unknown or missing user agents: {request}"
        );
        assert!(
            request_lowercase.contains("content-type: application/x-www-form-urlencoded"),
            "{request}"
        );
        assert!(
            request.ends_with("q=rust+async&format=json"),
            "form fields are q and format: {request}"
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
        assert_eq!(request_metadata["path"], "/search");
        assert!(request_metadata["query"].is_null());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(request_lines.next().expect("request body"))
                .expect("body"),
            serde_json::json!({ "q": "rust async", "format": "json" })
        );
        assert!(request_lines.next().is_none());

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

    const RESULTS_JSON: &str = r#"{
  "query": "rust async",
  "results": [
    {"url": "https://rust-lang.org/", "title": "Rust Programming Language", "content": "A   fast,\nreliable language."},
    {"url": "https://doc.rust-lang.org/book/", "title": "The Rust Book", "content": ""},
    {"url": "https://users-rust-lang.org/", "title": "Users forum"},
    {"url": "", "title": "Skipped: empty url"},
    {"url": "https://no-title.example/", "title": "   "}
  ],
  "answers": [],
  "corrections": [],
  "infoboxes": [],
  "suggestions": [],
  "unresponsive_engines": []
}"#;

    /// A `200` body that is not the SearXNG JSON shape.
    const HTML_BODY: &str = r#"<html><body><p>Service temporarily unavailable.</p></body></html>"#;

    #[test]
    fn parses_hits_from_a_json_response() {
        let hits =
            parse_searxng_results(RESULTS_JSON.as_bytes(), 10).expect("parses the JSON response");
        assert_eq!(hits.len(), 3, "entries without url/title are skipped");
        assert_eq!(hits[0].title, "Rust Programming Language");
        assert_eq!(hits[0].url, "https://rust-lang.org/");
        assert_eq!(
            hits[0].snippet, "A fast, reliable language.",
            "whitespace runs collapse to single spaces"
        );
        assert_eq!(
            hits[1].snippet, "",
            "an empty content yields an empty snippet"
        );
        assert_eq!(
            hits[2].snippet, "",
            "a result without a content key yields an empty snippet"
        );
    }

    #[test]
    fn result_order_follows_the_response_order() {
        let hits = parse_searxng_results(RESULTS_JSON.as_bytes(), 10).expect("parses");
        let titles: Vec<&str> = hits.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Rust Programming Language", "The Rust Book", "Users forum"]
        );
    }

    #[test]
    fn parse_caps_results_at_the_requested_limit() {
        let hits = parse_searxng_results(RESULTS_JSON.as_bytes(), 2).expect("parses");
        assert_eq!(hits.len(), 2, "limit must cap the parsed results");
    }

    #[test]
    fn valid_empty_results_are_an_empty_answer() {
        let hits = parse_searxng_results(br#"{"query": "x", "results": []}"#, 10)
            .expect("a genuine empty answer parses");
        assert!(hits.is_empty());
    }

    #[test]
    fn html_body_is_a_parse_error() {
        let error = parse_searxng_results(HTML_BODY.as_bytes(), 10)
            .expect_err("a non-JSON body must fail fast");
        assert!(error.to_string().contains("not a SearXNG JSON"), "{error}");
    }

    #[test]
    fn instance_error_object_is_a_parse_error_carrying_the_message() {
        let error = parse_searxng_results(br#"{"error": "Invalid format"}"#, 10)
            .expect_err("an instance error must fail fast");
        let message = error.to_string();
        assert!(message.contains("Invalid format"), "{message}");
    }

    #[test]
    fn empty_results_with_unresponsive_engines_is_a_parse_error_naming_them() {
        let error = parse_searxng_results(
            br#"{"results": [], "unresponsive_engines": [["brave", "too many requests"], ["duckduckgo", "captchas"]]}"#,
            10,
        )
        .expect_err("an engine-blamed empty answer must fail fast");
        let message = error.to_string();
        assert!(message.contains("brave: too many requests"), "{message}");
        assert!(message.contains("duckduckgo: captchas"), "{message}");
    }

    #[test]
    fn http_403_carries_the_json_format_hint() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dump_path = temp_dir.path().join("requests.jsonl");
        runtime().block_on(async {
            let backend = with_mock_server(
                |_| {
                    json_response(
                        "403 Forbidden",
                        "text/html",
                        "<html><body>403 Forbidden - the format is not allowed</body></html>",
                    )
                },
                &dump_path,
            )
            .await;
            let error = backend
                .search("anything", 5)
                .await
                .expect_err("403 must fail fast");
            assert!(
                matches!(&error, WebSearchError::StatusWithHint { status: 403, .. }),
                "a 403 must use the hint-carrying variant: {error}"
            );
            let message = error.to_string();
            assert!(
                message.contains("format=json") && message.contains("settings.yml"),
                "the hint must name the disabled JSON format: {message}"
            );
        });
    }

    #[test]
    fn http_429_is_a_status_error() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dump_path = temp_dir.path().join("requests.jsonl");
        runtime().block_on(async {
            let backend = with_mock_server(
                |_| json_response("429 Too Many Requests", "text/plain", ""),
                &dump_path,
            )
            .await;
            let error = backend
                .search("anything", 5)
                .await
                .expect_err("429 must fail fast");
            assert!(
                matches!(&error, WebSearchError::Status { status: 429 }),
                "{error}"
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
    fn network_target_is_the_configured_instance_authority() {
        let backend = SearXngBackend::with_base_url(
            "https://searx.example.org/searxng/",
            Duration::from_secs(1),
        );
        assert_eq!(backend.network_target(), "https://searx.example.org");
        let with_port =
            SearXngBackend::with_base_url("http://127.0.0.1:8080", Duration::from_secs(1));
        assert_eq!(with_port.network_target(), "http://127.0.0.1:8080");
    }

    #[test]
    fn zero_limit_short_circuits_without_requesting() {
        let backend = SearXngBackend::with_base_url("http://127.0.0.1:1", Duration::from_secs(1));
        let hits = runtime()
            .block_on(backend.search("anything", 0))
            .expect("a zero limit must not hit the network");
        assert!(hits.is_empty());
    }
}
