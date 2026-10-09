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

//! Immutable engine configuration and shared mutable request settings.

use crate::llm::InputModality;
use serde::Deserialize;
use std::sync::{Arc, RwLock};

/// Default context-length used when no explicit value is configured.
pub const DEFAULT_CONTEXT_LENGTH: usize = 128_000;

/// Default [`EngineConfig::compact_target_ratio`]: compaction shrinks the
/// model view back to 50% of the engine's context window.
pub const DEFAULT_COMPACT_TARGET_RATIO: f64 = 0.5;

/// Configuration for an Engine.
///
/// Phase 7 wires the provider-connection name, `model`, `temperature`, `context_length`,
/// `api_base_url`, and `api_key` from the narrow flat profile loader
/// (`~/.ylva/profiles/<name>/config.toml`, see [`crate::config`]) into
/// provider construction in `AgentFactory::from_profile`. `EngineConfig` is
/// therefore the SSOT for their initial values: `EngineImpl::chat()` still
/// delegates to the wrapped `Provider`, which is initialized from exactly
/// this struct. [`EngineRuntimeConfig`] owns the live mutable subset.
///
/// The layered configuration reader adds project and home files plus
/// code-owned built-in defaults without displacing that SSOT:
///
/// - **Phase 10 (Full Configuration & Persona System)** supplies the
///   project/home/built-in fallback chain and will add persona/`agents/*.md`
///   sourcing, which populate this same struct through additional layers.
/// - **Context compaction** reads per-engine settings to budget the model view
///   (see `crate::agent::compaction`).
#[derive(Clone, Debug, serde::Deserialize)]
pub struct EngineConfig {
    /// Named provider connection. Legacy flat profiles may use `provider`,
    /// which is accepted as an alias for this field.
    #[serde(alias = "provider")]
    pub provider_connection: String,
    /// Model identifier. May be left empty in a profile's `[engines.*]` table,
    /// in which case the profile's `default_model` fills it in.
    #[serde(default)]
    pub model: String,
    /// `None` means the provider uses the model's own default; `Some(v)` overrides it.
    #[serde(default, deserialize_with = "deserialize_optional_f32_or_string")]
    pub temperature: Option<f32>,
    /// Either a plain integer or a string carrying a `kb`/`mb` suffix (for
    /// example `"265kb"`, meaning `265 * 1024`) — the same 1024-based units
    /// `/models` uses to display context lengths (see
    /// `crate::tui_reducer_state::format_context_length`).
    ///
    /// `None` means the profile's `[engines.*]` table left this unset;
    /// [`EngineConfig::effective_context_length`] then falls back to the
    /// provider's advertised model context length, and finally to
    /// [`DEFAULT_CONTEXT_LENGTH`], so an explicit TOML value always wins but
    /// an unset one still benefits from what the provider knows about the
    /// model.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_context_length")]
    pub context_length: Option<usize>,
    #[serde(default)]
    pub api_base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Initial explicit reasoning effort, or `None` to use the provider's
    /// own default. Validated once, lazily before the first chat, against
    /// the model catalog: an effort the catalog proves is not advertised is
    /// dropped to the provider default with a warning (unverifiable cases —
    /// model absent, catalog fetch failed — keep the value). Later changes
    /// go through `/models`, which validates the effort against the model's
    /// advertised options before switching.
    #[serde(default)]
    pub reasoning: Option<String>,
    /// Target fraction of the context window a compaction shrinks the model
    /// view back to. Configured per `[engines.*]` table and
    /// defaults to [`DEFAULT_COMPACT_TARGET_RATIO`].
    #[serde(default = "default_compact_target_ratio")]
    pub compact_target_ratio: f64,
    /// Soft compaction trigger: the usage fraction at or above which a
    /// completed turn starts a background summary. Defaults to
    /// [`crate::agent::compaction::DEFAULT_SOFT_COMPACTION_THRESHOLD`];
    /// profile validation requires `0 < target < soft < hard <= 1`.
    #[serde(default = "default_soft_compaction_threshold")]
    pub soft_compaction_threshold: f64,
    /// Hard compaction trigger: the usage fraction above which the loop stops
    /// waiting for a background summary and drops synchronously. Defaults to
    /// [`crate::agent::compaction::DEFAULT_HARD_COMPACTION_THRESHOLD`].
    #[serde(default = "default_hard_compaction_threshold")]
    pub hard_compaction_threshold: f64,
}

fn default_compact_target_ratio() -> f64 {
    DEFAULT_COMPACT_TARGET_RATIO
}

fn default_soft_compaction_threshold() -> f64 {
    crate::agent::compaction::DEFAULT_SOFT_COMPACTION_THRESHOLD
}

fn default_hard_compaction_threshold() -> f64 {
    crate::agent::compaction::DEFAULT_HARD_COMPACTION_THRESHOLD
}

impl EngineConfig {
    /// Apply this configuration's override only to its configured model.
    pub fn context_length_for_model(
        &self,
        model: &str,
        catalog_context_length: Option<usize>,
    ) -> usize {
        let configured_context_length = (model == self.model)
            .then_some(self.context_length)
            .flatten();
        configured_context_length
            .or(catalog_context_length)
            .unwrap_or(DEFAULT_CONTEXT_LENGTH)
    }

    /// Resolve the context-length budget to use for this engine.
    ///
    /// An explicit TOML `context_length` always wins. Otherwise, falls back
    /// to `catalog_context_length` — the active model's context length as
    /// advertised by the provider's `/models` catalog — and finally to
    /// [`DEFAULT_CONTEXT_LENGTH`] if the provider doesn't advertise one
    /// either.
    pub fn effective_context_length(&self, catalog_context_length: Option<usize>) -> usize {
        self.context_length
            .or(catalog_context_length)
            .unwrap_or(DEFAULT_CONTEXT_LENGTH)
    }
}

fn deserialize_optional_f32_or_string<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum TemperatureValue {
        Integer(i64),
        Float(f32),
        Text(String),
    }

    match Option::<TemperatureValue>::deserialize(deserializer)? {
        Some(TemperatureValue::Integer(value)) => Ok(Some(value as f32)),
        Some(TemperatureValue::Float(value)) => Ok(Some(value)),
        Some(TemperatureValue::Text(value)) => {
            value.trim().parse::<f32>().map(Some).map_err(|error| {
                serde::de::Error::custom(format!("invalid temperature '{value}': {error}"))
            })
        }
        None => Ok(None),
    }
}

/// Number of context-length units per `kb`/`mb` suffix step, matching the
/// 1024-based convention `format_context_length` uses to display these
/// values in `/models` (see `crate::tui_reducer_state`).
const CONTEXT_LENGTH_KILO: usize = 1024;
const CONTEXT_LENGTH_MEGA: usize = CONTEXT_LENGTH_KILO * 1024;

/// Accept `context_length` as either a plain integer or a string carrying a
/// `kb`/`mb` suffix (for example `"265kb"`, meaning `265 * 1024`). Absent
/// entirely, the field deserializes to `None` via `#[serde(default)]`.
fn deserialize_context_length<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum ContextLengthValue {
        Number(usize),
        Text(String),
    }

    match <ContextLengthValue as serde::Deserialize>::deserialize(deserializer)? {
        ContextLengthValue::Number(value) => Ok(Some(value)),
        ContextLengthValue::Text(text) => parse_context_length(&text)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Parse a `context_length` string such as `"265kb"`, `"2mb"`, or a bare
/// `"32000"` into its expanded unit count.
fn parse_context_length(text: &str) -> Result<usize, String> {
    let trimmed = text.trim();
    let lowered = trimmed.to_lowercase();
    let (digits, multiplier) = if let Some(prefix) = lowered.strip_suffix("mb") {
        (prefix, CONTEXT_LENGTH_MEGA)
    } else if let Some(prefix) = lowered.strip_suffix("kb") {
        (prefix, CONTEXT_LENGTH_KILO)
    } else {
        (lowered.as_str(), 1)
    };
    digits
        .trim()
        .parse::<usize>()
        .map(|value| value * multiplier)
        .map_err(|_| {
            format!(
                "invalid context_length '{text}': expected an integer, optionally followed by 'kb' or 'mb'"
            )
        })
}

/// Mutable engine settings shared by an [`super::Engine`] and its [`Provider`].
///
/// A provider snapshots this value before starting a request, so all
/// request-level settings come from one coherent state even when an Engine
/// Engine runtime update happens concurrently. Connection settings remain in
/// [`EngineConfig`]
/// and are immutable for the lifetime of the engine.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineRuntimeConfig {
    pub model: String,
    pub temperature: Option<f32>,
    pub context_length: usize,
    pub reasoning_effort: Option<String>,
    /// Provider-advertised input modalities for the active model. `None`
    /// remains conservative: no media projection is enabled.
    pub input_modalities: Option<Vec<InputModality>>,
}

impl From<&EngineConfig> for EngineRuntimeConfig {
    fn from(config: &EngineConfig) -> Self {
        EngineRuntimeConfig {
            model: config.model.clone(),
            temperature: config.temperature,
            // No provider catalog is available yet at construction time, so
            // this only resolves the TOML value against the built-in
            // default; see `EngineConfig::effective_context_length`.
            context_length: config.effective_context_length(None),
            reasoning_effort: config.reasoning.clone(),
            input_modalities: None,
        }
    }
}

/// Shared handle for the mutable settings used by an engine and its provider.
pub type EngineRuntimeConfigHandle = Arc<RwLock<EngineRuntimeConfig>>;
