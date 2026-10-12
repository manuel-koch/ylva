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
use crate::llm::{InputModality, ModelInfo, Provider, ToolSchema};
use futures::future::BoxFuture;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Default `Engine` implementation — thin wrapper around an `Arc<dyn Provider>`.
pub struct EngineImpl {
    provider: Arc<dyn Provider>,
    /// Connection settings are immutable after construction.
    config: EngineConfig,
    /// Shared with providers that support Engine runtime settings.
    runtime_config: EngineRuntimeConfigHandle,
    /// Catalog metadata is discovered at most once per model selection,
    /// whether the attempt succeeds or fails. A direct model switch resets
    /// the guard; a `/models` selection supplies metadata and marks it done.
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
    fn switch_model_option(&self, option: &super::ProviderModelOption) -> anyhow::Result<()> {
        super::apply_model_option(self, option)?;
        self.catalog_hydrated.store(true, Ordering::Release);
        Ok(())
    }

    fn configured_copy(&self) -> anyhow::Result<Arc<dyn Engine>> {
        let runtime_config = Arc::new(RwLock::new(EngineRuntimeConfig::from(&self.config)));
        let provider = self.provider.with_runtime_config(runtime_config)?;
        Ok(Arc::new(Self::new(provider, self.config.clone())))
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
        let model_changed = self.config().model != model;
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
                    if let Some(context_length) =
                        active_model_info.and_then(|model| model.context_length)
                    {
                        self.adopt_catalog_context_length(Some(context_length));
                    }
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
