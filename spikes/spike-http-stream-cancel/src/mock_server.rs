use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// Interval at which the mock server emits one SSE frame.
pub const FRAME_TICK: Duration = Duration::from_millis(10);

const MAX_REQUEST_HEADER_BYTES: usize = 8192;

const RESPONSE_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\n\
Content-Type: text/event-stream\r\n\
Cache-Control: no-cache\r\n\
Connection: keep-alive\r\n\
\r\n";

const DONE_FRAME: &[u8] = b"data: [DONE]\n\n";

/// An in-process SSE mock server bound to an ephemeral loopback port.
///
/// It accepts one TCP connection at a time (serially) and streams OpenAI-style
/// `data: {...}\n\n` frames on a [`FRAME_TICK`] cadence until the client
/// disconnects (the read side returns `Ok(0)`/`Err`) or an optional frame cap
/// sends the `data: [DONE]` terminator.
pub struct MockServer {
    pub addr: SocketAddr,
    live_connections: Arc<AtomicUsize>,
    cumulative_connections: Arc<AtomicUsize>,
    handle: JoinHandle<()>,
}

impl MockServer {
    /// Bind and start the accept loop. `frame_cap` of `None` streams forever
    /// until the client disconnects.
    pub async fn start(frame_cap: Option<usize>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let live_connections = Arc::new(AtomicUsize::new(0));
        let cumulative_connections = Arc::new(AtomicUsize::new(0));

        let live = Arc::clone(&live_connections);
        let cumulative = Arc::clone(&cumulative_connections);
        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(_) => break,
                };
                live.fetch_add(1, Ordering::SeqCst);
                cumulative.fetch_add(1, Ordering::SeqCst);
                let live = Arc::clone(&live);
                tokio::spawn(handle_connection(stream, frame_cap, live));
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

    pub fn abort(self) {
        self.handle.abort();
    }
}

/// Build one OpenAI-style SSE frame carrying `text` in the content delta.
pub fn build_frame(index: usize, text: &str) -> String {
    let payload = serde_json::json!({
        "id": format!("chatcmpl-{index}"),
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

/// Small deterministic token to keep frames non-trivial and countable.
fn tick_text(index: usize) -> String {
    format!("token-{index} ")
}

/// Serve one client connection: read the request head, reply with SSE headers,
/// then stream frames until the client aborts or the frame cap is reached.
async fn handle_connection(
    stream: TcpStream,
    frame_cap: Option<usize>,
    live_connections: Arc<AtomicUsize>,
) {
    let (mut reader, mut writer) = stream.into_split();
    if read_request_head(&mut reader).await.is_err() {
        live_connections.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    if writer.write_all(RESPONSE_HEAD).await.is_err() {
        live_connections.fetch_sub(1, Ordering::SeqCst);
        return;
    }

    let mut index: usize = 0;
    let mut body_buffer = vec![0u8; 1024];

    loop {
        let cap_reached = frame_cap.is_some_and(|cap| index >= cap);
        if cap_reached {
            let _ = writer.write_all(DONE_FRAME).await;
            break;
        }

        tokio::select! {
            _ = tokio::time::sleep(FRAME_TICK) => {
                let frame = build_frame(index, &tick_text(index));
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

/// Read bytes until `\r\n\r\n` (end of HTTP request head) is observed.
async fn read_request_head(reader: &mut tokio::net::tcp::OwnedReadHalf) -> std::io::Result<()> {
    let mut buffer = Vec::with_capacity(MAX_REQUEST_HEADER_BYTES);
    let mut chunk = [0u8; 512];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed before request head completed",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(());
        }
        if buffer.len() > MAX_REQUEST_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head exceeds limit",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_frame_produces_data_framing() {
        let frame = build_frame(3, "hello");
        assert!(frame.starts_with("data: {"));
        assert!(frame.ends_with("\n\n"));
        let payload: serde_json::Value =
            serde_json::from_str(frame.trim_start_matches("data: ").trim_end())
                .expect("frame payload must be valid JSON");
        assert_eq!(payload["choices"][0]["delta"]["content"], "hello");
    }

    #[tokio::test]
    async fn server_tracks_live_connection_until_client_disconnects() {
        let server = MockServer::start(Some(100)).await.expect("bind failed");
        let stream = tokio::net::TcpStream::connect(server.addr)
            .await
            .expect("connect failed");
        let (mut reader, mut writer) = stream.into_split();

        writer
            .write_all(b"POST /v1/chat/completions HTTP/1.1\r\nhost: localhost\r\n\r\n")
            .await
            .unwrap();

        let mut response = Vec::new();
        reader.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200 OK"));

        assert_eq!(server.cumulative_connections(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn live_counter_returns_to_zero_after_disconnect() {
        let server = MockServer::start(Some(5)).await.expect("bind failed");
        let live = server.live_counter();

        for _ in 0..3 {
            let stream = tokio::net::TcpStream::connect(server.addr)
                .await
                .expect("connect failed");
            let (mut reader, mut writer) = stream.into_split();
            writer
                .write_all(b"GET /stream HTTP/1.1\r\nhost: localhost\r\n\r\n")
                .await
                .unwrap();
            let mut response = Vec::new();
            reader.read_to_end(&mut response).await.unwrap();
        }

        assert_eq!(server.cumulative_connections(), 3);
        assert_eq!(live.load(Ordering::SeqCst), 0);
        server.abort();
    }
}
