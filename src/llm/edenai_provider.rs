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
use crate::llm::{ModelInfo, Provider, ReasoningEffortPolicy, RequestCost, ToolSchema};

use super::openai_compatible_provider::{model_list_entries, ModelCatalog};
use super::{parse_input_modality_names, OpenAiCompatibleProvider};

// Eden AI has no application-attribution feature; the tag only labels the
// user's own usage in the Eden AI dashboard (application name and version).
// The header name is lowercase because `HeaderName::from_static` requires it.
const USAGE_TAGS_HEADER: &str = "x-edenai-tags";
const USAGE_TAGS: &str = concat!("app=ylva,version=", env!("CARGO_PKG_VERSION"));

/// Project readable Eden AI reasoning summaries before the shared stream event
/// is sent to the existing thinking UI path. Encrypted content is never read.
pub(crate) fn parse_edenai_sse_payload(data: &str) -> Vec<StreamEvent> {
    let standard_events = super::openai_compatible_provider::parse_sse_payload(data);
    if standard_events.is_some() {
        return standard_events.into_iter().collect();
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return Vec::new();
    };
    value
        .pointer("/choices/0/delta/reasoning_items")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|item| {
            item.get("summary")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|summary| summary.get("text").and_then(serde_json::Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .map(|text| StreamEvent::Thinking(text.to_owned()))
        .collect()
}

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
/// Capability evidence and advertised alternatives remain separate.
#[derive(Debug, Default)]
struct EdenAiModelCatalog {
    models: std::sync::RwLock<Vec<ModelInfo>>,
}

impl ModelCatalog for EdenAiModelCatalog {
    fn parse(&self, body: &serde_json::Value) -> Vec<ModelInfo> {
        let models: Vec<_> = model_list_entries(body)
            .filter(|entry| is_usable_chat_model(entry))
            .filter_map(|entry| {
                let (reasoning_evidence, reasoning_efforts, reasoning_default) =
                    parse_reasoning_evidence(entry);
                Some(ModelInfo {
                    reasoning_evidence,
                    id: entry.get("id")?.as_str()?.to_string(),
                    context_length: entry
                        .get("context_length")
                        .and_then(serde_json::Value::as_u64)
                        .map(|length| length as usize),
                    reasoning_efforts,
                    reasoning_default,
                    input_modalities: entry
                        .pointer("/capabilities/input_modalities")
                        .and_then(parse_input_modality_names),
                })
            })
            .collect();
        *self
            .models
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = models.clone();
        models
    }

    fn cached_model_info(&self, model: &str) -> Option<ModelInfo> {
        self.models
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|entry| entry.id == model)
            .cloned()
    }

    fn validate_runtime(&self, runtime: &EngineRuntimeConfig) -> anyhow::Result<()> {
        let cached = self.cached_model_info(&runtime.model);
        let model = cached.as_ref().or_else(|| {
            runtime
                .model_info
                .as_ref()
                .filter(|model| model.id == runtime.model)
        });
        EdenAiReasoningEffortPolicy.resolve(
            &runtime.model,
            model,
            runtime.reasoning_effort.as_deref(),
        )?;
        Ok(())
    }

    fn contextualize_error(
        &self,
        runtime: &EngineRuntimeConfig,
        error: anyhow::Error,
    ) -> anyhow::Error {
        contextualize_error(runtime, error)
    }
}

/// EdenAI v3 OpenAPI `POST /v3/chat/completions` reasoning_effort enum.
/// Source: https://api.edenai.run/v3/docs/openapi.json, researched 2026-10-31.
/// This is a gateway contract, not proof of upstream model compatibility.
pub const GATEWAY_REASONING_EFFORTS: &[&str] = &[
    "minimal", "low", "medium", "high", "max", "xhigh", "disable", "none",
];

/// Schema-derived reasoning rules owned by this provider.
#[derive(Debug)]
pub struct EdenAiReasoningEffortPolicy;

impl crate::llm::ReasoningEffortPolicy for EdenAiReasoningEffortPolicy {
    fn resolve(
        &self,
        model_id: &str,
        model: Option<&ModelInfo>,
        effort: Option<&str>,
    ) -> anyhow::Result<crate::llm::ReasoningEffortProvenance> {
        match effort {
            Some(effort) => resolve_reasoning_effort(model_id, model, effort),
            None => Ok(crate::llm::ReasoningEffortProvenance::ProviderDefault),
        }
    }
    fn validate_named_change(
        &self,
        model: &ModelInfo,
        current: Option<&str>,
        change: &crate::llm::ReasoningEffortChange,
    ) -> anyhow::Result<()> {
        let effort = match change {
            crate::llm::ReasoningEffortChange::Unchanged => current,
            crate::llm::ReasoningEffortChange::Set(effort) => effort.as_deref(),
        };
        self.resolve(&model.id, Some(model), effort).map(|_| ())
    }
    fn validate_request(
        &self,
        model_id: &str,
        model: Option<&ModelInfo>,
        effort: Option<&str>,
    ) -> anyhow::Result<()> {
        self.resolve(model_id, model, effort).map(|_| ())
    }
    fn requires_catalog_evidence(&self) -> bool {
        true
    }
    fn shows_capability_evidence(&self) -> bool {
        true
    }
    fn retain_profile_effort(&self, _model: &ModelInfo, _effort: &str) -> bool {
        true
    }
    fn unverified_choices(&self, model: Option<&ModelInfo>) -> &'static [&'static str] {
        if model.is_none_or(|model| {
            matches!(
                model.reasoning_evidence,
                crate::llm::ReasoningEvidence::Unknown | crate::llm::ReasoningEvidence::Supported
            ) && model.reasoning_efforts.as_ref().is_none_or(Vec::is_empty)
        }) {
            GATEWAY_REASONING_EFFORTS
        } else {
            &[]
        }
    }
    fn unverified_notice(&self, model: &str, effort: &str) -> Option<String> {
        Some(format!("EdenAI model '{model}' reasoning effort '{effort}' is unverified for this upstream model."))
    }
}

pub(crate) fn resolve_reasoning_effort(
    model_id: &str,
    model: Option<&ModelInfo>,
    effort: &str,
) -> anyhow::Result<crate::llm::ReasoningEffortProvenance> {
    use crate::llm::{ReasoningEffortProvenance, ReasoningEvidence};
    anyhow::ensure!(
        GATEWAY_REASONING_EFFORTS.contains(&effort),
        "EdenAI model '{model_id}' rejects reasoning effort '{effort}': invalid gateway value"
    );
    if let Some(model) = model {
        anyhow::ensure!(
            !matches!(
                model.reasoning_evidence,
                ReasoningEvidence::Unsupported
                    | ReasoningEvidence::Invalid
                    | ReasoningEvidence::Conflicting
            ),
            "EdenAI model '{model_id}' rejects reasoning effort '{effort}': {:?} catalog evidence",
            model.reasoning_evidence
        );
        if model.advertises_reasoning_effort(effort) {
            return Ok(ReasoningEffortProvenance::Advertised);
        }
        anyhow::ensure!(
            model.reasoning_efforts.as_ref().is_none_or(Vec::is_empty),
            "EdenAI model '{model_id}' does not advertise reasoning effort '{effort}'"
        );
    }
    Ok(ReasoningEffortProvenance::Unverified)
}

fn contextualize_error(runtime: &EngineRuntimeConfig, error: anyhow::Error) -> anyhow::Error {
    let details = format!("{error:#}");
    error.context(format!(
        "EdenAI model '{}' reasoning effort '{}': {details}",
        runtime.model,
        runtime.reasoning_effort.as_deref().unwrap_or("default")
    ))
}

fn parse_reasoning_evidence(
    entry: &serde_json::Value,
) -> (
    crate::llm::ReasoningEvidence,
    Option<Vec<String>>,
    Option<String>,
) {
    use crate::llm::ReasoningEvidence;
    let support = entry.pointer("/capabilities/supports_reasoning");
    let reasoning = entry.pointer("/capabilities/reasoning");
    let malformed_support = support.is_some_and(|value| !value.is_null() && !value.is_boolean());
    let malformed_object = reasoning.is_some_and(|value| !value.is_null() && !value.is_object());
    let enumeration = reasoning.and_then(|value| value.get("supported_efforts"));
    let malformed_list = enumeration.is_some_and(|value| {
        !value.is_null()
            && (!value.is_array()
                || value.as_array().is_some_and(|values| {
                    values.iter().any(|value| {
                        value
                            .as_str()
                            .is_none_or(|effort| !GATEWAY_REASONING_EFFORTS.contains(&effort))
                    })
                }))
    });
    let default = reasoning.and_then(|value| value.get("default_effort"));
    let malformed_default = default.is_some_and(|value| !value.is_null() && !value.is_string());
    let mut efforts: Vec<String> = enumeration
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter(|effort| GATEWAY_REASONING_EFFORTS.contains(effort))
        .map(String::from)
        .collect();
    efforts = crate::llm::order_reasoning_efforts(efforts);
    efforts.dedup();
    let evidence = if malformed_support || malformed_object || malformed_list || malformed_default {
        ReasoningEvidence::Invalid
    } else if support.and_then(serde_json::Value::as_bool) == Some(false) {
        if !efforts.is_empty() {
            ReasoningEvidence::Conflicting
        } else {
            ReasoningEvidence::Unsupported
        }
    } else if support.and_then(serde_json::Value::as_bool) == Some(true) || !efforts.is_empty() {
        ReasoningEvidence::Supported
    } else {
        ReasoningEvidence::Unknown
    };
    let default = default
        .and_then(serde_json::Value::as_str)
        .filter(|default| efforts.iter().any(|effort| effort == default))
        .map(String::from);
    let enumerated =
        (!efforts.is_empty() || evidence == ReasoningEvidence::Supported).then_some(efforts);
    (evidence, enumerated, default)
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
/// derives that path before constructing the shared transport. Explicit efforts
/// use the gateway's top-level `reasoning_effort` field.
#[derive(Debug)]
pub struct EdenAiProvider {
    inner: OpenAiCompatibleProvider,
}

impl EdenAiProvider {
    /// Eden AI uses a body `session_id` as its first-priority sticky-routing
    /// key, keeping unprefixed model names on the provider holding the prompt
    /// cache. It is never forwarded to the provider.
    fn customize_request_body(
        request_body: &mut serde_json::Value,
        runtime_config: &EngineRuntimeConfig,
        session_id: Option<uuid::Uuid>,
    ) {
        if let Some(effort) = runtime_config.reasoning_effort.as_deref() {
            request_body["reasoning_effort"] = serde_json::json!(effort);
        }
        if let Some(session_id) = session_id {
            request_body["session_id"] = serde_json::json!(session_id.to_string());
        }
    }

    /// Create an Eden AI provider.
    ///
    /// `base_url` should normally be `https://api.edenai.run`.
    pub fn new(name: String, base_url: String, model: String, api_key: Option<String>) -> Self {
        let openai_base_url = format!("{base_url}/v3");
        Self {
            inner: OpenAiCompatibleProvider::new_with_request_body_customizer(
                name,
                openai_base_url,
                model,
                api_key,
                Self::customize_request_body,
                super::openai_compatible_provider::redact_chat_completion_request_body,
            )
            .with_provider_kind("edenai")
            .with_stream_payload_parser(parse_edenai_sse_payload)
            .with_usage_cost_parser(parse_reported_cost)
            .with_model_catalog(std::sync::Arc::new(EdenAiModelCatalog::default()))
            .with_extra_headers(vec![(USAGE_TAGS_HEADER, USAGE_TAGS)]),
        }
    }
}

impl Provider for EdenAiProvider {
    fn reasoning_effort_policy(&self) -> std::sync::Arc<dyn crate::llm::ReasoningEffortPolicy> {
        std::sync::Arc::new(EdenAiReasoningEffortPolicy)
    }

    fn bind_engine_runtime(
        &self,
        runtime: EngineRuntimeConfigHandle,
    ) -> Option<std::sync::Arc<dyn Provider>> {
        Some(std::sync::Arc::new(Self {
            inner: self.inner.clone_with_runtime_config(runtime),
        }))
    }

    fn cached_model_info(&self, model: &str) -> Option<ModelInfo> {
        self.inner.cached_model_info(model)
    }

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
        self.inner.update_runtime_config(|runtime| {
            let model_info = self.cached_model_info(model);
            self.reasoning_effort_policy().resolve(
                model,
                model_info.as_ref(),
                runtime.reasoning_effort.as_deref(),
            )?;
            if runtime.model != model {
                runtime.model = model.to_string();
                runtime.model_info = model_info;
                runtime.input_modalities = None;
                runtime.context_length_override = None;
                runtime.context_length = crate::llm::DEFAULT_CONTEXT_LENGTH;
            }
            Ok(())
        })
    }

    fn set_reasoning_effort(&self, reasoning_effort: Option<&str>) -> anyhow::Result<()> {
        self.inner.update_runtime_config(|runtime| {
            let cached = self.cached_model_info(&runtime.model);
            let model = cached.as_ref().or_else(|| {
                runtime
                    .model_info
                    .as_ref()
                    .filter(|model| model.id == runtime.model)
            });
            self.reasoning_effort_policy()
                .resolve(&runtime.model, model, reasoning_effort)?;
            runtime.reasoning_effort = reasoning_effort.map(String::from);
            Ok(())
        })
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
    fn parses_readable_reasoning_item_summary_entries() {
        let events = parse_edenai_sse_payload(
            r#"{"choices":[{"delta":{"reasoning_items":[{"encrypted_content":"secret","summary":[{"text":"First."},{"text":" Second."}]}]}}]}"#,
        );
        assert!(
            matches!(events.as_slice(), [StreamEvent::Thinking(first), StreamEvent::Thinking(second)] if first == "First." && second == " Second.")
        );
    }

    #[test]
    fn skips_empty_and_encrypted_only_reasoning_items() {
        for payload in [
            r#"{"choices":[{"delta":{"reasoning_items":[{"summary":[]}]}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_items":[{"summary":[{"text":""}]}]}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_items":[{"encrypted_content":"secret"}]}}]}"#,
            r#"{"choices":[{"delta":{"other_field":"unrelated"}}]}"#,
        ] {
            assert!(parse_edenai_sse_payload(payload).is_empty());
        }
    }

    #[test]
    fn standard_openai_compatible_events_keep_existing_precedence() {
        let events = parse_edenai_sse_payload(
            r#"{"choices":[{"delta":{"content":"answer","reasoning_items":[{"summary":[{"text":"summary"}]}]}}]}"#,
        );
        assert!(matches!(events.as_slice(), [StreamEvent::Text(text)] if text == "answer"));
    }

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
    fn body_carries_session_id_only_when_known() {
        let runtime_config = EngineRuntimeConfig {
            model_info: None,
            context_length_override: None,
            model: "some/model".to_string(),
            temperature: None,
            context_length: crate::llm::engine::DEFAULT_CONTEXT_LENGTH,
            reasoning_effort: None,
            input_modalities: None,
        };
        let session_id = uuid::Uuid::new_v4();
        let mut with_session = serde_json::json!({});
        EdenAiProvider::customize_request_body(
            &mut with_session,
            &runtime_config,
            Some(session_id),
        );
        assert_eq!(with_session["session_id"], session_id.to_string());
        let mut without_session = serde_json::json!({});
        EdenAiProvider::customize_request_body(&mut without_session, &runtime_config, None);
        assert!(without_session.get("session_id").is_none());
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
        let models = EdenAiModelCatalog::default().parse(&serde_json::json!({
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
    fn catalog_preserves_reasoning_enumeration_and_default_membership() {
        let models = EdenAiModelCatalog::default().parse(&serde_json::json!({"data": [
            {"id":"supported", "capabilities":{"supports_reasoning":true,
                "reasoning":{"supported_efforts":["high","low","low"],"default_effort":"low"}}},
            {"id":"incomplete", "capabilities":{"supports_reasoning":true}},
            {"id":"false", "capabilities":{"supports_reasoning":false}},
            {"id":"missing"},
            {"id":"outside-default", "capabilities":{"reasoning":{
                "supported_efforts":["medium"],"default_effort":"high"}}}
        ]}));
        assert_eq!(
            models[0].reasoning_efforts,
            Some(vec!["low".into(), "high".into()])
        );
        assert_eq!(models[0].reasoning_default.as_deref(), Some("low"));
        assert_eq!(models[1].reasoning_efforts, Some(vec![]));
        assert_eq!(models[2].reasoning_efforts, None);
        assert_eq!(models[3].reasoning_efforts, None);
        assert_eq!(models[4].reasoning_default, None);
    }

    #[test]
    fn explicit_medium_is_serialized_alongside_session_id() {
        let provider = EdenAiProvider::new(
            "eden".into(),
            "http://localhost".into(),
            "model".into(),
            None,
        );
        let mut runtime = provider
            .runtime_config_handle()
            .unwrap()
            .read()
            .unwrap()
            .clone();
        runtime.reasoning_effort = Some("medium".into());
        let mut body = serde_json::json!({});
        let session_id = uuid::Uuid::new_v4();
        EdenAiProvider::customize_request_body(&mut body, &runtime, Some(session_id));
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["session_id"], session_id.to_string());
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

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::llm::{ReasoningEffortProvenance, ReasoningEvidence};

    fn catalog_model(capabilities: serde_json::Value) -> ModelInfo {
        EdenAiModelCatalog::default()
            .parse(&serde_json::json!({"data":[{"id":"model","capabilities":capabilities}]}))
            .remove(0)
    }

    #[test]
    fn gateway_schema_contract_keeps_disable_and_none_distinct_and_rejects_aliases() {
        assert_eq!(
            GATEWAY_REASONING_EFFORTS,
            ["minimal", "low", "medium", "high", "max", "xhigh", "disable", "none"]
        );
        for effort in GATEWAY_REASONING_EFFORTS {
            assert_eq!(
                EdenAiReasoningEffortPolicy
                    .resolve("model", None, Some(effort))
                    .unwrap(),
                ReasoningEffortProvenance::Unverified
            );
        }
        for effort in [
            "default",
            "med",
            "min",
            "extrahigh",
            "",
            "HIGH",
            " high",
            "1000",
        ] {
            let error = EdenAiReasoningEffortPolicy
                .resolve("model", None, Some(effort))
                .unwrap_err()
                .to_string();
            assert!(error.contains("model") && error.contains(effort));
        }
    }

    #[test]
    fn missing_null_and_empty_metadata_are_incomplete_not_invalid() {
        for capabilities in [
            serde_json::json!({}),
            serde_json::json!({"supports_reasoning":null,"reasoning":null}),
            serde_json::json!({"supports_reasoning":true}),
            serde_json::json!({"supports_reasoning":true,"reasoning":{}}),
            serde_json::json!({"supports_reasoning":true,"reasoning":{"supported_efforts":null}}),
            serde_json::json!({"supports_reasoning":true,"reasoning":{"supported_efforts":[]}}),
        ] {
            let model = catalog_model(capabilities);
            assert_eq!(
                EdenAiReasoningEffortPolicy
                    .resolve("model", Some(&model), Some("medium"))
                    .unwrap(),
                ReasoningEffortProvenance::Unverified
            );
            assert_eq!(
                EdenAiReasoningEffortPolicy.unverified_choices(Some(&model)),
                GATEWAY_REASONING_EFFORTS
            );
        }
    }

    #[test]
    fn unsupported_malformed_and_conflicting_evidence_reject_explicit_effort_but_allow_default() {
        for (capabilities, evidence) in [
            (
                serde_json::json!({"supports_reasoning":false}),
                ReasoningEvidence::Unsupported,
            ),
            (
                serde_json::json!({"supports_reasoning":"true"}),
                ReasoningEvidence::Invalid,
            ),
            (
                serde_json::json!({"reasoning":[]}),
                ReasoningEvidence::Invalid,
            ),
            (
                serde_json::json!({"reasoning":{"supported_efforts":"high"}}),
                ReasoningEvidence::Invalid,
            ),
            (
                serde_json::json!({"reasoning":{"supported_efforts":["high",1]}}),
                ReasoningEvidence::Invalid,
            ),
            (
                serde_json::json!({"reasoning":{"supported_efforts":["med"]}}),
                ReasoningEvidence::Invalid,
            ),
            (
                serde_json::json!({"reasoning":{"default_effort":42}}),
                ReasoningEvidence::Invalid,
            ),
            (
                serde_json::json!({"supports_reasoning":false,"reasoning":{"supported_efforts":["high"]}}),
                ReasoningEvidence::Conflicting,
            ),
        ] {
            let model = catalog_model(capabilities);
            assert_eq!(model.reasoning_evidence, evidence);
            assert!(EdenAiReasoningEffortPolicy
                .resolve("model", Some(&model), Some("high"))
                .is_err());
            assert_eq!(
                EdenAiReasoningEffortPolicy
                    .resolve("model", Some(&model), None)
                    .unwrap(),
                ReasoningEffortProvenance::ProviderDefault
            );
            assert!(EdenAiReasoningEffortPolicy
                .unverified_choices(Some(&model))
                .is_empty());
        }
    }

    #[test]
    fn advertised_lists_are_strict_and_do_not_force_mandatory_default() {
        let model = catalog_model(serde_json::json!({"supports_reasoning":true,"reasoning":{
            "supported_efforts":["high","low","low"],"default_effort":"max","mandatory":true}}));
        assert_eq!(model.reasoning_default, None);
        assert_eq!(
            model.reasoning_efforts,
            Some(vec!["low".into(), "high".into()])
        );
        assert_eq!(
            EdenAiReasoningEffortPolicy
                .resolve("model", Some(&model), Some("high"))
                .unwrap(),
            ReasoningEffortProvenance::Advertised
        );
        assert!(EdenAiReasoningEffortPolicy
            .resolve("model", Some(&model), Some("medium"))
            .is_err());
        assert!(crate::llm::AdvertisedReasoningEffortPolicy
            .resolve("model", None, Some("high"))
            .is_err());
    }

    #[tokio::test]
    async fn every_direct_entry_point_rejects_raw_invalid_handle_write_before_io() {
        let provider = EdenAiProvider::new(
            "not-edenai".into(),
            "http://127.0.0.1:1".into(),
            "model".into(),
            None,
        );
        provider
            .runtime_config_handle()
            .unwrap()
            .write()
            .unwrap()
            .reasoning_effort = Some("med".into());
        let no_estimate = || {
            Box::new(|_, _| Box::pin(async {}) as futures::future::BoxFuture<'static, ()>)
                as Box<dyn FnOnce(usize, usize) -> futures::future::BoxFuture<'static, ()> + Send>
        };
        for result in [
            provider
                .chat(vec![], vec![], CancellationToken::new())
                .await,
            provider
                .chat_with_session_id(vec![], vec![], CancellationToken::new(), None)
                .await,
            provider
                .chat_with_session_id_and_estimate(
                    vec![],
                    vec![],
                    CancellationToken::new(),
                    None,
                    no_estimate(),
                )
                .await,
            provider
                .chat_with_session_id_and_diagnostics(
                    vec![],
                    vec![],
                    CancellationToken::new(),
                    None,
                    no_estimate(),
                    None,
                )
                .await,
        ] {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("invalid gateway value"));
        }
    }

    #[test]
    fn rejected_direct_provider_switch_and_effort_leave_snapshot_untouched() {
        let catalog = std::sync::Arc::new(EdenAiModelCatalog::default());
        catalog.parse(&serde_json::json!({"data":[{"id":"unsupported","capabilities":{"supports_reasoning":false}}]}));
        let mut provider = EdenAiProvider::new(
            "eden".into(),
            "http://localhost".into(),
            "model".into(),
            None,
        );
        provider.inner = provider.inner.with_model_catalog(catalog);
        provider.set_reasoning_effort(Some("medium")).unwrap();
        let handle = provider.runtime_config_handle().unwrap();
        let before = handle.read().unwrap().clone();
        assert!(provider.set_model("unsupported").is_err());
        assert!(provider.set_reasoning_effort(Some("med")).is_err());
        assert_eq!(*handle.read().unwrap(), before);
    }

    #[test]
    fn backend_error_context_keeps_original_details_and_source() {
        let provider = EdenAiProvider::new(
            "eden".into(),
            "http://localhost".into(),
            "model".into(),
            None,
        );
        provider.set_reasoning_effort(Some("medium")).unwrap();
        let runtime = provider
            .runtime_config_handle()
            .unwrap()
            .read()
            .unwrap()
            .clone();
        let error = contextualize_error(
            &runtime,
            anyhow::anyhow!("HTTP 422: upstream unsupported reasoning_effort"),
        );
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("model")
                && rendered.contains("medium")
                && rendered.contains("HTTP 422")
        );
        assert_eq!(
            error.root_cause().to_string(),
            "HTTP 422: upstream unsupported reasoning_effort"
        );
    }
}
