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
    ProviderModelOption, ReasoningEffortChange,
};
use crate::agent::{Message, StreamEvent};
use crate::llm::{InputModality, ModelInfo, ToolSchema};
use futures::future::{join_all, BoxFuture};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
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
                            context_length_override: None,
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

    fn named_engine_info(&self) -> Vec<NamedEngineInfo> {
        let mut engines: Vec<NamedEngineInfo> = self
            .engines
            .iter()
            .map(|(name, engine)| NamedEngineInfo {
                name: name.clone(),
                config: engine.config(),
                reasoning_effort: engine.reasoning_effort(),
            })
            .collect();
        engines.sort_by(|left, right| left.name.cmp(&right.name));
        engines
    }

    fn switch_model_option(&self, option: &ProviderModelOption) -> anyhow::Result<()> {
        let (engine_name, engine) = self.provider_engine_for(&option.provider_connection)?;
        engine.switch_model_option(option)?;
        *self
            .active_engine
            .write()
            .map_err(|_| anyhow::anyhow!("the active engine lock is poisoned"))? = engine_name;
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
                let target_engine = self.engines.get(engine_name).ok_or_else(|| {
                    anyhow::anyhow!("named engine '{engine_name}' is not configured")
                })?;
                validate_named_engine_option(&target_engine.config(), option, reasoning_effort)?;
                (engine_name.clone(), target_engine.clone())
            }
            ModelSelection::ProviderModel(option) => {
                self.provider_engine_for(&option.provider_connection)?
            }
        };
        let config = target_engine.config();
        let context_length = match selection {
            ModelSelection::NamedEngine { option, .. } => option
                .context_length_override
                .unwrap_or_else(|| target_engine.effective_context_length()),
            ModelSelection::ProviderModel(option) => {
                option.context_length_override.unwrap_or_else(|| {
                    target_engine
                        .context_length_for_model(&option.model.id, option.model.context_length)
                })
            }
        };
        Ok(ModelSelectionTarget {
            engine_name: Some(engine_name),
            provider_connection: config.provider_connection,
            model: selection.model_id().to_string(),
            context_length,
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
                let engine = self.engines.get(engine_name).ok_or_else(|| {
                    anyhow::anyhow!("named engine '{engine_name}' is not configured")
                })?;
                validate_named_engine_option(&engine.config(), option, reasoning_effort)?;
                let mut active_engine = self
                    .active_engine
                    .write()
                    .map_err(|_| anyhow::anyhow!("the active engine lock is poisoned"))?;
                if let ReasoningEffortChange::Set(reasoning_effort) = reasoning_effort {
                    engine.set_reasoning_effort(reasoning_effort.as_deref())?;
                }
                if let Some(context_length) = option.context_length_override {
                    engine.set_context_length_override(Some(context_length))?;
                }
                *active_engine = engine_name.clone();
                Ok(())
            }
        }
    }
}
