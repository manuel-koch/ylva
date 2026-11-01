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

use schemars::JsonSchema;
use serde::Serialize;

pub mod copilot_provider;
pub mod edenai_provider;
pub mod engine;
pub mod mock_provider;
pub mod ollama_provider;
pub mod openai_compatible_provider;
pub(crate) mod openai_responses;
pub mod openrouter_provider;
pub mod provider_registry;
pub mod token_estimate;
pub(crate) mod token_usage;
pub mod usage_cost;

/// Maximum number of simultaneous HTTP requests per provider connection.
pub const MAX_CONCURRENT_PROVIDER_REQUESTS: usize = 4;

/// Return the shared HTTP request limiter for a provider connection.
pub(crate) fn provider_request_limiter(
    provider_connection: &str,
) -> crate::http::RequestConcurrencyLimiter {
    static LIMITERS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, crate::http::RequestConcurrencyLimiter>>,
    > = std::sync::OnceLock::new();
    let limiters = LIMITERS.get_or_init(Default::default);
    let mut limiters = limiters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    limiters
        .entry(provider_connection.to_string())
        .or_insert_with(|| {
            crate::http::RequestConcurrencyLimiter::new(MAX_CONCURRENT_PROVIDER_REQUESTS)
        })
        .clone()
}

pub use copilot_provider::CopilotProvider;
pub use edenai_provider::EdenAiProvider;
pub use engine::{
    CompactionEngineDisplayInfo, Engine, EngineConfig, EngineDisplayInfo, EngineImpl, EngineRouter,
    EngineRuntimeConfig, EngineRuntimeConfigHandle, ModelSelectionTarget, ProviderModelOption,
    ReasoningEffortChange, DEFAULT_CONTEXT_LENGTH,
};
pub use mock_provider::{MockEvent, MockProvider};
pub use ollama_provider::OllamaProvider;
pub use openai_compatible_provider::OpenAiCompatibleProvider;
pub use openrouter_provider::OpenRouterProvider;
pub use provider_registry::{ProviderEnv, ProviderRegistry, ProviderSelection};
pub use token_estimate::TokenEstimator;
pub use usage_cost::{CostLedger, RequestCost};
pub use MAX_CONCURRENT_PROVIDER_REQUESTS as DEFAULT_MAX_CONCURRENT_PROVIDER_REQUESTS;

/// A single tool definition in the OpenAI chat-completions wire shape.
///
/// This is the `{"type":"function","function":{name, description, parameters}}`
/// envelope. `parameters` is a serde-serialized [`schemars::schema::RootSchema`]
/// produced by deriving `JsonSchema` on a typed arguments struct — never a
/// hand-written JSON schema.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolSchema {
    /// Render the OpenAI function-tool envelope for the request body.
    pub fn to_request_value(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// Catalog evidence for reasoning support. Enumeration remains in `ModelInfo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReasoningEvidence {
    /// No usable capability evidence establishes whether reasoning is supported.
    /// This includes missing or null metadata and providers that do not populate
    /// this evidence separately from their advertised effort lists.
    #[default]
    Unknown,
    /// The catalog explicitly reports that the model does not support reasoning,
    /// without contradictory evidence advertising reasoning support.
    Unsupported,
    /// The catalog reports reasoning support or advertises valid reasoning efforts.
    /// Support alone does not establish which explicit efforts the model accepts;
    /// the advertised alternatives remain in [`ModelInfo::reasoning_efforts`].
    Supported,
    /// Reasoning metadata cannot be interpreted reliably because its structure,
    /// field types, or effort entries violate the provider's catalog contract.
    /// Missing optional metadata alone does not make evidence invalid.
    Invalid,
    /// Individually interpretable catalog claims disagree about reasoning support,
    /// such as an explicit denial of support alongside advertised effort values.
    Conflicting,
}

/// Provenance of an effective, explicit reasoning effort.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReasoningEffortProvenance {
    /// No explicit effort is selected; the request leaves effort control to
    /// the provider. This does not identify the provider's actual default value.
    #[default]
    ProviderDefault,
    /// The selected explicit effort is an exact member of the model's
    /// advertised effort list. Advertisement does not prove that the upstream
    /// model honors the value or produces readable thinking.
    Advertised,
    /// The provider's validation contract permits the selected explicit effort,
    /// but available catalog evidence does not confirm model-specific acceptance.
    /// This is an opted-in override, not an invented advertised alternative.
    Unverified,
}

/// Provider-neutral reasoning rules. Implementations own wire validation and evidence handling.
pub trait ReasoningEffortPolicy: Send + Sync + std::fmt::Debug {
    /// Validate a complete candidate without mutating its runtime.
    fn resolve(
        &self,
        model_id: &str,
        model: Option<&ModelInfo>,
        effort: Option<&str>,
    ) -> anyhow::Result<ReasoningEffortProvenance> {
        let Some(effort) = effort else {
            return Ok(ReasoningEffortProvenance::ProviderDefault);
        };
        anyhow::ensure!(
            model.is_some_and(|model| model.advertises_reasoning_effort(effort)),
            "model '{model_id}' does not advertise reasoning effort '{effort}'"
        );
        Ok(ReasoningEffortProvenance::Advertised)
    }

    /// Validate explicit named-picker edits. Untouched strict shortcuts retain legacy behavior.
    fn validate_named_change(
        &self,
        model: &ModelInfo,
        _current: Option<&str>,
        change: &ReasoningEffortChange,
    ) -> anyhow::Result<()> {
        match change {
            ReasoningEffortChange::Unchanged => Ok(()),
            ReasoningEffortChange::Set(Some(effort)) => self
                .resolve(&model.id, Some(model), Some(effort))
                .map(|_| ()),
            ReasoningEffortChange::Set(None) => {
                anyhow::ensure!(
                    model
                        .reasoning_efforts
                        .as_ref()
                        .is_some_and(|efforts| !efforts.is_empty()),
                    "model '{}' does not advertise selectable reasoning efforts",
                    model.id
                );
                Ok(())
            }
        }
    }

    /// Request-time validation is optional for legacy providers with lazy profile fallback.
    fn validate_request(
        &self,
        _model_id: &str,
        _model: Option<&ModelInfo>,
        _effort: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Whether reasoning evidence requires hydration even when modalities are known.
    fn requires_catalog_evidence(&self) -> bool {
        false
    }

    /// Whether catalog capability evidence should be shown alongside effort choices.
    fn shows_capability_evidence(&self) -> bool {
        false
    }

    /// Whether an explicit profile effort survives successful catalog hydration.
    fn retain_profile_effort(&self, model: &ModelInfo, effort: &str) -> bool {
        model.advertises_reasoning_effort(effort)
    }

    /// Deliberately labelled choices, separate from advertised alternatives.
    fn unverified_choices(&self, _model: Option<&ModelInfo>) -> &'static [&'static str] {
        &[]
    }

    /// Provider-specific wording, with timing and deduplication owned by the engine.
    fn unverified_notice(&self, _model: &str, _effort: &str) -> Option<String> {
        None
    }

    /// Immutable display contract for a model-picker row, not provider identity.
    fn picker_options(&self, model: &ModelInfo) -> ReasoningEffortOptions {
        ReasoningEffortOptions {
            selectable_efforts: Some(
                model
                    .reasoning_efforts
                    .iter()
                    .flatten()
                    .filter(|effort| self.resolve(&model.id, Some(model), Some(effort)).is_ok())
                    .cloned()
                    .collect(),
            ),
            unverified_efforts: self
                .unverified_choices(Some(model))
                .iter()
                .map(|effort| (*effort).to_string())
                .collect(),
            show_capability: self.shows_capability_evidence(),
        }
    }
}

/// Default exact-catalog validation for providers without specialized reasoning rules.
#[derive(Debug)]
pub struct AdvertisedReasoningEffortPolicy;
impl ReasoningEffortPolicy for AdvertisedReasoningEffortPolicy {}

/// Display-only reasoning choices resolved by the target's policy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReasoningEffortOptions {
    /// None lets compatibility display callers use the model's advertised list.
    pub selectable_efforts: Option<Vec<String>>,
    pub unverified_efforts: Vec<String>,
    pub show_capability: bool,
}

/// Metadata describing a single model available from a provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    /// Capability evidence, neutral for providers using legacy strict enumeration.
    pub reasoning_evidence: ReasoningEvidence,
    pub id: String,
    pub context_length: Option<usize>,
    /// Reasoning support advertised by the provider.
    ///
    /// - `None` means that reasoning support is not advertised.
    /// - `Some([])` means that reasoning is supported but the
    ///   provider exposes no selectable effort alternatives.
    /// - `Some(values)` means that reasoning is supported and
    ///   `values` contains the selectable effort values, weakest to strongest
    ///   (see [`order_reasoning_efforts`]).
    pub reasoning_efforts: Option<Vec<String>>,
    /// Provider-advertised default reasoning effort, when the provider
    /// exposes one; `None` otherwise.
    ///
    /// Rendered only in the `/models` picker, marking the matching advertised
    /// effort or labelling the leading default slot; it never changes the
    /// [`ModelInfo::reasoning_efforts`] three-state semantics.
    pub reasoning_default: Option<String>,
    /// Provider-advertised input modalities.
    ///
    /// `None` means that the provider did not advertise modality data.
    /// `Some([])` is a known text-only model. Supported values are `Image`
    /// and `File`; unknown provider modality names are ignored.
    pub input_modalities: Option<Vec<InputModality>>,
}

impl ModelInfo {
    /// Whether `reasoning_effort` is one of the advertised selectable
    /// efforts — the single membership rule shared by the `/models` switch
    /// validation and the pre-first-chat profile validation.
    ///
    /// This is exact catalog membership, not acceptance of unverified gateway
    /// overrides. `None` and `Some([])` both advertise no alternatives.
    pub fn advertises_reasoning_effort(&self, reasoning_effort: &str) -> bool {
        self.reasoning_efforts
            .as_ref()
            .is_some_and(|efforts| efforts.iter().any(|effort| effort == reasoning_effort))
    }
}

/// Canonical reasoning-effort ladder, weakest to strongest. Names in one group
/// are aliases that share a rank.
const REASONING_EFFORT_LADDER: &[&[&str]] = &[
    &["none"],
    &["min", "minimal"],
    &["low"],
    &["medium", "med"],
    &["high"],
    &["xhigh", "extrahigh"],
    &["max"],
];

/// Sort provider-advertised reasoning efforts weakest to strongest.
///
/// Providers return efforts in arbitrary order. Names missing from
/// [`REASONING_EFFORT_LADDER`] sort after all known names and keep their
/// server order relative to each other.
pub(crate) fn order_reasoning_efforts(mut reasoning_efforts: Vec<String>) -> Vec<String> {
    reasoning_efforts.sort_by_key(|effort| reasoning_effort_rank(effort));
    reasoning_efforts
}

fn reasoning_effort_rank(effort: &str) -> usize {
    REASONING_EFFORT_LADDER
        .iter()
        .position(|aliases| aliases.contains(&effort))
        .unwrap_or(REASONING_EFFORT_LADDER.len())
}

/// Provider-neutral input modality advertised for a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputModality {
    Image,
    File,
}

/// Map a JSON array of provider modality names to [`InputModality`] values.
/// Names other than `image` and `file` are ignored; `None` when `values` is
/// not an array.
pub(crate) fn parse_input_modality_names(values: &serde_json::Value) -> Option<Vec<InputModality>> {
    Some(
        values
            .as_array()?
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter_map(|modality| match modality {
                "image" => Some(InputModality::Image),
                "file" => Some(InputModality::File),
                _ => None,
            })
            .collect(),
    )
}

/// Read the selectable efforts and the advertised default from a
/// `{supported_efforts, default_effort}` reasoning object, the shape shared by
/// OpenRouter and EdenAI. `None` for the efforts means no reasoning object;
/// an object without usable efforts yields `Some([])`. A default is kept only
/// as a non-empty string.
pub(crate) fn parse_reasoning_object(
    reasoning: Option<&serde_json::Value>,
) -> (Option<Vec<String>>, Option<String>) {
    let Some(reasoning) = reasoning.filter(|reasoning| reasoning.is_object()) else {
        return (None, None);
    };
    let efforts = reasoning
        .get("supported_efforts")
        .and_then(serde_json::Value::as_array)
        .map(|efforts| {
            efforts
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|effort| !effort.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let default_effort = reasoning
        .get("default_effort")
        .and_then(serde_json::Value::as_str)
        .filter(|effort| !effort.is_empty())
        .map(String::from);
    (Some(order_reasoning_efforts(efforts)), default_effort)
}

/// Render the active reasoning effort for a `tracing` field.
///
/// `None` means no explicit effort is set and the provider's own default
/// behavior applies — shown as `default` to match the `/models` picker's
/// user-facing wording.
pub(crate) fn format_reasoning_effort_for_log(reasoning_effort: Option<&str>) -> String {
    reasoning_effort.unwrap_or("default").to_string()
}

/// Render provider-advertised input modalities for a `tracing` field.
///
/// Preserves the three-state distinction: `unknown` (not advertised),
/// `text-only` (advertised with no non-text inputs), or a fixed-order
/// `+`-joined list of the advertised non-text modalities — so a row reads
/// `image`, `file`, or `image+file` regardless of the advertised order.
pub(crate) fn format_input_modalities_for_log(
    input_modalities: Option<&[InputModality]>,
) -> String {
    let Some(input_modalities) = input_modalities else {
        return "unknown".to_string();
    };
    let mut modalities = Vec::new();
    if input_modalities.contains(&InputModality::Image) {
        modalities.push("image");
    }
    if input_modalities.contains(&InputModality::File) {
        modalities.push("file");
    }
    if modalities.is_empty() {
        "text-only".to_string()
    } else {
        modalities.join("+")
    }
}

/// Provider interface for a chat-completions-style streaming backend.
///
/// Native async trait (Rust 1.75+): no `async-trait` crate. The Provider
/// creates its own internal `mpsc::channel` and returns the receiver, so
/// retry logic and stream parsing stay hidden inside the implementation.
/// Returns a boxed future so the trait stays `dyn`-compatible (an `impl
/// Future` return position would make `Arc<dyn Provider>` unbuildable).
/// The implementation polls the `CancellationToken`; when it fires, the
/// in-flight stream is aborted and the receiver eventually yields
/// `StreamEvent::Cancelled`.
///
/// `tools` carries the schemas the model may call: the request body sends
/// them so the model can emit
/// `delta.tool_calls`, surfaced as [`StreamEvent::Delta`] fragments that the
/// agent's streaming task assembles. Tool assembly never happens here.
pub trait Provider: Send + Sync {
    /// Validation is selected by implementation, never by connection name.
    fn reasoning_effort_policy(&self) -> std::sync::Arc<dyn ReasoningEffortPolicy> {
        std::sync::Arc::new(AdvertisedReasoningEffortPolicy)
    }

    /// Bind a fresh engine-owned request runtime while retaining connection resources.
    /// None preserves the legacy shared-handle/setter contract. This capability is
    /// independent of reasoning validation, catalog support, and connection identity.
    fn bind_engine_runtime(
        &self,
        _runtime: EngineRuntimeConfigHandle,
    ) -> Option<std::sync::Arc<dyn Provider>> {
        None
    }

    /// Previously fetched target metadata, available for synchronous model switches.
    fn cached_model_info(&self, _model: &str) -> Option<ModelInfo> {
        None
    }

    /// Share connection resources with an independent set of request settings.
    fn with_runtime_config(
        &self,
        _runtime_config: EngineRuntimeConfigHandle,
    ) -> anyhow::Result<std::sync::Arc<dyn Provider>> {
        anyhow::bail!(
            "provider '{}' does not support independent runtimes",
            self.name()
        )
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>>;

    /// Start a chat request associated with a persisted session when one is
    /// available. Providers that do not record session metadata can use the
    /// default implementation.
    fn chat_with_session_id<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        _session_id: Option<uuid::Uuid>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.chat(messages, tools, cancel)
    }

    /// Start a request and report its serialized token estimate before the
    /// provider waits for response headers. Providers without a request-body
    /// estimate use the default implementation.
    fn chat_with_session_id_and_estimate<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        _on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.chat_with_session_id(messages, tools, cancel, session_id)
    }

    /// Start a request with an optional callback for recording a failure that
    /// occurs outside provider stream processing (for example, a caller-side
    /// timeout). The default implementation preserves providers without dump
    /// support.
    fn chat_with_session_id_and_diagnostics<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
        _on_failure: Option<crate::http::RequestDumpFailureHandler>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.chat_with_session_id_and_estimate(messages, tools, cancel, session_id, on_estimate)
    }

    /// Return the short name identifying this provider connection or the
    /// built-in implementation (e.g. `"ollama-cloud"`, `"openrouter"`,
    /// `"mock"`).
    fn name(&self) -> &str;

    /// Return the list of models available from this provider.
    ///
    /// Providers that cannot enumerate models return an error. An
    /// [`EngineRouter`](crate::llm::engine::EngineRouter) omits that connection
    /// when another configured connection succeeds and aggregates the
    /// failures when none do. A provider may use more than one backend
    /// endpoint to build the catalog; [`OllamaProvider`](crate::llm::OllamaProvider),
    /// for example, combines `/api/tags` with `/api/show` metadata.
    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>>;

    /// Return the shared Engine runtime settings when this provider supports them.
    ///
    /// [`EngineImpl`](crate::llm::engine::EngineImpl) uses the handle to expose a
    /// coherent [`EngineConfig`](crate::llm::engine::EngineConfig) snapshot without
    /// maintaining a second mutable copy of provider settings. Providers that
    /// do not expose mutable Engine runtime settings may keep the default.
    fn runtime_config_handle(&self) -> Option<EngineRuntimeConfigHandle> {
        None
    }

    /// Switch the model used by every subsequent `chat()` call.
    ///
    /// Providers share one `Arc<dyn Provider>` across all agents, so the
    /// switch takes effect through interior mutability rather than by
    /// rebuilding the provider. Providers that cannot switch (the mock)
    /// keep the default, which fails loudly.
    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "provider '{}' does not support switching to model '{model}'",
            self.name()
        )
    }

    /// Select a reasoning effort for subsequent requests.
    ///
    /// `None` clears an explicit effort and lets the provider use its own
    /// default behavior. Providers that do not support selectable reasoning
    /// efforts reject a non-empty value.
    fn set_reasoning_effort(&self, reasoning_effort: Option<&str>) -> anyhow::Result<()> {
        if let Some(reasoning_effort) = reasoning_effort {
            anyhow::bail!(
                "provider '{}' does not support reasoning effort '{reasoning_effort}'",
                self.name()
            );
        }
        Ok(())
    }
}

/// Tool results arrive as `role`:`"tool"` messages whose `content` is a JSON
/// envelope of this shape. `ok` is authoritative: when it is `false`, report
/// the error and do not claim the operation succeeded. When `result_metadata`
/// is present it carries structured facts about the result (e.g. byte counts,
/// match counts); `result_metadata_schema` describes each field that actually
/// appears — types and descriptions — so the shape is always self-documenting.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[schemars(title = "ToolResponseEnvelope")]
pub struct ToolEnvelope {
    pub(crate) ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result_metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result_metadata_schema: Option<serde_json::Value>,
}

impl ToolEnvelope {
    /// Serialize this envelope to a JSON string for a `role:"tool"` message's
    /// `content`. The struct only holds strings/bools, so serialization never
    /// fails.
    pub(crate) fn to_string_checked(&self) -> String {
        serde_json::to_string(self).expect("ToolEnvelope always serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn efforts(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn order_reasoning_efforts_sorts_weakest_to_strongest_with_aliases_sharing_a_rank() {
        assert_eq!(
            order_reasoning_efforts(efforts(&[
                "xhigh",
                "med",
                "none",
                "max",
                "minimal",
                "high",
                "low",
                "extrahigh",
                "min",
                "medium",
            ])),
            efforts(&[
                "none",
                "minimal",
                "min",
                "low",
                "med",
                "medium",
                "high",
                "xhigh",
                "extrahigh",
                "max",
            ])
        );
    }

    #[test]
    fn order_reasoning_efforts_keeps_unknown_names_after_known_ones_in_server_order() {
        assert_eq!(
            order_reasoning_efforts(efforts(&["zeta", "high", "weird", "low"])),
            efforts(&["low", "high", "zeta", "weird"])
        );
    }

    #[test]
    fn tool_schema_renders_function_envelope() {
        let schema = ToolSchema {
            name: "read".into(),
            description: "read a file".into(),
            parameters: serde_json::json!({ "type": "object" }),
        };
        assert_eq!(
            schema.to_request_value(),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "read a file",
                    "parameters": { "type": "object" },
                }
            })
        );
    }

    #[test]
    fn provider_request_limiters_are_shared_by_connection_name() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build Tokio runtime");
        runtime.block_on(async {
            let provider_connection = format!("limiter-test-{}", uuid::Uuid::new_v4());
            let first = provider_request_limiter(&provider_connection);
            let second = provider_request_limiter(&provider_connection);
            let permits = futures::future::try_join_all(
                (0..MAX_CONCURRENT_PROVIDER_REQUESTS).map(|_| first.acquire()),
            )
            .await
            .expect("all available permits");
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), second.acquire(),)
                    .await
                    .is_err(),
                "all engines for the connection share one cap"
            );
            drop(permits);
            let _released_permit = second.acquire().await.expect("released permit");
        });
    }

    #[test]
    fn format_reasoning_effort_for_log_renders_default_when_unset() {
        assert_eq!(format_reasoning_effort_for_log(None), "default");
        assert_eq!(format_reasoning_effort_for_log(Some("high")), "high");
    }

    #[test]
    fn format_input_modalities_for_log_preserves_the_three_states() {
        assert_eq!(format_input_modalities_for_log(None), "unknown");
        assert_eq!(format_input_modalities_for_log(Some(&[])), "text-only");
        assert_eq!(
            format_input_modalities_for_log(Some(&[InputModality::Image])),
            "image"
        );
        assert_eq!(
            format_input_modalities_for_log(Some(&[InputModality::File])),
            "file"
        );
        assert_eq!(
            format_input_modalities_for_log(Some(&[InputModality::File, InputModality::Image])),
            "image+file",
            "the log order must be fixed regardless of the advertised order"
        );
    }
}
