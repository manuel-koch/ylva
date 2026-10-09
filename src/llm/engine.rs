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

mod config;
mod router;
mod runtime;
mod selection;

pub use config::{
    EngineConfig, EngineRuntimeConfig, EngineRuntimeConfigHandle, DEFAULT_COMPACT_TARGET_RATIO,
    DEFAULT_CONTEXT_LENGTH,
};
pub use router::EngineRouter;
pub use runtime::EngineImpl;
pub use selection::{
    CompactionEngineDisplayInfo, EngineDisplayInfo, ModelSelection, ModelSelectionTarget,
    NamedEngineInfo, ProviderModelOption, ReasoningEffortChange,
};

use crate::agent::{Message, StreamEvent};
use crate::llm::{InputModality, ModelInfo, ToolSchema};
use futures::future::BoxFuture;
use selection::validate_named_engine_option;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Execution interface over a chat-completions-style streaming backend.
///
/// Keeps `AgentLoop` independent of concrete provider implementations while
/// allowing the agent factory to construct root and child engines consistently.
pub trait Engine: Send + Sync {
    /// Create an independent runtime from the engine's original configuration.
    fn configured_copy(&self) -> anyhow::Result<Arc<dyn Engine>> {
        anyhow::bail!("engine does not support creating an independent configured runtime")
    }

    /// Resolve a selected model's window without retaining another model's metadata.
    fn context_length_for_model(
        &self,
        model: &str,
        catalog_context_length: Option<usize>,
    ) -> usize {
        self.config()
            .context_length_for_model(model, catalog_context_length)
    }

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

    /// Return named engines with their current Engine runtime reasoning selection.
    ///
    /// Non-routed engines may expose configured engine snapshots through
    /// `named_engines`; their initial configured effort is the best available
    /// fallback. Routers override this to report each engine's live value.
    fn named_engine_info(&self) -> Vec<NamedEngineInfo> {
        self.named_engines()
            .into_iter()
            .map(|(name, config)| NamedEngineInfo {
                reasoning_effort: config.reasoning.clone(),
                name,
                config,
            })
            .collect()
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
    /// re-evaluates the budget.
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
        apply_model_option(self, option)
    }

    /// Apply a picker selection. Named shortcuts activate the exact existing
    /// engine; raw catalog options retain provider-based routing.
    fn select_model(&self, selection: &ModelSelection) -> anyhow::Result<()> {
        match selection {
            ModelSelection::ProviderModel(option) => self.switch_model_option(option),
            ModelSelection::NamedEngine {
                engine_name,
                option,
                reasoning_effort,
            } => {
                anyhow::ensure!(
                    self.active_engine_name().as_deref() == Some(engine_name),
                    "engine '{engine_name}' is not available for named selection"
                );
                validate_named_engine_option(&self.config(), option, reasoning_effort)?;
                if let ReasoningEffortChange::Set(reasoning_effort) = reasoning_effort {
                    self.set_reasoning_effort(reasoning_effort.as_deref())?;
                }
                Ok(())
            }
        }
    }

    /// Resolve the exact engine a picker selection would affect without
    /// changing Engine runtime state. The router overrides this for cross-connection
    /// catalog selections and inactive named-engine shortcuts.
    fn preview_model_selection(
        &self,
        selection: &ModelSelection,
    ) -> anyhow::Result<ModelSelectionTarget> {
        let config = self.config();
        let engine_name = match selection {
            ModelSelection::NamedEngine {
                engine_name,
                option,
                reasoning_effort,
            } => {
                anyhow::ensure!(
                    self.active_engine_name().as_deref() == Some(engine_name),
                    "engine '{engine_name}' is not available for named selection"
                );
                validate_named_engine_option(&config, option, reasoning_effort)?;
                Some(engine_name.clone())
            }
            ModelSelection::ProviderModel(option) => {
                anyhow::ensure!(
                    config.provider_connection == option.provider_connection,
                    "engine uses provider connection '{}', not '{}'",
                    config.provider_connection,
                    option.provider_connection
                );
                self.active_engine_name()
            }
        };
        let context_length = match selection {
            ModelSelection::NamedEngine { .. } => self.effective_context_length(),
            ModelSelection::ProviderModel(option) => {
                self.context_length_for_model(&option.model.id, option.model.context_length)
            }
        };
        Ok(ModelSelectionTarget {
            engine_name,
            provider_connection: selection.provider_connection().to_string(),
            model: selection.model_id().to_string(),
            context_length,
        })
    }
}

pub(super) fn apply_model_option(
    engine: &(impl Engine + ?Sized),
    option: &ProviderModelOption,
) -> anyhow::Result<()> {
    let config = engine.config();
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
    engine.set_reasoning_effort(option.reasoning_effort.as_deref())?;
    engine.switch_model(&option.model.id)?;
    engine.set_input_modalities(option.model.input_modalities.clone());
    engine.adopt_catalog_context_length(option.model.context_length);
    Ok(())
}

#[cfg(test)]
mod tests;
