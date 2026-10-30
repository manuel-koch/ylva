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

use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{Message, StreamEvent};
use crate::llm::engine::{EngineRuntimeConfig, EngineRuntimeConfigHandle};
use crate::llm::{ModelInfo, Provider, RequestCost, ToolSchema};

use super::openai_compatible_provider::{model_list_entries, ModelCatalog};
use super::{parse_input_modality_names, parse_reasoning_object, OpenAiCompatibleProvider};

// OpenRouter app attribution (https://openrouter.ai/docs/app-attribution).
// Header names are lowercase because `HeaderName::from_static` requires it.
const APP_ATTRIBUTION_REFERER_HEADER: &str = "http-referer";
const APP_ATTRIBUTION_TITLE_HEADER: &str = "x-openrouter-title";
const APP_ATTRIBUTION_CATEGORIES_HEADER: &str = "x-openrouter-categories";
const APP_ATTRIBUTION_REFERER: &str = env!("CARGO_PKG_REPOSITORY");
const APP_ATTRIBUTION_TITLE: &str = "Ylva";
const APP_ATTRIBUTION_CATEGORIES: &str = "cli-agent,personal-agent";

/// Parse the request cost OpenRouter reports as `usage.cost` (credits, USD)
/// in a stream frame. Usage-only frames carry no choices, so this is
/// independent of the user-visible stream event parser.
fn parse_reported_cost(data: &str) -> Option<RequestCost> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let amount = value
        .pointer("/usage/cost")
        .and_then(serde_json::Value::as_f64)
        .filter(|amount| amount.is_finite() && *amount >= 0.0)?;
    Some(RequestCost::new(amount, "USD"))
}

/// Reads OpenRouter's `/models` shape: `context_length`,
/// `architecture.input_modalities` and `reasoning`.
#[derive(Debug, Default)]
struct OpenRouterModelCatalog;

impl ModelCatalog for OpenRouterModelCatalog {
    fn parse(&self, body: &serde_json::Value) -> Vec<ModelInfo> {
        model_list_entries(body)
            .filter_map(|entry| {
                let id = entry.get("id")?.as_str()?;
                let (reasoning_efforts, reasoning_default) =
                    parse_reasoning_object(entry.get("reasoning"));
                Some(ModelInfo {
                    id: id.to_string(),
                    context_length: entry
                        .get("context_length")
                        .and_then(serde_json::Value::as_u64)
                        .map(|length| length as usize),
                    reasoning_efforts,
                    reasoning_default,
                    input_modalities: entry
                        .pointer("/architecture/input_modalities")
                        .and_then(parse_input_modality_names),
                })
            })
            .collect()
    }
}

/// OpenRouter provider.
///
/// OpenRouter exposes both chat completions and its model catalog through the
/// OpenAI-compatible `/api/v1` path. The provider accepts the OpenRouter
/// origin and derives that path before constructing the shared transport.
#[derive(Debug)]
pub struct OpenRouterProvider {
    inner: OpenAiCompatibleProvider,
}

impl OpenRouterProvider {
    /// Create an OpenRouter provider.
    ///
    /// `base_url` should normally be `https://openrouter.ai`.
    pub fn new(name: String, base_url: String, model: String, api_key: Option<String>) -> Self {
        let openai_base_url = format!("{base_url}/api/v1");
        Self {
            inner: OpenAiCompatibleProvider::new_with_request_body_customizer(
                name,
                openai_base_url,
                model,
                api_key,
                Self::customize_request_body,
                super::openai_compatible_provider::redact_chat_completion_request_body,
            )
            .with_provider_kind("openrouter")
            .with_model_catalog(std::sync::Arc::new(OpenRouterModelCatalog))
            .with_usage_cost_parser(parse_reported_cost)
            .with_extra_headers(vec![
                (APP_ATTRIBUTION_REFERER_HEADER, APP_ATTRIBUTION_REFERER),
                (APP_ATTRIBUTION_TITLE_HEADER, APP_ATTRIBUTION_TITLE),
                (
                    APP_ATTRIBUTION_CATEGORIES_HEADER,
                    APP_ATTRIBUTION_CATEGORIES,
                ),
            ]),
        }
    }

    fn customize_request_body(
        request_body: &mut serde_json::Value,
        runtime_config: &EngineRuntimeConfig,
    ) {
        if let Some(reasoning_effort) = runtime_config.reasoning_effort.as_deref() {
            request_body["reasoning"] = serde_json::json!({
                "effort": reasoning_effort,
            });
        }
    }
}

impl Provider for OpenRouterProvider {
    fn with_runtime_config(
        &self,
        runtime_config: EngineRuntimeConfigHandle,
    ) -> anyhow::Result<std::sync::Arc<dyn Provider>> {
        Ok(std::sync::Arc::new(Self {
            inner: self.inner.clone_with_runtime_config(runtime_config),
        }))
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn runtime_config_handle(&self) -> Option<EngineRuntimeConfigHandle> {
        self.inner.runtime_config_handle()
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        self.inner.list_models()
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        self.inner.set_model(model)
    }

    fn set_reasoning_effort(&self, reasoning_effort: Option<&str>) -> anyhow::Result<()> {
        self.inner.set_runtime_reasoning_effort(reasoning_effort)
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner.chat(messages, tools, cancel)
    }

    fn chat_with_session_id<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner
            .chat_with_session_id(messages, tools, cancel, session_id)
    }

    fn chat_with_session_id_and_estimate<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<
            dyn FnOnce(usize, usize) -> futures::future::BoxFuture<'static, ()> + Send,
        >,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner.chat_with_session_id_and_estimate(
            messages,
            tools,
            cancel,
            session_id,
            on_estimate,
        )
    }

    fn chat_with_session_id_and_diagnostics<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<
            dyn FnOnce(usize, usize) -> futures::future::BoxFuture<'static, ()> + Send,
        >,
        on_failure: Option<crate::http::RequestDumpFailureHandler>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner.chat_with_session_id_and_diagnostics(
            messages,
            tools,
            cancel,
            session_id,
            on_estimate,
            on_failure,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribution_referer_is_the_public_repository() {
        assert_eq!(
            APP_ATTRIBUTION_REFERER,
            "https://github.com/manuel-koch/ylva"
        );
    }

    fn parse_models(body: serde_json::Value) -> Vec<ModelInfo> {
        OpenRouterModelCatalog.parse(&body)
    }

    #[test]
    fn model_list_preserves_context_length_and_drops_entries_without_id() {
        let models = parse_models(serde_json::json!({
            "data": [
                {"id": "openai/gpt-oss-120b", "name": "x", "context_length": 131072},
                {"name": "missing-id"}
            ]
        }));
        assert_eq!(
            models,
            vec![ModelInfo {
                id: "openai/gpt-oss-120b".to_string(),
                context_length: Some(131072),
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            }]
        );
    }

    #[test]
    fn model_list_reads_ordered_efforts_and_conservative_defaults() {
        let models = parse_models(serde_json::json!({
            "data": [
                {"id": "unordered", "reasoning": {
                    "supported_efforts": ["high", "none", "xhigh", "low"],
                    "default_effort": "high"}},
                {"id": "no-selector", "reasoning": {}},
                {"id": "bad-default", "reasoning": {"default_effort": 3}},
                {"id": "empty-default", "reasoning": {"default_effort": ""}},
                {"id": "no-reasoning"}
            ]
        }));
        assert_eq!(
            models
                .iter()
                .map(|model| model.reasoning_efforts.clone())
                .collect::<Vec<_>>(),
            vec![
                Some(["none", "low", "high", "xhigh"].map(String::from).to_vec()),
                Some(Vec::new()),
                Some(Vec::new()),
                Some(Vec::new()),
                None,
            ]
        );
        assert_eq!(
            models
                .iter()
                .map(|model| model.reasoning_default.clone())
                .collect::<Vec<_>>(),
            vec![Some("high".to_string()), None, None, None, None]
        );
    }

    #[test]
    fn model_list_reads_input_modalities_conservatively() {
        let models = parse_models(serde_json::json!({
            "data": [
                {"id": "vision-and-files",
                 "architecture": {"input_modalities": ["text", "image", "file"]}},
                {"id": "known-text-only",
                 "architecture": {"input_modalities": ["text", "audio"]}},
                {"id": "unknown"}
            ]
        }));
        assert_eq!(
            models
                .iter()
                .map(|model| model.input_modalities.clone())
                .collect::<Vec<_>>(),
            vec![
                Some(vec![
                    crate::llm::InputModality::Image,
                    crate::llm::InputModality::File
                ]),
                Some(vec![]),
                None,
            ]
        );
    }

    #[test]
    fn parses_reported_cost_from_usage_frames() {
        assert_eq!(
            parse_reported_cost(r#"{"choices":[],"usage":{"prompt_tokens":1,"cost":0.00123}}"#),
            Some(RequestCost::new(0.00123, "USD"))
        );
        assert_eq!(
            parse_reported_cost(r#"{"usage":{"cost":0}}"#),
            Some(RequestCost::new(0.0, "USD"))
        );
        assert_eq!(
            parse_reported_cost(r#"{"usage":{"prompt_tokens":1}}"#),
            None
        );
        assert_eq!(parse_reported_cost(r#"{"usage":{"cost":"1.5"}}"#), None);
        assert_eq!(parse_reported_cost(r#"{"usage":{"cost":null}}"#), None);
        assert_eq!(parse_reported_cost("not json"), None);
    }

    #[test]
    fn delegates_identity_runtime_settings_and_chat() {
        crate::test_support::run_locked(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            use tokio::net::TcpListener;

            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                let listener = TcpListener::bind("127.0.0.1:0").await.expect("loopback server");
                let address = listener.local_addr().expect("server address");
                let server = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.expect("provider request");
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 2048];
                    let request_body = loop {
                        let bytes_read = stream.read(&mut buffer).await.expect("read request");
                        assert!(bytes_read > 0, "request must include its complete body");
                        request.extend_from_slice(&buffer[..bytes_read]);
                        let Some(header_end) = request
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .map(|position| position + 4)
                        else {
                            continue;
                        };
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        assert!(headers.starts_with("POST /api/v1/chat/completions HTTP/1.1"));
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().expect("content length"))
                            })
                            .expect("request content length");
                        if request.len() >= header_end + content_length {
                            break serde_json::from_slice::<serde_json::Value>(
                                &request[header_end..header_end + content_length],
                            )
                            .expect("request JSON");
                        }
                    };
                    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"delegated reply\"}}]}\n\ndata: [DONE]\n\n";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.expect("SSE response");
                    request_body
                });

                let provider = OpenRouterProvider::new(
                    "openrouter".to_string(),
                    format!("http://{address}"),
                    "some/model".to_string(),
                    Some("test-key".to_string()),
                );
                assert_eq!(provider.name(), "openrouter");
                let runtime_config = provider
                    .runtime_config_handle()
                    .expect("OpenRouter exposes Engine runtime settings");
                provider
                    .set_model("other/model")
                    .expect("OpenRouter supports model switching");
                assert_eq!(
                    runtime_config
                        .read()
                        .expect("Engine runtime configuration lock")
                        .model,
                    "other/model"
                );
                let mut events = provider
                    .chat(Vec::new(), Vec::new(), CancellationToken::new())
                    .await
                    .expect("delegated chat");
                assert!(matches!(
                    events.recv().await,
                    Some(StreamEvent::Text(text)) if text == "delegated reply"
                ));
                assert!(matches!(events.recv().await, Some(StreamEvent::Done)));
                assert_eq!(server.await.expect("server completed")["model"], "other/model");
            })
            .await
            .expect("delegated request must complete promptly");
        });
    }

    #[test]
    fn customizes_reasoning_request_body() {
        let mut request_body = serde_json::json!({});
        let runtime_config = crate::llm::engine::EngineRuntimeConfig {
            context_length_override: None,
            model: "some/model".to_string(),
            temperature: None,
            context_length: crate::llm::engine::DEFAULT_CONTEXT_LENGTH,
            reasoning_effort: Some("high".to_string()),
            input_modalities: None,
        };

        OpenRouterProvider::customize_request_body(&mut request_body, &runtime_config);

        assert_eq!(
            request_body,
            serde_json::json!({"reasoning": {"effort": "high"}})
        );
    }
}
