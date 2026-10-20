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

//! Chat, tool-call and model-listing flow of `CopilotProvider` against a
//! loopback server that mimics the Copilot token exchange and API.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use ylva::{CopilotProvider, Message, Provider, StreamEvent, UserMessageKind};

/// Serve three requests (exchange, models, chat) and record each raw request.
async fn start_server() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).await.expect("read");
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request).to_string();
                let Some(header_end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let content_length = text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request).to_string();
            let (content_type, body) = if text.starts_with("GET /token") {
                let expires_at = chrono_free_now() + 1800;
                (
                    "application/json",
                    format!(
                        r#"{{"token":"session-token","expires_at":{expires_at},"endpoints":{{"api":"http://{address}"}}}}"#
                    ),
                )
            } else if text.starts_with("GET /models") {
                (
                    "application/json",
                    concat!(
                        r#"{"data":[{"id":"gpt-4o"},{"id":"claude-sonnet-4"},"#,
                        r#"{"id":"gpt-6-luna","supported_endpoints":["/responses"]}]}"#
                    )
                    .to_string(),
                )
            } else if text.starts_with("POST /responses") {
                (
                    "text/event-stream",
                    concat!(
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
                        "data: {\"type\":\"response.completed\",\"copilot_usage\":",
                        "{\"total_nano_aiu\":2500000000},\"response\":{\"usage\":{\"input_tokens\":7}}}\n\n"
                    )
                    .to_string(),
                )
            } else {
                (
                    "text/event-stream",
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
                        "\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a\\\"}\"}}]}}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                    .to_string(),
                )
            };
            recorded.lock().unwrap().push(text);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.expect("write");
        }
    });
    (format!("http://{address}"), requests)
}

fn chrono_free_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

#[tokio::test]
async fn chat_with_tool_call_and_model_listing_use_the_exchanged_session_token() {
    let (base_url, requests) = start_server().await;
    let provider = CopilotProvider::with_endpoints(
        "copilot".to_string(),
        "gpt-4o".to_string(),
        "ghu_secret".to_string(),
        format!("{base_url}/token"),
        "http://fallback.invalid".to_string(),
        std::time::Duration::ZERO,
    )
    .expect("provider");

    let models = provider.list_models().await.expect("models");
    let mut events = provider
        .chat(
            vec![Message::User {
                content: "hi".to_string(),
                kind: UserMessageKind::Explicit,
            }],
            Vec::new(),
            CancellationToken::new(),
        )
        .await
        .expect("chat");

    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-4o", "claude-sonnet-4", "gpt-6-luna"]
    );
    match events.recv().await {
        Some(StreamEvent::Delta(delta)) => {
            assert_eq!(delta.function_name.as_deref(), Some("read"));
        }
        other => panic!("expected tool-call delta, got {other:?}"),
    }
    assert!(matches!(events.recv().await, Some(StreamEvent::Done)));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3, "one exchange is cached across requests");
    assert!(requests[0].starts_with("GET /token"));
    for request in &requests[1..] {
        let lowercase = request.to_lowercase();
        assert!(lowercase.contains("authorization: bearer session-token"));
        assert!(lowercase.contains("copilot-integration-id: vscode-chat"));
        assert!(!lowercase.contains("ghu_secret"));
    }
    assert!(requests[2].starts_with("POST /chat/completions"));
    assert!(requests[2].to_lowercase().contains("x-initiator: user"));
}

#[tokio::test]
async fn responses_only_model_is_served_through_the_responses_endpoint() {
    let (base_url, requests) = start_server().await;
    let provider = CopilotProvider::with_endpoints(
        "copilot".to_string(),
        "gpt-6-luna".to_string(),
        "ghu_secret".to_string(),
        format!("{base_url}/token"),
        "http://fallback.invalid".to_string(),
        std::time::Duration::ZERO,
    )
    .expect("provider");

    let mut events = provider
        .chat(
            vec![Message::User {
                content: "hi".to_string(),
                kind: UserMessageKind::Explicit,
            }],
            Vec::new(),
            CancellationToken::new(),
        )
        .await
        .expect("chat");

    assert!(matches!(events.recv().await, Some(StreamEvent::Text(text)) if text == "Hi"));
    assert!(matches!(events.recv().await, Some(StreamEvent::Done)));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3, "exchange, catalog, responses");
    assert!(requests[1].starts_with("GET /models"));
    assert!(requests[2].starts_with("POST /responses"));
    assert!(requests[2].contains("\"input\""));
    assert!(requests[2].to_lowercase().contains("x-initiator: user"));
}
