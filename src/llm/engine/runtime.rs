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

//! Provider-backed engine runtime and one-attempt catalog hydration.

use super::{Engine, EngineConfig, EngineRuntimeConfig, EngineRuntimeConfigHandle};
use crate::agent::{Message, StreamEvent};
use crate::config::ModelPolicy;
use crate::llm::{InputModality, ModelInfo, Provider, ToolSchema};
use futures::future::BoxFuture;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Default `Engine` implementation — thin wrapper around an `Arc<dyn Provider>`.
pub struct EngineImpl {
    provider: Arc<dyn Provider>,
    /// Bound providers consume this engine-owned runtime directly, without legacy setters.
    owns_provider_runtime: bool,
    /// Connection settings are immutable after construction.
    config: EngineConfig,
    /// Shared with providers that support Engine runtime settings.
    runtime_config: EngineRuntimeConfigHandle,
    /// Catalog metadata is discovered at most once per model selection,
    /// whether the attempt succeeds or fails. A direct model switch resets
    /// the guard; a `/models` selection supplies metadata and marks it done.
    catalog_hydrated: AtomicBool,
    /// One-shot notices for dropped profile efforts and unverified selections,
    /// accumulated by the engine and
    /// handed out via [`Engine::take_pending_notices`], so the agent loop
    /// can surface them as transcript notices without the engine knowing
    /// anything about the UI.
    pending_notices: Mutex<Vec<String>>,
    warned_selection: Mutex<Option<(String, String)>>,
    hydration_lock: tokio::sync::Mutex<()>,
    selection_revision: AtomicU64,
    /// Immutable allowed-model restriction of this engine's provider
    /// connection. Unrestricted unless set by [`EngineImpl::with_model_policy`].
    model_policy: ModelPolicy,
    /// Exact favorite model IDs of this engine's provider connection in
    /// configured order. Display data for the model picker only; it never
    /// restricts selection. Updated when a favorite save completes.
    favorite_models: RwLock<Vec<String>>,
}

impl EngineImpl {
    pub fn new(provider: Arc<dyn Provider>, config: EngineConfig) -> Self {
        let independent_runtime = Arc::new(RwLock::new(EngineRuntimeConfig::from(&config)));
        let bound_provider = provider.bind_engine_runtime(independent_runtime.clone());
        let owns_provider_runtime = bound_provider.is_some();
        let provider = bound_provider.unwrap_or(provider);
        let runtime_config = provider
            .runtime_config_handle()
            .unwrap_or(independent_runtime);
        *runtime_config
            .write()
            .expect("the engine runtime configuration lock is poisoned") =
            EngineRuntimeConfig::from(&config);
        EngineImpl {
            provider,
            owns_provider_runtime,
            config,
            runtime_config,
            catalog_hydrated: AtomicBool::new(false),
            pending_notices: Mutex::new(Vec::new()),
            warned_selection: Mutex::new(None),
            hydration_lock: tokio::sync::Mutex::new(()),
            selection_revision: AtomicU64::new(0),
            model_policy: ModelPolicy::unrestricted(),
            favorite_models: RwLock::new(Vec::new()),
        }
    }

    fn snapshot(&self) -> EngineRuntimeConfig {
        self.runtime_config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn resolve_runtime(
        &self,
        runtime: &EngineRuntimeConfig,
    ) -> anyhow::Result<crate::llm::ReasoningEffortProvenance> {
        let cached = self.provider.cached_model_info(&runtime.model);
        let model = cached.as_ref().or_else(|| {
            runtime
                .model_info
                .as_ref()
                .filter(|model| model.id == runtime.model)
        });
        self.reasoning_effort_policy()
            .resolve(&runtime.model, model, runtime.reasoning_effort.as_deref())
            .map_err(|error| {
                error.context(format!(
                    "engine on provider connection '{}' model '{}' reasoning effort '{}'",
                    self.config.provider_connection,
                    runtime.model,
                    runtime.reasoning_effort.as_deref().unwrap_or("default")
                ))
            })
    }

    fn validate_request_runtime(&self, runtime: &EngineRuntimeConfig) -> anyhow::Result<()> {
        let cached = self.provider.cached_model_info(&runtime.model);
        let model = cached.as_ref().or_else(|| {
            runtime
                .model_info
                .as_ref()
                .filter(|model| model.id == runtime.model)
        });
        self.reasoning_effort_policy()
            .validate_request(&runtime.model, model, runtime.reasoning_effort.as_deref())
            .map_err(|error| {
                error.context(format!(
                    "engine on provider connection '{}' model '{}' reasoning effort '{}'",
                    self.config.provider_connection,
                    runtime.model,
                    runtime.reasoning_effort.as_deref().unwrap_or("default")
                ))
            })
    }

    fn apply_runtime_option(
        &self,
        option: &super::ProviderModelOption,
        change: Option<&super::ReasoningEffortChange>,
    ) -> anyhow::Result<()> {
        let mut runtime = self
            .runtime_config
            .write()
            .map_err(|_| anyhow::anyhow!("the engine runtime configuration lock is poisoned"))?;
        anyhow::ensure!(
            self.config.provider_connection == option.provider_connection,
            "engine uses provider connection '{}', not '{}'",
            self.config.provider_connection,
            option.provider_connection
        );
        self.ensure_model_allowed(&option.model.id)?;
        if change.is_some() {
            anyhow::ensure!(
                runtime.model == option.model.id,
                "engine uses model '{}', not '{}'",
                runtime.model,
                option.model.id
            );
        }
        let effort = match change {
            Some(super::ReasoningEffortChange::Unchanged) => runtime.reasoning_effort.clone(),
            Some(super::ReasoningEffortChange::Set(effort)) => effort.clone(),
            None => option.reasoning_effort.clone(),
        };
        if let Some(change) = change {
            self.reasoning_effort_policy().validate_named_change(
                &option.model,
                runtime.reasoning_effort.as_deref(),
                change,
            )?;
        } else {
            self.reasoning_effort_policy().resolve(
                &option.model.id,
                Some(&option.model),
                effort.as_deref(),
            )?;
        }
        let context_override = if change.is_some() {
            option
                .context_length_override
                .or(runtime.context_length_override)
        } else {
            option.context_length_override
        };
        runtime.model = option.model.id.clone();
        runtime.reasoning_effort = effort;
        runtime.model_info = Some(option.model.clone());
        runtime.input_modalities = option.model.input_modalities.clone();
        runtime.context_length_override = context_override;
        runtime.context_length = context_override.unwrap_or_else(|| {
            self.context_length_for_model(&option.model.id, option.model.context_length)
        });
        // Reset warning when the actual effective selection changes, including default.
        let selection = runtime
            .reasoning_effort
            .as_ref()
            .map(|effort| (runtime.model.clone(), effort.clone()));
        let mut warned = self
            .warned_selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *warned != selection {
            *warned = None;
        }
        self.selection_revision.fetch_add(1, Ordering::AcqRel);
        self.catalog_hydrated.store(true, Ordering::Release);
        Ok(())
    }

    /// Seed the favorite model IDs of this engine's provider connection, in
    /// configured order.
    pub fn with_favorite_models(self, favorite_models: Vec<String>) -> Self {
        *self
            .favorite_models
            .write()
            .expect("the favorite models lock is poisoned") = favorite_models;
        self
    }

    /// Restrict the models this engine may select or send chat requests to.
    ///
    /// The configured model must already satisfy the policy; the caller
    /// validates it so construction fails before any request.
    pub fn with_model_policy(mut self, model_policy: ModelPolicy) -> Self {
        self.model_policy = model_policy;
        self
    }
}

impl EngineImpl {
    fn configured_favorite_models(&self) -> Vec<String> {
        self.favorite_models
            .read()
            .expect("the favorite models lock is poisoned")
            .clone()
    }
}

impl Engine for EngineImpl {
    fn runtime_snapshot(&self) -> Option<EngineRuntimeConfig> {
        Some(self.snapshot())
    }
    fn reasoning_effort_policy(&self) -> std::sync::Arc<dyn crate::llm::ReasoningEffortPolicy> {
        self.provider.reasoning_effort_policy()
    }

    fn effective_reasoning_provenance(&self) -> crate::llm::ReasoningEffortProvenance {
        self.resolve_runtime(&self.snapshot()).unwrap_or_default()
    }

    fn prepare_request_notices(&self) -> anyhow::Result<()> {
        let runtime = self
            .runtime_config
            .read()
            .map_err(|_| anyhow::anyhow!("the engine runtime configuration lock is poisoned"))?;
        self.validate_request_runtime(&runtime)?;
        let provenance = self.resolve_runtime(&runtime).unwrap_or_default();
        let mut warned = self
            .warned_selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if provenance == crate::llm::ReasoningEffortProvenance::Unverified {
            if let Some(effort) = runtime.reasoning_effort.as_ref() {
                let selection = (runtime.model.clone(), effort.clone());
                if warned.as_ref() != Some(&selection) {
                    if let Some(notice) = self
                        .reasoning_effort_policy()
                        .unverified_notice(&runtime.model, effort)
                    {
                        self.pending_notices
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(notice);
                    }
                    *warned = Some(selection);
                }
            }
        } else {
            *warned = None;
        }
        Ok(())
    }

    fn apply_named_model_option(
        &self,
        option: &super::ProviderModelOption,
        change: &super::ReasoningEffortChange,
    ) -> anyhow::Result<()> {
        if self.owns_provider_runtime {
            return self.apply_runtime_option(option, Some(change));
        }
        super::validate_named_engine_option(self, option, change)?;
        if let super::ReasoningEffortChange::Set(effort) = change {
            self.set_reasoning_effort(effort.as_deref())?;
        }
        if let Some(context_length) = option.context_length_override {
            self.set_context_length_override(Some(context_length))?;
        }
        Ok(())
    }

    fn favorite_models(&self) -> Vec<(String, Vec<String>)> {
        let favorite_models = self.configured_favorite_models();
        if favorite_models.is_empty() {
            return Vec::new();
        }
        vec![(self.config.provider_connection.clone(), favorite_models)]
    }

    fn record_favorite_membership(
        &self,
        provider_connection: &str,
        model_id: &str,
        favorite: bool,
    ) {
        if provider_connection != self.config.provider_connection {
            return;
        }
        let mut favorite_models = self
            .favorite_models
            .write()
            .expect("the favorite models lock is poisoned");
        if favorite {
            if !favorite_models.iter().any(|existing| existing == model_id) {
                favorite_models.push(model_id.to_string());
            }
        } else {
            favorite_models.retain(|existing| existing != model_id);
        }
    }

    fn ensure_model_allowed(&self, model_id: &str) -> anyhow::Result<()> {
        if model_id.is_empty() && self.model_policy != ModelPolicy::unrestricted() {
            anyhow::bail!(
                "no model is selected for provider connection '{}'; choose one with {}",
                self.config.provider_connection,
                crate::slash_command::MODELS_COMMAND_NAME
            );
        }
        self.model_policy
            .ensure_allowed(&self.config.provider_connection, model_id)
    }

    fn switch_model_option(&self, option: &super::ProviderModelOption) -> anyhow::Result<()> {
        if self.owns_provider_runtime {
            return self.apply_runtime_option(option, None);
        }
        super::apply_model_option(self, option)?;
        self.catalog_hydrated.store(true, Ordering::Release);
        Ok(())
    }

    fn configured_copy(&self) -> anyhow::Result<Arc<dyn Engine>> {
        let runtime_config = Arc::new(RwLock::new(EngineRuntimeConfig::from(&self.config)));
        let provider = self.provider.with_runtime_config(runtime_config)?;
        Ok(Arc::new(
            Self::new(provider, self.config.clone())
                .with_model_policy(self.model_policy.clone())
                .with_favorite_models(self.configured_favorite_models()),
        ))
    }

    fn context_length_for_model(
        &self,
        model: &str,
        catalog_context_length: Option<usize>,
    ) -> usize {
        self.config
            .context_length_for_model(model, catalog_context_length)
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            self.ensure_active_model_allowed()?;
            // Validate request policy before catalog I/O as well as chat dispatch.
            self.validate_request_runtime(&self.snapshot())?;
            self.hydrate_catalog_metadata().await;
            self.prepare_request_notices()?;
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
            self.ensure_active_model_allowed()?;
            // Validate request policy before catalog I/O as well as chat dispatch.
            self.validate_request_runtime(&self.snapshot())?;
            self.hydrate_catalog_metadata().await;
            self.prepare_request_notices()?;
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
            self.ensure_active_model_allowed()?;
            // Validate request policy before catalog I/O as well as chat dispatch.
            self.validate_request_runtime(&self.snapshot())?;
            self.hydrate_catalog_metadata().await;
            self.prepare_request_notices()?;
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
            self.ensure_active_model_allowed()?;
            // Validate request policy before catalog I/O as well as chat dispatch.
            self.validate_request_runtime(&self.snapshot())?;
            self.hydrate_catalog_metadata().await;
            self.prepare_request_notices()?;
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
        if runtime_config.model != self.config.model {
            config.context_length = None;
        }
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
        if self.owns_provider_runtime {
            self.ensure_model_allowed(model)?;
            let mut runtime = self.runtime_config.write().map_err(|_| {
                anyhow::anyhow!("the engine runtime configuration lock is poisoned")
            })?;
            if runtime.model == model {
                return Ok(());
            }
            let info = self.provider.cached_model_info(model);
            self.reasoning_effort_policy().validate_request(
                model,
                info.as_ref(),
                runtime.reasoning_effort.as_deref(),
            )?;
            runtime.model = model.to_string();
            runtime.model_info = info.clone();
            runtime.context_length_override = None;
            runtime.context_length = self.context_length_for_model(
                model,
                info.as_ref().and_then(|info| info.context_length),
            );
            runtime.input_modalities = info.and_then(|info| info.input_modalities);
            *self
                .warned_selection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            self.selection_revision.fetch_add(1, Ordering::AcqRel);
            self.catalog_hydrated.store(false, Ordering::Release);
            return Ok(());
        }
        self.ensure_model_allowed(model)?;
        let mut candidate = self.snapshot();
        let model_changed = candidate.model != model;
        candidate.model = model.to_string();
        if model_changed {
            candidate.model_info = self.provider.cached_model_info(model);
        }
        self.validate_request_runtime(&candidate)?;
        self.provider.set_model(model)?;
        let mut runtime_config = self
            .runtime_config
            .write()
            .map_err(|_| anyhow::anyhow!("the engine runtime configuration lock is poisoned"))?;
        if model_changed {
            runtime_config.context_length = self.context_length_for_model(model, None);
            runtime_config.input_modalities = None;
            self.catalog_hydrated.store(false, Ordering::Release);
        }
        runtime_config.model = model.to_string();
        if model_changed {
            *self
                .warned_selection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            self.selection_revision.fetch_add(1, Ordering::AcqRel);
        }
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
        self.runtime_config
            .read()
            .expect("the engine runtime configuration lock is poisoned")
            .context_length
    }

    fn set_context_length_override(&self, context_length: Option<usize>) -> anyhow::Result<()> {
        let mut runtime_config = self
            .runtime_config
            .write()
            .map_err(|_| anyhow::anyhow!("the engine runtime configuration lock is poisoned"))?;
        runtime_config.context_length_override = context_length;
        if let Some(context_length) = context_length {
            runtime_config.context_length = context_length;
        }
        Ok(())
    }

    fn adopt_catalog_context_length(&self, catalog_context_length: Option<usize>) {
        let mut runtime_config = self
            .runtime_config
            .write()
            .expect("the engine runtime configuration lock is poisoned");
        if runtime_config.context_length_override.is_some() {
            return;
        }
        let context_length =
            self.context_length_for_model(&runtime_config.model, catalog_context_length);
        if runtime_config.context_length == context_length {
            return;
        }
        tracing::info!(
            provider_connection = %self.config.provider_connection,
            model = %runtime_config.model,
            previous_context_length = runtime_config.context_length,
            context_length,
            "adopted catalog context length for the selected model"
        );
        runtime_config.context_length = context_length;
    }

    fn hydrate_catalog_metadata(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _hydration_guard = self.hydration_lock.lock().await;
            // Hydration may also be called before chat by routers and attachment projection.
            if self.validate_request_runtime(&self.snapshot()).is_err() {
                return;
            }
            if self.catalog_hydrated.load(Ordering::Acquire) {
                return;
            }
            let captured_revision = self.selection_revision.load(Ordering::Acquire);
            let captured = self.snapshot();
            let requires_evidence = self.reasoning_effort_policy().requires_catalog_evidence();
            if captured.input_modalities.is_some() && !requires_evidence {
                self.catalog_hydrated.store(true, Ordering::Release);
                return;
            }
            let models = match self.list_models().await {
                Ok(models) => Some(models),
                Err(error) => {
                    tracing::warn!(model = %captured.model, %error, "failed to discover active model input modalities");
                    None
                }
            };
            let model = models
                .as_ref()
                .and_then(|models| models.iter().find(|model| model.id == captured.model));
            if model.is_none()
                && !requires_evidence
                && captured.reasoning_effort.is_some()
                && models.is_some()
            {
                tracing::debug!(model = %captured.model, "profile reasoning effort kept: the catalog does not list the active model");
            }
            let mut runtime = self
                .runtime_config
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Every setting was captured before await. An intervening selection invalidates it.
            if *runtime != captured
                || self.selection_revision.load(Ordering::Acquire) != captured_revision
            {
                return;
            }
            if let Some(model) = model {
                runtime.model_info = Some(model.clone());
                if runtime.context_length_override.is_none() {
                    runtime.context_length =
                        self.context_length_for_model(&captured.model, model.context_length);
                }
                if let Some(modalities) = model.input_modalities.as_ref() {
                    runtime.input_modalities = Some(modalities.clone());
                    tracing::debug!(model = %captured.model,
                        input_modalities = %crate::llm::format_input_modalities_for_log(runtime.input_modalities.as_deref()),
                        "active model input modalities discovered");
                }
                if runtime.reasoning_effort.as_ref().is_some_and(|effort| {
                    !self
                        .reasoning_effort_policy()
                        .retain_profile_effort(model, effort)
                }) {
                    let effort = runtime.reasoning_effort.take().unwrap_or_default();
                    tracing::warn!(model = %model.id, reasoning_effort = %effort,
                        "profile reasoning effort is not advertised by the model; falling back to the provider default");
                    // Legacy providers may not share our handle. Keep their setter behavior.
                    if !self.owns_provider_runtime {
                        drop(runtime);
                        if let Err(error) = self.provider.set_reasoning_effort(None) {
                            tracing::warn!(%error, "failed to reset unadvertised reasoning effort");
                        }
                        runtime = self
                            .runtime_config
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        // A setter callback must not clear a newer selection after releasing the lock.
                        if self.selection_revision.load(Ordering::Acquire) != captured_revision
                            || runtime.model != captured.model
                            || runtime.reasoning_effort.is_some()
                        {
                            return;
                        }
                    }
                    self.pending_notices.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!(
                        "Model {}/{} does not advertise reasoning effort \"{}\"; using the provider default.", self.config.provider_connection, model.id, effort));
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
        if self.owns_provider_runtime {
            let mut runtime = self.runtime_config.write().map_err(|_| {
                anyhow::anyhow!("the engine runtime configuration lock is poisoned")
            })?;
            let mut candidate = runtime.clone();
            candidate.reasoning_effort = reasoning_effort.map(String::from);
            self.validate_request_runtime(&candidate)?;
            if runtime.reasoning_effort != candidate.reasoning_effort {
                *self
                    .warned_selection
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
            runtime.reasoning_effort = candidate.reasoning_effort;
            self.selection_revision.fetch_add(1, Ordering::AcqRel);
            return Ok(());
        }
        let mut candidate = self.snapshot();
        let effort_changed = candidate.reasoning_effort.as_deref() != reasoning_effort;
        candidate.reasoning_effort = reasoning_effort.map(String::from);
        self.validate_request_runtime(&candidate)?;
        self.provider.set_reasoning_effort(reasoning_effort)?;
        let mut runtime_config = self
            .runtime_config
            .write()
            .map_err(|_| anyhow::anyhow!("the engine runtime configuration lock is poisoned"))?;
        if effort_changed {
            *self
                .warned_selection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
        runtime_config.reasoning_effort = candidate.reasoning_effort;
        self.selection_revision.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}
