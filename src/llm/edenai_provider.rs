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
use crate::llm::engine::EngineRuntimeConfigHandle;
use crate::llm::{ModelInfo, Provider, RequestCost, ToolSchema};

use super::openai_compatible_provider::{model_list_entries, ModelCatalog};
use super::{parse_input_modality_names, OpenAiCompatibleProvider};

// Eden AI has no application-attribution feature; the tag only labels the
// user's own usage in the Eden AI dashboard (application name and version).
// The header name is lowercase because `HeaderName::from_static` requires it.
const USAGE_TAGS_HEADER: &str = "x-edenai-tags";
const USAGE_TAGS: &str = concat!("app=ylva,version=", env!("CARGO_PKG_VERSION"));

/// Parse the request cost Eden AI reports as a top-level `cost` (USD) in a
/// response or final stream frame; `usage.cost` is accepted defensively.
fn parse_reported_cost(data: &str) -> Option<RequestCost> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let amount = ["/cost", "/usage/cost"]
        .iter()
        .find_map(|pointer| value.pointer(pointer))
        .and_then(serde_json::Value::as_f64)
        .filter(|amount| amount.is_finite() && *amount >= 0.0)?;
    Some(RequestCost::new(amount, "USD"))
}

/// Reads Eden AI's `/v3/models` shape. Models that cannot produce text or
/// that explicitly lack function calling are dropped; alias entries are kept.
/// Reasoning efforts are never reported: Eden AI documents no request shape
/// for them, so offering a choice would have no effect.
#[derive(Debug, Default)]
struct EdenAiModelCatalog;

impl ModelCatalog for EdenAiModelCatalog {
    fn parse(&self, body: &serde_json::Value) -> Vec<ModelInfo> {
        model_list_entries(body)
            .filter(|entry| is_usable_chat_model(entry))
            .filter_map(|entry| {
                Some(ModelInfo {
                    id: entry.get("id")?.as_str()?.to_string(),
                    context_length: entry
                        .get("context_length")
                        .and_then(serde_json::Value::as_u64)
                        .map(|length| length as usize),
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: entry
                        .pointer("/capabilities/input_modalities")
                        .and_then(parse_input_modality_names),
                })
            })
            .collect()
    }
}

fn is_usable_chat_model(entry: &serde_json::Value) -> bool {
    let outputs_text = entry
        .pointer("/capabilities/output_modalities")
        .and_then(serde_json::Value::as_array)
        .is_none_or(|modalities| {
            modalities
                .iter()
                .any(|modality| modality.as_str() == Some("text"))
        });
    let supports_function_calling = entry
        .pointer("/capabilities/supports_function_calling")
        .and_then(serde_json::Value::as_bool)
        != Some(false);
    outputs_text && supports_function_calling
}

/// Eden AI provider.
///
/// Eden AI serves chat completions and its model catalog through an
/// OpenAI-compatible `/v3` path. The provider accepts the Eden AI origin and
/// derives that path before constructing the shared transport. Reasoning
/// parameters are not sent: Eden AI documents no provider-neutral shape.
#[derive(Debug)]
pub struct EdenAiProvider {
    inner: OpenAiCompatibleProvider,
}

impl EdenAiProvider {
    /// Create an Eden AI provider.
    ///
    /// `base_url` should normally be `https://api.edenai.run`.
    pub fn new(name: String, base_url: String, model: String, api_key: Option<String>) -> Self {
        let openai_base_url = format!("{base_url}/v3");
        Self {
            inner: OpenAiCompatibleProvider::new(name, openai_base_url, model, api_key)
                .with_provider_kind("edenai")
                .with_usage_cost_parser(parse_reported_cost)
                .with_model_catalog(std::sync::Arc::new(EdenAiModelCatalog))
                .with_extra_headers(vec![(USAGE_TAGS_HEADER, USAGE_TAGS)]),
        }
    }
}

impl Provider for EdenAiProvider {
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
    use crate::llm::InputModality;

    #[test]
    fn usage_tags_satisfy_eden_ai_tag_rules() {
        let tags: Vec<_> = USAGE_TAGS.split(',').collect();
        assert!(tags.len() <= 10);
        assert_eq!(tags[0], "app=ylva");
        for tag in tags {
            let (key, value) = tag.split_once('=').expect("key=value tag");
            assert!(!key.is_empty() && key.len() <= 64);
            assert!(key
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_.:/".contains(character)));
            assert!(!value.is_empty() && value.len() <= 128);
            assert!(!value.contains(['=', ',']));
        }
        assert_eq!(USAGE_TAGS_HEADER, USAGE_TAGS_HEADER.to_lowercase());
        assert!(USAGE_TAGS.ends_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn parses_reported_cost_from_top_level_or_usage() {
        assert_eq!(
            parse_reported_cost(r#"{"usage":{"prompt_tokens":1},"cost":0.0042}"#),
            Some(RequestCost::new(0.0042, "USD"))
        );
        assert_eq!(
            parse_reported_cost(r#"{"usage":{"cost":0.5}}"#),
            Some(RequestCost::new(0.5, "USD"))
        );
        assert_eq!(
            parse_reported_cost(r#"{"cost":0}"#),
            Some(RequestCost::new(0.0, "USD"))
        );
        assert_eq!(parse_reported_cost(r#"{"cost":-1}"#), None);
        assert_eq!(parse_reported_cost(r#"{"cost":"1.5"}"#), None);
        assert_eq!(parse_reported_cost(r#"{"usage":{}}"#), None);
        assert_eq!(parse_reported_cost("not json"), None);
    }

    #[test]
    fn catalog_reads_context_modalities_and_filters_unusable_models() {
        let models = EdenAiModelCatalog.parse(&serde_json::json!({
            "data": [
                {"id": "openai/gpt-latest", "alias_of": "openai/gpt-9", "context_length": 400000,
                 "capabilities": {"input_modalities": ["text", "image", "file", "audio"],
                                  "output_modalities": ["text"],
                                  "supports_function_calling": true}},
                {"id": "image-only-output",
                 "capabilities": {"output_modalities": ["image"], "supports_function_calling": true}},
                {"id": "image-and-text-output",
                 "capabilities": {"output_modalities": ["image", "text"]}},
                {"id": "no-tools",
                 "capabilities": {"output_modalities": ["text"], "supports_function_calling": false}},
                {"id": "text-only", "capabilities": {"input_modalities": ["text"]}},
                {"id": "bare"},
                {"name": "no-id"}
            ]
        }));

        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "openai/gpt-latest",
                "image-and-text-output",
                "text-only",
                "bare"
            ]
        );
        assert_eq!(models[0].context_length, Some(400000));
        assert_eq!(
            models[0].input_modalities,
            Some(vec![InputModality::Image, InputModality::File])
        );
        assert_eq!(models[2].input_modalities, Some(vec![]));
        assert_eq!(models[3].input_modalities, None);
    }

    #[test]
    fn catalog_never_reports_reasoning_efforts() {
        let models = EdenAiModelCatalog.parse(&serde_json::json!({
            "data": [
                {"id": "with-efforts", "capabilities": {"reasoning": {
                    "supported_efforts": ["high", "low"], "default_effort": "low"}}},
                {"id": "empty-efforts", "capabilities": {"supports_reasoning": true,
                    "reasoning": {"supported_efforts": [], "default_effort": null}}}
            ]
        }));

        for model in &models {
            assert_eq!(model.reasoning_efforts, None, "{}", model.id);
            assert_eq!(model.reasoning_default, None, "{}", model.id);
        }
    }

    #[test]
    fn provider_derives_the_v3_path_and_reports_its_name() {
        let provider = EdenAiProvider::new(
            "eden".to_string(),
            "https://api.edenai.run".to_string(),
            "openai/gpt-latest".to_string(),
            None,
        );
        assert_eq!(provider.name(), "eden");
        assert!(provider.runtime_config_handle().is_some());
    }
}
