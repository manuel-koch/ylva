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

// Test-support module: compiled into every integration-test crate via
// `tests/common.rs`, but only consumed by some of them. Allow dead code so
// a test binary that pulls in `mod common;` without using the mock server does
// not warn about helper items it never references.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// Interval at which the mock server emits one SSE frame.
pub const FRAME_TICK: Duration = Duration::from_millis(10);

const MAX_REQUEST_HEADER_BYTES: usize = 8192;

const SSE_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\n\
Content-Type: text/event-stream\r\n\
Cache-Control: no-cache\r\n\
Connection: keep-alive\r\n\
\r\n";

const DONE_FRAME: &[u8] = b"data: [DONE]\n\n";

/// Build one OpenAI-style SSE frame carrying `text` in `choices[0].delta.content`.
pub fn content_frame(text: &str) -> String {
    let payload = serde_json::json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "mock",
        "choices": [{
            "index": 0,
            "delta": { "content": text },
            "finish_reason": serde_json::Value::Null
        }]
    });
    format!("data: {payload}\n\n")
}

/// Build an OpenAI-style SSE frame carrying a `delta.tool_calls` fragment for
/// one tool call (`index`, `name`, and a JSON `arguments` string).
pub fn tool_call_frame(index: usize, name: &str, arguments: &str) -> String {
    let payload = serde_json::json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "mock",
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": index,
                    "id": format!("call_{index}"),
                    "type": "function",
                    "function": { "name": name, "arguments": arguments }
                }]
            },
            "finish_reason": serde_json::Value::Null
        }]
    });
    format!("data: {payload}\n\n")
}

/// The default generated frame used when a connection has no explicit frames.
fn default_frame(index: usize) -> String {
    content_frame(&format!("token-{index} "))
}

/// Per-connection behavior a [`MockLlmServerConfig`] describes.
#[derive(Debug, Clone)]
struct Behavior {
    status: u16,
    frames: Vec<String>,
    /// Loop `frames` (or generated frames) forever instead of ending.
    end_after_frames: bool,
    /// Model ids answered on `GET /models`.
    models: Vec<String>,
    /// Optional exact request target required for the model-list response.
    model_path: Option<String>,
    /// Context length returned by the Ollama `POST /api/show` endpoint.
    ollama_context_length: Option<usize>,
    /// Input modalities returned in the OpenAI-compatible model catalog's
    /// `architecture.input_modalities` field.
    model_input_modalities: Option<Vec<String>>,
    /// Capabilities returned by the Ollama `POST /api/show` endpoint.
    ollama_capabilities: Option<Vec<String>>,
}

/// Configuration for a [`MockLlmServer`].
///
/// `statuses` describes the HTTP status for each accepted connection, consumed
/// in order, falling back to `fallback_status` (default 200) once exhausted.
/// `frames` holds the OpenAI-spec SSE frames (built with [`content_frame`]);
/// when empty the server generates `token-{i} ` frames on the fly.
#[derive(Debug, Clone)]
pub struct MockLlmConfig {
    pub statuses: Vec<u16>,
    pub frames: Vec<String>,
    /// When true, emit `[DONE]` after `frames` are exhausted; when false (or
    /// frames empty) stream until the client disconnects.
    pub end_after_frames: bool,
    /// Status used once `statuses` is exhausted (default 200).
    pub fallback_status: u16,
    /// Model ids the server answers with on `GET /models` (the endpoint
    /// `Provider::list_models` calls). Empty means an empty `data` array.
    pub models: Vec<String>,
    /// When set, only this exact request target receives the model-list
    /// response. When unset, any target ending in `/models` is accepted.
    pub model_path: Option<String>,
    /// Context length returned by the Ollama model-details endpoint.
    pub ollama_context_length: Option<usize>,
    /// Optional OpenAI-compatible catalog modalities for every model.
    pub model_input_modalities: Option<Vec<String>>,
    /// Optional Ollama `/api/show` capabilities for every model.
    pub ollama_capabilities: Option<Vec<String>>,
}

impl Default for MockLlmConfig {
    fn default() -> Self {
        MockLlmConfig {
            statuses: Vec::new(),
            frames: Vec::new(),
            end_after_frames: false,
            fallback_status: 200,
            models: Vec::new(),
            model_path: None,
            ollama_context_length: None,
            model_input_modalities: None,
            ollama_capabilities: None,
        }
    }
}

impl MockLlmConfig {
    fn next_behavior(&self, position: usize) -> Behavior {
        let status = *self.statuses.get(position).unwrap_or(&self.fallback_status);
        Behavior {
            status,
            frames: self.frames.clone(),
            end_after_frames: self.end_after_frames,
            models: self.models.clone(),
            model_path: self.model_path.clone(),
            ollama_context_length: self.ollama_context_length,
            model_input_modalities: self.model_input_modalities.clone(),
            ollama_capabilities: self.ollama_capabilities.clone(),
        }
    }
}

/// One captured request's header lines as (`name`, `value`) pairs in the
/// order they appeared on the wire.
pub type RequestHeaderLines = Vec<(String, String)>;

/// Request bodies and headers captured across connections, so tests can make
/// wire-level assertions after the fact. Shared by the accept loop and every
/// connection handler; the two captures always travel together.
#[derive(Clone)]
struct CapturedRequests {
    bodies: Arc<Mutex<Vec<String>>>,
    headers: Arc<Mutex<Vec<RequestHeaderLines>>>,
}

impl CapturedRequests {
    fn new() -> Self {
        CapturedRequests {
            bodies: Arc::new(Mutex::new(Vec::new())),
            headers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn record_body(&self, request_body: String) {
        self.bodies
            .lock()
            .expect("request body lock")
            .push(request_body);
    }

    fn record_headers(&self, request_headers: RequestHeaderLines) {
        self.headers
            .lock()
            .expect("request header lock")
            .push(request_headers);
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().expect("request body lock").clone()
    }

    fn headers(&self) -> Vec<RequestHeaderLines> {
        self.headers.lock().expect("request header lock").clone()
    }
}

/// An in-process OpenAI-spec SSE mock server bound to an ephemeral loopback
/// port, modeled on `spikes/spike-http-stream-cancel/src/mock_server.rs`.
///
/// Connections carry a [`MockLlmConfig`]-derived [`Behavior`]; text streams
/// emit frames on a [`FRAME_TICK`] cadence until the client disconnects or the
/// frame stream ends. A live-connection counter tracks outstanding sockets for
/// cancellation tests.
pub struct MockLlmServer {
    pub addr: SocketAddr,
    live_connections: Arc<AtomicUsize>,
    cumulative_connections: Arc<AtomicUsize>,
    captured_requests: CapturedRequests,
    handle: JoinHandle<()>,
}

impl MockLlmServer {
    /// Bind and start the accept loop. `config` drives per-connection behavior.
    pub async fn start(config: MockLlmConfig) -> std::io::Result<Self> {
        Self::start_with_connection_frames(config, Vec::new()).await
    }

    /// Bind and start the accept loop, optionally replacing the configured
    /// frames for individual connections. This keeps deterministic
    /// multi-turn integration tests from needing a second provider server.
    pub async fn start_with_connection_frames(
        config: MockLlmConfig,
        connection_frames: Vec<Vec<String>>,
    ) -> std::io::Result<Self> {
        Self::start_with_connection_frame_rules(config, connection_frames, Vec::new()).await
    }

    /// Bind and start the server with connection frames plus optional
    /// request-body rules. A matching rule takes precedence over the
    /// connection-index frame, which keeps concurrent root/child chat
    /// requests deterministic.
    pub async fn start_with_connection_frame_rules(
        config: MockLlmConfig,
        connection_frames: Vec<Vec<String>>,
        request_frame_rules: Vec<(String, Vec<String>)>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let live_connections = Arc::new(AtomicUsize::new(0));
        let cumulative_connections = Arc::new(AtomicUsize::new(0));
        let captured_requests = CapturedRequests::new();
        let config = Arc::new(config);
        let connection_frames = Arc::new(connection_frames);
        let request_frame_rules = Arc::new(request_frame_rules);
        let chat_request_index = Arc::new(AtomicUsize::new(0));

        let live = Arc::clone(&live_connections);
        let cumulative = Arc::clone(&cumulative_connections);
        let captured_requests_for_accept_loop = captured_requests.clone();
        let connection_frames_for_accept_loop = Arc::clone(&connection_frames);
        let request_frame_rules_for_accept_loop = Arc::clone(&request_frame_rules);
        let chat_request_index_for_accept_loop = Arc::clone(&chat_request_index);
        let handle = tokio::spawn(async move {
            let mut position = 0usize;
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(_) => break,
                };
                live.fetch_add(1, Ordering::SeqCst);
                cumulative.fetch_add(1, Ordering::SeqCst);
                let behavior = config.next_behavior(position);
                position += 1;
                let live = Arc::clone(&live);
                let captured_requests = captured_requests_for_accept_loop.clone();
                let connection_frames = Arc::clone(&connection_frames_for_accept_loop);
                let request_frame_rules = Arc::clone(&request_frame_rules_for_accept_loop);
                let chat_request_index = Arc::clone(&chat_request_index_for_accept_loop);
                tokio::spawn(handle_connection(
                    stream,
                    behavior,
                    live,
                    captured_requests,
                    connection_frames,
                    request_frame_rules,
                    chat_request_index,
                ));
            }
        });

        Ok(Self {
            addr,
            live_connections,
            cumulative_connections,
            captured_requests,
            handle,
        })
    }

    pub fn live_connections(&self) -> usize {
        self.live_connections.load(Ordering::SeqCst)
    }

    pub fn cumulative_connections(&self) -> usize {
        self.cumulative_connections.load(Ordering::SeqCst)
    }

    pub fn request_bodies(&self) -> Vec<String> {
        self.captured_requests.bodies()
    }

    /// Per-request header lines (`name`, `value`) as received on the wire, one
    /// entry per accepted request, in acceptance order.
    pub fn request_headers(&self) -> Vec<RequestHeaderLines> {
        self.captured_requests.headers()
    }

    pub fn live_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.live_connections)
    }

    /// A server origin to pass to [`crate::OpenAiCompatibleProvider`] (which
    /// appends `/chat/completions` internally), [`crate::OpenRouterProvider`]
    /// (which derives `/api/v1`), or [`crate::OllamaProvider`] (which derives
    /// both `/v1` and native `/api` paths).
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn abort(self) {
        self.handle.abort();
    }
}

/// Serve one client connection: read the request head, reply with either a
/// non-200 error or a streaming SSE body, then close.
async fn handle_connection(
    stream: TcpStream,
    mut behavior: Behavior,
    live_connections: Arc<AtomicUsize>,
    captured_requests: CapturedRequests,
    connection_frames: Arc<Vec<Vec<String>>>,
    request_frame_rules: Arc<Vec<(String, Vec<String>)>>,
    chat_request_index: Arc<AtomicUsize>,
) {
    let (mut reader, mut writer) = stream.into_split();
    let (request_target, request_headers, request_body) = match read_request_head(&mut reader).await
    {
        Ok(request) => request,
        Err(_) => {
            live_connections.fetch_sub(1, Ordering::SeqCst);
            return;
        }
    };
    captured_requests.record_headers(request_headers);
    let request_body = String::from_utf8(request_body)
        .ok()
        .filter(|request_body| !request_body.is_empty());
    if let Some(request_body_text) = &request_body {
        captured_requests.record_body(request_body_text.clone());
    }
    if request_target.contains("/chat/completions") {
        let request_index = chat_request_index.fetch_add(1, Ordering::SeqCst);
        if let Some((_, frames)) = request_frame_rules.iter().find(|(marker, _)| {
            request_body
                .as_deref()
                .is_some_and(|request_body| request_body.contains(marker))
        }) {
            behavior.frames = frames.clone();
        } else if let Some(frames) = connection_frames.get(request_index) {
            behavior.frames = frames.clone();
        }
    }

    // Model listing and details are plain JSON requests, not streams — answer
    // them from the request target rather than streaming SSE at them.
    let model_path_matches = behavior
        .model_path
        .as_deref()
        .is_none_or(|model_path| request_target == model_path);
    if request_target.ends_with("/models") && model_path_matches && behavior.status == 200 {
        write_model_list_response(
            &mut writer,
            &behavior.models,
            behavior.model_input_modalities.as_deref(),
        )
        .await;
        live_connections.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    if request_target.ends_with("/api/tags") && behavior.status == 200 {
        write_ollama_tags_response(&mut writer, &behavior.models).await;
        live_connections.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    if request_target.ends_with("/api/show") && behavior.status == 200 {
        write_ollama_show_response(
            &mut writer,
            behavior.ollama_context_length,
            behavior.ollama_capabilities.as_deref(),
        )
        .await;
        live_connections.fetch_sub(1, Ordering::SeqCst);
        return;
    }

    if behavior.status != 200 {
        write_error_response(&mut writer, behavior.status).await;
        live_connections.fetch_sub(1, Ordering::SeqCst);
        return;
    }

    if writer.write_all(SSE_HEAD).await.is_err() {
        live_connections.fetch_sub(1, Ordering::SeqCst);
        return;
    }

    let mut index = 0usize;
    let mut body_buffer = vec![0u8; 1024];
    loop {
        let frame = if index < behavior.frames.len() {
            behavior.frames[index].clone()
        } else if behavior.end_after_frames && !behavior.frames.is_empty() {
            let _ = writer.write_all(DONE_FRAME).await;
            break;
        } else {
            default_frame(index)
        };

        tokio::select! {
            _ = tokio::time::sleep(FRAME_TICK) => {
                if writer.write_all(frame.as_bytes()).await.is_err() {
                    break;
                }
                index += 1;
            }
            read = reader.read(&mut body_buffer) => {
                match read {
                    Ok(0) => break,
                    Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
    }

    live_connections.fetch_sub(1, Ordering::SeqCst);
}

/// Write a minimal non-200 HTTP error response and close the connection.
async fn write_error_response(writer: &mut tokio::net::tcp::OwnedWriteHalf, status: u16) {
    let reason = reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Error");
    let body = format!("{{\"error\":{{\"message\":\"mock {status} {reason}\"}}}}");
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
Content-Type: application/json\r\n\
Content-Length: {}\r\n\
\r\n",
        body.len()
    );
    // Combine head and body into one write to avoid interleaving.
    let _ = writer.write_all(head.as_bytes()).await;
    let _ = writer.write_all(body.as_bytes()).await;
}

/// Write an OpenAI-spec `GET /models` response listing `models`.
async fn write_model_list_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    models: &[String],
    input_modalities: Option<&[String]>,
) {
    let payload = serde_json::json!({
        "object": "list",
        "data": models
            .iter()
            .map(|id| {
                let mut model = serde_json::json!({ "id": id, "object": "model" });
                if let Some(input_modalities) = input_modalities {
                    model["architecture"] = serde_json::json!({
                        "input_modalities": input_modalities,
                    });
                }
                model
            })
            .collect::<Vec<serde_json::Value>>(),
    });
    let body = payload.to_string();
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
Content-Type: application/json\r\n\
Content-Length: {}\r\n\
\r\n",
        body.len()
    );
    let _ = writer.write_all(head.as_bytes()).await;
    let _ = writer.write_all(body.as_bytes()).await;
}

/// Write a minimal native Ollama `GET /api/tags` response.
async fn write_ollama_tags_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    models: &[String],
) {
    let payload = serde_json::json!({
        "models": models
            .iter()
            .map(|id| serde_json::json!({ "name": id, "model": id }))
            .collect::<Vec<serde_json::Value>>(),
    });
    write_json_response(writer, &payload).await;
}

/// Write a minimal native Ollama `POST /api/show` response.
async fn write_ollama_show_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    context_length: Option<usize>,
    capabilities: Option<&[String]>,
) {
    let mut payload = serde_json::json!({});
    if let Some(context_length) = context_length {
        payload["model_info"] = serde_json::json!({ "llama.context_length": context_length });
    }
    if let Some(capabilities) = capabilities {
        payload["capabilities"] = serde_json::json!(capabilities);
    }
    write_json_response(writer, &payload).await;
}

async fn write_json_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    payload: &serde_json::Value,
) {
    let body = payload.to_string();
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
Content-Type: application/json\r\n\
Content-Length: {}\r\n\
\r\n",
        body.len()
    );
    let _ = writer.write_all(head.as_bytes()).await;
    let _ = writer.write_all(body.as_bytes()).await;
}

/// Read the full HTTP request (head + body) so the server never responds
/// before the client has finished sending it, which would race reqwest's
/// request-body write. Returns the request target (path) from the request
/// line, the parsed header lines, and the request body, so callers can
/// distinguish `/chat/completions` from `/models`, inspect captured chat
/// payloads, and assert on wire-level headers.
async fn read_request_head(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
) -> std::io::Result<(String, RequestHeaderLines, Vec<u8>)> {
    let mut buffer = Vec::with_capacity(MAX_REQUEST_HEADER_BYTES);
    let mut chunk = [0u8; 512];
    let head_end = loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed before request head completed",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() > MAX_REQUEST_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head exceeds limit",
            ));
        }
    };

    // Consume the request body (Content-Length bytes) so a subsequent response
    // never interleaves with the client's still-sending body. Some body bytes
    // may already be buffered after the head.
    let body = if let Some(content_length) = parse_content_length(&buffer[..head_end]) {
        let buffered_body = buffer.len().saturating_sub(head_end);
        let buffered_body_length = buffered_body.min(content_length);
        let mut body = buffer[head_end..head_end + buffered_body_length].to_vec();
        let mut remaining = content_length.saturating_sub(buffered_body_length);
        let mut body_chunk = [0u8; 512];
        while remaining > 0 {
            let wanted = remaining.min(body_chunk.len());
            let read = reader.read(&mut body_chunk[..wanted]).await?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "client closed before request body completed",
                ));
            }
            remaining -= read;
            body.extend_from_slice(&body_chunk[..read]);
        }
        body
    } else {
        Vec::new()
    };
    Ok((
        parse_request_target(&buffer[..head_end]),
        parse_request_headers(&buffer[..head_end]),
        body,
    ))
}

/// Extract the request target from an HTTP request line ("GET /models HTTP/1.1").
fn parse_request_target(head: &[u8]) -> String {
    std::str::from_utf8(head)
        .ok()
        .and_then(|head| head.lines().next())
        .and_then(|request_line| request_line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string()
}

/// Parse the header lines of an HTTP request head into [`RequestHeaderLines`],
/// skipping the request line. Header names keep their wire casing.
fn parse_request_headers(head: &[u8]) -> RequestHeaderLines {
    let Some(head) = std::str::from_utf8(head).ok() else {
        return Vec::new();
    };
    head.lines()
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

/// Parse the `Content-Length` header (in bytes) from a request head.
fn parse_content_length(head: &[u8]) -> Option<usize> {
    let head = std::str::from_utf8(head).ok()?;
    head.to_ascii_lowercase()
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
}
