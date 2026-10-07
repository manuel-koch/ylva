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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use futures::future::{join_all, BoxFuture};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::agent::{Message, StreamEvent};
use crate::llm::{InputModality, ModelInfo, Provider, ToolSchema};

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
    /// view back to (blueprint §4C). Configured per `[engines.*]` table and
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

/// The named engine, provider connection, and model shown for the conversation
/// currently on screen.
///
/// This intentionally contains only display metadata. Engine runtime request
/// settings remain owned by [`EngineRuntimeConfig`], while the immutable
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
        }
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

/// Mutable engine settings shared by an [`Engine`] and its [`Provider`].
///
/// A provider snapshots this value before starting a request, so all
/// request-level settings come from one coherent state even when a runtime
/// update happens concurrently. Connection settings remain in [`EngineConfig`]
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
}

/// Runtime abstraction over a chat-completions-style streaming backend.
///
/// Keeps `AgentLoop` independent of concrete provider implementations while
/// allowing the agent factory to construct root and child engines consistently.
pub trait Engine: Send + Sync {
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>>;

    /// Start a chat request associated with a persisted session when one is
    /// available. Engines that do not propagate request metadata can use the
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
    /// provider waits for response headers. Engines without request metadata
    /// retain the normal chat behavior.
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

    /// Start a request with optional diagnostics for caller-side failures.
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

    /// A snapshot of the engine's configuration.
    ///
    /// Returned by value rather than by reference because `/models` mutates
    /// the active model through interior mutability: a borrowed accessor
    /// would either hand out a stale `model` or pin a lock guard across
    /// caller code.
    fn config(&self) -> EngineConfig;

    /// Return the active profile engine name when this Engine runtime routes among
    /// named engines. A directly constructed engine has no named key.
    fn active_engine_name(&self) -> Option<String> {
        None
    }

    /// List the models the underlying provider offers.
    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>>;

    /// List models from every configured provider connection represented by
    /// this engine or router.
    ///
    /// A regular engine exposes its own connection under its connection name.
    /// [`EngineRouter`] overrides this to query all configured provider
    /// connections.
    fn list_model_options(&self) -> BoxFuture<'_, anyhow::Result<Vec<ProviderModelOption>>> {
        let config = self.config();
        let provider_connection = config.provider_connection;
        let models = self.list_models();
        Box::pin(async move {
            models.await.map(|models| {
                models
                    .into_iter()
                    .map(|model| ProviderModelOption {
                        provider_connection: provider_connection.clone(),
                        model,
                        reasoning_effort: None,
                    })
                    .collect()
            })
        })
    }

    /// Every configured named engine's connection/model settings, sorted by
    /// name.
    ///
    /// `/models` lists these as shortcuts ahead of the raw provider/model
    /// catalog from [`Engine::list_model_options`], so picking one jumps
    /// straight to the provider connection, model, and sampling parameters a
    /// profile's `[engines.<name>]` table (or its implicit
    /// one-per-connection equivalent) names. A bare, non-routed engine has no
    /// named siblings to offer, so the default returns an empty list;
    /// [`EngineRouter`] overrides this with its full configured set.
    fn named_engines(&self) -> Vec<(String, EngineConfig)> {
        Vec::new()
    }

    /// Point this engine — and therefore every agent sharing it — at
    /// `model` for all subsequent turns.
    fn switch_model(&self, model: &str) -> anyhow::Result<()>;

    /// Return the explicit reasoning effort used by subsequent requests.
    ///
    /// Engines without selectable reasoning efforts return `None`.
    fn reasoning_effort(&self) -> Option<String> {
        None
    }

    /// Set the explicit reasoning effort used by subsequent requests.
    ///
    /// Engines without selectable reasoning efforts reject a non-empty value.
    fn set_reasoning_effort(&self, reasoning_effort: Option<&str>) -> anyhow::Result<()> {
        if let Some(reasoning_effort) = reasoning_effort {
            anyhow::bail!("engine does not support reasoning effort '{reasoning_effort}'");
        }
        Ok(())
    }

    /// Return the active model's provider-advertised input modalities.
    fn input_modalities(&self) -> Option<Vec<InputModality>> {
        None
    }

    /// Update the active model's provider-advertised input modalities.
    fn set_input_modalities(&self, _input_modalities: Option<Vec<InputModality>>) {}

    /// The context window the active model serves, in tokens.
    ///
    /// [`EngineConfig::effective_context_length`] resolved for the *current*
    /// model: the profile's `context_length` when set, otherwise the
    /// provider-catalog length adopted by the last `/models` selection,
    /// otherwise [`DEFAULT_CONTEXT_LENGTH`]. Compaction re-reads this on
    /// every sweep, so switching to a model with a different window
    /// re-evaluates the budget (blueprint §4C).
    fn effective_context_length(&self) -> usize {
        self.config().effective_context_length(None)
    }

    /// Adopt the provider-catalog context length of a newly selected model.
    ///
    /// Engines whose profile leaves `context_length` unset follow the
    /// catalog; an explicit profile value always wins. The default is a
    /// no-op for engines with no Engine runtime context length to update.
    fn adopt_catalog_context_length(&self, _catalog_context_length: Option<usize>) {}

    /// Discover the active model's provider-advertised catalog metadata —
    /// input modalities and a reasoning-effort validation pass — from the
    /// provider catalog, at most once per engine.
    ///
    /// The default is a no-op (there is nothing to discover).
    /// [`EngineImpl`] performs the discovery lazily before the first chat
    /// request, so an initially configured model gains its modality state
    /// without requiring a `/models` re-selection, and a profile-configured
    /// reasoning effort is validated against the catalog exactly once.
    /// Discovery failures leave the modality state `unknown`; a later
    /// `/models` selection still resolves it.
    fn hydrate_catalog_metadata(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// Drain engine-generated notices that have not been surfaced yet.
    ///
    /// The engine keeps such notices until a driver asks for them:
    /// [`EngineImpl`] records one when the pre-chat validation drops a
    /// profile reasoning effort, and the agent loop forwards whatever is
    /// pending before the stream starts. The default is empty — there is
    /// nothing to surface.
    fn take_pending_notices(&self) -> Vec<String> {
        Vec::new()
    }

    /// Activate the engine and model represented by a catalog option.
    ///
    /// A regular engine can only select options for its own provider
    /// connection; [`EngineRouter`] also changes the active connection.
    fn switch_model_option(&self, option: &ProviderModelOption) -> anyhow::Result<()> {
        let config = self.config();
        anyhow::ensure!(
            config.provider_connection == option.provider_connection,
            "engine uses provider connection '{}', not '{}'",
            config.provider_connection,
            option.provider_connection
        );
        if let Some(reasoning_effort) = option.reasoning_effort.as_deref() {
            anyhow::ensure!(
                option.model.advertises_reasoning_effort(reasoning_effort),
                "model '{}' does not advertise reasoning effort '{}'",
                option.model.id,
                reasoning_effort
            );
        }
        self.set_reasoning_effort(option.reasoning_effort.as_deref())?;
        self.switch_model(&option.model.id)?;
        self.set_input_modalities(option.model.input_modalities.clone());
        self.adopt_catalog_context_length(option.model.context_length);
        Ok(())
    }
}

/// Routes chat and model operations to one active engine from a configured
/// collection.
///
/// The router keeps connection settings in the underlying engines. Switching
/// provider connections only changes which already-constructed engine receives
/// future requests, while model changes continue to use each connection's existing
/// Engine runtime configuration handle.
pub struct EngineRouter {
    engines: HashMap<String, Arc<dyn Engine>>,
    active_engine: RwLock<String>,
}

impl EngineRouter {
    pub fn new(
        engines: HashMap<String, Arc<dyn Engine>>,
        active_engine: &str,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !engines.is_empty(),
            "cannot create an engine router without configured engines"
        );
        anyhow::ensure!(
            engines.contains_key(active_engine),
            "active engine '{active_engine}' is not configured"
        );
        Ok(Self {
            engines,
            active_engine: RwLock::new(active_engine.to_string()),
        })
    }

    fn active_engine_entry(&self) -> (String, Arc<dyn Engine>) {
        let active_engine = self
            .active_engine
            .read()
            .expect("the active engine lock is poisoned");
        let engine_name = active_engine.clone();
        let engine = self
            .engines
            .get(active_engine.as_str())
            .expect("the active engine must remain configured")
            .clone();
        (engine_name, engine)
    }

    fn active_engine(&self) -> Arc<dyn Engine> {
        self.active_engine_entry().1
    }

    /// Return one representative engine for each named provider connection.
    ///
    /// Multiple named engines may reference the same provider connection. The
    /// active engine wins for its connection so a model switch updates the
    /// engine currently serving the conversation; all other connections use a
    /// deterministic engine-name order.
    fn provider_engines(&self) -> Vec<(String, String, Arc<dyn Engine>)> {
        let active_engine_name = self
            .active_engine
            .read()
            .expect("the active engine lock is poisoned")
            .clone();
        let mut engines: Vec<_> = self
            .engines
            .iter()
            .map(|(name, engine)| (name.clone(), engine.clone()))
            .collect();
        engines.sort_by(|left, right| left.0.cmp(&right.0));

        let mut provider_connections: HashMap<String, (String, Arc<dyn Engine>)> = HashMap::new();
        for (engine_name, engine) in engines {
            let provider_connection = engine.config().provider_connection;
            provider_connections
                .entry(provider_connection)
                .or_insert((engine_name, engine));
        }
        if let Some(active_engine) = self.engines.get(&active_engine_name).cloned() {
            let provider_connection = active_engine.config().provider_connection;
            provider_connections.insert(provider_connection, (active_engine_name, active_engine));
        }

        let mut providers: Vec<_> = provider_connections
            .into_iter()
            .map(|(provider_connection, (engine_name, engine))| {
                (provider_connection, engine_name, engine)
            })
            .collect();
        providers.sort_by(|left, right| left.0.cmp(&right.0));
        providers
    }
}

impl Engine for EngineRouter {
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            let (engine_name, engine) = self.active_engine_entry();
            // Discover the active model's catalog metadata (modalities and
            // the profile reasoning effort) before the selection is logged
            // and before the request is serialized, so both log lines and
            // the attachment projection see the hydrated state.
            engine.hydrate_catalog_metadata().await;
            let config = engine.config();
            tracing::debug!(
                engine = %engine_name,
                provider_connection = %config.provider_connection,
                model = %config.model,
                reasoning_effort = %crate::llm::format_reasoning_effort_for_log(
                    engine.reasoning_effort().as_deref()
                ),
                input_modalities = %crate::llm::format_input_modalities_for_log(
                    engine.input_modalities().as_deref()
                ),
                "named engine selected for chat request"
            );
            let span = tracing::debug_span!(
                "engine_request",
                engine = %engine_name,
                provider_connection = %config.provider_connection,
            );
            engine.chat(messages, tools, cancel).instrument(span).await
        })
    }

    fn chat_with_session_id<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            let (engine_name, engine) = self.active_engine_entry();
            engine.hydrate_catalog_metadata().await;
            let config = engine.config();
            tracing::debug!(
                engine = %engine_name,
                provider_connection = %config.provider_connection,
                model = %config.model,
                reasoning_effort = %crate::llm::format_reasoning_effort_for_log(
                    engine.reasoning_effort().as_deref()
                ),
                input_modalities = %crate::llm::format_input_modalities_for_log(
                    engine.input_modalities().as_deref()
                ),
                "named engine selected for chat request"
            );
            let span = tracing::debug_span!(
                "engine_request",
                engine = %engine_name,
                provider_connection = %config.provider_connection,
            );
            engine
                .chat_with_session_id(messages, tools, cancel, session_id)
                .instrument(span)
                .await
        })
    }

    fn chat_with_session_id_and_estimate<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            let (_engine_name, engine) = self.active_engine_entry();
            engine
                .chat_with_session_id_and_estimate(messages, tools, cancel, session_id, on_estimate)
                .await
        })
    }

    fn chat_with_session_id_and_diagnostics<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
        on_failure: Option<crate::http::RequestDumpFailureHandler>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            let (_engine_name, engine) = self.active_engine_entry();
            engine
                .chat_with_session_id_and_diagnostics(
                    messages,
                    tools,
                    cancel,
                    session_id,
                    on_estimate,
                    on_failure,
                )
                .await
        })
    }

    fn config(&self) -> EngineConfig {
        self.active_engine().config()
    }

    fn effective_context_length(&self) -> usize {
        self.active_engine().effective_context_length()
    }

    fn active_engine_name(&self) -> Option<String> {
        Some(self.active_engine_entry().0)
    }

    fn reasoning_effort(&self) -> Option<String> {
        self.active_engine().reasoning_effort()
    }

    fn input_modalities(&self) -> Option<Vec<InputModality>> {
        self.active_engine().input_modalities()
    }

    fn hydrate_catalog_metadata(&self) -> BoxFuture<'_, ()> {
        let engine = self.active_engine();
        Box::pin(async move { engine.hydrate_catalog_metadata().await })
    }

    /// Drain pending notices from every configured engine, not just the
    /// active one: a chat may have hydrated one connection while a `/models`
    /// switch made another connection active before the notice was taken.
    fn take_pending_notices(&self) -> Vec<String> {
        self.engines
            .values()
            .flat_map(|engine| engine.take_pending_notices())
            .collect()
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        let engine = self.active_engine();
        Box::pin(async move { engine.list_models().await })
    }

    fn list_model_options(&self) -> BoxFuture<'_, anyhow::Result<Vec<ProviderModelOption>>> {
        let providers = self.provider_engines();

        Box::pin(async move {
            let requests = providers.into_iter().map(
                |(provider_connection, _engine_name, engine)| async move {
                    let result = engine.list_models().await;
                    (provider_connection, result)
                },
            );
            let results = join_all(requests).await;
            let mut options = Vec::new();
            let mut failures = Vec::new();
            for (provider_connection, result) in results {
                match result {
                    Ok(models) => {
                        options.extend(models.into_iter().map(|model| ProviderModelOption {
                            provider_connection: provider_connection.clone(),
                            model,
                            reasoning_effort: None,
                        }))
                    }
                    Err(error) => {
                        tracing::warn!(
                            provider_connection = %provider_connection,
                            %error,
                            "failed to list models from configured provider connection"
                        );
                        failures.push(format!("{provider_connection}: {error}"));
                    }
                }
            }
            options.sort_by(|left, right| {
                left.provider_connection
                    .cmp(&right.provider_connection)
                    .then_with(|| left.model.id.cmp(&right.model.id))
            });
            if options.is_empty() && !failures.is_empty() {
                anyhow::bail!(
                    "no configured provider connection returned models: {}",
                    failures.join("; ")
                );
            }
            Ok(options)
        })
    }

    fn switch_model(&self, model: &str) -> anyhow::Result<()> {
        self.active_engine().switch_model(model)
    }

    fn named_engines(&self) -> Vec<(String, EngineConfig)> {
        let mut engines: Vec<(String, EngineConfig)> = self
            .engines
            .iter()
            .map(|(name, engine)| (name.clone(), engine.config()))
            .collect();
        engines.sort_by(|left, right| left.0.cmp(&right.0));
        engines
    }

    fn switch_model_option(&self, option: &ProviderModelOption) -> anyhow::Result<()> {
        let (provider_connection, engine_name, engine) = self
            .provider_engines()
            .into_iter()
            .find(|(provider_connection, _, _)| provider_connection == &option.provider_connection)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "provider connection '{}' is not configured",
                    option.provider_connection
                )
            })?;
        anyhow::ensure!(
            provider_connection == option.provider_connection,
            "provider connection '{}' does not match '{}'",
            provider_connection,
            option.provider_connection
        );
        engine.switch_model_option(option)?;
        *self
            .active_engine
            .write()
            .map_err(|_| anyhow::anyhow!("the active engine lock is poisoned"))? = engine_name;
        Ok(())
    }
}

/// Default `Engine` implementation — thin wrapper around an `Arc<dyn Provider>`.
pub struct EngineImpl {
    provider: Arc<dyn Provider>,
    /// Connection settings are immutable after construction.
    config: EngineConfig,
    /// Shared with providers that support Engine runtime settings.
    runtime_config: EngineRuntimeConfigHandle,
    /// One-shot guard: the active model's catalog metadata — input
    /// modalities plus the profile reasoning-effort validation — is
    /// discovered at most once per engine instance, whether the attempt
    /// succeeds or fails. A `/models` switch writes the state directly and
    /// validates the effort itself, so it does not need this guard.
    catalog_hydrated: AtomicBool,
    /// One-shot notices accumulated by the engine — currently only a
    /// profile reasoning effort dropped by the pre-chat validation — and
    /// handed out via [`Engine::take_pending_notices`], so the agent loop
    /// can surface them as transcript notices without the engine knowing
    /// anything about the UI.
    pending_notices: Mutex<Vec<String>>,
}

impl EngineImpl {
    pub fn new(provider: Arc<dyn Provider>, config: EngineConfig) -> Self {
        let runtime_config = provider
            .runtime_config_handle()
            .unwrap_or_else(|| Arc::new(RwLock::new(EngineRuntimeConfig::from(&config))));
        *runtime_config
            .write()
            .expect("the engine runtime configuration lock is poisoned") =
            EngineRuntimeConfig::from(&config);
        EngineImpl {
            provider,
            config,
            runtime_config,
            catalog_hydrated: AtomicBool::new(false),
            pending_notices: Mutex::new(Vec::new()),
        }
    }

    /// Validate the current (profile-seeded) reasoning effort against the
    /// catalog, once, lazily before the first chat.
    ///
    /// - `active_model` advertises the effort as a member of its
    ///   `Some(values)` list → keep it silently;
    /// - `active_model` is present but does not advertise it (including
    ///   `None` / `Some([])`, which advertise no sendable alternative) →
    ///   drop to the provider default with a warning, strictly matching the
    ///   `/models` switch rule, and queue a notice for
    ///   [`Engine::take_pending_notices`];
    /// - `active_model` is absent from the catalog → keep the effort (the
    ///   catalog cannot disprove it) and log at debug level.
    ///
    /// A failed catalog fetch never reaches this method — the hydration
    /// caller keeps its existing warn-and-set-guard behavior, so the effort
    /// is then validated only by a later `/models` switch.
    fn validate_profile_reasoning_effort(&self, active_model: Option<&ModelInfo>) {
        let Some(reasoning_effort) = self.reasoning_effort() else {
            return;
        };
        let Some(model) = active_model else {
            tracing::debug!(
                model = %self.config().model,
                reasoning_effort = %reasoning_effort,
                "profile reasoning effort kept: the catalog does not list the active model"
            );
            return;
        };
        if model.advertises_reasoning_effort(&reasoning_effort) {
            return;
        }
        tracing::warn!(
            model = %model.id,
            reasoning_effort = %reasoning_effort,
            advertised_reasoning_efforts = ?model.reasoning_efforts,
            "profile reasoning effort is not advertised by the model; \
             falling back to the provider default"
        );
        match self.set_reasoning_effort(None) {
            Ok(()) => {
                self.pending_notices
                    .lock()
                    .expect("the engine pending-notices lock is poisoned")
                    .push(format!(
                        "Model {}/{} does not advertise reasoning effort \"{}\"; \
                         using the provider default.",
                        self.config.provider_connection, model.id, reasoning_effort
                    ));
            }
            Err(error) => {
                tracing::warn!(
                    model = %model.id,
                    %error,
                    "failed to reset the unadvertised profile reasoning effort"
                );
            }
        }
    }
}

impl Engine for EngineImpl {
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            self.hydrate_catalog_metadata().await;
            self.provider.chat(messages, tools, cancel).await
        })
    }

    fn chat_with_session_id<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            self.hydrate_catalog_metadata().await;
            self.provider
                .chat_with_session_id(messages, tools, cancel, session_id)
                .await
        })
    }

    fn chat_with_session_id_and_estimate<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            self.hydrate_catalog_metadata().await;
            self.provider
                .chat_with_session_id_and_estimate(messages, tools, cancel, session_id, on_estimate)
                .await
        })
    }

    fn chat_with_session_id_and_diagnostics<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
        on_failure: Option<crate::http::RequestDumpFailureHandler>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            self.hydrate_catalog_metadata().await;
            self.provider
                .chat_with_session_id_and_diagnostics(
                    messages,
                    tools,
                    cancel,
                    session_id,
                    on_estimate,
                    on_failure,
                )
                .await
        })
    }

    fn config(&self) -> EngineConfig {
        let runtime_config = self
            .runtime_config
            .read()
            .expect("the engine runtime configuration lock is poisoned")
            .clone();
        let mut config = self.config.clone();
        config.model = runtime_config.model;
        config.temperature = runtime_config.temperature;
        config
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async move {
            tracing::debug!(
                provider_connection = %self.config.provider_connection,
                api_base_url = ?self.config.api_base_url,
                "model catalog queried"
            );
            self.provider.list_models().await
        })
    }

    fn switch_model(&self, model: &str) -> anyhow::Result<()> {
        self.provider.set_model(model)?;
        let mut runtime_config = self
            .runtime_config
            .write()
            .map_err(|_| anyhow::anyhow!("the engine runtime configuration lock is poisoned"))?;
        runtime_config.model = model.to_string();
        Ok(())
    }

    fn reasoning_effort(&self) -> Option<String> {
        self.runtime_config
            .read()
            .expect("the engine runtime configuration lock is poisoned")
            .reasoning_effort
            .clone()
    }

    fn input_modalities(&self) -> Option<Vec<InputModality>> {
        self.runtime_config
            .read()
            .expect("the engine runtime configuration lock is poisoned")
            .input_modalities
            .clone()
    }

    fn set_input_modalities(&self, input_modalities: Option<Vec<InputModality>>) {
        self.runtime_config
            .write()
            .expect("the engine runtime configuration lock is poisoned")
            .input_modalities = input_modalities;
    }

    fn effective_context_length(&self) -> usize {
        // The profile's own value always wins; otherwise the Engine runtime value
        // (seeded from the built-in default, replaced by the catalog length
        // a `/models` selection adopts).
        let runtime_context_length = self
            .runtime_config
            .read()
            .expect("the engine runtime configuration lock is poisoned")
            .context_length;
        self.config.context_length.unwrap_or(runtime_context_length)
    }

    fn adopt_catalog_context_length(&self, catalog_context_length: Option<usize>) {
        if self.config.context_length.is_some() {
            // An explicit profile `context_length` outranks the catalog.
            return;
        }
        let Some(catalog_context_length) = catalog_context_length else {
            return;
        };
        let mut runtime_config = self
            .runtime_config
            .write()
            .expect("the engine runtime configuration lock is poisoned");
        if runtime_config.context_length == catalog_context_length {
            return;
        }
        tracing::info!(
            provider_connection = %self.config.provider_connection,
            model = %runtime_config.model,
            previous_context_length = runtime_config.context_length,
            context_length = catalog_context_length,
            "adopted catalog context length for the selected model"
        );
        runtime_config.context_length = catalog_context_length;
    }

    fn hydrate_catalog_metadata(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if self.catalog_hydrated.load(Ordering::Acquire) {
                return;
            }
            if self.input_modalities().is_some() {
                // A `/models` selection already supplied the state — and it
                // validated the reasoning effort, so neither needs a fetch.
                self.catalog_hydrated.store(true, Ordering::Release);
                return;
            }
            let active_model = self.config().model;
            match self.list_models().await {
                Ok(models) => {
                    let active_model_info = models.iter().find(|model| model.id == active_model);
                    self.adopt_catalog_context_length(
                        active_model_info.and_then(|model| model.context_length),
                    );
                    if let Some(input_modalities) =
                        active_model_info.and_then(|model| model.input_modalities.clone())
                    {
                        self.set_input_modalities(Some(input_modalities));
                        tracing::debug!(
                            model = %active_model,
                            input_modalities = %crate::llm::format_input_modalities_for_log(
                                self.input_modalities().as_deref()
                            ),
                            "active model input modalities discovered"
                        );
                    }
                    self.validate_profile_reasoning_effort(active_model_info);
                }
                Err(error) => {
                    tracing::warn!(
                        model = %active_model,
                        error = %error,
                        "failed to discover active model input modalities; \
                         staying unknown until a /models selection"
                    );
                }
            }
            self.catalog_hydrated.store(true, Ordering::Release);
        })
    }

    fn take_pending_notices(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .pending_notices
                .lock()
                .expect("the engine pending-notices lock is poisoned"),
        )
    }

    fn set_reasoning_effort(&self, reasoning_effort: Option<&str>) -> anyhow::Result<()> {
        self.provider.set_reasoning_effort(reasoning_effort)?;
        let mut runtime_config = self
            .runtime_config
            .write()
            .map_err(|_| anyhow::anyhow!("the engine runtime configuration lock is poisoned"))?;
        runtime_config.reasoning_effort = reasoning_effort.map(String::from);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::llm::{MockProvider, OpenAiCompatibleProvider, Provider};

    fn make_engine_config() -> EngineConfig {
        EngineConfig {
            provider_connection: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: None,
            api_key: None,
            reasoning: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        }
    }

    struct CatalogEngine {
        config: RwLock<EngineConfig>,
        models: Vec<ModelInfo>,
        model_list_error: Option<String>,
    }

    impl CatalogEngine {
        fn new(provider: &str, model: &str, models: &[&str]) -> Self {
            CatalogEngine {
                config: RwLock::new(EngineConfig {
                    provider_connection: provider.to_string(),
                    model: model.to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: Some(format!("https://{provider}.test/v1")),
                    api_key: None,
                    reasoning: None,

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
                }),
                models: models
                    .iter()
                    .map(|id| ModelInfo {
                        id: (*id).to_string(),
                        context_length: None,
                        reasoning_efforts: None,
                        reasoning_default: None,
                        input_modalities: None,
                    })
                    .collect(),
                model_list_error: None,
            }
        }

        fn failing(provider: &str, message: &str) -> Self {
            let mut engine = Self::new(provider, "unavailable", &[]);
            engine.model_list_error = Some(message.to_string());
            engine
        }
    }

    impl Engine for CatalogEngine {
        fn chat<'a>(
            &'a self,
            _messages: Vec<Message>,
            _tools: Vec<ToolSchema>,
            _cancel: CancellationToken,
        ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            Box::pin(async { anyhow::bail!("catalog engine does not chat") })
        }

        fn config(&self) -> EngineConfig {
            self.config
                .read()
                .expect("catalog engine config lock")
                .clone()
        }

        fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
            let models = self.models.clone();
            let error = self.model_list_error.clone();
            Box::pin(async move {
                match error {
                    Some(error) => anyhow::bail!(error),
                    None => Ok(models),
                }
            })
        }

        fn switch_model(&self, model: &str) -> anyhow::Result<()> {
            self.config
                .write()
                .expect("catalog engine config lock")
                .model = model.to_string();
            Ok(())
        }
    }

    /// Provider stub whose catalog can carry input modalities and reasoning
    /// efforts, for exercising [`EngineImpl::hydrate_catalog_metadata`]. `chat` always fails — the
    /// hydration tests only need the catalog fetch to run.
    struct CatalogProvider {
        models: Vec<ModelInfo>,
        list_models_calls: std::sync::atomic::AtomicUsize,
        fail_list_models: bool,
    }

    impl CatalogProvider {
        fn new(models: Vec<ModelInfo>) -> Self {
            CatalogProvider {
                models,
                list_models_calls: std::sync::atomic::AtomicUsize::new(0),
                fail_list_models: false,
            }
        }

        fn failing() -> Self {
            CatalogProvider {
                models: Vec::new(),
                list_models_calls: std::sync::atomic::AtomicUsize::new(0),
                fail_list_models: true,
            }
        }

        fn list_models_calls(&self) -> usize {
            self.list_models_calls.load(Ordering::SeqCst)
        }
    }

    impl Provider for CatalogProvider {
        fn name(&self) -> &str {
            "catalog"
        }

        fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
            self.list_models_calls.fetch_add(1, Ordering::SeqCst);
            let models = self.models.clone();
            let fail_list_models = self.fail_list_models;
            Box::pin(async move {
                if fail_list_models {
                    anyhow::bail!("catalog unavailable");
                }
                Ok(models)
            })
        }

        fn chat<'a>(
            &'a self,
            _messages: Vec<Message>,
            _tools: Vec<ToolSchema>,
            _cancel: CancellationToken,
        ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            Box::pin(async { anyhow::bail!("catalog provider does not chat") })
        }
    }

    fn model_info_with_modalities(
        id: &str,
        input_modalities: Option<Vec<InputModality>>,
    ) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            context_length: None,
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities,
        }
    }

    fn model_info_with_reasoning_efforts(
        id: &str,
        reasoning_efforts: Option<Vec<String>>,
    ) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            context_length: None,
            reasoning_efforts,
            reasoning_default: None,
            input_modalities: None,
        }
    }

    /// An engine seeded with a profile `reasoning` effort on top of the
    /// standard `test-model` engine config.
    fn engine_with_profile_reasoning(provider: Arc<dyn Provider>, reasoning: &str) -> EngineImpl {
        let config = EngineConfig {
            reasoning: Some(reasoning.to_string()),

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
            ..make_engine_config()
        };
        EngineImpl::new(provider, config)
    }

    #[test]
    fn engine_impl_config_returns_injected_config() {
        let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
        let config = make_engine_config();
        let engine = EngineImpl::new(provider, config.clone());

        assert_eq!(
            engine.config().provider_connection,
            config.provider_connection
        );
        assert_eq!(engine.config().model, config.model);
        assert_eq!(engine.config().context_length, None);
    }

    #[test]
    fn engine_config_context_length_accepts_a_plain_integer() {
        let config: EngineConfig = toml::from_str(
            r#"
provider_connection = "ollama"
model = "local"
context_length = 32000
"#,
        )
        .expect("a plain integer context_length parses");
        assert_eq!(config.context_length, Some(32_000));
    }

    #[test]
    fn engine_config_context_length_defaults_to_none_when_unset() {
        let config: EngineConfig = toml::from_str(
            r#"
provider_connection = "ollama"
model = "local"
"#,
        )
        .expect("an EngineConfig without context_length parses");
        assert_eq!(
            config.context_length, None,
            "an unset context_length must not silently become a concrete default"
        );
    }

    #[test]
    fn engine_config_context_length_accepts_kb_and_mb_suffixed_strings() {
        let kilobytes: EngineConfig = toml::from_str(
            r#"
provider_connection = "openrouter"
model = "deepseek/deepseek-v4-flash-0731"
context_length = "265kb"
"#,
        )
        .expect("a 'kb'-suffixed context_length parses");
        assert_eq!(kilobytes.context_length, Some(265 * 1024));

        let megabytes: EngineConfig = toml::from_str(
            r#"
provider_connection = "openrouter"
model = "deepseek/deepseek-v4-flash-0731"
context_length = "2mb"
"#,
        )
        .expect("an 'mb'-suffixed context_length parses");
        assert_eq!(megabytes.context_length, Some(2 * 1024 * 1024));
    }

    #[test]
    fn engine_config_context_length_rejects_an_unparsable_string() {
        let error = toml::from_str::<EngineConfig>(
            r#"
provider_connection = "ollama"
model = "local"
context_length = "not-a-size"
"#,
        )
        .expect_err("a non-numeric, non-suffixed context_length must be rejected");
        assert!(
            error.to_string().contains("invalid context_length"),
            "the error must name the offending value: {error}"
        );
    }

    #[test]
    fn engine_config_reasoning_seeds_the_initial_reasoning_effort() {
        let config: EngineConfig = toml::from_str(
            r#"
provider_connection = "openrouter"
model = "deepseek/deepseek-v4-flash-0731"
reasoning = "xhigh"
"#,
        )
        .expect("a reasoning field parses");
        assert_eq!(config.reasoning.as_deref(), Some("xhigh"));

        let runtime_config = EngineRuntimeConfig::from(&config);
        assert_eq!(
            runtime_config.reasoning_effort.as_deref(),
            Some("xhigh"),
            "the initial Engine runtime reasoning effort comes from the engine config"
        );
    }

    #[test]
    fn engine_config_reasoning_defaults_to_none() {
        let config: EngineConfig = toml::from_str(
            r#"
provider_connection = "ollama"
model = "local"
"#,
        )
        .expect("an engine config without 'reasoning' still parses");
        assert_eq!(config.reasoning, None);
    }

    #[test]
    fn effective_context_length_prefers_the_configured_toml_value() {
        let config = EngineConfig {
            context_length: Some(32_000),

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
            ..make_engine_config()
        };
        assert_eq!(config.effective_context_length(Some(999_999)), 32_000);
    }

    #[test]
    fn effective_context_length_falls_back_to_the_catalogs_context_length() {
        let config = EngineConfig {
            context_length: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
            ..make_engine_config()
        };
        assert_eq!(config.effective_context_length(Some(64_000)), 64_000);
    }

    #[test]
    fn effective_context_length_falls_back_to_the_built_in_default() {
        let config = EngineConfig {
            context_length: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
            ..make_engine_config()
        };
        assert_eq!(
            config.effective_context_length(None),
            DEFAULT_CONTEXT_LENGTH
        );
    }

    #[test]
    fn compact_target_ratio_defaults_to_half_the_window_and_is_configurable_per_engine() {
        let default_config: EngineConfig =
            toml::from_str("provider = \"mock\"\nmodel = \"test-model\"\n")
                .expect("a config without compact_target_ratio parses");
        assert_eq!(
            default_config.compact_target_ratio,
            DEFAULT_COMPACT_TARGET_RATIO
        );
        assert_eq!(default_config.compact_target_ratio, 0.5);

        let configured: EngineConfig = toml::from_str(
            "provider = \"mock\"\nmodel = \"test-model\"\ncompact_target_ratio = 0.25\n",
        )
        .expect("an explicit compact_target_ratio parses");
        assert_eq!(configured.compact_target_ratio, 0.25);
    }

    #[test]
    fn the_compaction_budget_is_read_off_the_active_engine() {
        let engine = CatalogEngine::new("mock", "test-model", &[]);

        let budget = crate::agent::compaction::CompactionBudget::from_engine(&engine)
            .expect("a non-zero window always yields a budget");
        assert_eq!(
            budget.context_length, DEFAULT_CONTEXT_LENGTH,
            "no profile context_length and no catalog selection fall back to \
             the built-in window"
        );
        assert_eq!(budget.target_ratio, 0.5);
        assert_eq!(
            budget.soft_threshold,
            crate::agent::compaction::DEFAULT_SOFT_COMPACTION_THRESHOLD
        );
        assert_eq!(
            budget.hard_threshold,
            crate::agent::compaction::DEFAULT_HARD_COMPACTION_THRESHOLD
        );
        assert_eq!(budget.target_tokens(), DEFAULT_CONTEXT_LENGTH / 2);
    }

    #[test]
    fn a_models_switch_adopts_the_catalog_context_length_only_when_unconfigured() {
        let catalog_model = ModelInfo {
            id: "catalog-model".to_string(),
            context_length: Some(8_192),
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities: None,
        };
        let mut selection = ProviderModelOption {
            provider_connection: "mock".to_string(),
            model: catalog_model,
            reasoning_effort: None,
        };

        let mut unconfigured = make_engine_config();
        unconfigured.context_length = None;
        let provider_connection = unconfigured.provider_connection.clone();
        let engine = EngineImpl::new(
            Arc::new(OpenAiCompatibleProvider::new(
                "test".to_string(),
                "http://localhost:1234/v1".to_string(),
                "provider-model".to_string(),
                None,
            )),
            unconfigured,
        );
        assert_eq!(
            engine.effective_context_length(),
            DEFAULT_CONTEXT_LENGTH,
            "no profile value and no catalog value yet"
        );

        selection.provider_connection = provider_connection.clone();
        engine
            .switch_model_option(&selection)
            .expect("model selection succeeds");
        assert_eq!(
            engine.effective_context_length(),
            8_192,
            "the catalog length adopted by the switch is what budgets measure"
        );

        let mut pinned = make_engine_config();
        pinned.context_length = Some(4_096);
        selection.provider_connection = pinned.provider_connection.clone();
        let engine = EngineImpl::new(
            Arc::new(OpenAiCompatibleProvider::new(
                "test".to_string(),
                "http://localhost:1234/v1".to_string(),
                "provider-model".to_string(),
                None,
            )),
            pinned,
        );
        engine
            .switch_model_option(&selection)
            .expect("model selection succeeds");
        assert_eq!(
            engine.effective_context_length(),
            4_096,
            "an explicit profile context_length outranks the catalog"
        );
    }

    #[test]
    fn engine_and_openai_compatible_provider_share_runtime_configuration() {
        let provider = Arc::new(OpenAiCompatibleProvider::new(
            "test".to_string(),
            "http://localhost:1234/v1".to_string(),
            "provider-model".to_string(),
            None,
        ));
        let config = EngineConfig {
            provider_connection: "test".to_string(),
            model: "engine-model".to_string(),
            temperature: Some(0.2),
            context_length: Some(32_000),
            api_base_url: Some("http://localhost:1234/v1".to_string()),
            api_key: None,
            reasoning: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };
        let engine = EngineImpl::new(Arc::clone(&provider) as Arc<dyn Provider>, config);

        assert_eq!(engine.config().model, "engine-model");
        assert_eq!(engine.config().temperature, Some(0.2));
        assert_eq!(engine.config().context_length, Some(32_000));

        provider
            .set_model("updated-model")
            .expect("the provider supports model switching");
        assert_eq!(engine.config().model, "updated-model");
    }

    #[test]
    fn selecting_a_model_carries_its_input_modalities_into_runtime_state() {
        let provider = Arc::new(OpenAiCompatibleProvider::new(
            "test".to_string(),
            "http://localhost:1234/v1".to_string(),
            "text-model".to_string(),
            None,
        ));
        let engine = EngineImpl::new(provider, make_engine_config());
        assert_eq!(engine.input_modalities(), None);

        engine
            .switch_model_option(&ProviderModelOption {
                provider_connection: "mock".to_string(),
                model: ModelInfo {
                    id: "vision-model".to_string(),
                    context_length: None,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: Some(vec![InputModality::Image]),
                },
                reasoning_effort: None,
            })
            .expect("model selection succeeds");
        assert_eq!(engine.input_modalities(), Some(vec![InputModality::Image]));

        engine
            .switch_model_option(&ProviderModelOption {
                provider_connection: "mock".to_string(),
                model: ModelInfo {
                    id: "text-model".to_string(),
                    context_length: None,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: Some(vec![]),
                },
                reasoning_effort: None,
            })
            .expect("text-only model selection succeeds");
        assert_eq!(engine.input_modalities(), Some(vec![]));
    }

    #[tokio::test]
    async fn engine_impl_chat_delegates_to_mock_provider() {
        let provider = Arc::new(MockProvider::new(
            vec!["hello".to_string()],
            Duration::from_millis(0),
        ));
        let engine = EngineImpl::new(provider, make_engine_config());
        let cancel = CancellationToken::new();

        let result = engine.chat(vec![], vec![], cancel).await;
        assert!(
            result.is_ok(),
            "chat should delegate to MockProvider successfully"
        );
    }

    #[test]
    fn engine_router_logs_the_selected_named_engine_for_chat_requests() {
        let log = crate::test_support::capture_logs(async {
            let engine: Arc<dyn Engine> =
                Arc::new(CatalogEngine::new("ollama-local", "local-model", &[]));
            let router = EngineRouter::new(HashMap::from([("local".to_string(), engine)]), "local")
                .expect("router construction");

            let _request = router.chat(vec![], vec![], CancellationToken::new()).await;
        });

        assert!(log.contains("named engine selected for chat request"));
        assert!(log.contains("engine=local"));
        assert!(log.contains("provider_connection=ollama-local"));
        assert!(log.contains("model=local-model"));
        assert!(
            log.contains("reasoning_effort=default"),
            "the selection log must carry the thinking mode: {log}"
        );
        assert!(
            log.contains("input_modalities=unknown"),
            "the selection log must carry the modalities: {log}"
        );
    }

    #[test]
    fn engine_router_logs_thinking_and_modalities_after_hydration() {
        let log = crate::test_support::capture_logs(async {
            let provider = Arc::new(CatalogProvider::new(vec![model_info_with_modalities(
                "local-model",
                Some(vec![InputModality::Image]),
            )]));
            let mut config = make_engine_config();
            config.provider_connection = "ollama-local".to_string();
            config.model = "local-model".to_string();
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, config));
            let router = EngineRouter::new(HashMap::from([("local".to_string(), engine)]), "local")
                .expect("router construction");

            let _request = router.chat(vec![], vec![], CancellationToken::new()).await;
        });

        assert!(log.contains("named engine selected for chat request"));
        assert!(
            log.contains("input_modalities=image"),
            "hydration must complete before the selection is logged: {log}"
        );
        assert!(log.contains("active model input modalities discovered"));
    }

    #[test]
    fn engine_impl_logs_when_the_model_catalog_is_queried() {
        let log = crate::test_support::capture_logs(async {
            let provider = Arc::new(CatalogProvider::new(vec![]));
            let engine = EngineImpl::new(provider, make_engine_config());
            let _models = engine.list_models().await.expect("catalog");
        });

        assert!(
            log.contains("model catalog queried"),
            "the catalog fetch must be logged: {log}"
        );
        assert!(log.contains("provider_connection=mock"));
    }

    #[test]
    fn engine_impl_hydrates_catalog_metadata_before_the_first_chat() {
        crate::test_support::run_locked(async {
            let provider = Arc::new(CatalogProvider::new(vec![model_info_with_modalities(
                "test-model",
                Some(vec![InputModality::Image]),
            )]));
            let engine = EngineImpl::new(
                Arc::clone(&provider) as Arc<dyn Provider>,
                make_engine_config(),
            );
            assert_eq!(engine.input_modalities(), None);

            // The stub's chat fails; hydration must have run regardless.
            let _ = engine.chat(vec![], vec![], CancellationToken::new()).await;

            assert_eq!(engine.input_modalities(), Some(vec![InputModality::Image]));
            assert_eq!(provider.list_models_calls(), 1);
        });
    }

    #[test]
    fn engine_impl_hydration_fetches_the_catalog_at_most_once() {
        crate::test_support::run_locked(async {
            let provider = Arc::new(CatalogProvider::new(vec![model_info_with_modalities(
                "test-model",
                Some(vec![InputModality::Image]),
            )]));
            let engine =
                EngineImpl::new(provider.clone() as Arc<dyn Provider>, make_engine_config());

            engine.hydrate_catalog_metadata().await;
            engine.hydrate_catalog_metadata().await;

            assert_eq!(engine.input_modalities(), Some(vec![InputModality::Image]));
            assert_eq!(provider.list_models_calls(), 1);
        });
    }

    #[test]
    fn engine_impl_hydration_resolves_catalog_context_length_unless_profile_overrides_it() {
        crate::test_support::run_locked(async {
            for configured_context_length in [None, Some(32_000)] {
                let mut model = model_info_with_modalities("test-model", None);
                model.context_length = Some(8_000);
                let provider = Arc::new(CatalogProvider::new(vec![model]));
                let mut config = make_engine_config();
                config.context_length = configured_context_length;
                let engine = EngineImpl::new(provider, config);
                engine.hydrate_catalog_metadata().await;
                assert_eq!(
                    engine.effective_context_length(),
                    configured_context_length.unwrap_or(8_000),
                );
            }
        });
    }

    #[test]
    fn engine_impl_hydration_skips_when_modalities_are_already_known() {
        crate::test_support::run_locked(async {
            let provider = Arc::new(CatalogProvider::new(vec![model_info_with_modalities(
                "test-model",
                Some(vec![InputModality::File]),
            )]));
            // The catalog stub advertises no reasoning efforts at all, so a
            // validation pass would drop this effort. A `/models` selection
            // supplied both the modalities and the (validated) effort, so
            // the skip path must leave it untouched without fetching.
            let engine =
                engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");
            engine.set_input_modalities(Some(vec![InputModality::File]));

            engine.hydrate_catalog_metadata().await;

            assert_eq!(engine.input_modalities(), Some(vec![InputModality::File]));
            assert_eq!(
                provider.list_models_calls(),
                0,
                "a /models-provided state must not trigger a catalog fetch"
            );
            assert_eq!(
                engine.reasoning_effort().as_deref(),
                Some("max"),
                "the skip path must not re-validate a switch-validated effort"
            );
        });
    }

    #[test]
    fn engine_impl_hydration_failure_keeps_modalities_unknown_without_retries() {
        let log = crate::test_support::capture_logs(async {
            let provider = Arc::new(CatalogProvider::failing());
            let engine =
                EngineImpl::new(provider.clone() as Arc<dyn Provider>, make_engine_config());

            engine.hydrate_catalog_metadata().await;
            engine.hydrate_catalog_metadata().await;

            assert_eq!(engine.input_modalities(), None);
            assert_eq!(
                provider.list_models_calls(),
                1,
                "a failed attempt must not be retried on every request"
            );
        });
        assert!(
            log.contains("failed to discover active model input modalities"),
            "the failure must be logged: {log}"
        );
    }

    #[test]
    fn engine_impl_keeps_a_profile_reasoning_effort_the_model_advertises() {
        crate::test_support::run_locked(async {
            let provider = Arc::new(CatalogProvider::new(vec![
                model_info_with_reasoning_efforts(
                    "test-model",
                    Some(vec!["low".to_string(), "max".to_string()]),
                ),
            ]));
            let engine =
                engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");

            engine.hydrate_catalog_metadata().await;

            assert_eq!(
                engine.reasoning_effort().as_deref(),
                Some("max"),
                "an advertised effort must survive validation untouched"
            );
            assert!(
                engine.take_pending_notices().is_empty(),
                "a kept effort must not produce a notice"
            );
            assert_eq!(provider.list_models_calls(), 1);
        });
    }

    #[test]
    fn engine_impl_drops_a_profile_reasoning_effort_the_model_does_not_advertise() {
        let log = crate::test_support::capture_logs(async {
            let provider = Arc::new(CatalogProvider::new(vec![
                model_info_with_reasoning_efforts("test-model", Some(vec!["low".to_string()])),
            ]));
            let engine =
                engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");

            engine.hydrate_catalog_metadata().await;

            assert_eq!(
                engine.reasoning_effort(),
                None,
                "an effort outside the advertised list must fall back to the provider default"
            );
            assert_eq!(
                engine.take_pending_notices(),
                vec![
                    "Model mock/test-model does not advertise reasoning effort \"max\"; using the provider default."
                        .to_string()
                ],
                "the drop must be queued as a one-shot notice"
            );
            assert!(
                engine.take_pending_notices().is_empty(),
                "a notice must be handed out only once"
            );
        });
        assert!(
            log.contains("profile reasoning effort is not advertised by the model"),
            "the drop must be logged as a warning: {log}"
        );
        assert!(
            log.contains("model=test-model") && log.contains("reasoning_effort=max"),
            "the warning must name the model and the dropped effort: {log}"
        );
    }

    #[test]
    fn engine_impl_drops_a_profile_reasoning_effort_when_the_model_advertises_no_alternatives() {
        let log = crate::test_support::capture_logs(async {
            for advertised_efforts in [Some(Vec::new()), None::<Vec<String>>] {
                let provider = Arc::new(CatalogProvider::new(vec![
                    model_info_with_reasoning_efforts("test-model", advertised_efforts),
                ]));
                let engine =
                    engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");

                engine.hydrate_catalog_metadata().await;

                assert_eq!(
                    engine.reasoning_effort(),
                    None,
                    "a model without selectable alternatives must not keep an explicit effort"
                );
            }
        });
        assert!(
            log.contains("profile reasoning effort is not advertised by the model"),
            "both drops must be logged: {log}"
        );
    }

    #[test]
    fn engine_impl_keeps_a_profile_reasoning_effort_when_the_model_is_absent_from_the_catalog() {
        let log = crate::test_support::capture_logs(async {
            let provider = Arc::new(CatalogProvider::new(vec![
                model_info_with_reasoning_efforts("other-model", Some(vec!["low".to_string()])),
            ]));
            let engine =
                engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");

            engine.hydrate_catalog_metadata().await;

            assert_eq!(
                engine.reasoning_effort().as_deref(),
                Some("max"),
                "an unlisted model cannot disprove the effort, so it must be kept"
            );
        });
        assert!(
            log.contains("the catalog does not list the active model"),
            "the unverifiable case must be logged: {log}"
        );
    }

    #[test]
    fn engine_impl_keeps_a_profile_reasoning_effort_when_the_catalog_fetch_fails() {
        let log = crate::test_support::capture_logs(async {
            let provider = Arc::new(CatalogProvider::failing());
            let engine =
                engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");

            engine.hydrate_catalog_metadata().await;
            engine.hydrate_catalog_metadata().await;

            assert_eq!(
                engine.reasoning_effort().as_deref(),
                Some("max"),
                "a failed fetch cannot validate, so the effort must be kept"
            );
            assert_eq!(
                provider.list_models_calls(),
                1,
                "a failed attempt must not be retried on every request"
            );
        });
        assert!(
            log.contains("failed to discover active model input modalities"),
            "the failure must be logged: {log}"
        );
    }

    #[test]
    fn switch_model_option_rejects_a_reasoning_effort_the_model_does_not_advertise() {
        let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
        let engine = EngineImpl::new(provider, make_engine_config());

        let error = engine
            .switch_model_option(&ProviderModelOption {
                provider_connection: "mock".to_string(),
                model: model_info_with_reasoning_efforts(
                    "test-model",
                    Some(vec!["low".to_string()]),
                ),
                reasoning_effort: Some("max".to_string()),
            })
            .expect_err("an unadvertised effort must be rejected at switch time");
        assert!(
            error
                .to_string()
                .contains("does not advertise reasoning effort 'max'"),
            "the error must name the rejected effort: {error}"
        );
    }

    #[test]
    fn engine_router_drains_pending_notices_from_its_engines() {
        crate::test_support::run_locked(async {
            let provider = Arc::new(CatalogProvider::new(vec![
                model_info_with_reasoning_efforts("test-model", Some(vec!["low".to_string()])),
            ]));
            let engine = engine_with_profile_reasoning(provider as Arc<dyn Provider>, "max");
            engine.hydrate_catalog_metadata().await;
            let router = EngineRouter::new(
                HashMap::from([("mock".to_string(), Arc::new(engine) as Arc<dyn Engine>)]),
                "mock",
            )
            .expect("router construction");

            assert_eq!(
                router.take_pending_notices().len(),
                1,
                "the router must surface the notice its engine queued"
            );
            assert!(
                router.take_pending_notices().is_empty(),
                "draining must be one-shot through the router too"
            );
        });
    }

    #[tokio::test]
    async fn engine_router_lists_all_providers_and_switches_the_active_engine() {
        let local: Arc<dyn Engine> =
            Arc::new(CatalogEngine::new("ollama", "local", &["local", "other"]));
        let hosted: Arc<dyn Engine> =
            Arc::new(CatalogEngine::new("openrouter", "hosted", &["hosted"]));
        let router = EngineRouter::new(
            HashMap::from([
                ("default".to_string(), local),
                ("openrouter".to_string(), hosted),
            ]),
            "default",
        )
        .expect("router construction");

        let options = router
            .list_model_options()
            .await
            .expect("all model lists should load");
        assert_eq!(
            options
                .iter()
                .map(|option| { format!("{}/{}", option.provider_connection, option.model.id) })
                .collect::<Vec<_>>(),
            vec![
                "ollama/local".to_string(),
                "ollama/other".to_string(),
                "openrouter/hosted".to_string()
            ]
        );

        let hosted_option = options
            .iter()
            .find(|option| option.provider_connection == "openrouter")
            .expect("hosted option");
        router
            .switch_model_option(hosted_option)
            .expect("provider/model switch");
        assert_eq!(router.config().provider_connection, "openrouter");
        assert_eq!(router.config().model, "hosted");
    }

    #[tokio::test]
    async fn engine_router_lists_each_named_provider_connection_once() {
        let local_default: Arc<dyn Engine> =
            Arc::new(CatalogEngine::new("ollama-local", "local", &["local"]));
        let local_alternate: Arc<dyn Engine> = Arc::new(CatalogEngine::new(
            "ollama-local",
            "alternate",
            &["alternate"],
        ));
        let cloud: Arc<dyn Engine> =
            Arc::new(CatalogEngine::new("ollama-cloud", "cloud", &["cloud"]));
        let router = EngineRouter::new(
            HashMap::from([
                ("local".to_string(), local_default),
                ("alternate".to_string(), local_alternate),
                ("cloud".to_string(), cloud),
            ]),
            "local",
        )
        .expect("router construction");

        let options = router
            .list_model_options()
            .await
            .expect("provider model lists should load");

        assert_eq!(
            options
                .iter()
                .map(|option| { format!("{}/{}", option.provider_connection, option.model.id) })
                .collect::<Vec<_>>(),
            vec![
                "ollama-cloud/cloud".to_string(),
                "ollama-local/local".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn engine_router_keeps_successful_models_when_one_provider_fails() {
        let failing: Arc<dyn Engine> =
            Arc::new(CatalogEngine::failing("ollama", "server unavailable"));
        let working: Arc<dyn Engine> =
            Arc::new(CatalogEngine::new("openrouter", "hosted", &["hosted"]));
        let router = EngineRouter::new(
            HashMap::from([
                ("default".to_string(), failing),
                ("openrouter".to_string(), working),
            ]),
            "default",
        )
        .expect("router construction");

        let options = router
            .list_model_options()
            .await
            .expect("one provider succeeding should be enough");

        assert_eq!(options.len(), 1);
        assert_eq!(options[0].provider_connection, "openrouter");
        assert_eq!(options[0].model.id, "hosted");
    }

    #[tokio::test]
    async fn engine_router_reports_an_aggregate_error_when_all_providers_fail() {
        let router = EngineRouter::new(
            HashMap::from([
                (
                    "local".to_string(),
                    Arc::new(CatalogEngine::failing("ollama", "server unavailable"))
                        as Arc<dyn Engine>,
                ),
                (
                    "hosted".to_string(),
                    Arc::new(CatalogEngine::failing("openrouter", "unauthorized"))
                        as Arc<dyn Engine>,
                ),
            ]),
            "local",
        )
        .expect("router construction");

        let error = router
            .list_model_options()
            .await
            .expect_err("all provider failures should be reported");
        assert_eq!(
            error.to_string(),
            "no configured provider connection returned models: ollama: server unavailable; openrouter: unauthorized"
        );
    }
}
