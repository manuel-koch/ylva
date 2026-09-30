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

//! Integration tests for [`HttpFetchBackend`] against a local mock HTTP
//! server on `127.0.0.1` (raw `tokio::net::TcpListener`, the same pattern as
//! `tests/common/mock_llm_server.rs`). No test touches the external network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ylva::agent::tools::backends::web_fetch::{
    HttpFetchBackend, WebFetchBackend, WebFetchBody, WebFetchError, WebFetchResponse,
    WEB_FETCH_MAX_OUTPUT_BYTES,
};
use ylva::agent::tools::read::READ_MEDIA_MAX_BYTES;

// ── Local mock HTTP server ────────────────────────────────────────────────────

/// A canned raw HTTP response served verbatim for one request path.
#[derive(Debug, Clone)]
struct MockRoute {
    /// Status line parts; only `200` and error statuses used by the tests.
    status_line: &'static str,
    /// Extra `Name: value` header lines (before the terminating empty line).
    headers: Vec<String>,
    /// Body bytes exactly as sent (length taken from `body.len()` when no
    /// explicit `Content-Length` header is given in `headers`).
    body: Vec<u8>,
}

impl MockRoute {
    fn ok(content_type: &str, body: Vec<u8>) -> Self {
        MockRoute {
            status_line: "HTTP/1.1 200 OK",
            headers: vec![format!("Content-Type: {content_type}")],
            body,
        }
    }

    fn with_status(status_line: &'static str, content_type: &str, body: Vec<u8>) -> Self {
        MockRoute {
            status_line,
            headers: vec![format!("Content-Type: {content_type}")],
            body,
        }
    }

    fn with_header(mut self, header: String) -> Self {
        self.headers.push(header);
        self
    }

    fn into_raw_response(self) -> Vec<u8> {
        let mut response = format!("{}\r\n", self.status_line).into_bytes();
        let has_content_length = self
            .headers
            .iter()
            .any(|header| header.to_ascii_lowercase().starts_with("content-length:"));
        for header in &self.headers {
            response.extend_from_slice(format!("{header}\r\n").as_bytes());
        }
        if !has_content_length {
            response
                .extend_from_slice(format!("Content-Length: {}\r\n", self.body.len()).as_bytes());
        }
        // One request per connection keeps the mock stateless and avoids
        // connection-reuse races between tests.
        response.extend_from_slice(b"Connection: close\r\n\r\n");
        response.extend_from_slice(&self.body);
        response
    }
}

/// Path-routed mock server: each connection reads one request head, records
/// it, and serves the canned response for the request path.
struct MockHttpServer {
    base_url: String,
    /// Recorded request heads (method line + headers), in arrival order.
    requests: Arc<Mutex<Vec<String>>>,
}

impl MockHttpServer {
    async fn start(routes: HashMap<String, MockRoute>) -> Self {
        Self::start_with(|_| routes).await
    }

    /// Start with routes that depend on the (random) base URL — needed for
    /// absolute `Location` headers in redirect responses.
    async fn start_with(build_routes: impl FnOnce(&str) -> HashMap<String, MockRoute>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock server binds");
        let address = listener.local_addr().expect("mock server address");
        let base_url = format!("http://{address}");
        let routes = build_routes(&base_url);
        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let routes = routes.clone();
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut head = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                        let Ok(read_bytes) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if read_bytes == 0 {
                            return;
                        }
                        head.extend_from_slice(&chunk[..read_bytes]);
                    }
                    let head_text = String::from_utf8_lossy(&head).to_string();
                    let path = head_text
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .split('?')
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    recorded.lock().expect("requests lock").push(head_text);
                    let response = routes
                        .get(&path)
                        .cloned()
                        .unwrap_or_else(|| {
                            MockRoute::with_status(
                                "HTTP/1.1 404 Not Found",
                                "text/plain",
                                b"missing route".to_vec(),
                            )
                        })
                        .into_raw_response();
                    let _ = socket.write_all(&response).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        MockHttpServer { base_url, requests }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn recorded_requests(&self) -> Vec<String> {
        self.requests.lock().expect("requests lock").clone()
    }
}

fn png_bytes() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&[0u8; 16]);
    bytes
}

fn backend() -> HttpFetchBackend {
    HttpFetchBackend::new(Duration::from_secs(30))
}

async fn fetch(url: &str) -> Result<WebFetchResponse, WebFetchError> {
    backend().fetch(url).await
}

fn text_body(response: &WebFetchResponse) -> &str {
    match &response.body {
        WebFetchBody::Text { text } => text.as_str(),
        WebFetchBody::Media { media_type, .. } => {
            panic!("expected a text body, got media '{media_type}'")
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn html_fetch_is_converted_to_markdown() {
    let mut routes = HashMap::new();
    routes.insert(
        "/page".to_string(),
        MockRoute::ok(
            "text/html; charset=utf-8",
            b"<html><body><h1>Docs</h1><p>Hello <b>world</b></p></body></html>".to_vec(),
        ),
    );
    let server = MockHttpServer::start(routes).await;

    let response = fetch(&server.url("/page")).await.expect("html fetch");
    let text = text_body(&response);
    assert!(text.contains("Docs"), "{text}");
    assert!(text.contains("world"), "{text}");
    assert!(!text.contains("<h1>"), "markup is stripped: {text}");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert!(!response.truncated);
    assert_eq!(response.final_url, server.url("/page"));
}

#[tokio::test]
async fn json_and_xml_pass_through_verbatim() {
    let mut routes = HashMap::new();
    routes.insert(
        "/data.json".to_string(),
        MockRoute::ok("application/json", br#"{"answer": 42}"#.to_vec()),
    );
    routes.insert(
        "/feed.xml".to_string(),
        MockRoute::ok("application/xml", b"<feed><item/></feed>".to_vec()),
    );
    let server = MockHttpServer::start(routes).await;

    let json = fetch(&server.url("/data.json")).await.expect("json fetch");
    assert_eq!(text_body(&json), r#"{"answer": 42}"#);
    let xml = fetch(&server.url("/feed.xml")).await.expect("xml fetch");
    assert_eq!(text_body(&xml), "<feed><item/></feed>");
}

#[tokio::test]
async fn png_is_returned_as_media_with_the_sniffed_media_type() {
    let mut routes = HashMap::new();
    routes.insert(
        "/image".to_string(),
        // A lying Content-Type proves magic numbers win over headers.
        MockRoute::ok("text/html", png_bytes()),
    );
    let server = MockHttpServer::start(routes).await;

    let response = fetch(&server.url("/image")).await.expect("png fetch");
    match &response.body {
        WebFetchBody::Media { bytes, media_type } => {
            assert_eq!(bytes, &png_bytes());
            assert_eq!(media_type, "image/png");
        }
        other => panic!("expected media, got {other:?}"),
    }
    assert!(!response.truncated, "media is never truncated");
}

#[tokio::test]
async fn non_success_status_is_an_error_naming_url_and_status() {
    let mut routes = HashMap::new();
    routes.insert(
        "/missing".to_string(),
        MockRoute::with_status("HTTP/1.1 404 Not Found", "text/plain", b"gone".to_vec()),
    );
    let server = MockHttpServer::start(routes).await;

    match fetch(&server.url("/missing")).await {
        Err(WebFetchError::Status { url, status }) => {
            assert_eq!(status, 404);
            assert_eq!(url, server.url("/missing"));
        }
        other => panic!("expected a status error, got {other:?}"),
    }
}

#[tokio::test]
async fn unrecognized_binary_fails_with_an_unsupported_content_error() {
    let mut routes = HashMap::new();
    routes.insert(
        "/blob".to_string(),
        MockRoute::ok("application/octet-stream", b"\x00\x01\x02\x03\x04".to_vec()),
    );
    let server = MockHttpServer::start(routes).await;

    match fetch(&server.url("/blob")).await {
        Err(WebFetchError::UnsupportedContentType { detail }) => {
            assert!(detail.contains("application/octet-stream"), "{detail}");
        }
        other => panic!("expected an unsupported-content error, got {other:?}"),
    }
}

#[tokio::test]
async fn oversized_media_with_content_length_fails_fast_as_too_large() {
    let mut body = png_bytes();
    body.resize(256, 0);
    let mut routes = HashMap::new();
    routes.insert(
        "/huge.png".to_string(),
        MockRoute::ok("image/png", body).with_header(format!(
            "Content-Length: {}",
            READ_MEDIA_MAX_BYTES as u64 * 3
        )),
    );
    let server = MockHttpServer::start(routes).await;

    match fetch(&server.url("/huge.png")).await {
        Err(WebFetchError::TooLarge { size, limit }) => {
            assert_eq!(size, READ_MEDIA_MAX_BYTES * 3);
            assert_eq!(limit, READ_MEDIA_MAX_BYTES);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[tokio::test]
async fn oversized_media_streamed_over_the_limit_is_too_large() {
    let mut body = png_bytes();
    body.resize(READ_MEDIA_MAX_BYTES + 128, 0x2a);
    let body_len = body.len();
    let mut routes = HashMap::new();
    routes.insert(
        "/streamed.png".to_string(),
        MockRoute::ok("image/png", body),
    );
    let server = MockHttpServer::start(routes).await;

    match fetch(&server.url("/streamed.png")).await {
        Err(WebFetchError::TooLarge { size, limit }) => {
            assert_eq!(size, body_len);
            assert_eq!(limit, READ_MEDIA_MAX_BYTES);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[tokio::test]
async fn text_over_the_raw_limit_is_truncated_and_flagged() {
    let body = "a".repeat(READ_MEDIA_MAX_BYTES + 128);
    let mut routes = HashMap::new();
    routes.insert(
        "/long.txt".to_string(),
        MockRoute::ok("text/plain", body.into_bytes()),
    );
    let server = MockHttpServer::start(routes).await;

    let response = fetch(&server.url("/long.txt")).await.expect("text fetch");
    assert!(response.truncated, "raw limit cut the body");
    assert_eq!(
        text_body(&response).len(),
        WEB_FETCH_MAX_OUTPUT_BYTES,
        "output cap bounds the returned text"
    );
}

#[tokio::test]
async fn text_over_the_output_cap_is_truncated_and_flagged() {
    let body = "b".repeat(WEB_FETCH_MAX_OUTPUT_BYTES * 2);
    let mut routes = HashMap::new();
    routes.insert(
        "/wide.txt".to_string(),
        MockRoute::ok("text/plain", body.into_bytes()),
    );
    let server = MockHttpServer::start(routes).await;

    let response = fetch(&server.url("/wide.txt")).await.expect("text fetch");
    assert!(response.truncated);
    assert_eq!(text_body(&response).len(), WEB_FETCH_MAX_OUTPUT_BYTES);
}

#[tokio::test]
async fn redirects_are_followed_and_final_url_is_reported() {
    let server = MockHttpServer::start_with(|base_url| {
        let mut routes = HashMap::new();
        routes.insert(
            "/start".to_string(),
            MockRoute::with_status("HTTP/1.1 302 Found", "text/plain", b"".to_vec())
                .with_header(format!("Location: {base_url}/final")),
        );
        routes.insert(
            "/final".to_string(),
            MockRoute::ok("application/json", br#"{"landed": true}"#.to_vec()),
        );
        routes
    })
    .await;

    let response = fetch(&server.url("/start")).await.expect("redirect fetch");
    assert_eq!(text_body(&response), r#"{"landed": true}"#);
    assert_eq!(response.final_url, server.url("/final"));
}

#[tokio::test]
async fn invalid_scheme_is_rejected_before_any_socket_work() {
    let mut routes = HashMap::new();
    routes.insert(
        "/never".to_string(),
        MockRoute::ok("text/plain", b"unreachable".to_vec()),
    );
    let server = MockHttpServer::start(routes).await;

    for url in ["file:///etc/passwd", "ftp://127.0.0.1/never", "not a url"] {
        match fetch(url).await {
            Err(WebFetchError::InvalidUrl { url: reported }) => assert_eq!(reported, url),
            other => panic!("expected InvalidUrl for {url}, got {other:?}"),
        }
    }
    assert!(
        server.recorded_requests().is_empty(),
        "no request must reach the network"
    );
}

#[tokio::test]
async fn the_ylva_user_agent_is_sent() {
    let mut routes = HashMap::new();
    routes.insert(
        "/agent".to_string(),
        MockRoute::ok("text/plain", b"ok".to_vec()),
    );
    let server = MockHttpServer::start(routes).await;

    fetch(&server.url("/agent")).await.expect("fetch");
    let requests = server.recorded_requests();
    assert_eq!(requests.len(), 1);
    let head = requests[0].to_ascii_lowercase();
    assert!(
        head.contains("user-agent: ylva/"),
        "request must carry the Ylva user-agent: {}",
        requests[0]
    );
}
