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
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::tools::agent_finish::AgentFinishToolConfig;
use crate::agent::tools::{default_registry, ToolRegistryConfig, DEFAULT_CLI_TOOLS};
use crate::agent::{AgentLoop, ChildRoleConfig, Context, Message, Role, UiUpdate};
use crate::child_agent_manager::{ChildAgentManager, FinishResult, SpawnRequest};
use crate::config::ProfileConfig;
use crate::llm::engine::{
    Engine, EngineConfig, EngineDisplayInfo, EngineRouter, DEFAULT_CONTEXT_LENGTH,
};
use crate::llm::ProviderRegistry;
use crate::sandbox::Sandbox;
use crate::store::Store;

/// Constructs [`AgentLoop`] instances from a named set of [`Engine`]s and a
/// [`ProfileConfig`].
///
/// `Role::Root` agents whose resolved `AgentConfig::can_spawn` is `true` are
/// wired with a [`ChildAgentManager`] capable of running `Role::Child`
/// agents via this very factory (recursively, one level deep — a child's own
/// registry never contains `spawn_subagent`).
pub struct AgentFactory {
    engines: HashMap<String, Arc<dyn Engine>>,
    /// When `true`, the `edit` tool is auto-approved without user confirmation.
    /// Intended for automated / e2e testing where no human is present to approve.
    auto_approve_edit: bool,
    /// When `true`, the `write` tool is auto-approved without user confirmation.
    /// Intended for automated / e2e testing where no human is present to approve.
    auto_approve_write: bool,
    /// When `true`, the `spawn_subagent` tool is auto-approved without user
    /// confirmation. Intended for automated / e2e testing where no human is
    /// present to approve.
    auto_approve_spawn: bool,
    /// Explicit list of tool names to register. Defaults to [`DEFAULT_CLI_TOOLS`].
    allowed_tools: Vec<String>,
    /// Session store handed to every `Role::Root` agent this factory builds,
    /// when persistence is enabled.
    store: Option<Store>,
}

impl AgentFactory {
    pub fn new(engines: HashMap<String, Arc<dyn Engine>>) -> Self {
        AgentFactory {
            engines,
            auto_approve_edit: false,
            auto_approve_write: false,
            auto_approve_spawn: false,
            allowed_tools: DEFAULT_CLI_TOOLS.iter().map(|s| s.to_string()).collect(),
            store: None,
        }
    }

    /// Build a factory whose engines come from a loaded profile's
    /// `[engines.*]` tables (see [`crate::config::ProfileConfig::load`]).
    ///
    /// Each entry is turned into a live [`EngineImpl`] through the named
    /// provider connection it references. A provider connection that is not
    /// referenced by an explicit engine is also materialized so `/models`
    /// can query every configured provider connection. Profiles with provider
    /// connections but no explicit engines get one implicit engine per
    /// provider connection.
    pub fn from_profile(profile: &ProfileConfig) -> anyhow::Result<Self> {
        let mut engines: HashMap<String, Arc<dyn Engine>> = HashMap::new();
        if profile.engines.is_empty() {
            let mut provider_connection_names: Vec<_> =
                profile.provider_connections.keys().cloned().collect();
            provider_connection_names.sort();
            for (index, provider_connection_name) in
                provider_connection_names.into_iter().enumerate()
            {
                let engine_name = if index == 0 {
                    profile.default_engine.clone()
                } else {
                    provider_connection_name.clone()
                };
                let engine_name = unique_engine_name(&engines, &engine_name, index);
                let engine_config = implicit_engine_config(
                    &provider_connection_name,
                    profile.default_model.as_deref(),
                );
                let engine =
                    engine_from_profile_config(&engine_config, &profile.provider_connections)?;
                engines.insert(engine_name, engine);
            }
        } else {
            for (engine_name, engine_config) in &profile.engines {
                engines.insert(
                    engine_name.clone(),
                    engine_from_profile_config(engine_config, &profile.provider_connections)?,
                );
            }

            let referenced_provider_connections: std::collections::HashSet<_> = profile
                .engines
                .values()
                .map(|engine_config| engine_config.provider_connection.as_str())
                .collect();
            let mut provider_connection_names: Vec<_> = profile
                .provider_connections
                .keys()
                .filter(|provider_connection_name| {
                    !referenced_provider_connections.contains(provider_connection_name.as_str())
                })
                .cloned()
                .collect();
            provider_connection_names.sort();
            for (index, provider_connection_name) in
                provider_connection_names.into_iter().enumerate()
            {
                let engine_name = unique_engine_name(&engines, &provider_connection_name, index);
                let engine_config = implicit_engine_config(
                    &provider_connection_name,
                    profile.default_model.as_deref(),
                );
                let engine =
                    engine_from_profile_config(&engine_config, &profile.provider_connections)?;
                engines.insert(engine_name, engine);
            }
        }
        for required in [
            profile.default_engine.as_str(),
            profile.compact_engine_name(),
        ] {
            anyhow::ensure!(
                engines.contains_key(required),
                "profile references engine '{required}', which no [engines.*] table defines"
            );
        }
        Ok(AgentFactory::new(engines))
    }

    /// Snapshot of the named engine's configuration, if it exists.
    ///
    /// Used to record the initial [`crate::store::StoreEvent::ModelChanged`]
    /// right after a session is created — before any `/models` switch, so
    /// the session's history is never missing its starting
    /// provider-connection/model.
    pub fn engine_config(&self, name: &str) -> Option<EngineConfig> {
        self.engines.get(name).map(|engine| engine.config())
    }

    /// Enable persistence: every `Role::Root` agent built by this factory
    /// appends its committed messages to `store`'s session file, and every
    /// child run it spawns appends `child_start`/`child_result` records plus
    /// its own transcript alongside it.
    pub fn with_store(mut self, store: Store) -> Self {
        self.store = Some(store);
        self
    }

    /// Enable automatic approval of `write` tool calls (no user prompt).
    pub fn with_auto_approve_write(mut self) -> Self {
        self.auto_approve_write = true;
        self
    }

    /// Enable automatic approval of `edit` tool calls (no user prompt).
    pub fn with_auto_approve_edit(mut self) -> Self {
        self.auto_approve_edit = true;
        self
    }

    /// Enable automatic approval of `spawn_subagent` tool calls (no user prompt).
    pub fn with_auto_approve_spawn(mut self) -> Self {
        self.auto_approve_spawn = true;
        self
    }

    /// Override the allowed tool set. Pass [`DEFAULT_CLI_TOOLS`], [`DEFAULT_TUI_TOOLS`],
    /// or a custom subset for per-agent restrictions.
    pub fn with_allowed_tools(mut self, tools: &[&str]) -> Self {
        self.allowed_tools = tools.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Creates an [`AgentLoop`] and its paired UI-update receiver.
    ///
    /// Engine resolution order:
    /// 1. `profile.agents[agent_type].engine` (when present).
    /// 2. `profile.default_engine` (fallback).
    ///
    /// Returns `Err` when the resolved engine name is not registered in
    /// `self.engines`.
    ///
    /// A `Role::Root` agent whose `agent_type` resolves `can_spawn: true` is
    /// additionally wired with a [`ChildAgentManager`]; a `Role::Child` agent
    /// is wired via [`AgentLoop::with_child_role`] and has `agent_finish` (not
    /// `spawn_subagent`) registered in its tool registry.
    pub fn create(
        &self,
        profile: &ProfileConfig,
        agent_type: &str,
        role: Role,
    ) -> anyhow::Result<(AgentLoop, mpsc::Receiver<UiUpdate>)> {
        Self::build(
            &self.engines,
            &self.allowed_tools,
            self.auto_approve_edit,
            self.auto_approve_write,
            self.auto_approve_spawn,
            profile,
            agent_type,
            role,
            self.store.clone(),
        )
    }

    /// Shared construction logic for both the top-level `create` call and the
    /// `spawn_fn` closure a `ChildAgentManager` uses to start children — kept
    /// as a free function (rather than requiring `Arc<Self>`) so the closure
    /// can capture cheap, owned clones of everything it needs instead of the
    /// whole factory.
    #[expect(clippy::too_many_arguments)]
    fn build(
        engines: &HashMap<String, Arc<dyn Engine>>,
        allowed_tools: &[String],
        auto_approve_edit: bool,
        auto_approve_write: bool,
        auto_approve_spawn: bool,
        profile: &ProfileConfig,
        agent_type: &str,
        role: Role,
        store: Option<Store>,
    ) -> anyhow::Result<(AgentLoop, mpsc::Receiver<UiUpdate>)> {
        let engine_name = resolve_engine_name(profile, agent_type);

        let configured_engine = engines
            .get(engine_name)
            .ok_or_else(|| anyhow::anyhow!("unknown engine: {engine_name}"))?;
        let engine: Arc<dyn Engine> = if engines.len() == 1 {
            configured_engine.clone()
        } else {
            Arc::new(EngineRouter::new(engines.clone(), engine_name)?)
        };

        let child_role_config = match role {
            Role::Root => None,
            Role::Child(config) => Some(config),
        };
        let is_child = child_role_config.is_some();

        let can_spawn = child_role_config.is_none()
            && profile
                .agents
                .get(agent_type)
                .map(|agent_config| agent_config.can_spawn)
                .unwrap_or(false);

        let agent_finish_config = child_role_config
            .as_ref()
            .map(|config| AgentFinishToolConfig {
                finish_tx: config.finish_tx.clone(),
                run_id: config.run_id,
                call_id: config.call_id,
            });

        // Per-`agent_type` `allowed_tools` (`profile.toml`'s `[agents.*]`
        // entries) takes precedence over the flat, factory-wide default when
        // configured, so a child spawned with a distinct `agent_type` can be
        // restricted to a genuinely different tool set than its parent (or
        // the root). Falls back to the flat list unchanged when unconfigured,
        // preserving today's behavior for every existing profile.
        let configured_allowed_tools = profile
            .agents
            .get(agent_type)
            .and_then(|agent_config| agent_config.allowed_tools.clone())
            .unwrap_or_else(|| allowed_tools.to_vec());

        let sandbox = std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new()));
        let registry = Arc::new(default_registry(
            sandbox,
            ToolRegistryConfig {
                auto_approve_edit,
                auto_approve_write,
                auto_approve_spawn,
                allowed_tools: configured_allowed_tools,
                include_spawn_subagent: can_spawn,
                agent_finish: agent_finish_config,
            },
        ));
        let (mut agent_loop, ui_rx) = AgentLoop::new(Arc::clone(&engine), registry);

        if let Some(child_role_config) = child_role_config {
            agent_loop = agent_loop.with_child_role(child_role_config);
        } else if can_spawn {
            let mut manager = Self::build_child_agent_manager(
                engines.clone(),
                allowed_tools.to_vec(),
                auto_approve_edit,
                auto_approve_write,
                auto_approve_spawn,
                profile.clone(),
                store.clone(),
            );
            if let Some(store) = store.clone() {
                manager.manager = manager.manager.with_store(store);
            }
            agent_loop = agent_loop.with_child_agent_manager(
                manager.manager,
                manager.finish_rx,
                manager.subagent_ui_rx,
            );
        }

        // Only the root agent streams into the session file; a child's
        // transcript is written once, at completion, into its own
        // sub-session file.
        if !is_child {
            if let Some(store) = store {
                agent_loop = agent_loop.with_store(store);
            }
        }

        Ok((agent_loop, ui_rx))
    }

    /// Build a [`ChildAgentManager`] whose `spawn_fn` recursively calls
    /// [`Self::build`] with `Role::Child` for each spawn request, runs the
    /// resulting child `AgentLoop` to completion, and reports the outcome
    /// through `finish_tx` — unless the child's own `agent_finish` tool already
    /// did so (checked from the child's final `Context`), or the child was
    /// cancelled by the parent (checked via the per-child `CancellationToken`).
    fn build_child_agent_manager(
        engines: HashMap<String, Arc<dyn Engine>>,
        allowed_tools: Vec<String>,
        auto_approve_edit: bool,
        auto_approve_write: bool,
        auto_approve_spawn: bool,
        profile: ProfileConfig,
        parent_store: Option<Store>,
    ) -> BuiltChildAgentManager {
        let (finish_tx, finish_rx) = mpsc::channel::<FinishResult>(16);
        let (subagent_ui_tx, subagent_ui_rx) = mpsc::channel::<UiUpdate>(100);

        let spawn_fn: crate::child_agent_manager::SpawnFn = Box::new(
            move |request: SpawnRequest,
                  cancel: CancellationToken,
                  forward_ui_tx: mpsc::Sender<UiUpdate>| {
                let engines = engines.clone();
                let allowed_tools = allowed_tools.clone();
                let profile = profile.clone();
                let finish_tx = finish_tx.clone();
                let parent_store = parent_store.clone();
                let child_engine_name =
                    resolve_engine_name(&profile, &request.agent_type).to_string();
                let child_engine_config = engines
                    .get(&child_engine_name)
                    .map(|engine| engine.config());
                let child_engine_display = engines
                    .get(&child_engine_name)
                    .map(|engine| EngineDisplayInfo::from_engine(engine.as_ref()));
                let run_id = request.run_id;
                let call_id = request.call_id;

                let child_role_config = ChildRoleConfig {
                    run_id,
                    call_id,
                    briefing: request.briefing.clone(),
                    parent_cancel: cancel.clone(),
                    subagent_ui_tx: forward_ui_tx.clone(),
                    finish_tx: finish_tx.clone(),
                };

                // Constructing the child `AgentLoop` is synchronous
                // (`AgentFactory::build` performs no I/O), so it runs here,
                // directly in the `spawn_fn` closure, before `tokio::spawn` —
                // this is what lets the manager retain the child's own
                // `command_sender` for `ChildAgentManager::running_command_sender`
                // (see `SpawnOutcome`). Only the "drive to completion" body
                // below (unchanged apart from being split out) moves into the
                // spawned task.
                let built = AgentFactory::build(
                    &engines,
                    &allowed_tools,
                    auto_approve_edit,
                    auto_approve_write,
                    auto_approve_spawn,
                    &profile,
                    &request.agent_type,
                    Role::Child(child_role_config),
                    None,
                );
                let (child_loop, child_ui_rx) = match built {
                    Ok(built) => built,
                    Err(error) => {
                        tracing::error!(%run_id, %error, "failed to construct child agent");
                        // No async context is available synchronously here, so
                        // this failure report uses a non-blocking send instead
                        // of the usual `.send(...).await` — the channel's
                        // capacity is small and this is a single immediate
                        // report, so a dropped send (full channel) is
                        // acceptable for this fail-safe path.
                        let _ = finish_tx.try_send(FinishResult {
                            run_id,
                            call_id,
                            status: "failure".to_string(),
                            result: None,
                            error: Some(error.to_string()),
                            sources: vec![],
                        });
                        return crate::child_agent_manager::SpawnOutcome {
                            command_sender: None,
                            handle: tokio::spawn(async {}),
                        };
                    }
                };

                if let Some(engine) = child_engine_display {
                    let _ = forward_ui_tx
                        .try_send(UiUpdate::ChildAgentEngineConfigured { run_id, engine });
                }

                let command_sender = child_loop.command_sender();
                let task = request.task.clone();
                let handle = tokio::spawn(async move {
                    drive_child_to_completion(
                        child_loop,
                        child_ui_rx,
                        task,
                        run_id,
                        call_id,
                        cancel,
                        finish_tx,
                        forward_ui_tx,
                        ChildTranscriptTarget {
                            parent_store,
                            engine_config: child_engine_config,
                        },
                    )
                    .await;
                });

                crate::child_agent_manager::SpawnOutcome {
                    command_sender: Some(command_sender),
                    handle,
                }
            },
        );

        BuiltChildAgentManager {
            manager: ChildAgentManager::new(subagent_ui_tx, spawn_fn),
            finish_rx,
            subagent_ui_rx,
        }
    }
}

/// Return value of [`AgentFactory::build_child_agent_manager`]: the manager
/// plus the receiving ends of the channels its children send on, which the
/// caller stores on the parent `AgentLoop`.
struct BuiltChildAgentManager {
    manager: ChildAgentManager,
    finish_rx: mpsc::Receiver<FinishResult>,
    subagent_ui_rx: mpsc::Receiver<UiUpdate>,
}

/// Drives an already-constructed child `AgentLoop` to completion and reports
/// its outcome via `finish_tx`, unless the child was cancelled (its own
/// `cancel` token fired) or it already reported via its own `agent_finish`
/// tool call (detected from its final `Context`). Split out of the former
/// `run_child` so the synchronous construction step (`AgentFactory::build`)
/// can run in the `spawn_fn` closure itself, ahead of `tokio::spawn`, letting
/// the caller retain the child's `command_sender` (see
/// `build_child_agent_manager`).
/// Where a finished child's own transcript is persisted: the **parent**
/// session's store (`None` when persistence is disabled) plus the child's own
/// resolved engine config, stamped into the sub-session's `meta` header.
struct ChildTranscriptTarget {
    parent_store: Option<Store>,
    /// The child's own resolved provider-connection/model (which can differ from the
    /// parent's), recorded in the child session's `meta` header and its
    /// initial `ModelChanged` event. `None` when the engine name failed to
    /// resolve — should not happen given `from_profile`'s upfront
    /// validation, but persisting nothing is safer than persisting a
    /// misleading empty pair.
    engine_config: Option<EngineConfig>,
}

/// Build one live engine from its configuration.
fn engine_from_config(engine_config: &EngineConfig) -> anyhow::Result<Arc<dyn Engine>> {
    let provider = ProviderRegistry::provider_from_engine_config(engine_config)?;
    Ok(Arc::new(crate::llm::engine::EngineImpl::new(
        provider,
        engine_config.clone(),
    )))
}

/// Build an engine using a named provider connection when one exists, while
/// retaining support for the legacy flat engine schema where `provider`
/// directly names the implementation kind.
fn engine_from_profile_config(
    engine_config: &EngineConfig,
    providers: &HashMap<String, crate::config::ProviderConnectionConfig>,
) -> anyhow::Result<Arc<dyn Engine>> {
    let Some(provider_config) = providers.get(&engine_config.provider_connection) else {
        return engine_from_config(engine_config);
    };
    let mut resolved_config = engine_config.clone();
    if resolved_config.api_base_url.is_none() {
        resolved_config.api_base_url = provider_config.api_base_url.clone();
    }
    if resolved_config.api_key.is_none() {
        resolved_config.api_key = provider_config.api_key.clone();
    }
    let provider = ProviderRegistry::provider_from_provider_config(
        &resolved_config.provider_connection,
        provider_config,
        &resolved_config,
    )?;
    Ok(Arc::new(crate::llm::engine::EngineImpl::new(
        provider,
        resolved_config,
    )))
}

fn implicit_engine_config(
    provider_connection_name: &str,
    default_model: Option<&str>,
) -> EngineConfig {
    EngineConfig {
        provider_connection: provider_connection_name.to_string(),
        model: default_model.unwrap_or_default().to_string(),
        temperature: None,
        context_length: DEFAULT_CONTEXT_LENGTH,
        api_base_url: None,
        api_key: None,
    }
}

fn unique_engine_name(
    engines: &HashMap<String, Arc<dyn Engine>>,
    requested_name: &str,
    index: usize,
) -> String {
    if !engines.contains_key(requested_name) {
        return requested_name.to_string();
    }
    format!("{requested_name}-provider-{index}")
}

/// Persist a finished child's transcript once, into its own sub-session file
/// under the parent's `project_key`. A failure to
/// write is logged and swallowed: losing a child transcript must never fail
/// the run that produced it.
async fn write_child_transcript(target: &ChildTranscriptTarget, task: &str, context: &Context) {
    let Some(parent_store) = &target.parent_store else {
        return;
    };
    let provider_connection_and_model = target
        .engine_config
        .as_ref()
        .map(|config| (config.provider_connection.as_str(), config.model.as_str()));
    match parent_store
        .write_child_transcript(provider_connection_and_model, task, context.messages())
        .await
    {
        Ok(child_session_id) => {
            tracing::debug!(%child_session_id, "persisted child agent transcript");
        }
        Err(error) => {
            tracing::warn!(%error, "failed to persist child agent transcript");
        }
    }
}

/// Engine name resolution shared by [`AgentFactory::build`] and the
/// `spawn_fn` closure: `profile.agents[agent_type].engine`, else
/// `profile.default_engine`.
fn resolve_engine_name<'profile>(
    profile: &'profile ProfileConfig,
    agent_type: &str,
) -> &'profile str {
    profile
        .agents
        .get(agent_type)
        .and_then(|agent_config| agent_config.engine.as_deref())
        .unwrap_or(profile.default_engine.as_str())
}

#[expect(clippy::too_many_arguments)]
async fn drive_child_to_completion(
    mut child_loop: AgentLoop,
    mut child_ui_rx: mpsc::Receiver<UiUpdate>,
    task: String,
    run_id: uuid::Uuid,
    call_id: uuid::Uuid,
    cancel: CancellationToken,
    finish_tx: mpsc::Sender<FinishResult>,
    forward_ui_tx: mpsc::Sender<UiUpdate>,
    transcript_target: ChildTranscriptTarget,
) {
    let forward_task = tokio::spawn(async move {
        while let Some(update) = child_ui_rx.recv().await {
            let _ = forward_ui_tx.send(update).await;
        }
    });

    tracing::debug!(%run_id, tool_call_id = %call_id, "run_child: starting child AgentLoop");
    let task_for_transcript = task.clone();
    let handle = child_loop.run();
    child_loop
        .send_command(crate::agent::AgentCommand::Prompt(task))
        .await;

    let result = handle.await;
    tracing::debug!(%run_id, tool_call_id = %call_id, ok = result.is_ok(), "run_child: child AgentLoop::run() returned");
    // `handle.await` resolving only guarantees the child's `run()` future
    // completed — i.e. its own `ui_tx.send(...)` calls were enqueued, not
    // that `forward_task` has already popped and forwarded all of them. In
    // particular the child's terminal `agent_finish` `ToolDone` can still be
    // sitting unforwarded in `child_ui_rx` at this point. Aborting
    // `forward_task` here would race that drain and could drop the update,
    // leaving the forwarded bubble stuck in `Running` in the parent's UI.
    // Instead, drop `child_loop` (its last `ui_tx` clone) so `child_ui_rx`
    // closes once drained, and await `forward_task` so it only exits after
    // forwarding everything buffered.
    drop(child_loop);
    let _ = forward_task.await;
    tracing::debug!(%run_id, tool_call_id = %call_id, "run_child: forward_task drained and closed");

    if cancel.is_cancelled() {
        tracing::debug!(%run_id, tool_call_id = %call_id, "run_child: parent already cancelled this run, not reporting");
        // The parent already handled this cancellation synchronously
        // (`ChildAgentManager::cancel_all`); nothing left to report.
        return;
    }

    match result {
        Ok(Ok(context)) => {
            write_child_transcript(&transcript_target, &task_for_transcript, &context).await;
            if child_context_called_finish(&context) {
                // The `agent_finish` tool already sent its own `FinishResult`.
                tracing::debug!(%run_id, tool_call_id = %call_id, "run_child: agent_finish already reported result, not sending fallback");
                return;
            }
            let text = extract_last_assistant_text(&context);
            tracing::debug!(%run_id, tool_call_id = %call_id, "run_child: child never called agent_finish, sending unstructured fallback");
            let _ = finish_tx
                .send(FinishResult {
                    run_id,
                    call_id,
                    status: "unstructured".to_string(),
                    result: Some(text),
                    error: None,
                    sources: vec![],
                })
                .await;
        }
        Ok(Err(error)) => {
            let _ = finish_tx
                .send(FinishResult {
                    run_id,
                    call_id,
                    status: "failure".to_string(),
                    result: None,
                    error: Some(error.to_string()),
                    sources: vec![],
                })
                .await;
        }
        Err(join_error) => {
            let _ = finish_tx
                .send(FinishResult {
                    run_id,
                    call_id,
                    status: "failure".to_string(),
                    result: None,
                    error: Some(format!("child agent task panicked: {join_error}")),
                    sources: vec![],
                })
                .await;
        }
    }
}

/// Whether the child's final context contains a completed `agent_finish` tool
/// call — if so, its own `AgentFinishTool::run` already sent a `FinishResult` and
/// the caller must not send a second, "unstructured" one.
fn child_context_called_finish(context: &Context) -> bool {
    context.messages().iter().any(|message| {
        matches!(
            message,
            Message::ToolResult { name, .. } if name == crate::agent::tools::agent_finish::AgentFinishTool::NAME
        )
    })
}

/// The last `Message::Assistant` text in `context`, or an empty string when
/// none exists. Used to build the "unstructured fallback" `FinishResult`
/// when a child never called `agent_finish`.
fn extract_last_assistant_text(context: &Context) -> String {
    context
        .messages()
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant { content, .. } if !content.is_empty() => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::llm::engine::{EngineConfig, EngineImpl, DEFAULT_CONTEXT_LENGTH};
    use crate::llm::MockProvider;
    use crate::{AgentConfig, AssistantStatus, ProfileConfig};
    use uuid::Uuid;

    fn test_engine_config() -> EngineConfig {
        EngineConfig {
            provider_connection: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: DEFAULT_CONTEXT_LENGTH,
            api_base_url: None,
            api_key: None,
        }
    }

    fn make_factory() -> (AgentFactory, ProfileConfig) {
        let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, test_engine_config()));
        let factory = AgentFactory::new(HashMap::from([("default".to_string(), engine)]));
        let profile = ProfileConfig {
            default_engine: "default".to_string(),
            compact_engine: None,
            agents: HashMap::from([(
                "default".to_string(),
                AgentConfig {
                    engine: None,
                    allowed_tools: None,
                    timeout_secs: None,
                    persona: None,
                    can_spawn: true,
                },
            )]),
            ..ProfileConfig::default()
        };
        (factory, profile)
    }

    #[test]
    fn create_returns_ok_for_root_role_with_known_engine() {
        let (factory, profile) = make_factory();
        let result = factory.create(&profile, "default", Role::Root);
        assert!(
            result.is_ok(),
            "expected Ok for Role::Root with known engine"
        );
        let (_agent_loop, ui_rx) = result.unwrap();
        // The channel should still be open (sender not dropped).
        assert!(!ui_rx.is_closed(), "UiUpdate receiver should be open");
    }

    /// A profile with two mock engines: `main` and `fast`.
    fn two_engine_profile() -> ProfileConfig {
        let engine_config = |model: &str| EngineConfig {
            provider_connection: "mock".to_string(),
            model: model.to_string(),
            temperature: None,
            context_length: crate::llm::engine::DEFAULT_CONTEXT_LENGTH,
            api_base_url: None,
            api_key: None,
        };
        ProfileConfig {
            default_engine: "main".to_string(),
            engines: HashMap::from([
                ("main".to_string(), engine_config("main-model")),
                ("fast".to_string(), engine_config("fast-model")),
            ]),
            agents: HashMap::from([
                (
                    "default".to_string(),
                    AgentConfig {
                        engine: None,
                        allowed_tools: None,
                        timeout_secs: None,
                        persona: None,
                        can_spawn: false,
                    },
                ),
                (
                    "quick".to_string(),
                    AgentConfig {
                        engine: Some("fast".to_string()),
                        allowed_tools: None,
                        timeout_secs: None,
                        persona: None,
                        can_spawn: false,
                    },
                ),
            ]),
            ..ProfileConfig::default()
        }
    }

    #[test]
    fn from_profile_builds_every_configured_engine_and_honors_agent_overrides() {
        let profile = two_engine_profile();
        let factory = AgentFactory::from_profile(&profile).expect("the profile builds");

        let (root, _ui_rx) = factory
            .create(&profile, "default", Role::Root)
            .expect("the default agent type resolves to default_engine");
        assert_eq!(root.engine().config().model, "main-model");

        let (quick, _ui_rx) = factory
            .create(&profile, "quick", Role::Root)
            .expect("the overriding agent type resolves to its own engine");
        assert_eq!(quick.engine().config().model, "fast-model");
    }

    #[test]
    fn from_profile_resolves_multiple_connections_using_one_provider_kind() {
        let profile = ProfileConfig {
            default_engine: "ollama/local".to_string(),
            provider_connections: HashMap::from([
                (
                    "ollama-local".to_string(),
                    crate::ProviderConnectionConfig {
                        provider: "ollama".to_string(),
                        api_base_url: Some("http://localhost:11434".to_string()),
                        api_key: None,
                    },
                ),
                (
                    "ollama-cloud".to_string(),
                    crate::ProviderConnectionConfig {
                        provider: "ollama".to_string(),
                        api_base_url: Some("https://ollama.com".to_string()),
                        api_key: Some("cloud-secret".to_string()),
                    },
                ),
            ]),
            engines: HashMap::from([
                (
                    "ollama/local".to_string(),
                    EngineConfig {
                        provider_connection: "ollama-local".to_string(),
                        model: "local-model".to_string(),
                        temperature: None,
                        context_length: DEFAULT_CONTEXT_LENGTH,
                        api_base_url: None,
                        api_key: None,
                    },
                ),
                (
                    "ollama/cloud".to_string(),
                    EngineConfig {
                        provider_connection: "ollama-cloud".to_string(),
                        model: "cloud-model".to_string(),
                        temperature: None,
                        context_length: DEFAULT_CONTEXT_LENGTH,
                        api_base_url: None,
                        api_key: None,
                    },
                ),
            ]),
            ..ProfileConfig::default()
        };

        let factory = AgentFactory::from_profile(&profile).expect("the profile builds");
        let local = factory
            .engine_config("ollama/local")
            .expect("the local engine exists");
        let cloud = factory
            .engine_config("ollama/cloud")
            .expect("the cloud engine exists");

        assert_eq!(local.provider_connection, "ollama-local");
        assert_eq!(
            local.api_base_url.as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(cloud.provider_connection, "ollama-cloud");
        assert_eq!(cloud.api_base_url.as_deref(), Some("https://ollama.com"));
    }

    #[test]
    fn from_profile_rejects_a_default_engine_no_table_defines() {
        let mut profile = two_engine_profile();
        profile.default_engine = "missing".to_string();
        let error = AgentFactory::from_profile(&profile)
            .err()
            .expect("an undefined default_engine must fail loudly");
        assert!(error.to_string().contains("missing"), "{error}");
    }

    #[test]
    fn from_profile_requires_an_api_base_url_for_an_http_provider() {
        let profile = ProfileConfig {
            default_engine: "remote".to_string(),
            engines: HashMap::from([(
                "remote".to_string(),
                EngineConfig {
                    provider_connection: "ollama".to_string(),
                    model: "some-model".to_string(),
                    temperature: None,
                    context_length: crate::llm::engine::DEFAULT_CONTEXT_LENGTH,
                    api_base_url: None,
                    api_key: None,
                },
            )]),
            ..ProfileConfig::default()
        };
        let error = AgentFactory::from_profile(&profile)
            .err()
            .expect("an http provider without a base url must fail loudly");
        assert!(error.to_string().contains("api_base_url"), "{error}");
    }

    #[test]
    fn create_returns_err_for_unknown_engine() {
        let (factory, _) = make_factory();
        let profile = ProfileConfig {
            default_engine: "nonexistent".to_string(),
            agents: HashMap::new(),
            ..ProfileConfig::default()
        };
        let result = factory.create(&profile, "default", Role::Root);
        assert!(result.is_err(), "expected Err for unknown engine name");
        let error_message = result.err().unwrap().to_string();
        assert!(
            error_message.contains("nonexistent"),
            "error should mention the unknown engine name"
        );
    }

    #[test]
    fn create_returns_ok_for_child_role_with_known_engine() {
        let (factory, profile) = make_factory();
        let (finish_tx, _finish_rx) = mpsc::channel(1);
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::channel(1);
        let child_role_config = ChildRoleConfig {
            run_id: uuid::Uuid::new_v4(),
            call_id: uuid::Uuid::new_v4(),
            briefing: Some("briefing text".to_string()),
            parent_cancel: CancellationToken::new(),
            subagent_ui_tx,
            finish_tx,
        };
        let result = factory.create(&profile, "default", Role::Child(child_role_config));
        assert!(
            result.is_ok(),
            "expected Ok for Role::Child with known engine"
        );
        let (_agent_loop, ui_rx) = result.unwrap();
        assert!(!ui_rx.is_closed(), "UiUpdate receiver should be open");
    }

    #[test]
    fn create_uses_agent_config_engine_when_present() {
        let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, test_engine_config()));
        let factory = AgentFactory::new(HashMap::from([("custom".to_string(), engine)]));
        let profile = ProfileConfig {
            default_engine: "nonexistent".to_string(),
            compact_engine: None,
            agents: HashMap::from([(
                "myagent".to_string(),
                AgentConfig {
                    engine: Some("custom".to_string()),
                    allowed_tools: None,
                    timeout_secs: None,
                    persona: None,
                    can_spawn: false,
                },
            )]),
            ..ProfileConfig::default()
        };
        let result = factory.create(&profile, "myagent", Role::Root);
        assert!(
            result.is_ok(),
            "should resolve engine from AgentConfig.engine override"
        );
    }

    #[test]
    fn create_honors_per_agent_type_allowed_tools_override() {
        // Regression test for Part A: `profile.agents.<agent_type>.allowed_tools`
        // must be honored per `agent_type`, not silently ignored in favor of
        // the factory-wide default list — so a distinct `agent_type` (e.g. a
        // restricted child role) can genuinely be limited to a different tool
        // set than the root.
        let (factory, profile) = make_factory();

        // "default" has no `allowed_tools` override in `make_factory`, so it
        // falls back to the factory-wide default list, which includes `bash`.
        let (unrestricted, _ui_rx) = factory
            .create(&profile, "default", Role::Root)
            .expect("factory creates the unrestricted agent");
        assert!(
            unrestricted.registry().get("bash").is_some(),
            "expected the unrestricted agent_type to keep the factory-wide \
             default tool set, which includes bash"
        );

        let mut restricted_profile = profile.clone();
        restricted_profile.agents.insert(
            "restricted".to_string(),
            AgentConfig {
                engine: None,
                allowed_tools: Some(vec!["read".to_string()]),
                timeout_secs: None,
                persona: None,
                can_spawn: false,
            },
        );
        let (restricted, _ui_rx) = factory
            .create(&restricted_profile, "restricted", Role::Root)
            .expect("factory creates the restricted agent");
        assert!(
            restricted.registry().get("bash").is_none(),
            "expected the per-agent_type allowed_tools override to exclude \
             bash, which was not in its configured list"
        );
        assert!(
            restricted.registry().get("read").is_some(),
            "expected the per-agent_type allowed_tools override to still \
             register the tool it did configure"
        );
    }

    #[test]
    fn child_context_called_finish_detects_finish_tool_result() {
        let mut context = Context::new();
        context.push_tool_result(
            Uuid::new_v4(),
            crate::agent::tools::agent_finish::AgentFinishTool::NAME.to_string(),
            "{}".to_string(),
            None,
            None,
        );
        assert!(child_context_called_finish(&context));
    }

    #[test]
    fn child_context_called_finish_is_false_without_finish_tool_result() {
        let mut context = Context::new();
        context.push_tool_result(
            Uuid::new_v4(),
            "read".to_string(),
            "ok".to_string(),
            None,
            None,
        );
        assert!(!child_context_called_finish(&context));
    }

    #[test]
    fn extract_last_assistant_text_returns_most_recent_non_empty_text() {
        let mut context = Context::new();
        context.push_assistant("first".to_string(), AssistantStatus::Complete);
        context.push_assistant(String::new(), AssistantStatus::Complete);
        context.push_assistant("last".to_string(), AssistantStatus::Complete);
        assert_eq!(extract_last_assistant_text(&context), "last");
    }

    #[test]
    fn extract_last_assistant_text_defaults_to_empty_string_when_none_present() {
        let context = Context::new();
        assert_eq!(extract_last_assistant_text(&context), "");
    }

    #[tokio::test]
    async fn a_finished_child_transcript_is_written_to_its_own_session_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let parent_store = crate::store::Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
        )
        .expect("parent store created");
        let mut child_context = Context::new();
        child_context.push_assistant("the child's answer".to_string(), AssistantStatus::Complete);

        write_child_transcript(
            &ChildTranscriptTarget {
                parent_store: Some(parent_store.clone()),
                engine_config: Some(test_engine_config()),
            },
            "summarize the module",
            &child_context,
        )
        .await;

        let sessions = crate::store::Store::list_sessions(&chats_root, parent_store.project_key())
            .expect("sessions listable");
        let child_summary = sessions
            .iter()
            .find(|summary| summary.session_id != parent_store.session_id())
            .expect("the child transcript has its own session file");
        assert_eq!(
            child_summary.title, "summarize the module",
            "the child session is auto-titled from its task"
        );
        assert_eq!(
            child_summary.parent_session,
            Some(parent_store.session_id()),
            "the listed child summary names its parent session"
        );
        let transcript = std::fs::read_to_string(&child_summary.path).expect("transcript readable");
        assert!(
            transcript.contains("the child's answer"),
            "the child's messages are persisted: {transcript}"
        );
        assert!(
            transcript.contains(r#""provider_connection":"mock""#)
                && transcript.contains(r#""model":"test-model""#),
            "the child's own initial provider/model is recorded: {transcript}"
        );
    }

    #[tokio::test]
    async fn a_finished_child_transcript_is_skipped_when_persistence_is_disabled() {
        let mut child_context = Context::new();
        child_context.push_assistant("unpersisted".to_string(), AssistantStatus::Complete);
        write_child_transcript(
            &ChildTranscriptTarget {
                parent_store: None,
                engine_config: Some(test_engine_config()),
            },
            "task",
            &child_context,
        )
        .await;
    }

    #[tokio::test]
    async fn run_child_sends_unstructured_finish_result_when_finish_never_called() {
        // The child's mock script produces only plain assistant text (no
        // tool call), so the child loop goes idle without ever calling
        // `agent_finish`; `run_child` must synthesize an "unstructured"
        // `FinishResult` from the child's last assistant text.
        let provider = Arc::new(MockProvider::new(
            vec!["child's final answer".to_string()],
            Duration::from_millis(0),
        ));
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, test_engine_config()));
        let engines = HashMap::from([("default".to_string(), engine)]);
        let profile = ProfileConfig {
            default_engine: "default".to_string(),
            compact_engine: None,
            agents: HashMap::from([(
                "default".to_string(),
                AgentConfig {
                    engine: None,
                    allowed_tools: None,
                    timeout_secs: None,
                    persona: None,
                    can_spawn: false,
                },
            )]),
            ..ProfileConfig::default()
        };

        let (finish_tx, mut finish_rx) = mpsc::channel::<FinishResult>(1);
        let (forward_ui_tx, mut forward_ui_rx) = mpsc::channel::<UiUpdate>(16);
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();
        let request = SpawnRequest {
            run_id,
            call_id,
            task: "do the child task".to_string(),
            briefing: None,
            agent_type: "default".to_string(),
            label: "label".to_string(),
        };
        let cancel = CancellationToken::new();

        let child_role_config = ChildRoleConfig {
            run_id,
            call_id,
            briefing: request.briefing.clone(),
            parent_cancel: cancel.clone(),
            subagent_ui_tx: forward_ui_tx.clone(),
            finish_tx: finish_tx.clone(),
        };
        let (child_loop, child_ui_rx) = AgentFactory::build(
            &engines,
            &[],
            false,
            false,
            false,
            &profile,
            &request.agent_type,
            Role::Child(child_role_config),
            None,
        )
        .expect("child agent should construct successfully");

        drive_child_to_completion(
            child_loop,
            child_ui_rx,
            request.task,
            run_id,
            call_id,
            cancel,
            finish_tx,
            forward_ui_tx,
            ChildTranscriptTarget {
                parent_store: None,
                engine_config: None,
            },
        )
        .await;

        // Drain (and ignore) any forwarded UI updates from the child's run.
        while forward_ui_rx.try_recv().is_ok() {}

        let finish_result = tokio::time::timeout(Duration::from_secs(1), finish_rx.recv())
            .await
            .expect("finish_rx should receive promptly")
            .expect("finish_tx should not be dropped without sending");
        assert_eq!(finish_result.run_id, run_id);
        assert_eq!(finish_result.call_id, call_id);
        assert_eq!(finish_result.status, "unstructured");
        assert_eq!(
            finish_result.result.as_deref(),
            Some("child's final answer")
        );
    }
}
