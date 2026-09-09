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
use crate::llm::{ModelInfo, Provider, ToolSchema};

use super::http::{get_json, post_json};
use super::OpenAiCompatibleProvider;

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
            inner: OpenAiCompatibleProvider::new_with_request_body_customizer(
                name,
                openai_base_url,
                model,
                api_key,
                Self::customize_request_body,
            ),
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

        Box::pin(async move {
            let tags_body =
                get_json(&client, &tags_url, api_key.as_deref(), "model listing").await?;
            let models = parse_tags(&tags_body);
            let requests = models.into_iter().map(|mut model| {
                let client = client.clone();
                let api_key = api_key.clone();
                let show_url = show_url.clone();
                async move {
                    if model.context_length.is_some() && model.reasoning_efforts.is_some() {
                        return model;
                    }
                    let request_body = serde_json::json!({ "model": model.id });
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
                            if model.reasoning_efforts.is_none() {
                                model.reasoning_efforts = parse_reasoning_efforts(&body);
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
    let capabilities = body.get("capabilities")?.as_array()?;
    capabilities
        .iter()
        .filter_map(serde_json::Value::as_str)
        .any(|capability| capability == "thinking")
        .then(Vec::new)
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

        assert_eq!(parse_reasoning_efforts(&thinking_body), Some(Vec::new()));
        assert_eq!(parse_reasoning_efforts(&regular_body), None);
    }

    #[test]
    fn customizes_openai_compatible_reasoning_request_body() {
        let mut request_body = serde_json::json!({});
        let runtime_config = crate::llm::engine::EngineRuntimeConfig {
            model: "qwen3:8b".to_string(),
            temperature: None,
            context_length: crate::llm::engine::DEFAULT_CONTEXT_LENGTH,
            reasoning_effort: Some("medium".to_string()),
        };

        OllamaProvider::customize_request_body(&mut request_body, &runtime_config);

        assert_eq!(
            request_body,
            serde_json::json!({"reasoning_effort": "medium"})
        );
    }
}
