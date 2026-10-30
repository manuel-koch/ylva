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

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn default_catalog_reads_generic_model_metadata() {
    let body = serde_json::json!({
        "data": [
            {
                "id": "openai/gpt-oss-120b",
                "context_length": 131072,
                "reasoning": {"supported_efforts": ["low"], "default_effort": "low"},
                "architecture": {"input_modalities": ["image"]},
                "supported_endpoints": ["/responses"]
            },
            {"id": "plain"},
            {"name": "missing-id"}
        ]
    });

    assert_eq!(
        DefaultModelCatalog.parse(&body),
        vec![
            crate::llm::ModelInfo {
                id: "openai/gpt-oss-120b".to_string(),
                context_length: Some(131072),
                reasoning_efforts: Some(vec!["low".to_string()]),
                reasoning_default: Some("low".to_string()),
                input_modalities: Some(vec![crate::llm::InputModality::Image]),
            },
            crate::llm::ModelInfo {
                id: "plain".to_string(),
                context_length: None,
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            },
        ]
    );
    assert!(DefaultModelCatalog.is_loaded());
    assert!(!DefaultModelCatalog.uses_responses_api("openai/gpt-oss-120b"));
}

#[test]
fn reported_input_tokens_are_read_from_responses_completion_frames() {
    let usage = parse_reported_input_tokens(
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":42}}}"#,
    );

    assert_eq!(
        usage,
        Some((42, "openai_compatible.response.usage.input_tokens"))
    );
}

/// Marks every model as `/responses`-only once a listing was parsed.
#[derive(Debug, Default)]
struct StubCatalog {
    loaded: AtomicBool,
    parse_calls: AtomicUsize,
}

impl ModelCatalog for StubCatalog {
    fn parse(&self, _body: &serde_json::Value) -> Vec<crate::llm::ModelInfo> {
        self.parse_calls.fetch_add(1, Ordering::SeqCst);
        self.loaded.store(true, Ordering::SeqCst);
        Vec::new()
    }

    fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::SeqCst)
    }

    fn uses_responses_api(&self, _model: &str) -> bool {
        true
    }
}

#[test]
fn unloaded_catalog_is_listed_once_before_chat_and_selects_the_responses_endpoint() {
    crate::test_support::run_locked(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let mut request_lines = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut buffer = vec![0_u8; 16384];
                let bytes_read = stream.read(&mut buffer).await.expect("read");
                let request = String::from_utf8_lossy(&buffer[..bytes_read]).to_string();
                request_lines.push(request.lines().next().unwrap_or_default().to_string());
                let (content_type, body) = if request.starts_with("GET /models") {
                    ("application/json", "{\"data\":[]}")
                } else {
                    ("text/event-stream", "data: [DONE]\n\n")
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.expect("write");
            }
            request_lines
        });
        let catalog = Arc::new(StubCatalog::default());
        let provider = OpenAiCompatibleProvider::new(
            "stub".to_string(),
            format!("http://{address}"),
            "any-model".to_string(),
            None,
        )
        .with_model_catalog(catalog.clone());

        for _ in 0..2 {
            let mut events = provider
                .chat(Vec::new(), Vec::new(), CancellationToken::new())
                .await
                .expect("chat");
            while let Some(event) = events.recv().await {
                if matches!(event, StreamEvent::Done) {
                    break;
                }
            }
        }

        let request_lines = server.await.expect("server");
        assert!(
            request_lines[0].starts_with("GET /models"),
            "{request_lines:?}"
        );
        assert!(
            request_lines[1].starts_with("POST /responses"),
            "{request_lines:?}"
        );
        assert!(
            request_lines[2].starts_with("POST /responses"),
            "{request_lines:?}"
        );
        assert_eq!(catalog.parse_calls.load(Ordering::SeqCst), 1);
    });
}
