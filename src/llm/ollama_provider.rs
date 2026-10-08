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

use futures::future::{join_all, BoxFuture};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{Message, StreamEvent};
use crate::llm::engine::{EngineRuntimeConfig, EngineRuntimeConfigHandle};
use crate::llm::openai_compatible_messages::ContentPartFormat;
use crate::llm::{InputModality, ModelInfo, Provider, ToolSchema};

use super::OpenAiCompatibleProvider;
use crate::http::{get_json, post_json};

/// Ollama provider.
///
/// Ollama serves chat completions below `/v1`, but its native model catalog
/// lives below `/api`. Consequently the configured base URL is the Ollama
/// server origin (for example, `http://localhost:11434`), not the
/// OpenAI-compatible `/v1` path.
#[derive(Debug)]
pub struct OllamaProvider {
    inner: OpenAiCompatibleProvider,
    native_base_url: String,
}

impl OllamaProvider {
    /// Create an Ollama provider from the server origin.
    pub fn new(name: String, base_url: String, model: String, api_key: Option<String>) -> Self {
        let native_base_url = base_url.trim_end_matches('/').to_string();
        let openai_base_url = format!("{native_base_url}/v1");
        Self {
            inner: OpenAiCompatibleProvider::new_with_request_body_customizer_and_format(
                name,
                openai_base_url,
                model,
                api_key,
                Self::customize_request_body,
                ContentPartFormat::OllamaCompatibility,
                super::openai_compatible_provider::redact_chat_completion_request_body,
            )
            .with_provider_kind("ollama"),
            native_base_url,
        }
    }

    fn customize_request_body(
        request_body: &mut serde_json::Value,
        runtime_config: &EngineRuntimeConfig,
    ) {
        if let Some(reasoning_effort) = runtime_config.reasoning_effort.as_deref() {
            request_body["reasoning_effort"] = serde_json::json!(reasoning_effort);
        }
    }

    fn api_url(&self, endpoint: &str) -> String {
        format!("{}/api/{endpoint}", self.native_base_url)
    }
}

impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn runtime_config_handle(&self) -> Option<EngineRuntimeConfigHandle> {
        self.inner.runtime_config_handle()
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        let tags_url = self.api_url("tags");
        let show_url = self.api_url("show");
        let client = self.inner.http_client();
        let api_key = self.inner.api_key().map(str::to_owned);
        let request_limiter = crate::llm::provider_request_limiter(self.name());

        Box::pin(async move {
            let tags_body = {
                let _permit = request_limiter.acquire().await?;
                get_json(&client, &tags_url, api_key.as_deref(), "model listing").await?
            };
            let models = parse_tags(&tags_body);
            let requests = models.into_iter().map(|mut model| {
                let client = client.clone();
                let api_key = api_key.clone();
                let show_url = show_url.clone();
                let request_limiter = request_limiter.clone();
                async move {
                    // `/api/tags` carries no `thinking` object, so the
                    // show-response enrichment below is the only source of
                    // thinking levels and the advertised default.
                    let request_body = serde_json::json!({ "model": model.id });
                    let _permit = match request_limiter.acquire().await {
                        Ok(permit) => permit,
                        Err(error) => {
                            tracing::warn!(model = %model.id, %error, "failed to acquire provider request slot");
                            return model;
                        }
                    };
                    match post_json(
                        &client,
                        &show_url,
                        &request_body,
                        api_key.as_deref(),
                        "model details",
                    )
                    .await
                    {
                        Ok(body) => {
                            if model.context_length.is_none() {
                                model.context_length = parse_context_length(&body);
                            }
                            let (reasoning_efforts, reasoning_default) = merge_reasoning_metadata(
                                model.reasoning_efforts.clone(),
                                model.reasoning_default.clone(),
                                &body,
                            );
                            model.reasoning_efforts = reasoning_efforts;
                            model.reasoning_default = reasoning_default;
                            if model.input_modalities.is_none() {
                                model.input_modalities = parse_input_modalities(&body);
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                model = %model.id,
                                %error,
                                "failed to load Ollama model details"
                            );
                        }
                    }
                    model
                }
            });
            Ok(join_all(requests).await)
        })
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

fn parse_tags(body: &serde_json::Value) -> Vec<ModelInfo> {
    body.get("models")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = entry
                .get("model")
                .or_else(|| entry.get("name"))
                .and_then(serde_json::Value::as_str)?;
            let context_length = entry
                .get("context_length")
                .and_then(json_usize)
                .or_else(|| {
                    entry
                        .get("details")
                        .and_then(|details| details.get("context_length"))
                        .and_then(json_usize)
                });
            Some(ModelInfo {
                id: id.to_string(),
                context_length,
                reasoning_efforts: parse_reasoning_efforts(entry),
                reasoning_default: None,
                input_modalities: None,
            })
        })
        .collect()
}

fn parse_context_length(body: &serde_json::Value) -> Option<usize> {
    body.get("context_length").and_then(json_usize).or_else(|| {
        body.get("model_info")
            .and_then(serde_json::Value::as_object)
            .and_then(|model_info| {
                model_info.iter().find_map(|(key, value)| {
                    key.rsplit('.')
                        .next()
                        .filter(|suffix| suffix.eq_ignore_ascii_case("context_length"))
                        .and_then(|_| json_usize(value))
                })
            })
    })
}

fn json_usize(value: &serde_json::Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .or_else(|| value.as_str()?.parse().ok())
}

fn parse_reasoning_efforts(body: &serde_json::Value) -> Option<Vec<String>> {
    if let Some(efforts) = parse_thinking_values(body) {
        return Some(efforts);
    }
    let capabilities = body.get("capabilities")?.as_array()?;
    capabilities
        .iter()
        .filter_map(serde_json::Value::as_str)
        .any(|capability| capability == "thinking")
        .then(Vec::new)
}

/// Parse the selectable reasoning levels of `/api/show`'s top-level
/// `thinking.values` array.
///
/// Upstream: https://github.com/ollama/ollama/pull/18473 and
/// https://github.com/ollama/ollama/commit/d0c8cdb795d4640890d87c146d674cc311df4138.
///
/// De-duplicates while preserving the server's order. Returns `None` when
/// `thinking`/`values` is absent or when no entry maps to an effort name, so
/// callers fall through to the capability-based logic.
fn parse_thinking_values(body: &serde_json::Value) -> Option<Vec<String>> {
    let values = body.pointer("/thinking/values")?.as_array()?;
    let mut efforts: Vec<String> = Vec::new();
    for value in values {
        if let Some(effort) = map_thinking_value(value) {
            if !efforts.contains(&effort) {
                efforts.push(effort);
            }
        }
    }
    (!efforts.is_empty()).then_some(efforts)
}

/// Parse `/api/show`'s `thinking.default` — the effort a request falls back
/// to when it omits thinking controls — or `None` when it is absent or does
/// not map to an effort name.
fn parse_reasoning_default(body: &serde_json::Value) -> Option<String> {
    map_thinking_value(body.pointer("/thinking/default")?)
}

/// Map one `thinking.values`/`thinking.default` JSON entry to an effort name.
///
/// Non-empty strings pass through as-is; `false` maps to `"none"` (Ollama
/// resolves it to `think: false`) and `true` to `"high"` (a boolean-only
/// model treats any non-`none` effort as `think: true`, and `"high"` is also
/// a real level for named models). Every other JSON type is skipped.
fn map_thinking_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(effort) if !effort.is_empty() => Some(effort.clone()),
        serde_json::Value::Bool(true) => Some("high".to_string()),
        serde_json::Value::Bool(false) => Some("none".to_string()),
        _ => None,
    }
}

/// Upgrade a `/api/tags`-derived entry's reasoning metadata from an
/// `/api/show` body.
///
/// Fills `None`/`Some([])` efforts and a `None` default from the body; an
/// already-populated effort list is never downgraded nor replaced.
fn merge_reasoning_metadata(
    current_efforts: Option<Vec<String>>,
    current_default: Option<String>,
    body: &serde_json::Value,
) -> (Option<Vec<String>>, Option<String>) {
    let reasoning_efforts = match current_efforts {
        Some(efforts) if !efforts.is_empty() => Some(efforts),
        current => parse_reasoning_efforts(body).or(current),
    };
    let reasoning_default = current_default.or_else(|| parse_reasoning_default(body));
    (reasoning_efforts, reasoning_default)
}

fn parse_input_modalities(body: &serde_json::Value) -> Option<Vec<InputModality>> {
    let capabilities = body.get("capabilities")?.as_array()?;
    Some(
        if capabilities
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|capability| capability == "vision")
        {
            vec![InputModality::Image]
        } else {
            vec![]
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tags_uses_model_id_and_optional_metadata() {
        let body = serde_json::json!({
            "models": [
                {
                    "name": "gemma4:latest",
                    "model": "gemma4:latest",
                    "context_length": 32768
                },
                {}
            ]
        });

        assert_eq!(
            parse_tags(&body),
            vec![ModelInfo {
                id: "gemma4:latest".to_string(),
                context_length: Some(32768),
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            }]
        );
    }

    #[test]
    fn parse_context_length_reads_ollama_model_info_keys() {
        let body = serde_json::json!({
            "model_info": {
                "general.architecture": "llama",
                "llama.context_length": 131072
            }
        });
        assert_eq!(parse_context_length(&body), Some(131072));
    }

    #[test]
    fn parse_reasoning_efforts_maps_thinking_capability_without_provider_defaults() {
        let thinking_body = serde_json::json!({
            "capabilities": ["completion", "thinking"]
        });
        let regular_body = serde_json::json!({
            "capabilities": ["completion"]
        });
        let empty_values_body = serde_json::json!({
            "capabilities": ["completion", "thinking"],
            "thinking": { "values": [] }
        });
        let unmappable_values_body = serde_json::json!({
            "capabilities": ["completion", "thinking"],
            "thinking": { "values": [1, null, {}] }
        });
        let empty_object_body = serde_json::json!({
            "capabilities": ["completion", "thinking"],
            "thinking": {}
        });
        let unmappable_without_capability_body = serde_json::json!({
            "thinking": { "values": [1, null] }
        });

        assert_eq!(parse_reasoning_efforts(&thinking_body), Some(Vec::new()));
        assert_eq!(parse_reasoning_efforts(&regular_body), None);
        assert_eq!(
            parse_reasoning_efforts(&empty_values_body),
            Some(Vec::new())
        );
        assert_eq!(
            parse_reasoning_efforts(&unmappable_values_body),
            Some(Vec::new())
        );
        assert_eq!(
            parse_reasoning_efforts(&empty_object_body),
            Some(Vec::new())
        );
        assert_eq!(
            parse_reasoning_efforts(&unmappable_without_capability_body),
            None
        );
    }

    #[test]
    fn parse_reasoning_efforts_passes_named_thinking_levels_through_in_server_order() {
        let body = serde_json::json!({
            "capabilities": ["completion", "thinking"],
            "thinking": { "values": [false, "low", "medium", "xhigh"], "default": "medium" }
        });

        assert_eq!(
            parse_reasoning_efforts(&body),
            Some(vec![
                "none".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "xhigh".to_string(),
            ])
        );
    }

    #[test]
    fn parse_reasoning_efforts_maps_boolean_thinking_values_and_deduplicates() {
        let booleans_only = serde_json::json!({ "thinking": { "values": [false, true] } });
        let with_duplicate_name = serde_json::json!({
            "thinking": { "values": [false, true, "high"] }
        });

        assert_eq!(
            parse_reasoning_efforts(&booleans_only),
            Some(vec!["none".to_string(), "high".to_string()])
        );
        assert_eq!(
            parse_reasoning_efforts(&with_duplicate_name),
            Some(vec!["none".to_string(), "high".to_string()])
        );
    }

    #[test]
    fn parse_reasoning_efforts_reads_thinking_values_without_a_capabilities_key() {
        let body = serde_json::json!({
            "thinking": { "values": ["medium"], "default": "medium" }
        });

        assert_eq!(
            parse_reasoning_efforts(&body),
            Some(vec!["medium".to_string()])
        );
    }

    #[test]
    fn parse_reasoning_default_reads_provider_defaults() {
        assert_eq!(
            parse_reasoning_default(&serde_json::json!({ "thinking": { "default": "medium" } })),
            Some("medium".to_string())
        );
        assert_eq!(
            parse_reasoning_default(&serde_json::json!({ "thinking": { "default": true } })),
            Some("high".to_string())
        );
        assert_eq!(
            parse_reasoning_default(&serde_json::json!({ "thinking": { "default": false } })),
            Some("none".to_string())
        );
        assert_eq!(
            parse_reasoning_default(&serde_json::json!({ "thinking": { "default": "" } })),
            None
        );
        assert_eq!(
            parse_reasoning_default(&serde_json::json!({ "thinking": { "default": 3 } })),
            None
        );
        assert_eq!(
            parse_reasoning_default(&serde_json::json!({ "capabilities": ["thinking"] })),
            None
        );
        assert_eq!(parse_reasoning_default(&serde_json::json!({})), None);
    }

    #[test]
    fn merge_reasoning_metadata_upgrades_unset_reasoning_without_touching_populated_lists() {
        let show_body = serde_json::json!({
            "capabilities": ["completion", "thinking"],
            "thinking": { "values": ["low", "high"], "default": "low" }
        });

        assert_eq!(
            merge_reasoning_metadata(None, None, &show_body),
            (
                Some(vec!["low".to_string(), "high".to_string()]),
                Some("low".to_string())
            ),
            "unset metadata must be filled from the show response"
        );
        assert_eq!(
            merge_reasoning_metadata(Some(Vec::new()), Some("medium".to_string()), &show_body),
            (
                Some(vec!["low".to_string(), "high".to_string()]),
                Some("medium".to_string())
            ),
            "a capability-derived empty list must be upgraded, a known default kept"
        );
        assert_eq!(
            merge_reasoning_metadata(Some(vec!["xhigh".to_string()]), None, &show_body),
            (Some(vec!["xhigh".to_string()]), Some("low".to_string()),),
            "a populated list must never be replaced"
        );
        assert_eq!(
            merge_reasoning_metadata(
                Some(Vec::new()),
                None,
                &serde_json::json!({ "capabilities": ["completion"] })
            ),
            (Some(Vec::new()), None),
            "a body without reasoning metadata must leave the current state alone"
        );
    }

    #[test]
    fn parse_input_modalities_maps_vision_without_inventing_file_support() {
        let vision_body = serde_json::json!({
            "capabilities": ["completion", "vision"]
        });
        let text_body = serde_json::json!({
            "capabilities": ["completion"]
        });
        let missing_body = serde_json::json!({});

        assert_eq!(
            parse_input_modalities(&vision_body),
            Some(vec![InputModality::Image])
        );
        assert_eq!(parse_input_modalities(&text_body), Some(vec![]));
        assert_eq!(parse_input_modalities(&missing_body), None);
    }

    #[test]
    fn customizes_openai_compatible_reasoning_request_body() {
        let mut request_body = serde_json::json!({});
        let runtime_config = crate::llm::engine::EngineRuntimeConfig {
            model: "qwen3:8b".to_string(),
            temperature: None,
            context_length: crate::llm::engine::DEFAULT_CONTEXT_LENGTH,
            reasoning_effort: Some("medium".to_string()),
            input_modalities: None,
        };

        OllamaProvider::customize_request_body(&mut request_body, &runtime_config);

        assert_eq!(
            request_body,
            serde_json::json!({"reasoning_effort": "medium"})
        );
    }
}
