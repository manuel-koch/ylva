// Test-support module: compiled into every integration-test crate via
// `tests/common.rs`, but only consumed by some of them. Allow dead code so
// a test binary that pulls in `mod common;` without using the mock server does
// not warn about helper items it never references.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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
}

impl Default for MockLlmConfig {
    fn default() -> Self {
        MockLlmConfig {
            statuses: Vec::new(),
            frames: Vec::new(),
            end_after_frames: false,
            fallback_status: 200,
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
        }
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
    handle: JoinHandle<()>,
}

impl MockLlmServer {
    /// Bind and start the accept loop. `config` drives per-connection behavior.
    pub async fn start(config: MockLlmConfig) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let live_connections = Arc::new(AtomicUsize::new(0));
        let cumulative_connections = Arc::new(AtomicUsize::new(0));
        let config = Arc::new(config);

        let live = Arc::clone(&live_connections);
        let cumulative = Arc::clone(&cumulative_connections);
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
                tokio::spawn(handle_connection(stream, behavior, live));
            }
        });

        Ok(Self {
            addr,
            live_connections,
            cumulative_connections,
            handle,
        })
    }

    pub fn live_connections(&self) -> usize {
        self.live_connections.load(Ordering::SeqCst)
    }

    pub fn cumulative_connections(&self) -> usize {
        self.cumulative_connections.load(Ordering::SeqCst)
    }

    pub fn live_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.live_connections)
    }

    /// A `base_url` to pass to [`crate::OpenAiProvider`] (which appends
    /// `/chat/completions` internally).
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
    behavior: Behavior,
    live_connections: Arc<AtomicUsize>,
) {
    let (mut reader, mut writer) = stream.into_split();
    if read_request_head(&mut reader).await.is_err() {
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

/// Read the full HTTP request (head + body) so the server never responds
/// before the client has finished sending it, which would race reqwest's
/// request-body write.
async fn read_request_head(reader: &mut tokio::net::tcp::OwnedReadHalf) -> std::io::Result<()> {
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
    if let Some(content_length) = parse_content_length(&buffer[..head_end]) {
        let buffered_body = buffer.len().saturating_sub(head_end);
        let mut remaining = content_length.saturating_sub(buffered_body);
        while remaining > 0 {
            let wanted = remaining.min(chunk.len());
            let read = reader.read(&mut chunk[..wanted]).await?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "client closed before request body completed",
                ));
            }
            remaining -= read;
        }
    }
    Ok(())
}

/// Parse the `Content-Length` header (in bytes) from a request head.
fn parse_content_length(head: &[u8]) -> Option<usize> {
    let head = std::str::from_utf8(head).ok()?;
    head.to_ascii_lowercase()
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
}
