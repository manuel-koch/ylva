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

//! Named-engine routing and provider-connection target resolution.

use super::selection::validate_named_engine_option;
use super::{
    Engine, EngineConfig, ModelSelection, ModelSelectionTarget, NamedEngineInfo,
    NamedEnginePublication, ProviderModelOption,
};
use crate::agent::{Message, StreamEvent};
use crate::llm::{InputModality, ModelInfo, ToolSchema};
use futures::future::{join_all, BoxFuture};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

/// Routes chat and model operations to one active engine from a configured
/// collection.
///
/// The router keeps connection settings in the underlying engines. Switching
/// provider connections only changes which already-constructed engine receives
/// future requests, while model changes continue to use each connection's existing
/// Engine runtime configuration handle.
pub struct EngineRouter {
    engines: RwLock<HashMap<String, Arc<dyn Engine>>>,
    /// While set and only one engine exists, the router presents itself like
    /// that bare engine: no named engines and no active engine name. This keeps
    /// the picker of a single-engine profile showing catalog rows only until
    /// a second engine is registered.
    transparent_single_engine: bool,
    active_engine: RwLock<String>,
    catalog_cache: CatalogCache,
    catalog_cache_lifetime: Duration,
}

/// Fetched model catalogs by provider connection name.
type CatalogCache = Arc<Mutex<HashMap<String, (Instant, Vec<ModelInfo>)>>>;

/// How long a provider connection's model catalog is reused by `/models`.
const CATALOG_CACHE_LIFETIME: Duration = Duration::from_secs(30 * 60);

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
            engines: RwLock::new(engines),
            transparent_single_engine: false,
            active_engine: RwLock::new(active_engine.to_string()),
            catalog_cache: Arc::new(Mutex::new(HashMap::new())),
            catalog_cache_lifetime: CATALOG_CACHE_LIFETIME,
        })
    }

    /// Hide a sole engine's identity until a second engine is registered.
    pub fn with_transparent_single_engine(mut self) -> Self {
        self.transparent_single_engine = true;
        self
    }

    fn engine_snapshot(&self) -> HashMap<String, Arc<dyn Engine>> {
        self.engines
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn engine_named(&self, name: &str) -> Option<Arc<dyn Engine>> {
        self.engines
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    fn hides_engine_identity(&self) -> bool {
        self.transparent_single_engine && self.engine_snapshot().len() <= 1
    }

    #[cfg(test)]
    pub(crate) fn with_catalog_cache_lifetime(mut self, lifetime: Duration) -> Self {
        self.catalog_cache_lifetime = lifetime;
        self
    }

    fn active_engine_entry(&self) -> (String, Arc<dyn Engine>) {
        let active_engine = self
            .active_engine
            .read()
            .expect("the active engine lock is poisoned");
        let engine_name = active_engine.clone();
        let engine = self
            .engine_named(active_engine.as_str())
            .expect("the active engine must remain configured");
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
        let mut engines: Vec<_> = self.engine_snapshot().into_iter().collect();
        engines.sort_by(|left, right| left.0.cmp(&right.0));

        let mut provider_connections: HashMap<String, (String, Arc<dyn Engine>)> = HashMap::new();
        for (engine_name, engine) in engines {
            let provider_connection = engine.config().provider_connection;
            provider_connections
                .entry(provider_connection)
                .or_insert((engine_name, engine));
        }
        if let Some(active_engine) = self.engine_named(&active_engine_name) {
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

    fn provider_engine_for_active(
        &self,
        connection: &str,
        active: &str,
    ) -> anyhow::Result<(String, Arc<dyn Engine>)> {
        if let Some(engine) = self
            .engine_named(active)
            .filter(|engine| engine.config().provider_connection == connection)
        {
            return Ok((active.to_string(), engine));
        }
        self.engine_snapshot()
            .into_iter()
            .filter(|(_, engine)| engine.config().provider_connection == connection)
            .min_by(|left, right| left.0.cmp(&right.0))
            .ok_or_else(|| anyhow::anyhow!("provider connection '{connection}' is not configured"))
    }

    fn provider_engine_for(
        &self,
        provider_connection: &str,
    ) -> anyhow::Result<(String, Arc<dyn Engine>)> {
        self.provider_engines()
            .into_iter()
            .find(|(connection, _, _)| connection == provider_connection)
            .map(|(_, engine_name, engine)| (engine_name, engine))
            .ok_or_else(|| {
                anyhow::anyhow!("provider connection '{provider_connection}' is not configured")
            })
    }
}

impl Engine for EngineRouter {
    fn runtime_snapshot(&self) -> Option<super::EngineRuntimeConfig> {
        self.active_engine().runtime_snapshot()
    }
    fn configured_engine_config(&self) -> Option<EngineConfig> {
        self.active_engine().configured_engine_config()
    }
    fn reasoning_effort_policy(&self) -> Arc<dyn crate::llm::ReasoningEffortPolicy> {
        self.active_engine().reasoning_effort_policy()
    }
    fn effective_reasoning_provenance(&self) -> crate::llm::ReasoningEffortProvenance {
        self.active_engine().effective_reasoning_provenance()
    }
    fn prepare_request_notices(&self) -> anyhow::Result<()> {
        self.active_engine().prepare_request_notices()
    }
    fn unverified_reasoning_choices(
        &self,
        provider_connection: &str,
        model: Option<&ModelInfo>,
    ) -> Vec<String> {
        self.provider_engine_for(provider_connection)
            .map(|(_, engine)| engine.unverified_reasoning_choices(provider_connection, model))
            .unwrap_or_default()
    }
    fn selection_reasoning_options(
        &self,
        selection: &super::ModelSelection,
    ) -> anyhow::Result<crate::llm::ReasoningEffortOptions> {
        let engine = match selection {
            ModelSelection::NamedEngine { engine_name, .. } => self
                .engine_named(engine_name)
                .ok_or_else(|| anyhow::anyhow!("named engine '{engine_name}' is not configured"))?,
            ModelSelection::ProviderModel(option) => {
                self.provider_engine_for(&option.provider_connection)?.1
            }
        };
        engine.selection_reasoning_options(selection)
    }

    fn resolve_selection_reasoning(
        &self,
        selection: &ModelSelection,
    ) -> anyhow::Result<crate::llm::ReasoningEffortProvenance> {
        let engine = match selection {
            ModelSelection::NamedEngine { engine_name, .. } => self
                .engine_named(engine_name)
                .ok_or_else(|| anyhow::anyhow!("named engine '{engine_name}' is not configured"))?,
            ModelSelection::ProviderModel(option) => {
                self.provider_engine_for(&option.provider_connection)?.1
            }
        };
        engine.resolve_selection_reasoning(selection)
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            let (engine_name, engine) = self.active_engine_entry();
            engine.ensure_active_model_allowed()?;
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
            engine.ensure_active_model_allowed()?;
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
            engine.ensure_active_model_allowed()?;
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
            engine.ensure_active_model_allowed()?;
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

    fn ensure_model_allowed(&self, model_id: &str) -> anyhow::Result<()> {
        self.active_engine().ensure_model_allowed(model_id)
    }

    fn favorite_models(&self) -> Vec<(String, Vec<String>)> {
        // `provider_engines` yields one representative engine per connection,
        // sorted by connection name.
        self.provider_engines()
            .into_iter()
            .flat_map(|(_provider_connection, _engine_name, engine)| engine.favorite_models())
            .collect()
    }

    fn record_favorite_membership(
        &self,
        provider_connection: &str,
        model_id: &str,
        favorite: bool,
    ) {
        // Every named engine on the connection keeps its own copy.
        for engine in self.engine_snapshot().values() {
            engine.record_favorite_membership(provider_connection, model_id, favorite);
        }
    }

    fn ensure_provider_model_allowed(
        &self,
        provider_connection: &str,
        model_id: &str,
    ) -> anyhow::Result<()> {
        let (_engine_name, engine) = self.provider_engine_for(provider_connection)?;
        engine.ensure_model_allowed(model_id)
    }

    fn effective_context_length(&self) -> usize {
        self.active_engine().effective_context_length()
    }

    fn effective_max_output_tokens(&self) -> Option<usize> {
        self.active_engine().effective_max_output_tokens()
    }

    fn with_output_limit_cap(
        &self,
        maximum_output_tokens: usize,
    ) -> anyhow::Result<Arc<dyn Engine>> {
        self.active_engine()
            .with_output_limit_cap(maximum_output_tokens)
    }

    fn with_minimum_output_limit(
        &self,
        minimum_output_tokens: usize,
    ) -> anyhow::Result<Arc<dyn Engine>> {
        self.active_engine()
            .with_minimum_output_limit(minimum_output_tokens)
    }

    fn active_engine_name(&self) -> Option<String> {
        if self.hides_engine_identity() {
            return None;
        }
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
        self.engine_snapshot()
            .values()
            .flat_map(|engine| engine.take_pending_notices())
            .collect()
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        let engine = self.active_engine();
        Box::pin(async move { engine.list_models().await })
    }

    fn list_model_options(&self) -> BoxFuture<'_, anyhow::Result<Vec<ProviderModelOption>>> {
        let result = self.list_model_options_with_failures();
        Box::pin(async move { result.await.map(|(options, _failures)| options) })
    }

    fn list_model_options_with_failures(
        &self,
    ) -> BoxFuture<'_, anyhow::Result<(Vec<ProviderModelOption>, Vec<String>)>> {
        let providers = self.provider_engines();
        let allowed_engines: HashMap<String, Arc<dyn Engine>> = providers
            .iter()
            .map(|(provider_connection, _engine_name, engine)| {
                (provider_connection.clone(), Arc::clone(engine))
            })
            .collect();
        let catalog_cache = Arc::clone(&self.catalog_cache);
        let lifetime = self.catalog_cache_lifetime;

        Box::pin(async move {
            let requests =
                providers
                    .into_iter()
                    .map(|(provider_connection, _engine_name, engine)| {
                        let catalog_cache = Arc::clone(&catalog_cache);
                        async move {
                            let cached = catalog_cache
                                .lock()
                                .expect("the catalog cache lock is poisoned")
                                .get(&provider_connection)
                                .filter(|(fetched_at, _)| fetched_at.elapsed() < lifetime)
                                .map(|(_, models)| models.clone());
                            if let Some(models) = cached {
                                return (provider_connection, Ok(models));
                            }
                            let result = engine.list_models().await;
                            if let Ok(models) = &result {
                                catalog_cache
                                    .lock()
                                    .expect("the catalog cache lock is poisoned")
                                    .insert(
                                        provider_connection.clone(),
                                        (Instant::now(), models.clone()),
                                    );
                            }
                            (provider_connection, result)
                        }
                    });
            let results = join_all(requests).await;
            let mut options = Vec::new();
            let mut failures = Vec::new();
            for (provider_connection, result) in results {
                match result {
                    Ok(models) => {
                        // The cache keeps the provider's full catalog; the
                        // connection's allow list filters what is selectable.
                        let engine = allowed_engines.get(&provider_connection);
                        options.extend(models.into_iter().filter_map(|model| {
                            engine
                                .is_none_or(|engine| engine.ensure_model_allowed(&model.id).is_ok())
                                .then_some(ProviderModelOption {
                                    context_length_override: None,
                                    max_output_tokens_override: None,
                                    provider_connection: provider_connection.clone(),
                                    model,
                                    reasoning_effort: None,
                                })
                        }));
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
            Ok((options, failures))
        })
    }

    fn switch_model(&self, model: &str) -> anyhow::Result<()> {
        self.active_engine().switch_model(model)
    }

    fn publish_named_engine(
        &self,
        name: &str,
        engine: Arc<dyn Engine>,
        publication: NamedEnginePublication,
    ) -> anyhow::Result<()> {
        let mut engines = self
            .engines
            .write()
            .map_err(|_| anyhow::anyhow!("the engine map lock is poisoned"))?;
        let exists = engines.contains_key(name);
        match (exists, publication) {
            (true, NamedEnginePublication::Add) => {
                anyhow::bail!("engine '{name}' is already registered")
            }
            (false, NamedEnginePublication::Replace) => {
                anyhow::bail!("engine '{name}' is not registered")
            }
            _ => {}
        }
        engines.insert(name.to_string(), engine);
        Ok(())
    }

    fn named_engines(&self) -> Vec<(String, EngineConfig)> {
        if self.hides_engine_identity() {
            return Vec::new();
        }
        let mut engines: Vec<(String, EngineConfig)> = self
            .engine_snapshot()
            .iter()
            .map(|(name, engine)| (name.clone(), engine.config()))
            .collect();
        engines.sort_by(|left, right| left.0.cmp(&right.0));
        engines
    }

    fn named_engine_info(&self) -> Vec<NamedEngineInfo> {
        if self.hides_engine_identity() {
            return Vec::new();
        }
        let mut engines: Vec<NamedEngineInfo> = self
            .engine_snapshot()
            .iter()
            .map(|(name, engine)| {
                let runtime = engine.runtime_snapshot();
                NamedEngineInfo {
                    model_info: runtime
                        .as_ref()
                        .and_then(|runtime| runtime.model_info.clone()),
                    name: name.clone(),
                    config: engine.config(),
                    reasoning_effort: engine.reasoning_effort(),
                    configured: engine.configured_engine_config(),
                    runtime,
                }
            })
            .collect();
        engines.sort_by(|left, right| left.name.cmp(&right.name));
        engines
    }

    fn switch_model_option(&self, option: &ProviderModelOption) -> anyhow::Result<()> {
        let mut active_engine = self
            .active_engine
            .write()
            .map_err(|_| anyhow::anyhow!("the active engine lock is poisoned"))?;
        let (engine_name, engine) =
            self.provider_engine_for_active(&option.provider_connection, &active_engine)?;
        engine.ensure_model_allowed(&option.model.id)?;
        engine.switch_model_option(option)?;
        *active_engine = engine_name;
        Ok(())
    }

    fn preview_model_selection(
        &self,
        selection: &ModelSelection,
    ) -> anyhow::Result<ModelSelectionTarget> {
        let (engine_name, target_engine) = match selection {
            ModelSelection::NamedEngine {
                engine_name,
                option,
                reasoning_effort,
            } => {
                let target_engine = self.engine_named(engine_name).ok_or_else(|| {
                    anyhow::anyhow!("named engine '{engine_name}' is not configured")
                })?;
                validate_named_engine_option(target_engine.as_ref(), option, reasoning_effort)?;
                target_engine.ensure_model_allowed(&option.model.id)?;
                (engine_name.clone(), target_engine)
            }
            ModelSelection::ProviderModel(option) => {
                let target = self.provider_engine_for(&option.provider_connection)?;
                target.1.ensure_model_allowed(&option.model.id)?;
                target.1.reasoning_effort_policy().resolve(
                    &option.model.id,
                    Some(&option.model),
                    option.reasoning_effort.as_deref(),
                )?;
                target
            }
        };
        let config = target_engine.config();
        let context_length = match selection {
            ModelSelection::NamedEngine { option, .. } => {
                super::named_selection_limits(target_engine.as_ref(), option).0
            }
            ModelSelection::ProviderModel(option) => {
                option.context_length_override.unwrap_or_else(|| {
                    target_engine
                        .context_length_for_model(&option.model.id, option.model.context_length)
                })
            }
        };
        let max_output_tokens = match selection {
            ModelSelection::NamedEngine { option, .. } => {
                super::named_selection_limits(target_engine.as_ref(), option).1
            }
            ModelSelection::ProviderModel(option) => {
                option.max_output_tokens_override.or_else(|| {
                    target_engine.max_output_tokens_for_model(
                        &option.model.id,
                        option.model.max_output_tokens,
                    )
                })
            }
        };
        Ok(ModelSelectionTarget {
            engine_name: Some(engine_name),
            provider_connection: config.provider_connection,
            model: selection.model_id().to_string(),
            context_length,
            max_output_tokens,
        })
    }

    fn select_model(&self, selection: &ModelSelection) -> anyhow::Result<()> {
        match selection {
            ModelSelection::ProviderModel(option) => self.switch_model_option(option),
            ModelSelection::NamedEngine {
                engine_name,
                option,
                reasoning_effort,
            } => {
                let engine = self.engine_named(engine_name).ok_or_else(|| {
                    anyhow::anyhow!("named engine '{engine_name}' is not configured")
                })?;
                validate_named_engine_option(engine.as_ref(), option, reasoning_effort)?;
                engine.ensure_model_allowed(&option.model.id)?;
                let mut active_engine = self
                    .active_engine
                    .write()
                    .map_err(|_| anyhow::anyhow!("the active engine lock is poisoned"))?;
                engine.apply_named_model_option(option, reasoning_effort)?;
                *active_engine = engine_name.clone();
                Ok(())
            }
        }
    }
}
