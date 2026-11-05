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

//! Typed model-picker selections and display-only snapshots.

use super::{Engine, EngineConfig};
use crate::llm::ModelInfo;

/// The named engine, provider connection, and model shown for the conversation
/// currently on screen.
///
/// This intentionally contains only display metadata. Engine runtime request
/// settings remain owned by [`super::EngineRuntimeConfig`], while the immutable
/// connection details stay in [`EngineConfig`]. The reasoning effort is
/// copied from the Engine runtime snapshot for display only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineDisplayInfo {
    /// The profile key under `[engines.*]`, when the Engine runtime has one.
    ///
    /// Directly constructed engines used by compatibility callers may not have
    /// a named-engine key and leave this empty.
    pub engine_name: String,
    pub provider_connection: String,
    pub model: String,
    /// Explicit reasoning effort, or `None` when the provider default is used.
    pub reasoning_effort: Option<String>,
    /// Current summary-engine identity and whether it follows or shares the
    /// conversation engine.
    pub compaction_engine: Option<CompactionEngineDisplayInfo>,
    /// Whether this conversation shares an Engine runtime with its parent agent.
    pub inherits_parent_runtime: bool,
    /// Whether the live model, reasoning effort or overrides differ from the
    /// named engine's configured baseline, so the engine name no longer
    /// describes what runs.
    pub config_modified: bool,
}

/// Display-only selection and context window of the engine used for summaries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactionEngineDisplayInfo {
    pub engine_name: String,
    pub provider_connection: String,
    pub model: String,
    /// Effective context window in tokens.
    pub context_length: usize,
    pub follows_conversation: bool,
    pub shares_conversation: bool,
    /// Copied from the conversation display when the summarizer follows or
    /// shares the conversation engine.
    pub config_modified: bool,
}

impl EngineDisplayInfo {
    /// Snapshot the identity and explicit reasoning effort shown on screen.
    pub fn from_engine(engine: &dyn Engine) -> Self {
        let config = engine.config();
        Self {
            engine_name: engine.active_engine_name().unwrap_or_default(),
            provider_connection: config.provider_connection,
            model: config.model,
            reasoning_effort: engine.reasoning_effort(),
            compaction_engine: None,
            inherits_parent_runtime: false,
            config_modified: engine.active_engine_config_modified(),
        }
    }
}

impl From<&EngineConfig> for EngineDisplayInfo {
    fn from(config: &EngineConfig) -> Self {
        EngineDisplayInfo {
            engine_name: String::new(),
            provider_connection: config.provider_connection.clone(),
            model: config.model.clone(),
            reasoning_effort: None,
            compaction_engine: None,
            inherits_parent_runtime: false,
            config_modified: false,
        }
    }
}

/// One model option returned by a named provider connection.
///
/// The `provider_connection` field is the connection name, not the
/// implementation kind. This keeps two Ollama connections distinct even
/// though both use the `"ollama"` implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderModelOption {
    pub provider_connection: String,
    pub model: ModelInfo,
    /// Explicit reasoning effort selected in the model picker.
    ///
    /// `None` means that the provider's own default behavior is used.
    pub reasoning_effort: Option<String>,
    /// Context window edited in the model picker's parameter popup, in
    /// tokens. `None` keeps the catalog or profile window. The override is
    /// session-only and never written to the profile.
    pub context_length_override: Option<usize>,
    /// Output-token limit edited in the model picker's parameter popup.
    /// `None` keeps the profile or unlimited behavior. Session-only like
    /// [`Self::context_length_override`].
    pub max_output_tokens_override: Option<usize>,
}

/// How a named-engine shortcut should treat the engine's current reasoning
/// setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReasoningEffortChange {
    /// Keep the named engine's current Engine runtime effort.
    Unchanged,
    /// Apply the picker value, where `None` selects the provider default.
    Set(Option<String>),
}

/// A model-picker selection that keeps named-engine identity separate from
/// raw provider/model catalog choices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelSelection {
    NamedEngine {
        engine_name: String,
        option: ProviderModelOption,
        reasoning_effort: ReasoningEffortChange,
    },
    ProviderModel(ProviderModelOption),
}

impl ModelSelection {
    pub fn provider_connection(&self) -> &str {
        match self {
            Self::NamedEngine { option, .. } | Self::ProviderModel(option) => {
                &option.provider_connection
            }
        }
    }

    pub fn model_id(&self) -> &str {
        match self {
            Self::NamedEngine { option, .. } | Self::ProviderModel(option) => &option.model.id,
        }
    }
}

/// Non-mutating routing result and effective context window for a model-picker
/// selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSelectionTarget {
    pub engine_name: Option<String>,
    pub provider_connection: String,
    pub model: String,
    /// Effective context window after applying the selection, in tokens.
    pub context_length: usize,
    /// Effective output-token limit after applying the selection; `None`
    /// means no limit is sent.
    pub max_output_tokens: Option<usize>,
}

/// Engine runtime settings displayed for one named engine.
#[derive(Clone, Debug)]
pub struct NamedEngineInfo {
    /// Cached live catalog evidence for this named engine's selected model.
    pub model_info: Option<ModelInfo>,
    pub name: String,
    pub config: EngineConfig,
    pub reasoning_effort: Option<String>,
    /// The engine's configured profile baseline, when its runtime reports one.
    pub configured: Option<EngineConfig>,
    /// The engine's live runtime settings, including session-only overrides.
    pub runtime: Option<super::EngineRuntimeConfig>,
}

pub(super) fn validate_named_engine_option(
    engine: &(impl Engine + ?Sized),
    option: &ProviderModelOption,
    reasoning_effort: &ReasoningEffortChange,
) -> anyhow::Result<()> {
    let config = engine.config();
    anyhow::ensure!(
        config.provider_connection == option.provider_connection,
        "engine uses provider connection '{}', not '{}'",
        config.provider_connection,
        option.provider_connection
    );
    // A named selection may also restore the configured model after a raw
    // pick moved the engine to another model of the same connection.
    let configured_model = engine
        .configured_runtime_config()
        .map(|configured| configured.model);
    anyhow::ensure!(
        config.model == option.model.id || configured_model.as_deref() == Some(&option.model.id),
        "engine uses model '{}', not '{}'",
        config.model,
        option.model.id
    );
    engine.reasoning_effort_policy().validate_named_change(
        &option.model,
        engine.reasoning_effort().as_deref(),
        reasoning_effort,
    )?;
    Ok(())
}
