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
use std::sync::{Arc, RwLock};

use futures::future::{join_all, BoxFuture};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::agent::{Message, StreamEvent};
use crate::llm::{ModelInfo, Provider, ToolSchema};

/// Default context-length used when no explicit value is configured.
pub const DEFAULT_CONTEXT_LENGTH: usize = 128_000;

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
/// - **Phase 11 (Context Compaction & Token Budgeting)** reads
///   `context_length` to drive `maybe_compact`'s budget; it adds no fields
///   here.
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
    /// own default. Applied once at construction — later changes go through
    /// `/models`, which validates the effort against the model's advertised
    /// options before switching.
    #[serde(default)]
    pub reasoning: Option<String>,
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
/// This intentionally contains only display metadata. Runtime request
/// settings remain owned by [`EngineRuntimeConfig`], while the immutable
/// connection details stay in [`EngineConfig`]. The reasoning effort is
/// copied from the runtime snapshot for display only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineDisplayInfo {
    /// The profile key under `[engines.*]`, when the runtime has one.
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

    /// A snapshot of the engine's configuration.
    ///
    /// Returned by value rather than by reference because `/models` mutates
    /// the active model through interior mutability: a borrowed accessor
    /// would either hand out a stale `model` or pin a lock guard across
    /// caller code.
    fn config(&self) -> EngineConfig;

    /// Return the active profile engine name when this runtime routes among
    /// named engines. A directly constructed engine has no named key.
    fn active_engine_name(&self) -> Option<String> {
        None
    }

    /// List the models the underlying provider offers.
    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>>;

    /// List models from every configured provider connection represented by
    /// this runtime.
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
            let supported_efforts = option
                .model
                .reasoning_efforts
                .as_ref()
                .is_some_and(|efforts| efforts.iter().any(|effort| effort == reasoning_effort));
            anyhow::ensure!(
                supported_efforts,
                "model '{}' does not advertise reasoning effort '{}'",
                option.model.id,
                reasoning_effort
            );
        }
        self.set_reasoning_effort(option.reasoning_effort.as_deref())?;
        self.switch_model(&option.model.id)
    }
}

/// Routes chat and model operations to one active engine from a configured
/// collection.
///
/// The router keeps connection settings in the underlying engines. Switching
/// provider connections only changes which already-constructed engine receives
/// future requests, while model changes continue to use each connection's existing
/// runtime configuration handle.
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
        let (engine_name, engine) = self.active_engine_entry();
        let config = engine.config();
        tracing::debug!(
            engine = %engine_name,
            provider_connection = %config.provider_connection,
            model = %config.model,
            "named engine selected for chat request"
        );
        let span = tracing::debug_span!(
            "engine_request",
            engine = %engine_name,
            provider_connection = %config.provider_connection,
        );
        Box::pin(async move { engine.chat(messages, tools, cancel).await }.instrument(span))
    }

    fn config(&self) -> EngineConfig {
        self.active_engine().config()
    }

    fn active_engine_name(&self) -> Option<String> {
        Some(self.active_engine_entry().0)
    }

    fn reasoning_effort(&self) -> Option<String> {
        self.active_engine().reasoning_effort()
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
    /// Shared with providers that support runtime engine settings.
    runtime_config: EngineRuntimeConfigHandle,
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
        self.provider.chat(messages, tools, cancel)
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
        self.provider.list_models()
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
                }),
                models: models
                    .iter()
                    .map(|id| ModelInfo {
                        id: (*id).to_string(),
                        context_length: None,
                        reasoning_efforts: None,
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
            "the initial runtime reasoning effort comes from the engine config"
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
            ..make_engine_config()
        };
        assert_eq!(config.effective_context_length(Some(999_999)), 32_000);
    }

    #[test]
    fn effective_context_length_falls_back_to_the_catalogs_context_length() {
        let config = EngineConfig {
            context_length: None,
            ..make_engine_config()
        };
        assert_eq!(config.effective_context_length(Some(64_000)), 64_000);
    }

    #[test]
    fn effective_context_length_falls_back_to_the_built_in_default() {
        let config = EngineConfig {
            context_length: None,
            ..make_engine_config()
        };
        assert_eq!(
            config.effective_context_length(None),
            DEFAULT_CONTEXT_LENGTH
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

            let _request = router.chat(vec![], vec![], CancellationToken::new());
        });

        assert!(log.contains("named engine selected for chat request"));
        assert!(log.contains("engine=local"));
        assert!(log.contains("provider_connection=ollama-local"));
        assert!(log.contains("model=local-model"));
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
