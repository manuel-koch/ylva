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
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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
/// Later phases add file layers and readers on top without displacing that
/// SSOT:
///
/// - **Phase 8 (Full Configuration & Persona System)** adds the three-layer
///   project/home/embedded fallback chain and persona/`agents/*.md` sourcing,
///   which populate this same struct through additional layers.
/// - **Phase 9 (Context Compaction & Token Budgeting)** reads
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
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default = "default_context_length")]
    pub context_length: usize,
    #[serde(default)]
    pub api_base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// The provider and model names shown for the conversation currently on
/// screen.
///
/// This intentionally contains only display metadata. Runtime request
/// settings remain owned by [`EngineRuntimeConfig`], while the immutable
/// connection details stay in [`EngineConfig`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineDisplayInfo {
    pub provider_connection: String,
    pub model: String,
}

impl From<&EngineConfig> for EngineDisplayInfo {
    fn from(config: &EngineConfig) -> Self {
        EngineDisplayInfo {
            provider_connection: config.provider_connection.clone(),
            model: config.model.clone(),
        }
    }
}

fn default_context_length() -> usize {
    DEFAULT_CONTEXT_LENGTH
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
            context_length: config.context_length,
            reasoning_effort: None,
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

    fn active_engine(&self) -> Arc<dyn Engine> {
        let active_engine = self
            .active_engine
            .read()
            .expect("the active engine lock is poisoned");
        self.engines
            .get(active_engine.as_str())
            .expect("the active engine must remain configured")
            .clone()
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
        let engine = self.active_engine();
        Box::pin(async move { engine.chat(messages, tools, cancel).await })
    }

    fn config(&self) -> EngineConfig {
        self.active_engine().config()
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
        config.context_length = runtime_config.context_length;
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
            context_length: DEFAULT_CONTEXT_LENGTH,
            api_base_url: None,
            api_key: None,
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
                    context_length: DEFAULT_CONTEXT_LENGTH,
                    api_base_url: Some(format!("https://{provider}.test/v1")),
                    api_key: None,
                }),
                models: models
                    .iter()
                    .map(|id| ModelInfo {
                        id: (*id).to_string(),
                        name: None,
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
        assert_eq!(engine.config().context_length, DEFAULT_CONTEXT_LENGTH);
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
            context_length: 32_000,
            api_base_url: Some("http://localhost:1234/v1".to_string()),
            api_key: None,
        };
        let engine = EngineImpl::new(Arc::clone(&provider) as Arc<dyn Provider>, config);

        assert_eq!(engine.config().model, "engine-model");
        assert_eq!(engine.config().temperature, Some(0.2));
        assert_eq!(engine.config().context_length, 32_000);

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
