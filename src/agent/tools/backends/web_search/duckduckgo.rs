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
//! DuckDuckGo HTML backend for the [`WebSearchBackend`] seam: a form
//! `POST` against the endpoint, using the request shape accepted by
//! `ddgr`, and parsed with `scraper`.
//!
//! Every request fails fast: a transport error, a non-success HTTP status
//! (including bot-detection/captcha responses), or an unparseable body is a
//! [`WebSearchError`] — never a retry and never a silently-empty result set.

use std::path::Path;
use std::time::Duration;

use futures::future::BoxFuture;

use super::{WebSearchBackend, WebSearchError, WebSearchHit};
use crate::http::OUTBOUND_USER_AGENT;

// ── DuckDuckGo HTML backend ───────────────────────────────────────────────────

/// The DuckDuckGo HTML backend: a form `POST` against the endpoint, using the
/// request shape accepted by `ddgr`, and parsed with `scraper`.
#[derive(Debug)]
pub struct DuckDuckGoBackend {
    client: reqwest::Client,
    base_url: String,
    dump_source: crate::http::RequestDumpSource,
}

impl DuckDuckGoBackend {
    /// Production endpoint root; tests override it via
    /// [`with_base_url`](Self::with_base_url).
    pub const DEFAULT_BASE_URL: &'static str = "https://html.duckduckgo.com";

    /// Build a backend against [`DEFAULT_BASE_URL`]. `http_timeout` bounds a
    /// single request (the tool's outer timeout must exceed it).
    pub fn new(http_timeout: Duration) -> Self {
        Self::with_base_url_and_dump_source(
            Self::DEFAULT_BASE_URL,
            http_timeout,
            crate::http::RequestDumpSource::Environment,
        )
    }

    /// Build a backend against an explicit endpoint root (trailing slashes
    /// ignored). The test hook points the real `reqwest` path at a local mock
    /// server; production callers use [`new`](Self::new).
    pub fn with_base_url(base_url: &str, http_timeout: Duration) -> Self {
        Self::with_base_url_and_dump_source(
            base_url,
            http_timeout,
            crate::http::RequestDumpSource::Environment,
        )
    }

    /// Build a backend against an explicit endpoint and dump-path template.
    ///
    /// This keeps tests on the actual request/dump path without mutating the
    /// process environment; production callers normally use [`Self::new`],
    /// which resolves `YLVA_REQUEST_DUMP_PATH` when each request starts.
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
        DuckDuckGoBackend {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            dump_source,
        }
    }
}

impl WebSearchBackend for DuckDuckGoBackend {
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
            let url = format!("{}/html", self.base_url);
            tracing::debug!(backend = "duckduckgo", terms = %terms, limit, "web search request");
            let form = [
                ("q", terms),
                ("b", ""),
                ("df", ""),
                ("kf", "-1"),
                ("kh", "1"),
                ("kl", ""),
                ("kp", "1"),
                ("k1", "-1"),
            ];
            let request = self
                .client
                .post(&url)
                .header(reqwest::header::ACCEPT_ENCODING, "gzip")
                .header("DNT", "1")
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
                        "b": "",
                        "df": "",
                        "kf": "-1",
                        "kh": "1",
                        "kl": "",
                        "kp": "1",
                        "k1": "-1",
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
                return Err(WebSearchError::Status {
                    status: status.as_u16(),
                });
            }
            let body = String::from_utf8_lossy(&body);
            parse_duckduckgo_results(&body, limit)
        })
    }
}

// ── Response parsing ──────────────────────────────────────────────────────────

/// Parse a DuckDuckGo HTML results page into at most `limit` hits.
///
/// DuckDuckGo answers a bot-detection challenge with HTTP `202` (a 2xx, so
/// the status check alone cannot catch it): a body without recognizable
/// results is therefore a parse failure, never an empty result set — a
/// silently-empty answer would tell the model "nothing was found" when the
/// backend actually refused the query. The one exception is a body carrying
/// DuckDuckGo's own no-results marker, which is a genuine empty answer.
fn parse_duckduckgo_results(html: &str, limit: usize) -> Result<Vec<WebSearchHit>, WebSearchError> {
    let document = scraper::Html::parse_document(html);
    let result_selector = parse_selector("div.result")?;
    let link_selector = parse_selector("a.result__a")?;
    let snippet_selector = parse_selector(".result__snippet")?;

    let mut hits = Vec::new();
    for result in document.select(&result_selector) {
        if hits.len() >= limit {
            break;
        }
        let Some(link) = result.select(&link_selector).next() else {
            continue;
        };
        let href = link.attr("href").unwrap_or_default();
        let title = normalize_whitespace(&link.text().collect::<String>());
        if title.is_empty() || href.is_empty() {
            continue;
        }
        let snippet = result
            .select(&snippet_selector)
            .next()
            .map(|snippet| normalize_whitespace(&snippet.text().collect::<String>()))
            .unwrap_or_default();
        hits.push(WebSearchHit {
            title,
            url: resolve_result_url(href),
            snippet,
        });
    }

    if !hits.is_empty() {
        return Ok(hits);
    }
    let anomaly_selector = parse_selector(".anomaly-modal")?;
    if document.select(&anomaly_selector).next().is_some() {
        return Err(WebSearchError::Parse(
            "the backend returned a bot-detection challenge instead of results".to_string(),
        ));
    }
    let no_results_selector = parse_selector(".no-results, .no-results__message")?;
    if document.select(&no_results_selector).next().is_some() {
        return Ok(Vec::new());
    }
    Err(WebSearchError::Parse(
        "the response carried no recognizable results (possible block page)".to_string(),
    ))
}

/// Parse a CSS selector that is a compile-time constant in this module; a
/// failure is a programming bug, not an execution-time condition.
fn parse_selector(css: &str) -> Result<scraper::Selector, WebSearchError> {
    scraper::Selector::parse(css)
        .map_err(|error| WebSearchError::Parse(format!("invalid selector '{css}': {error}")))
}

/// Resolve a result anchor's `href` to the page it points at.
///
/// DuckDuckGo wraps external links in its own redirect
/// (`//duckduckgo.com/l/?uddg=<percent-encoded-url>`); unwrap the `uddg`
/// parameter so the model sees the real destination. Anything else is used
/// as-is, with a protocol-relative `//…` form given an `https:` scheme.
fn resolve_result_url(href: &str) -> String {
    let absolute = match href.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => href.to_string(),
    };
    if let Ok(parsed) = reqwest::Url::parse(&absolute) {
        if let Some((_, target)) = parsed.query_pairs().find(|(key, _)| key == "uddg") {
            if !target.is_empty() {
                return target.into_owned();
            }
        }
    }
    absolute
}

/// Collapse every run of whitespace into a single space and trim the ends, so
/// markup-derived text (titles, snippets) is single-line and comparison-stable.
fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duckduckgo_posts_ddgr_shape_and_dumps_html_responses() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dump_path = temp_dir.path().join("requests.jsonl");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build Tokio runtime");

        runtime.block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            use tokio::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
            let address = listener.local_addr().expect("server address");
            let server = tokio::spawn(async move {
                use std::io::Write;

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
                let request_text = std::str::from_utf8(&request).expect("request is UTF-8");
                let request_lowercase = request_text.to_ascii_lowercase();
                assert!(
                    request_lowercase.starts_with("post /html http/1.1"),
                    "DuckDuckGo must receive a form POST to /html: {request_text}"
                );
                assert!(request_lowercase.contains("accept-encoding: gzip"));
                assert!(request_lowercase.contains("dnt: 1"));
                assert!(request_lowercase.contains("content-type: application/x-www-form-urlencoded"));
                assert!(
                    request_text.ends_with("q=rust+async&b=&df=&kf=-1&kh=1&kl=&kp=1&k1=-1"),
                    "DuckDuckGo form fields must match ddgr: {request_text}"
                );
                let mut encoder = flate2::write::GzEncoder::new(
                    Vec::new(),
                    flate2::Compression::default(),
                );
                encoder
                    .write_all(RESULTS_HTML.as_bytes())
                    .expect("compress response");
                let compressed_response = encoder.finish().expect("finish response");
                let response_headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    compressed_response.len()
                );
                stream
                    .write_all(response_headers.as_bytes())
                    .await
                    .expect("write response headers");
                stream
                    .write_all(&compressed_response)
                    .await
                    .expect("write response body");
            });

            let backend = DuckDuckGoBackend::with_base_url_and_dump_path(
                &format!("http://{address}"),
                Duration::from_secs(1),
                &dump_path,
            );
            let hits = backend
                .search("rust async", 10)
                .await
                .expect("search response");
            assert_eq!(hits.len(), 3);
            server.await.expect("server task");
        });

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
        assert_eq!(request_metadata["path"], "/html");
        assert!(request_metadata["query"].is_null());
        assert_eq!(request_metadata["headers"]["accept-encoding"], "gzip");
        assert_eq!(request_metadata["headers"]["dnt"], "1");
        assert_eq!(
            request_metadata["headers"]["content-type"],
            "application/x-www-form-urlencoded"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(request_lines.next().expect("request body"))
                .expect("body"),
            serde_json::json!({
                "q": "rust async",
                "b": "",
                "df": "",
                "kf": "-1",
                "kh": "1",
                "kl": "",
                "kp": "1",
                "k1": "-1",
            })
        );
        assert!(request_lines.next().is_none());

        let mut response_paths = std::fs::read_dir(temp_dir.path())
            .expect("dump directory")
            .map(|entry| entry.expect("dump entry").path())
            .filter(|path| path.to_string_lossy().ends_with("_response.html"))
            .collect::<Vec<_>>();
        response_paths.sort();
        assert_eq!(response_paths.len(), 1);
        let response_contents = std::fs::read_to_string(&response_paths[0]).expect("response dump");
        assert_eq!(response_contents, RESULTS_HTML);
    }

    const RESULTS_HTML: &str = r#"
<html><body>
<div id="links" class="results">
  <div class="result results_links results_links_deep web-result ">
    <div class="links_main links_deep result__body">
      <h2 class="result__title">
        <a rel="nofollow" class="result__a"
           href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust%2Dlang.org%2F&rut=abc">Rust Programming Language</a>
      </h2>
      <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust%2Dlang.org%2F">
        <b>Rust</b> is a   fast, reliable language.
      </a>
    </div>
  </div>
  <div class="result results_links results_links_deep web-result ">
    <div class="links_main links_deep result__body">
      <h2 class="result__title">
        <a rel="nofollow" class="result__a" href="https://doc.rust-lang.org/book/">The Rust Book</a>
      </h2>
    </div>
  </div>
  <div class="result results_links results_links_deep web-result ">
    <div class="links_main links_deep result__body">
      <h2 class="result__title">
        <a rel="nofollow" class="result__a"
           href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fusers%2Drust%2Dlang%2Forg%2F">Users forum</a>
      </h2>
      <a class="result__snippet">   </a>
    </div>
  </div>
</div>
</body></html>
"#;

    /// DuckDuckGo's bot-detection challenge (answered with HTTP `202`).
    const ANOMALY_HTML: &str = r#"
<html><body>
<div class="anomaly-modal__mask"></div>
<div class="anomaly-modal">
  <div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>
</div>
</body></html>
"#;

    /// A body carrying DuckDuckGo's explicit no-results marker.
    const NO_RESULTS_HTML: &str = r#"
<html><body>
<div class="results">
  <div class="no-results__message">No results.</div>
</div>
</body></html>
"#;

    /// A `200` body that is neither results nor a recognized empty answer.
    const UNEXPECTED_HTML: &str =
        r#"<html><body><p>Service temporarily unavailable.</p></body></html>"#;

    #[test]
    fn parses_titles_urls_and_snippets_from_a_results_page() {
        let hits = parse_duckduckgo_results(RESULTS_HTML, 10).expect("parses");
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].title, "Rust Programming Language");
        assert_eq!(
            hits[0].url, "https://rust-lang.org/",
            "a redirect href must unwrap to its uddg target"
        );
        assert_eq!(
            hits[0].snippet, "Rust is a fast, reliable language.",
            "markup is flattened to its text and whitespace runs collapse"
        );
        assert_eq!(hits[1].title, "The Rust Book");
        assert_eq!(
            hits[1].url, "https://doc.rust-lang.org/book/",
            "an absolute href passes through"
        );
        assert_eq!(
            hits[1].snippet, "",
            "a result without a snippet yields an empty snippet"
        );
        assert_eq!(
            hits[2].snippet, "",
            "whitespace-only snippets normalize to empty"
        );
    }

    #[test]
    fn result_order_follows_document_order() {
        let hits = parse_duckduckgo_results(RESULTS_HTML, 10).expect("parses");
        let titles: Vec<&str> = hits.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Rust Programming Language", "The Rust Book", "Users forum"]
        );
    }

    #[test]
    fn parse_caps_results_at_the_requested_limit() {
        let hits = parse_duckduckgo_results(RESULTS_HTML, 2).expect("parses");
        assert_eq!(hits.len(), 2, "limit must cap the parsed results");
    }

    #[test]
    fn bot_detection_page_is_a_parse_error_not_an_empty_result() {
        let error = parse_duckduckgo_results(ANOMALY_HTML, 10)
            .expect_err("a challenge page must fail fast");
        assert!(
            error.to_string().contains("bot-detection"),
            "the error must name the challenge: {error}"
        );
    }

    #[test]
    fn explicit_no_results_marker_yields_an_empty_result() {
        let hits = parse_duckduckgo_results(NO_RESULTS_HTML, 10).expect("parses");
        assert!(hits.is_empty());
    }

    #[test]
    fn unrecognizable_body_is_a_parse_error() {
        let error =
            parse_duckduckgo_results(UNEXPECTED_HTML, 10).expect_err("unrecognized markup fails");
        assert!(
            error.to_string().contains("no recognizable results"),
            "{error}"
        );
    }

    #[test]
    fn resolve_result_url_unwraps_the_duckduckgo_redirect() {
        assert_eq!(
            resolve_result_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpath&rut=x"),
            "https://example.com/path"
        );
    }

    #[test]
    fn resolve_result_url_keeps_plain_and_protocol_relative_targets() {
        assert_eq!(
            resolve_result_url("https://example.com/a?b=1"),
            "https://example.com/a?b=1"
        );
        assert_eq!(
            resolve_result_url("//example.com/a"),
            "https://example.com/a"
        );
        assert_eq!(
            resolve_result_url("//duckduckgo.com/l/?rut=only"),
            "https://duckduckgo.com/l/?rut=only",
            "a redirect without a uddg target keeps its own URL"
        );
    }

    #[test]
    fn duckduckgo_network_target_is_the_endpoint_authority() {
        let backend = DuckDuckGoBackend::new(Duration::from_secs(1));
        assert_eq!(backend.network_target(), "https://html.duckduckgo.com");
        let custom =
            DuckDuckGoBackend::with_base_url("http://127.0.0.1:8080/html", Duration::from_secs(1));
        assert_eq!(custom.network_target(), "http://127.0.0.1:8080");
    }

    #[test]
    #[ignore = "requires live DuckDuckGo access"]
    fn live_duckduckgo_search_returns_results() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build Tokio runtime");
        let backend = DuckDuckGoBackend::new(Duration::from_secs(30));
        let result =
            runtime.block_on(backend.search("search engine api structured data json limits", 3));
        let hits = result.expect("live DuckDuckGo search must return results");
        assert!(
            !hits.is_empty(),
            "live DuckDuckGo search returned no results"
        );
    }

    #[test]
    fn zero_limit_short_circuits_without_requesting() {
        let backend =
            DuckDuckGoBackend::with_base_url("http://127.0.0.1:1", Duration::from_secs(1));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build Tokio runtime");
        let hits = runtime
            .block_on(backend.search("anything", 0))
            .expect("a zero limit must not hit the network");
        assert!(hits.is_empty());
    }
}
