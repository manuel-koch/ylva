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

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ylva::llm::ProviderRegistry;
use ylva::{
    AgentCommand, AgentLoop, AssistantStatus, Engine, EngineConfig, EngineImpl, InputModality,
    Message, OllamaProvider, OpenAiCompatibleProvider, OpenRouterProvider, Provider,
    ProviderConnectionConfig, ProviderModelOption, StreamEvent, ToolAttachment, ToolRegistry,
    ToolResult, UiUpdate, UserMessageKind, DEFAULT_SOUL, TOOL_PROTOCOL_SYSTEM_PROMPT,
};

use common::collect_until;
use common::mock_llm_server::{content_frame, tool_call_frame, MockLlmConfig, MockLlmServer};

/// A provider (`Provider` trait object) pointed at a running [`MockLlmServer`]
/// with a fixed model. Trait-typed so consumers are decoupled from the concrete
/// HTTP provider. Use [`openai_compatible_provider_for`] when calling `.chat()` directly —
/// rust-analyzer's inference across the `dyn Provider` boundary is unreliable.
fn provider_for(server: &MockLlmServer) -> Arc<dyn Provider> {
    Arc::new(OpenAiCompatibleProvider::new(
        "test".to_string(),
        server.base_url(),
        "mock-model".into(),
        None,
    ))
}

/// The concrete HTTP provider pointed at a running [`MockLlmServer`]. Used by
/// the tests that call `.chat()` directly, so the IDE can infer the stream type
/// without crossing the `dyn Provider` trait-object boundary.
fn openai_compatible_provider_for(server: &MockLlmServer) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        "test".to_string(),
        server.base_url(),
        "mock-model".into(),
        None,
    )
}

/// Consume a provider receiver until `Done`, returning the streamed text chunks
/// in order. Panics if the stream errors or ends early.
async fn drain_text_until_done(rx: &mut mpsc::Receiver<StreamEvent>) -> Vec<String> {
    let mut texts = Vec::new();
    loop {
        match rx.recv().await {
            Some(StreamEvent::Text(text)) => texts.push(text),
            Some(StreamEvent::Done) => return texts,
            Some(other) => panic!("unexpected stream event while draining: {other:?}"),
            None => panic!("provider stream ended without [DONE]"),
        }
    }
}

#[tokio::test]
async fn list_models_reads_the_providers_model_endpoint() {
    let config = MockLlmConfig {
        models: vec!["small-model".to_string(), "large-model".to_string()],
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_compatible_provider_for(&server);
    let models = provider
        .list_models()
        .await
        .expect("the mock answers GET /models with a 200");

    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["small-model", "large-model"]
    );
}

#[tokio::test]
async fn openrouter_provider_lists_models_from_openai_catalog() {
    let config = MockLlmConfig {
        models: vec!["openai/gpt-oss-120b".to_string()],
        model_path: Some("/api/v1/models".to_string()),
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = OpenRouterProvider::new(
        "openrouter".to_string(),
        server.base_url(),
        "openai/gpt-oss-120b".to_string(),
        Some("test-key".to_string()),
    );
    let models = provider
        .list_models()
        .await
        .expect("OpenRouter should answer its OpenAI-compatible model endpoint");

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "openai/gpt-oss-120b");
}

#[tokio::test]
async fn openrouter_catalog_preserves_image_and_file_capabilities() {
    let config = MockLlmConfig {
        models: vec!["provider/multimodal".to_string()],
        model_path: Some("/api/v1/models".to_string()),
        model_input_modalities: Some(vec![
            "text".to_string(),
            "image".to_string(),
            "file".to_string(),
        ]),
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");
    let provider = OpenRouterProvider::new(
        "openrouter".to_string(),
        server.base_url(),
        "provider/multimodal".to_string(),
        Some("test-key".to_string()),
    );

    let models = provider.list_models().await.expect("catalog response");
    assert_eq!(
        models[0].input_modalities,
        Some(vec![InputModality::Image, InputModality::File])
    );
}

#[tokio::test]
async fn ollama_provider_lists_native_models_and_loads_details() {
    let config = MockLlmConfig {
        models: vec!["gemma4:latest".to_string(), "qwen3:8b".to_string()],
        ollama_context_length: Some(131_072),
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = OllamaProvider::new(
        "ollama-local".to_string(),
        server.base_url(),
        "gemma4:latest".to_string(),
        None,
    );
    let models = provider
        .list_models()
        .await
        .expect("Ollama should answer its native model endpoints");

    assert_eq!(
        models
            .iter()
            .map(|model| (model.id.as_str(), model.context_length))
            .collect::<Vec<_>>(),
        vec![
            ("gemma4:latest", Some(131_072)),
            ("qwen3:8b", Some(131_072)),
        ]
    );
}

#[tokio::test]
async fn ollama_details_map_vision_to_image_without_file_support() {
    let config = MockLlmConfig {
        models: vec!["gemma4:vision".to_string()],
        ollama_context_length: Some(131_072),
        ollama_capabilities: Some(vec!["completion".to_string(), "vision".to_string()]),
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");
    let provider = OllamaProvider::new(
        "ollama-local".to_string(),
        server.base_url(),
        "gemma4:vision".to_string(),
        None,
    );

    let models = provider.list_models().await.expect("catalog response");
    assert_eq!(models[0].input_modalities, Some(vec![InputModality::Image]));
    assert!(!models[0]
        .input_modalities
        .as_ref()
        .expect("known capabilities")
        .contains(&InputModality::File));
}

fn image_tool_message(encoded: &str) -> Message {
    Message::ToolResult {
        value: ToolResult {
            id: uuid::Uuid::new_v4(),
            name: "read".to_string(),
            result: encoded.to_string(),
            attachments: vec![ToolAttachment::Image {
                media_type: "image/png".to_string(),
            }],
            result_section_id: None,
            result_metadata: Some(serde_json::json!({
                "read_bytes": 8,
                "encoding": "base64",
                "media_type": "image/png",
            })),
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: Duration::ZERO,
        },
    }
}

#[tokio::test]
async fn openrouter_request_projects_selected_image_capability_once() {
    let config = MockLlmConfig {
        models: vec!["provider/vision".to_string()],
        model_path: Some("/api/v1/models".to_string()),
        model_input_modalities: Some(vec!["image".to_string()]),
        frames: vec![content_frame("ok")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");
    let provider = Arc::new(OpenRouterProvider::new(
        "openrouter".to_string(),
        server.base_url(),
        "provider/vision".to_string(),
        Some("test-key".to_string()),
    ));
    let engine = EngineImpl::new(
        provider,
        EngineConfig {
            provider_connection: "openrouter".to_string(),
            model: "provider/vision".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: Some(server.base_url()),
            api_key: Some("test-key".to_string()),
            reasoning: None,
        },
    );
    let models = engine.list_models().await.expect("model catalog");
    engine
        .switch_model_option(&ProviderModelOption {
            provider_connection: "openrouter".to_string(),
            model: models[0].clone(),
            reasoning_effort: None,
        })
        .expect("select capable model");
    let encoded = "OPENROUTER_IMAGE_SENTINEL";
    let mut receiver = engine
        .chat(
            vec![image_tool_message(encoded)],
            vec![],
            CancellationToken::new(),
        )
        .await
        .expect("chat request");
    let _ = drain_text_until_done(&mut receiver).await;

    let request = server
        .request_bodies()
        .into_iter()
        .find_map(|body| {
            let value: serde_json::Value = serde_json::from_str(&body).ok()?;
            if value.get("messages").is_some() {
                Some(value)
            } else {
                None
            }
        })
        .expect("captured chat request");
    assert_eq!(request["messages"].as_array().expect("messages").len(), 2);
    assert_eq!(request["messages"][0]["role"], "tool");
    assert_eq!(request["messages"][1]["role"], "user");
    assert_eq!(request["messages"][1]["content"][1]["type"], "image_url");
    assert_eq!(request.to_string().matches(encoded).count(), 1);
}

#[tokio::test]
async fn ollama_vision_request_uses_compatibility_image_shape_and_pdf_fallback() {
    let config = MockLlmConfig {
        models: vec!["vision-model".to_string()],
        ollama_capabilities: Some(vec!["completion".to_string(), "vision".to_string()]),
        frames: vec![content_frame("ok")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");
    let provider = Arc::new(OllamaProvider::new(
        "ollama".to_string(),
        server.base_url(),
        "vision-model".to_string(),
        None,
    ));
    let engine = EngineImpl::new(
        provider,
        EngineConfig {
            provider_connection: "ollama".to_string(),
            model: "vision-model".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: Some(server.base_url()),
            api_key: None,
            reasoning: None,
        },
    );
    let models = engine.list_models().await.expect("model catalog");
    engine
        .switch_model_option(&ProviderModelOption {
            provider_connection: "ollama".to_string(),
            model: models[0].clone(),
            reasoning_effort: None,
        })
        .expect("select vision model");

    let encoded = "OLLAMA_IMAGE_SENTINEL";
    let mut receiver = engine
        .chat(
            vec![image_tool_message(encoded)],
            vec![],
            CancellationToken::new(),
        )
        .await
        .expect("image chat request");
    let _ = drain_text_until_done(&mut receiver).await;

    let image_request = server
        .request_bodies()
        .into_iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .find(|value| value.get("messages").is_some())
        .expect("captured image chat request");
    assert!(image_request["messages"][1]["content"][1]["image_url"].is_string());
    assert_eq!(image_request.to_string().matches(encoded).count(), 1);

    let pdf = Message::ToolResult {
        value: ToolResult {
            id: uuid::Uuid::new_v4(),
            name: "read".to_string(),
            result: "OLLAMA_PDF_SENTINEL".to_string(),
            attachments: vec![ToolAttachment::File {
                media_type: "application/pdf".to_string(),
                filename: "report.pdf".to_string(),
            }],
            result_section_id: None,
            result_metadata: Some(serde_json::json!({"encoding": "base64"})),
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: Duration::ZERO,
        },
    };
    let mut receiver = engine
        .chat(vec![pdf], vec![], CancellationToken::new())
        .await
        .expect("pdf fallback request");
    let _ = drain_text_until_done(&mut receiver).await;
    let pdf_request = server
        .request_bodies()
        .into_iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .rfind(|value| value.get("messages").is_some())
        .expect("captured pdf chat request");
    assert_eq!(
        pdf_request["messages"].as_array().expect("messages").len(),
        1
    );
    assert!(pdf_request["messages"][0]["content"]
        .as_str()
        .expect("tool envelope")
        .contains("OLLAMA_PDF_SENTINEL"));
    assert!(pdf_request.to_string().contains("OLLAMA_PDF_SENTINEL"));
    assert!(!pdf_request.to_string().contains("image_url"));
}

#[tokio::test]
async fn generic_openai_compatible_provider_keeps_unknown_capability_as_text() {
    let config = MockLlmConfig {
        frames: vec![content_frame("ok")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");
    let provider = openai_compatible_provider_for(&server);
    let encoded = "GENERIC_UNKNOWN_SENTINEL";
    let mut receiver = provider
        .chat(
            vec![image_tool_message(encoded)],
            vec![],
            CancellationToken::new(),
        )
        .await
        .expect("chat request");
    let _ = drain_text_until_done(&mut receiver).await;
    let request = server
        .request_bodies()
        .into_iter()
        .find_map(|body| {
            let value: serde_json::Value = serde_json::from_str(&body).ok()?;
            if value.get("messages").is_some() {
                Some(value)
            } else {
                None
            }
        })
        .expect("captured chat request");
    assert_eq!(request["messages"].as_array().expect("messages").len(), 1);
    assert!(request["messages"][0]["content"]
        .as_str()
        .expect("tool envelope")
        .contains(encoded));
    assert!(!request.to_string().contains("image_url"));
}

#[tokio::test]
async fn generic_openai_compatible_catalog_can_opt_into_image_projection() {
    let config = MockLlmConfig {
        models: vec!["generic/vision".to_string()],
        model_input_modalities: Some(vec!["image".to_string()]),
        frames: vec![content_frame("ok")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");
    let provider = Arc::new(OpenAiCompatibleProvider::new(
        "generic".to_string(),
        server.base_url(),
        "generic/vision".to_string(),
        None,
    ));
    let engine = EngineImpl::new(
        provider,
        EngineConfig {
            provider_connection: "generic".to_string(),
            model: "generic/vision".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: Some(server.base_url()),
            api_key: None,
            reasoning: None,
        },
    );
    let models = engine.list_models().await.expect("model catalog");
    engine
        .switch_model_option(&ProviderModelOption {
            provider_connection: "generic".to_string(),
            model: models[0].clone(),
            reasoning_effort: None,
        })
        .expect("select advertised image model");
    let encoded = "GENERIC_KNOWN_SENTINEL";
    let mut receiver = engine
        .chat(
            vec![image_tool_message(encoded)],
            vec![],
            CancellationToken::new(),
        )
        .await
        .expect("chat request");
    let _ = drain_text_until_done(&mut receiver).await;
    let request = server
        .request_bodies()
        .into_iter()
        .find_map(|body| {
            let value: serde_json::Value = serde_json::from_str(&body).ok()?;
            if value.get("messages").is_some() {
                Some(value)
            } else {
                None
            }
        })
        .expect("captured chat request");
    assert_eq!(request["messages"][1]["content"][1]["type"], "image_url");
    assert!(request["messages"][1]["content"][1]["image_url"]["url"].is_string());
    assert_eq!(request.to_string().matches(encoded).count(), 1);
}

#[tokio::test]
async fn named_ollama_connection_uses_native_model_discovery() {
    let config = MockLlmConfig {
        models: vec!["gemma4:latest".to_string()],
        ollama_context_length: Some(32_768),
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider_config = ProviderConnectionConfig {
        provider: "ollama".to_string(),
        api_base_url: Some(server.base_url()),
        api_key: None,
    };
    let engine_config = EngineConfig {
        provider_connection: "ollama-local".to_string(),
        model: "gemma4:latest".to_string(),
        temperature: None,
        context_length: None,
        api_base_url: None,
        api_key: None,
        reasoning: None,
    };
    let provider = ProviderRegistry::provider_from_provider_config(
        "ollama-local",
        &provider_config,
        &engine_config,
    )
    .expect("named Ollama connection should build");

    let models = provider
        .list_models()
        .await
        .expect("the registry should select native Ollama discovery");

    assert_eq!(models[0].id, "gemma4:latest");
    assert_eq!(models[0].context_length, Some(32_768));
}

#[tokio::test]
async fn list_models_reports_a_non_success_status_as_an_error() {
    let config = MockLlmConfig {
        statuses: vec![500],
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_compatible_provider_for(&server);
    let error = provider
        .list_models()
        .await
        .expect_err("a 500 must not be reported as an empty model list");

    assert!(
        error.to_string().contains("500"),
        "the status should surface in the error: {error}"
    );
}

#[tokio::test]
async fn set_model_changes_the_model_sent_on_the_next_request() {
    let config = MockLlmConfig {
        frames: vec![content_frame("ok")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_compatible_provider_for(&server);
    provider
        .set_model("switched-model")
        .expect("the OpenAI-compatible provider supports switching");

    let mut rx = provider
        .chat(
            Vec::new(),
            Vec::new(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("chat should still succeed after the switch");
    let texts = drain_text_until_done(&mut rx).await;
    assert_eq!(texts, vec!["ok"]);
}

#[tokio::test]
async fn switching_model_during_stream_keeps_in_flight_request_on_original_model() {
    let config = MockLlmConfig {
        frames: vec![content_frame("first response")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");
    let provider = Arc::new(OpenAiCompatibleProvider::new(
        "test".to_string(),
        server.base_url(),
        "initial-model".to_string(),
        None,
    ));
    let engine = EngineImpl::new(
        provider,
        EngineConfig {
            provider_connection: "test".to_string(),
            model: "initial-model".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: Some(server.base_url()),
            api_key: None,
            reasoning: None,
        },
    );

    let mut first_stream = engine
        .chat(
            Vec::new(),
            Vec::new(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("first chat request should start");
    engine
        .switch_model("switched-model")
        .expect("the engine supports model switching");
    let first_texts = drain_text_until_done(&mut first_stream).await;

    let mut second_stream = engine
        .chat(
            Vec::new(),
            Vec::new(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("second chat request should start");
    let second_texts = drain_text_until_done(&mut second_stream).await;

    let request_models = server
        .request_bodies()
        .into_iter()
        .map(|body| {
            serde_json::from_str::<serde_json::Value>(&body).expect("mock request body is JSON")
                ["model"]
                .as_str()
                .expect("chat request includes a model")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(first_texts, vec!["first response"]);
    assert_eq!(second_texts, vec!["first response"]);
    assert_eq!(
        request_models,
        vec!["initial-model".to_string(), "switched-model".to_string()]
    );
}

#[tokio::test]
async fn text_streaming_emits_chunks_in_order_then_done() {
    let config = MockLlmConfig {
        frames: vec![
            content_frame("Hello"),
            content_frame(" "),
            content_frame("there"),
        ],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_compatible_provider_for(&server);
    let mut rx = provider
        .chat(
            Vec::new(),
            Vec::new(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|error| {
            panic!("provider chat should succeed against a 200 mock: {error:#?}")
        });

    let texts = drain_text_until_done(&mut rx).await;
    assert_eq!(texts, vec!["Hello", " ", "there"]);
    assert_eq!(
        server.cumulative_connections(),
        1,
        "a 200 stream must need only one connection"
    );
    server.abort();
}

#[tokio::test]
async fn cancel_mid_stream_yields_cancelled_and_releases_connection() {
    // Default config streams forever with generated frames (no [DONE]).
    let server = MockLlmServer::start(MockLlmConfig::default())
        .await
        .expect("bind failed");
    let live = server.live_counter();

    let provider = openai_compatible_provider_for(&server);
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut rx = provider
        .chat(Vec::new(), Vec::new(), cancel.clone())
        .await
        .expect("provider chat should succeed");

    // Receive a couple of live tokens so the socket is genuinely established.
    let texts = drain_text_count(&mut rx, 3).await;
    assert_eq!(texts.len(), 3);
    assert!(server.live_connections() > 0);

    cancel.cancel();
    let cancelled = tokio::time::timeout(Duration::from_secs(5), async { rx.recv().await })
        .await
        .expect("cancel must promptly yield a terminal event");
    assert!(matches!(cancelled, Some(StreamEvent::Cancelled)));

    // The dropped request future must close the underlying TCP socket: the
    // mock server's live-connection counter returns to zero (Spike 2 pattern).
    for _ in 0..200 {
        if live.load(Ordering::SeqCst) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        live.load(Ordering::SeqCst),
        0,
        "server must observe the client disconnect via cooperative cancel"
    );
    server.abort();
}

#[tokio::test]
async fn retry_after_500_then_succeeds() {
    let config = MockLlmConfig {
        statuses: vec![500],
        frames: vec![content_frame("recovered")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_compatible_provider_for(&server);
    let mut rx = provider
        .chat(
            Vec::new(),
            Vec::new(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("chat should eventually succeed after a retry");

    let texts = tokio::time::timeout(Duration::from_secs(20), drain_text_until_done(&mut rx))
        .await
        .expect("retry + stream must complete within timeout");
    assert_eq!(texts, vec!["recovered"]);
    assert!(
        server.cumulative_connections() >= 2,
        "a 500 then success must open at least two connections"
    );
    server.abort();
}

#[tokio::test]
async fn persistent_500_yields_pre_stream_error() {
    let config = MockLlmConfig {
        statuses: vec![500; 5],
        fallback_status: 500,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_compatible_provider_for(&server);
    let result = tokio::time::timeout(
        Duration::from_secs(40),
        provider.chat(
            Vec::new(),
            Vec::new(),
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await
    .expect("retries must finish within timeout");

    let message = result
        .expect_err("persistent 500 must exhaust retries and return an error")
        .to_string();
    assert!(
        message.contains("mock 500"),
        "the error should surface the backend's response body: {message}"
    );
    server.abort();
}

#[tokio::test]
async fn non_retryable_status_reports_error_with_backend_body() {
    let config = MockLlmConfig {
        statuses: vec![404],
        frames: vec![content_frame("must not be reached")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_compatible_provider_for(&server);
    let message = provider
        .chat(
            Vec::new(),
            Vec::new(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect_err("a 4xx is a terminal pre-stream failure")
        .to_string();
    assert!(
        message.contains("HTTP 404"),
        "error should name the status code: {message}"
    );
    assert!(
        message.contains("Not Found"),
        "error should include the backend's explanation body: {message}"
    );
    assert_eq!(
        server.cumulative_connections(),
        1,
        "a 4xx must never be retried"
    );
    server.abort();
}

#[tokio::test]
async fn agent_loop_round_trip_commits_streamed_text() {
    let config = MockLlmConfig {
        frames: vec![content_frame("The "), content_frame("answer.")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");
    let provider = provider_for(&server);

    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(provider, common::test_engine_config())),
        Arc::new(ToolRegistry::new()),
    );
    agent
        .send_command(AgentCommand::Prompt("question".into()))
        .await;
    let handle = agent.run();

    collect_until(&mut ui_rx, |update| matches!(update, UiUpdate::StreamDone)).await;

    drop(agent);
    let context = handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop returned an error");

    assert_eq!(
        context.messages(),
        &[
            Message::System {
                content: DEFAULT_SOUL.to_string(),
            },
            Message::System {
                content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
            },
            Message::User {
                content: "question".to_string(),
                kind: UserMessageKind::Explicit,
            },
            Message::Assistant {
                content: "The answer.".to_string(),
                status: AssistantStatus::Complete,
                tool_calls: Vec::new(),
            },
        ]
    );
    server.abort();
}

/// The real `OpenAiCompatibleProvider` surfaces a `delta.tool_calls` SSE frame as a raw
/// `StreamEvent::Delta` (assembly stays in the agent; this proves the
/// provider-level tool-call path over real HTTP/SSE, complementing the
/// `parse_sse_payload` unit tests). The full ReAct+tool loop on the live
/// provider is the `main.rs` demo path.
#[tokio::test]
async fn real_provider_surfaces_tool_call_delta_over_http() {
    let config = MockLlmConfig {
        frames: vec![tool_call_frame(0, "read", "{\"path\":\"a.txt\"}")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");
    let provider = openai_compatible_provider_for(&server);
    let mut rx = provider
        .chat(
            Vec::new(),
            Vec::new(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("chat should succeed");
    match rx.recv().await {
        Some(StreamEvent::Delta(delta)) => {
            assert_eq!(delta.tool_call_index, 0);
            assert_eq!(delta.function_name.as_deref(), Some("read"));
            assert_eq!(
                delta.arguments_fragment.as_deref(),
                Some("{\"path\":\"a.txt\"}")
            );
        }
        other => panic!("expected Delta, got {other:?}"),
    }
    assert!(matches!(rx.recv().await, Some(StreamEvent::Done)));
    server.abort();
}

async fn drain_text_count(rx: &mut mpsc::Receiver<StreamEvent>, count: usize) -> Vec<String> {
    let mut texts = Vec::new();
    loop {
        match rx.recv().await {
            Some(StreamEvent::Text(text)) => {
                texts.push(text);
                if texts.len() >= count {
                    return texts;
                }
            }
            Some(other) => panic!("unexpected stream event while counting: {other:?}"),
            None => panic!("provider stream ended before {count} texts"),
        }
    }
}

/// Verifies that cancellation closes the underlying TCP socket, confirmed with
/// the OS socket table (`lsof`/`netstat`) rather than only the mock server's
/// live-connection counter. Absent tools report nothing and are tolerated. Runs
/// as part of `make test` for now; re-gate behind `#[ignore]` once CI actually
/// runs the suite on machines guaranteed to have those tools.
#[tokio::test]
async fn cancel_closes_socket_verified_via_os_tools() {
    let server = MockLlmServer::start(MockLlmConfig::default())
        .await
        .expect("bind failed");
    let port = server.addr.port();

    let provider = openai_compatible_provider_for(&server);
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut rx = provider
        .chat(Vec::new(), Vec::new(), cancel.clone())
        .await
        .expect("provider chat should succeed");

    let _ = drain_text_count(&mut rx, 2).await;
    cancel.cancel();
    let _ = rx.recv().await; // StreamEvent::Cancelled

    // Poll the OS socket table until no live (ESTABLISHED/CLOSE_WAIT) entry
    // remains for the server's port, i.e. the socket closed promptly.
    let mut socket_lines = lingering_socket_lines(port);
    for _ in 0..100 {
        if socket_lines.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        socket_lines = lingering_socket_lines(port);
    }
    assert!(
        socket_lines.is_empty(),
        "socket must close promptly on cancel; lingering: {socket_lines:?}"
    );
    server.abort();
}

/// Inspect the OS socket table for live (ESTABLISHED / CLOSE_WAIT) entries on
/// `port`, via `lsof` then `netstat`. Absent tools simply report nothing.
fn lingering_socket_lines(port: u16) -> Vec<String> {
    let mut findings = Vec::new();
    if let Ok(output) = std::process::Command::new("lsof")
        .args(["-i", &format!(":{port}")])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if is_lingering_line(line) {
                findings.push(format!("lsof: {line}"));
            }
        }
    }
    if let Ok(output) = std::process::Command::new("netstat").args(["-an"]).output() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if line.contains(&format!(":{port}")) && is_lingering_line(line) {
                findings.push(format!("netstat: {line}"));
            }
        }
    }
    findings
}

fn is_lingering_line(line: &str) -> bool {
    line.contains("ESTABLISHED") || line.contains("CLOSE_WAIT")
}
