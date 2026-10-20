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
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn request_body_redactor_redacts_only_chat_completion_media_payloads() {
    let encoded = "QklOQVJZX1NFQ1JFVA==";
    let ordinary_media_marker = "TEXT_MEDIA_SENTINEL";
    let body = serde_json::json!({
        "model": "vision-model",
        "messages": [
            {
                "role": "tool",
                "tool_call_id": "call-1",
                "content": serde_json::json!({
                    "ok": true,
                    "result": encoded,
                    "result_metadata": {"encoding": "base64", "media_type": "image/png"}
                }).to_string()
            },
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "inspect"},
                    {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{encoded}")}},
                    {"type": "image_url", "image_url": format!("data:image/jpeg;base64,{encoded}")},
                    {"type": "file", "file": {"file_data": format!("data:application/pdf;base64,{encoded}"), "filename": "report.pdf"}},
                    {"type": "text", "url": format!("data:image/png;base64,{ordinary_media_marker}"), "file_data": "ordinary-field"}
                ]
            }
        ]
    });
    let original = body.clone();
    let redacted = redact_chat_completion_request_body(&body);

    assert_eq!(body, original);
    let redacted_text = redacted.to_string();
    assert!(!redacted_text.contains(encoded));
    assert!(redacted_text.contains("<REDACTED_BASE64_RESULT>"));
    assert!(redacted_text.contains("data:image/png;base64,<REDACTED_MEDIA_DATA>"));
    assert!(redacted_text.contains("data:image/jpeg;base64,<REDACTED_MEDIA_DATA>"));
    assert!(redacted_text.contains("data:application/pdf;base64,<REDACTED_MEDIA_DATA>"));
    assert!(redacted_text.contains(ordinary_media_marker));
    assert!(redacted_text.contains("ordinary-field"));
    assert!(redacted_text.contains("report.pdf"));
}

#[test]
fn provider_logs_effective_runtime_snapshot_without_sensitive_request_data() {
    let log = crate::test_support::capture_logs(async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local test server");
        let address = listener.local_addr().expect("local test server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept provider request");
            let response_body = "data: [DONE]\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/event-stream\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\
                     \r\n\
                     {}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write streaming response");
        });

        let provider = OpenAiCompatibleProvider::new(
            "test-connection".to_string(),
            format!("http://{address}"),
            "runtime-model".to_string(),
            Some("test-api-key".to_string()),
        );
        {
            let mut runtime_config = provider
                .runtime_config
                .write()
                .expect("Engine runtime config lock");
            runtime_config.temperature = Some(0.25);
            runtime_config.context_length = 64_000;
            runtime_config.reasoning_effort = Some("high".to_string());
            runtime_config.input_modalities = Some(vec![InputModality::Image]);
        }

        let mut receiver = provider
            .chat(
                Vec::new(),
                vec![ToolSchema {
                    name: "request-body-secret".to_string(),
                    description: "request-body-secret".to_string(),
                    parameters: serde_json::json!({}),
                }],
                CancellationToken::new(),
            )
            .await
            .expect("mock server response");
        while let Some(event) = receiver.recv().await {
            if matches!(event, StreamEvent::Error(_)) {
                break;
            }
        }
        server.await.expect("test server task");
    });

    assert!(log.contains("chat request started"));
    assert!(log.contains("provider_connection=test-connection"));
    assert!(log.contains("model=runtime-model"));
    assert!(log.contains("temperature=Some(0.25)"));
    assert!(log.contains("effective_context_length=64000"));
    assert!(
        log.contains("reasoning_effort=high"),
        "the request log must carry the thinking mode: {log}"
    );
    assert!(
        log.contains("input_modalities=image"),
        "the request log must carry the modalities: {log}"
    );
    assert!(log.contains("dump_id="));
    assert!(!log.contains("test-api-key"));
    assert!(!log.contains("request-body-secret"));
}

#[test]
fn emits_serialized_request_estimate_before_provider_response() {
    crate::test_support::run_locked(async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local test server");
        let address = listener.local_addr().expect("local test server address");
        let (request_sender, request_receiver) = tokio::sync::oneshot::channel();
        let (response_sender, response_receiver) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 2048];
            let request_body = loop {
                let bytes_read = stream.read(&mut buffer).await.expect("read request");
                request.extend_from_slice(&buffer[..bytes_read]);
                let Some(header_end) = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|position| position + 4)
                else {
                    continue;
                };
                let header_text = String::from_utf8_lossy(&request[..header_end]);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().expect("content length"))
                    })
                    .expect("content length header");
                if request.len() >= header_end + content_length {
                    break serde_json::from_slice::<serde_json::Value>(
                        &request[header_end..header_end + content_length],
                    )
                    .expect("JSON request body");
                }
            };
            let _ = request_sender.send(request_body);
            let _ = response_receiver.await;
            let response_body = "data: [DONE]\n\n";
            let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write streaming response");
        });

        let provider = Arc::new(OpenAiCompatibleProvider::new(
            "estimate-test".to_string(),
            format!("http://{address}"),
            "test-model".to_string(),
            None,
        ));
        let (estimate_sender, estimate_receiver) = tokio::sync::oneshot::channel();
        let provider_request = tokio::spawn(async move {
            provider
                .chat_with_session_id_and_estimate(
                    vec![Message::User {
                        content: "estimate before response".to_string(),
                        kind: crate::agent::UserMessageKind::Explicit,
                    }],
                    Vec::new(),
                    CancellationToken::new(),
                    None,
                    Box::new(move |estimated_tokens, context_length| {
                        Box::pin(async move {
                            let _ = estimate_sender.send((estimated_tokens, context_length));
                        })
                    }),
                )
                .await
                .expect("provider stream")
        });
        let request_body = request_receiver.await.expect("server received request");
        let (estimated_tokens, context_length) =
            tokio::time::timeout(std::time::Duration::from_secs(1), estimate_receiver)
                .await
                .expect("estimate arrives before a response")
                .expect("estimate callback");
        assert_eq!(context_length, DEFAULT_CONTEXT_LENGTH);
        assert_eq!(
            estimated_tokens,
            crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&request_body.to_string())
        );

        let _ = response_sender.send(());
        let mut events = provider_request.await.expect("provider task");
        assert!(matches!(events.recv().await, Some(StreamEvent::Done)));
        server.await.expect("test server task");
    });
}

#[test]
fn caller_timeout_is_appended_to_the_matching_response_dump() {
    crate::test_support::run_locked(async {
        let temp_dir = tempfile::tempdir().expect("dump directory");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local test server");
        let address = listener.local_addr().expect("local test server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await.expect("read request");
            stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100\r\n\r\n",
                    )
                    .await
                    .expect("write response headers");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });

        let provider = OpenAiCompatibleProvider::new(
            "timeout-dump-test".to_string(),
            format!("http://{address}"),
            "test-model".to_string(),
            None,
        )
        .with_request_dump_source(crate::http::RequestDumpSource::from_template(
            temp_dir.path().join("summary.jsonl"),
        ));
        let failure_handler = crate::http::RequestDumpFailureHandler::default();
        let mut response = provider
            .chat_with_session_id_and_diagnostics(
                vec![Message::User {
                    content: "summarize".to_string(),
                    kind: crate::agent::UserMessageKind::ImplicitToolNote,
                }],
                Vec::new(),
                CancellationToken::new(),
                None,
                Box::new(|_, _| Box::pin(async {})),
                Some(failure_handler.clone()),
            )
            .await
            .expect("provider request starts");

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), response.recv())
                .await
                .is_err()
        );
        failure_handler.report("compaction summary timed out after 60 seconds");

        let request_path = std::fs::read_dir(temp_dir.path())
            .expect("dump directory readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.to_string_lossy().contains("_request."))
            .expect("request dump exists");
        let request_stem = request_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("request filename")
            .replace("_request.jsonl", "");
        let response_path = temp_dir
            .path()
            .join(format!("{request_stem}_response.jsonl"));
        let response_dump = std::fs::read_to_string(response_path).expect("response dump");
        assert!(response_dump.contains("compaction summary timed out after 60 seconds"));

        server.abort();
    });
}

#[test]
fn each_request_gets_its_own_estimate_and_optional_reported_usage_record() {
    crate::test_support::run_locked(async {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let dump_path = temporary_directory.path().join("token-usage.jsonl");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local test server");
        let address = listener.local_addr().expect("local test server address");
        let server = tokio::spawn(async move {
            let mut request_bodies = Vec::new();
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 2048];
                loop {
                    let bytes_read = stream.read(&mut buffer).await.expect("read request");
                    if bytes_read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..bytes_read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let header_end = request
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .expect("request headers")
                            + 4;
                        let header_text = String::from_utf8_lossy(&request[..header_end]);
                        let content_length = header_text
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().expect("content length"))
                            })
                            .unwrap_or_default();
                        if request.len() >= header_end + content_length {
                            request_bodies.push(
                                serde_json::from_slice::<serde_json::Value>(
                                    &request[header_end..header_end + content_length],
                                )
                                .expect("JSON request body"),
                            );
                            break;
                        }
                    }
                }
                let frames = if request_index == 0 {
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":31}}\n\ndata: [DONE]\n\n"
                } else {
                    "data: {\"choices\":[],\"usage\":null}\n\ndata: [DONE]\n\n"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    frames.len(),
                    frames
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write streaming response");
            }
            request_bodies
        });

        let provider = OpenAiCompatibleProvider::new(
            "test-connection".to_string(),
            format!("http://{address}"),
            "test-model".to_string(),
            None,
        )
        .with_token_usage_dump_path(&dump_path);
        let session_id = uuid::Uuid::new_v4();
        for request_content in ["first request sentinel", "second request sentinel"] {
            let mut events = provider
                .chat_with_session_id(
                    vec![Message::User {
                        content: request_content.to_string(),
                        kind: crate::agent::UserMessageKind::Explicit,
                    }],
                    Vec::new(),
                    CancellationToken::new(),
                    Some(session_id),
                )
                .await
                .expect("provider stream");
            while let Some(event) = events.recv().await {
                if matches!(event, StreamEvent::Error(_)) {
                    break;
                }
            }
        }
        let request_bodies = server.await.expect("server task");
        assert_eq!(request_bodies.len(), 2);
        assert!(request_bodies
            .iter()
            .all(|body| { body["stream_options"]["include_usage"] == true }));

        let records = std::fs::read_to_string(dump_path).expect("token usage dump");
        let records = records
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON record"))
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2, "one record per model request");
        assert_ne!(records[0]["request_id"], records[1]["request_id"]);
        for record in &records {
            assert_eq!(record["session_id"], session_id.to_string());
            let timestamp = record["timestamp"].as_str().expect("timestamp");
            chrono::DateTime::parse_from_rfc3339(timestamp).expect("RFC 3339 timestamp");
        }
        assert_eq!(records[0]["reported_input_tokens"], 31);
        assert_eq!(
            records[0]["reported_source"],
            "openai_compatible.usage.prompt_tokens"
        );
        assert!(records[1]["reported_input_tokens"].is_null());
        assert!(records[1]["reported_source"].is_null());
        assert!(records.iter().all(|record| {
            record["estimated_input_tokens"]
                .as_u64()
                .unwrap_or_default()
                > 0
                && record["estimate_source"] == "cl100k_base_serialized_chat_request"
        }));
        for (record, body) in records.iter().zip(&request_bodies) {
            let expected_estimate =
                crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&body.to_string());
            assert_eq!(record["estimated_input_tokens"], expected_estimate);
        }
        let dump_text = serde_json::to_string(&records).expect("records serialize");
        assert!(!dump_text.contains("first request sentinel"));
        assert!(!dump_text.contains("second request sentinel"));
    });
}

#[test]
fn interrupted_stream_is_recorded_without_inventing_usage() {
    crate::test_support::run_locked(async {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let dump_path = temporary_directory.path().join("token-usage.jsonl");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local test server");
        let address = listener.local_addr().expect("local test server address");
        let (headers_sent, headers_received) = tokio::sync::oneshot::channel();
        let (close_stream, wait_to_close) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await.expect("read request");
            let first_frame = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:x}\r\n{}\r\n",
                first_frame.len(),
                first_frame
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response headers and first chunk");
            let _ = headers_sent.send(());
            let _ = wait_to_close.await;
        });

        let provider = OpenAiCompatibleProvider::new(
            "test-connection".to_string(),
            format!("http://{address}"),
            "test-model".to_string(),
            None,
        )
        .with_token_usage_dump_path(&dump_path);
        let cancellation = CancellationToken::new();
        let mut events = provider
            .chat(
                vec![Message::User {
                    content: "interrupted request sentinel".to_string(),
                    kind: crate::agent::UserMessageKind::Explicit,
                }],
                Vec::new(),
                cancellation.clone(),
            )
            .await
            .expect("provider stream");
        headers_received.await.expect("server sent response");
        assert!(matches!(events.recv().await, Some(StreamEvent::Text(text)) if text == "partial"));
        cancellation.cancel();
        assert!(matches!(events.recv().await, Some(StreamEvent::Cancelled)));
        let _ = close_stream.send(());
        server.await.expect("server task");

        let contents = std::fs::read_to_string(dump_path).expect("token usage dump");
        let record: serde_json::Value = serde_json::from_str(contents.trim()).expect("JSON record");
        assert!(
            record["estimated_input_tokens"]
                .as_u64()
                .unwrap_or_default()
                > 0
        );
        assert!(record["reported_input_tokens"].is_null());
        assert!(record["reported_source"].is_null());
        assert!(!contents.contains("interrupted request sentinel"));
    });
}

/// Serve one streaming response with a usage frame carrying `cost`, drain the
/// provider stream and return the ledger totals text.
async fn totals_after_request_with_cost(with_parser: bool) -> Option<String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local test server");
    let address = listener.local_addr().expect("local test server address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept request");
        let mut request = [0_u8; 8192];
        let _ = stream.read(&mut request).await.expect("read request");
        let frames = "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"cost\":0.1234}}\n\ndata: [DONE]\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            frames.len(),
            frames
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write streaming response");
    });
    let ledger = CostLedger::new();
    let mut provider = OpenAiCompatibleProvider::new(
        "cost-test-connection".to_string(),
        format!("http://{address}"),
        "test-model".to_string(),
        None,
    )
    .with_cost_ledger(ledger.clone());
    if with_parser {
        provider = provider.with_usage_cost_parser(|data| {
            let value: serde_json::Value = serde_json::from_str(data).ok()?;
            Some(RequestCost::new(
                value.pointer("/usage/cost")?.as_f64()?,
                "USD",
            ))
        });
    }
    let mut events = provider
        .chat(Vec::new(), Vec::new(), CancellationToken::new())
        .await
        .expect("provider stream");
    while events.recv().await.is_some() {}
    server.await.expect("server task");
    ledger.totals_text()
}

#[test]
fn reported_cost_is_recorded_only_with_a_usage_cost_parser() {
    crate::test_support::run_locked(async {
        assert_eq!(
            totals_after_request_with_cost(true).await.as_deref(),
            Some("0.12 USD")
        );
        assert_eq!(totals_after_request_with_cost(false).await, None);
    });
}
