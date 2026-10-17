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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::agent::tools::agent_finish::AgentFinishToolConfig;
use crate::agent::tools::spawn_subagent::AgentTypeInfo;
use crate::agent::tools::{
    default_registry, AutoApproveTool, ToolRegistryConfig, DEFAULT_CLI_TOOLS, DEFAULT_TUI_TOOLS,
};
use crate::agent::{AgentLoop, Context, Message, Role, SubagentRoleConfig, UiUpdate};
use crate::config::ProfileConfig;
use crate::llm::engine::{Engine, EngineConfig, EngineRouter};
use crate::llm::ProviderRegistry;
use crate::sandbox::Sandbox;
use crate::session_title::TitleAttempt;
use crate::store::Store;
use crate::subagent_manager::{FinishResult, FinishResultOrigin, SpawnRequest, SubagentManager};

/// Constructs [`AgentLoop`] instances from a named set of [`Engine`]s and a
/// [`ProfileConfig`].
///
/// `Role::Root` agents whose resolved `AgentConfig::can_spawn` is `true` are
/// wired with a [`SubagentManager`] capable of running `Role::Subagent`
/// agents via this very factory (recursively, one level deep — a subagent's own
/// registry never contains `spawn_subagent`).
pub struct AgentFactory {
    skills: SkillRuntime,
    engines: HashMap<String, Arc<dyn Engine>>,
    /// The set of tools whose approval prompt is bypassed (auto-approved)
    /// without user confirmation. Intended for automated / e2e testing where
    /// no human is present to approve.
    auto_approve: HashSet<AutoApproveTool>,
    /// Explicit list of tool names to register. Defaults to [`DEFAULT_CLI_TOOLS`].
    allowed_tools: Vec<String>,
    /// Session store handed to every `Role::Root` agent this factory builds,
    /// when persistence is enabled.
    store: Option<Store>,
    /// Resolved `SOUL.md` text used only for newly created root contexts.
    system_prompt: Option<String>,
}

#[derive(Clone, Default)]
struct SkillRuntime {
    catalog: Arc<crate::skills::SkillCatalog>,
    interactive: bool,
}

#[derive(Debug)]
pub struct RootAgentTypeSelection {
    pub agent_type: String,
    pub override_changes_active_type: bool,
}

pub fn select_root_agent_type(
    explicit_agent_type: Option<&str>,
    stored_agent_type: Option<&str>,
    resumed: bool,
    profile: &ProfileConfig,
) -> Result<RootAgentTypeSelection, String> {
    let mut available: Vec<_> = profile.agents.keys().map(String::as_str).collect();
    available.sort_unstable();
    let available = available.join(", ");
    let agent_type = explicit_agent_type
        .or(stored_agent_type)
        .unwrap_or(crate::DEFAULT_PROFILE_NAME);
    if !profile.agents.contains_key(agent_type) {
        return Err(if explicit_agent_type.is_some() {
            format!("unknown agent type '{agent_type}' selected by --agent; available types: {available}")
        } else {
            format!("session stored agent type '{agent_type}' is not configured in the selected profile; pass --agent <type> to choose one of: {available}")
        });
    }
    Ok(RootAgentTypeSelection {
        agent_type: agent_type.to_string(),
        override_changes_active_type: resumed
            && explicit_agent_type.is_some()
            && agent_type != stored_agent_type.unwrap_or(crate::DEFAULT_PROFILE_NAME),
    })
}

impl AgentFactory {
    pub fn new(engines: HashMap<String, Arc<dyn Engine>>) -> Self {
        AgentFactory {
            skills: SkillRuntime::default(),
            engines,
            auto_approve: HashSet::new(),
            allowed_tools: DEFAULT_CLI_TOOLS.iter().map(|s| s.to_string()).collect(),
            store: None,
            system_prompt: None,
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
            profile.compaction_engine_name(),
            profile.utility_engine_name(),
        ] {
            anyhow::ensure!(
                engines.contains_key(required),
                "profile references engine '{required}', which no [engines.*] table defines"
            );
        }
        validate_profile_agent_engines(&engines, profile)?;
        for agent_type in profile.agents.keys() {
            let compaction_engine_name = profile.compaction_engine_name_for(agent_type);
            anyhow::ensure!(
                engines.contains_key(compaction_engine_name),
                "agent type '{agent_type}' references compaction engine '{compaction_engine_name}', but no such engine is configured"
            );
        }
        validate_profile_agent_compaction_timeouts(&engines, profile)?;
        validate_profile_agent_tool_names(profile)?;
        Ok(AgentFactory::new(engines))
    }

    /// Validate every configured agent type against the engines available to
    /// this factory, and every configured tool name (profile-level and
    /// per-agent-type) against the registered tool set. This runs during
    /// startup so an unused agent type cannot hide an invalid engine or tool
    /// name reference until a subagent is spawned.
    pub fn validate_profile_agent_engines(&self, profile: &ProfileConfig) -> anyhow::Result<()> {
        validate_profile_agent_engines(&self.engines, profile)?;
        validate_profile_agent_tool_names(profile)
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

    /// Return the raw configured engine named by `name`.
    ///
    /// Unlike an [`EngineRouter`], this does not participate in active model
    /// or provider-connection selection. Utility requests use this accessor
    /// so they cannot mutate the conversation engine's Engine runtime selection.
    pub fn engine(&self, name: &str) -> Option<Arc<dyn Engine>> {
        self.engines.get(name).cloned()
    }

    /// Enable persistence: every `Role::Root` agent built by this factory
    /// appends its committed messages to `store`'s session file, and every
    /// subagent run it spawns appends `subagent_start`/`subagent_result` records plus
    /// its own transcript alongside it.
    pub fn with_store(mut self, store: Store) -> Self {
        self.store = Some(store);
        self
    }

    /// Configuration owner for future roots, without retaining a session lock.
    pub fn without_store(&self) -> Self {
        Self {
            skills: self.skills.clone(),
            engines: self.engines.clone(),
            auto_approve: self.auto_approve.clone(),
            allowed_tools: self.allowed_tools.clone(),
            store: None,
            system_prompt: self.system_prompt.clone(),
        }
    }

    /// Build an isolated configured runtime for an explicitly opened root store.
    pub fn create_root_for_store(
        &self,
        profile: &ProfileConfig,
        agent_type: &str,
        store: Store,
    ) -> anyhow::Result<(AgentLoop, mpsc::Receiver<UiUpdate>)> {
        self.validate_profile_skills(profile)?;
        let engines = self
            .engines
            .iter()
            .map(|(name, engine)| Ok((name.clone(), engine.configured_copy()?)))
            .collect::<anyhow::Result<HashMap<_, _>>>()?;
        Self::build(
            &self.skills,
            &engines,
            &self.allowed_tools,
            &self.auto_approve,
            profile,
            agent_type,
            Role::Root,
            Some(store),
            self.system_prompt.clone(),
            None,
        )
    }

    /// Set the initial system message for root agents built by this factory.
    /// The resolved agent-type persona and system-prompt body are appended
    /// when an agent is created. The same global text is inherited by subagents.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    /// Supply the process snapshot and the host's actual approval capability.
    /// Library construction otherwise starts with an empty catalog.
    pub fn with_skill_catalog(
        mut self,
        catalog: Arc<crate::skills::SkillCatalog>,
        interactive: bool,
    ) -> Self {
        self.skills = SkillRuntime {
            catalog,
            interactive,
        };
        self
    }

    pub fn validate_profile_skills(&self, profile: &ProfileConfig) -> anyhow::Result<()> {
        let sandbox = Arc::new(std::sync::RwLock::new(Sandbox::new()));
        let mut types: Vec<_> = profile.agents.keys().collect();
        types.sort();
        for agent_type in types {
            crate::skills::policy::CallableSkills::resolve(
                &self.skills.catalog,
                profile,
                agent_type,
                &self.allowed_tools,
                self.skills.interactive,
                sandbox.clone(),
            )?;
        }
        Ok(())
    }

    /// Enable automatic approval of the given tool's calls (no user prompt).
    pub fn with_auto_approve(mut self, tool: AutoApproveTool) -> Self {
        self.auto_approve.insert(tool);
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
    /// A subagent spawned by a root agent may inherit the root's active engine
    /// when both agents resolve the same named engine. This keeps provider and
    /// model switches made through `/models` effective for subagents spawned
    /// afterwards, while a subagent with a different engine override uses its own
    /// configured engine.
    ///
    /// A `Role::Root` agent whose `agent_type` resolves `can_spawn: true` is
    /// additionally wired with a [`SubagentManager`]; a `Role::Subagent` agent
    /// is wired via [`AgentLoop::with_subagent_role`] and has `agent_finish` (not
    /// `spawn_subagent`) registered in its tool registry.
    pub fn create(
        &self,
        profile: &ProfileConfig,
        agent_type: &str,
        role: Role,
    ) -> anyhow::Result<(AgentLoop, mpsc::Receiver<UiUpdate>)> {
        self.validate_profile_skills(profile)?;
        Self::build(
            &self.skills,
            &self.engines,
            &self.allowed_tools,
            &self.auto_approve,
            profile,
            agent_type,
            role,
            self.store.clone(),
            self.system_prompt.clone(),
            None,
        )
    }

    /// Shared construction logic for both the top-level `create` call and the
    /// `spawn_fn` closure a `SubagentManager` uses to start subagents — kept
    /// as a free function (rather than requiring `Arc<Self>`) so the closure
    /// can capture cheap, owned clones of everything it needs instead of the
    /// whole factory.
    #[expect(clippy::too_many_arguments)]
    fn build(
        skills: &SkillRuntime,
        engines: &HashMap<String, Arc<dyn Engine>>,
        allowed_tools: &[String],
        auto_approve: &HashSet<AutoApproveTool>,
        profile: &ProfileConfig,
        agent_type: &str,
        role: Role,
        store: Option<Store>,
        system_prompt: Option<String>,
        inherited_engine: Option<Arc<dyn Engine>>,
    ) -> anyhow::Result<(AgentLoop, mpsc::Receiver<UiUpdate>)> {
        let engine_name = resolve_engine_name(profile, agent_type);
        let inherits_parent_runtime = inherited_engine.is_some();

        let configured_engine = engines
            .get(engine_name)
            .ok_or_else(|| anyhow::anyhow!("unknown engine: {engine_name}"))?;
        let engine: Arc<dyn Engine> = if let Some(inherited_engine) = inherited_engine {
            inherited_engine
        } else if engines.len() == 1 {
            configured_engine.clone()
        } else {
            Arc::new(EngineRouter::new(engines.clone(), engine_name)?)
        };

        let subagent_role_config = match role {
            Role::Root => None,
            Role::Subagent(config) => Some(config),
        };
        let is_subagent = subagent_role_config.is_some();

        let can_spawn = subagent_role_config.is_none()
            && profile
                .agents
                .get(agent_type)
                .map(|agent_config| agent_config.can_spawn)
                .unwrap_or(false);
        let spawn_agent_types = if can_spawn {
            configured_agent_types(profile)
        } else {
            Vec::new()
        };

        let agent_finish_config =
            subagent_role_config
                .as_ref()
                .map(|config| AgentFinishToolConfig {
                    finish_tx: config.finish_tx.clone(),
                    run_id: config.run_id,
                    call_id: config.call_id,
                });

        // Per-`agent_type` `allowed_tools` (`profile.toml`'s `[agents.*]`
        // entries) takes precedence over the flat, factory-wide default when
        // configured, so a subagent spawned with a distinct `agent_type` can be
        // restricted to a genuinely different tool set than its parent (or
        // the root). Falls back to the flat list unchanged when unconfigured,
        // preserving today's behavior for every existing profile.
        let configured_allowed_tools = profile
            .agents
            .get(agent_type)
            .and_then(|agent_config| agent_config.allowed_tools.clone())
            .unwrap_or_else(|| allowed_tools.to_vec());

        // Per-tool sandbox rules resolve most-specific-wins, each tier fully
        // replacing the next (no merging across tiers): the agent type's own
        // `[agents.<type>.tools.<name>]` table, then the profile-wide
        // `[tools.<name>]` table, then (absent from both) the built-in default
        // floor handled by `default_registry`/`SandboxContext`.
        let sandbox = std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new()));
        let callable_skills = Arc::new(crate::skills::policy::CallableSkills::resolve(
            &skills.catalog,
            profile,
            agent_type,
            allowed_tools,
            skills.interactive,
            sandbox.clone(),
        )?);
        let agent_type_tools = profile
            .agents
            .get(agent_type)
            .map(|agent_config| &agent_config.tools);
        let tool_sandbox_rules = profile
            .tools
            .keys()
            .chain(agent_type_tools.into_iter().flat_map(|tools| tools.keys()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter_map(|tool_name| {
                let resolved = agent_type_tools
                    .and_then(|tools| tools.get(tool_name))
                    .or_else(|| profile.tools.get(tool_name))?;
                Some((tool_name.clone(), resolved.rules.clone()))
            })
            .collect();
        let registry = Arc::new(default_registry(
            sandbox,
            ToolRegistryConfig {
                skills: callable_skills.clone(),
                auto_approve: auto_approve.clone(),
                allowed_tools: configured_allowed_tools,
                include_spawn_subagent: can_spawn,
                spawn_agent_types,
                agent_finish: agent_finish_config,
                tool_sandbox_rules,
                web_search: profile.web_search.clone(),
                web_fetch: profile.web_fetch.clone(),
            },
        ));
        let (agent_loop, ui_rx) = AgentLoop::new(Arc::clone(&engine), registry);
        let mut agent_loop = agent_loop
            .with_configured_engine_name(engine_name)
            .with_inherited_parent_runtime(inherits_parent_runtime)
            .with_agent_type(agent_type)
            .with_max_tool_iterations(profile.max_tool_iterations_for(agent_type));
        if let Some(system_prompt) = compose_agent_system_prompt(
            system_prompt.as_deref(),
            profile.agents.get(agent_type),
            &callable_skills,
        ) {
            agent_loop = agent_loop.with_system_prompt(system_prompt);
        }

        if let Some(subagent_role_config) = subagent_role_config {
            agent_loop = agent_loop.with_subagent_role(subagent_role_config);
        } else if can_spawn {
            let mut manager = Self::build_subagent_manager(
                skills.clone(),
                engines.clone(),
                allowed_tools.to_vec(),
                auto_approve.clone(),
                profile.clone(),
                agent_loop.current_store_handle(),
                engine.clone(),
                engine_name.to_string(),
                system_prompt,
            );
            if let Some(store) = store.clone() {
                manager.manager = manager.manager.with_store(store);
            }
            agent_loop = agent_loop.with_subagent_manager(
                manager.manager,
                manager.finish_rx,
                manager.subagent_ui_rx,
            );
        }

        let explicit_compaction_engine_name = profile
            .explicit_compaction_engine_name_for(agent_type)
            .map(str::to_string);
        let compaction_engine = match explicit_compaction_engine_name.as_deref() {
            Some(compaction_engine_name) => {
                engines.get(compaction_engine_name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "agent type '{agent_type}' references compaction engine '{compaction_engine_name}', but no such engine is configured"
                    )
                })?.configured_copy().map_err(|error| {
                    anyhow::anyhow!("cannot isolate compaction engine '{compaction_engine_name}': {error}")
                })?
            }
            None => engine.clone(),
        };
        let compaction_engine_name = explicit_compaction_engine_name
            .clone()
            .unwrap_or_else(|| engine_name.to_string());
        {
            let summary_prompt = crate::config::compaction_prompt::load_compaction_prompt(
                crate::config::layered::current_workspace_root().as_deref(),
            )?;
            agent_loop = agent_loop
                .with_compaction_engine_selection(
                    compaction_engine,
                    Some(compaction_engine_name),
                    explicit_compaction_engine_name.is_none(),
                )
                .with_compaction_timeout_secs(profile.compaction_timeout_secs_for(agent_type))
                .with_compaction_summary_prompt(summary_prompt);
            if !is_subagent {
                let mut agent_types: Vec<_> = profile
                    .agents
                    .keys()
                    .filter(|configured_type| configured_type.as_str() != agent_type)
                    .collect();
                agent_types.sort();
                let checks = agent_types
                    .into_iter()
                    .map(|configured_type| {
                        let conversation_engine_name =
                            resolve_engine_name(profile, configured_type);
                        let conversation_engine = engines
                            .get(conversation_engine_name)
                            .cloned()
                            .ok_or_else(|| {
                                anyhow::anyhow!("unknown engine: {conversation_engine_name}")
                            })?;
                        let explicit_summary_engine =
                            profile.explicit_compaction_engine_name_for(configured_type);
                        let summary_engine = match explicit_summary_engine {
                            Some(summary_engine_name) => {
                                engines.get(summary_engine_name).ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "unknown compaction engine: {summary_engine_name}"
                                    )
                                })?.configured_copy().map_err(|error| {
                                    anyhow::anyhow!("cannot isolate compaction engine '{summary_engine_name}': {error}")
                                })?
                            }
                            None => conversation_engine.clone(),
                        };
                        Ok(crate::agent::compaction::AgentCompactionCapacityCheck {
                            agent_type: configured_type.clone(),
                            conversation_engine,
                            summary_engine,
                        })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                agent_loop = agent_loop.with_startup_compaction_checks(checks);
            }
        }

        // Only the root agent streams into the session file; a subagent's
        // transcript is written once, at completion, into its own
        // sub-session file.
        if !is_subagent {
            if let Some(store) = store {
                agent_loop = agent_loop.with_store(store);
            }
        }

        Ok((agent_loop, ui_rx))
    }

    /// Build a [`SubagentManager`] whose `spawn_fn` recursively calls
    /// [`Self::build`] with `Role::Subagent` for each spawn request, runs the
    /// resulting subagent `AgentLoop` to completion, and reports the outcome
    /// through `finish_tx` — unless the subagent's own `agent_finish` tool already
    /// did so (checked from the subagent's final `Context`), or the subagent was
    /// cancelled by the parent (checked via the per-subagent `CancellationToken`).
    ///
    /// The root engine and its resolved name are captured so a subagent using the
    /// same named engine shares the root's active router; subagents resolving a
    /// different name still use their configured engine selection.
    #[expect(clippy::too_many_arguments)]
    fn build_subagent_manager(
        skills: SkillRuntime,
        engines: HashMap<String, Arc<dyn Engine>>,
        allowed_tools: Vec<String>,
        auto_approve: HashSet<AutoApproveTool>,
        profile: ProfileConfig,
        parent_store: Arc<std::sync::Mutex<Option<Store>>>,
        parent_engine: Arc<dyn Engine>,
        parent_engine_name: String,
        system_prompt: Option<String>,
    ) -> BuiltSubagentManager {
        let (finish_tx, finish_rx) = mpsc::unbounded_channel::<FinishResult>();
        let (subagent_ui_tx, subagent_ui_rx) = mpsc::unbounded_channel::<UiUpdate>();
        let max_concurrent_subagents = profile.max_concurrent_subagents_limit();

        let spawn_fn: crate::subagent_manager::SpawnFn = Box::new(
            move |request: SpawnRequest,
                  cancel: CancellationToken,
                  forward_ui_tx: mpsc::UnboundedSender<UiUpdate>| {
                let engines = engines.clone();
                let skills = skills.clone();
                let allowed_tools = allowed_tools.clone();
                let auto_approve = auto_approve.clone();
                let profile = profile.clone();
                let finish_tx = finish_tx.clone();
                let parent_store = parent_store.lock().expect("current store mutex").clone();
                let parent_engine = parent_engine.clone();
                let parent_engine_name = parent_engine_name.clone();
                let system_prompt = system_prompt.clone();
                let utility_engine = engines.get(profile.utility_engine_name()).cloned();
                let subagent_engine_name =
                    resolve_engine_name(&profile, &request.agent_type).to_string();
                let run_id = request.run_id;
                let call_id = request.call_id;
                let session_id = parent_store.as_ref().map(Store::session_id);
                let subagent_span = tracing::info_span!(
                    "subagent",
                    run_id = %run_id,
                    parent_session_id = session_id.map(|identity| identity.to_string()).as_deref(),
                );
                let inherited_engine =
                    (subagent_engine_name == parent_engine_name).then_some(parent_engine);

                // The subagent's `agent_finish` tool reports into this private
                // channel; `drive_subagent_to_completion` forwards it to the shared
                // `finish_tx` only after the subagent's UI has fully drained.
                let (subagent_finish_tx, subagent_finish_rx) = mpsc::channel::<FinishResult>(1);
                let subagent_role_config = SubagentRoleConfig {
                    run_id,
                    call_id,
                    session_id,
                    briefing: request.briefing.clone(),
                    parent_cancel: cancel.clone(),
                    subagent_ui_tx: forward_ui_tx.clone(),
                    finish_tx: subagent_finish_tx,
                };

                // Constructing the subagent `AgentLoop` is synchronous
                // (`AgentFactory::build` performs no I/O), so it runs here,
                // directly in the `spawn_fn` closure, before `tokio::spawn` —
                // this is what lets the manager retain the subagent's own
                // `command_sender` for `SubagentManager::running_command_sender`
                // (see `SpawnOutcome`). Only the "drive to completion" body
                // below (unchanged apart from being split out) moves into the
                // spawned task.
                let built = AgentFactory::build(
                    &skills,
                    &engines,
                    &allowed_tools,
                    &auto_approve,
                    &profile,
                    &request.agent_type,
                    Role::Subagent(subagent_role_config),
                    None,
                    system_prompt,
                    inherited_engine,
                );
                let (subagent_loop, subagent_ui_rx) = match built {
                    Ok(built) => built,
                    Err(error) => {
                        tracing::error!(%run_id, %error, "failed to construct subagent");
                        // The failure report goes to the unbounded shared finish
                        // channel, so it is never dropped and needs no await.
                        let _ = finish_tx.send(FinishResult {
                            run_id,
                            call_id,
                            origin: FinishResultOrigin::Runtime,
                            status: "failure".to_string(),
                            result: None,
                            error: Some(error.to_string()),
                            sources: vec![],
                        });
                        return crate::subagent_manager::SpawnOutcome {
                            command_sender: None,
                            handle: tokio::spawn(async {}),
                        };
                    }
                };

                let subagent_engine = subagent_loop.engine();
                let subagent_engine_config = Some(subagent_engine.config());
                let subagent_engine_display = subagent_loop.engine_display_info();
                let _ = forward_ui_tx.send(UiUpdate::SubagentEngineConfigured {
                    run_id,
                    engine: subagent_engine_display,
                });

                let command_sender = subagent_loop.command_sender();
                let task = request.task.clone();
                let handle = tokio::spawn(
                    async move {
                        drive_subagent_to_completion(
                            subagent_loop,
                            subagent_ui_rx,
                            subagent_finish_rx,
                            task,
                            run_id,
                            call_id,
                            cancel,
                            finish_tx,
                            forward_ui_tx,
                            SubagentTranscriptTarget {
                                parent_store,
                                engine_config: subagent_engine_config,
                                utility_engine,
                            },
                        )
                        .await;
                    }
                    .instrument(subagent_span),
                );

                crate::subagent_manager::SpawnOutcome {
                    command_sender: Some(command_sender),
                    handle,
                }
            },
        );

        BuiltSubagentManager {
            manager: SubagentManager::new(subagent_ui_tx, spawn_fn, max_concurrent_subagents),
            finish_rx,
            subagent_ui_rx,
        }
    }
}

fn compose_agent_system_prompt(
    global_system_prompt: Option<&str>,
    agent_config: Option<&crate::config::AgentConfig>,
    skills: &crate::skills::policy::CallableSkills,
) -> Option<String> {
    let mut prompt_sections = Vec::new();
    if let Some(global_system_prompt) = global_system_prompt {
        prompt_sections.push(global_system_prompt);
    }
    if let Some(agent_config) = agent_config {
        if let Some(persona) = agent_config.persona.as_deref() {
            prompt_sections.push(persona);
        }
    }
    let advertisement = skills.advertisement();
    if let Some(advertisement) = &advertisement {
        if prompt_sections.is_empty() {
            prompt_sections.push(crate::config::soul::DEFAULT_SOUL);
        }
        prompt_sections.push(advertisement);
    }
    (!prompt_sections.is_empty()).then(|| prompt_sections.join("\n\n"))
}

/// Return value of [`AgentFactory::build_subagent_manager`]: the manager
/// plus the receiving ends of the channels its subagents send on, which the
/// caller stores on the parent `AgentLoop`.
struct BuiltSubagentManager {
    manager: SubagentManager,
    finish_rx: mpsc::UnboundedReceiver<FinishResult>,
    subagent_ui_rx: mpsc::UnboundedReceiver<UiUpdate>,
}

/// Drives an already-constructed subagent `AgentLoop` to completion and reports
/// its outcome via `finish_tx`, unless the subagent was cancelled (its own
/// `cancel` token fired) or it already reported via its own `agent_finish`
/// tool call (detected from its final `Context`). Split out of the former
/// `run_subagent` so the synchronous construction step (`AgentFactory::build`)
/// can run in the `spawn_fn` closure itself, ahead of `tokio::spawn`, letting
/// the caller retain the subagent's `command_sender` (see
/// `build_subagent_manager`).
/// Where a finished subagent's own transcript is persisted: the **parent**
/// session's store (`None` when persistence is disabled) plus the subagent's own
/// resolved engine config, stamped into the sub-session's `meta` header.
struct SubagentTranscriptTarget {
    parent_store: Option<Store>,
    /// The subagent's own resolved provider-connection/model (which can differ from the
    /// parent's), recorded in the subagent session's `meta` header and its
    /// initial `ModelChanged` event. `None` when the engine name failed to
    /// resolve — should not happen given `from_profile`'s upfront
    /// validation, but persisting nothing is safer than persisting a
    /// misleading empty pair.
    engine_config: Option<EngineConfig>,
    /// The independently resolved utility engine used to title this subagent
    /// transcript without routing through the subagent's active engine.
    utility_engine: Option<Arc<dyn Engine>>,
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

fn validate_profile_agent_engines(
    engines: &HashMap<String, Arc<dyn Engine>>,
    profile: &ProfileConfig,
) -> anyhow::Result<()> {
    for (agent_type, agent_config) in &profile.agents {
        if let Some(engine_name) = agent_config.engine.as_deref() {
            anyhow::ensure!(
                engines.contains_key(engine_name),
                "agent type '{agent_type}' references engine '{engine_name}', but no such engine is configured"
            );
        }
    }
    for agent_type in profile.agents.keys() {
        let compaction_engine_name = profile.compaction_engine_name_for(agent_type);
        anyhow::ensure!(
            engines.contains_key(compaction_engine_name),
            "agent type '{agent_type}' references compaction engine '{compaction_engine_name}', but no such engine is configured"
        );
    }
    Ok(())
}

fn validate_profile_agent_compaction_timeouts(
    _engines: &HashMap<String, Arc<dyn Engine>>,
    profile: &ProfileConfig,
) -> anyhow::Result<()> {
    for (agent_type, agent_config) in &profile.agents {
        let Some(compaction_config) = &agent_config.compaction else {
            continue;
        };
        anyhow::ensure!(
            compaction_config.timeout_sec != Some(0),
            "agent type '{agent_type}' compaction timeout_sec must be greater than 0"
        );
    }
    Ok(())
}

/// Validate that every tool name configured in the profile's `[tools.<name>]`
/// table and each agent type's `[agents.<type>.tools.<name>]` table is one of
/// the registered tool names, so a typo or renamed tool fails fast at
/// startup instead of silently never applying its configured rules.
fn validate_profile_agent_tool_names(profile: &ProfileConfig) -> anyhow::Result<()> {
    for tool_name in profile.tools.keys() {
        anyhow::ensure!(
            DEFAULT_TUI_TOOLS.contains(&tool_name.as_str()),
            "profile-level [tools.{tool_name}] table names unknown tool '{tool_name}'; \
             registered tools are: {}",
            DEFAULT_TUI_TOOLS.join(", ")
        );
    }
    for (agent_type, agent_config) in &profile.agents {
        for tool_name in agent_config.tools.keys() {
            anyhow::ensure!(
                DEFAULT_TUI_TOOLS.contains(&tool_name.as_str()),
                "agent type '{agent_type}' [agents.{agent_type}.tools.{tool_name}] table \
                 names unknown tool '{tool_name}'; registered tools are: {}",
                DEFAULT_TUI_TOOLS.join(", ")
            );
        }
    }
    Ok(())
}

fn implicit_engine_config(
    provider_connection_name: &str,
    default_model: Option<&str>,
) -> EngineConfig {
    EngineConfig {
        provider_connection: provider_connection_name.to_string(),
        model: default_model.unwrap_or_default().to_string(),
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

/// Persist a finished subagent's transcript once, into its own sub-session file
/// under the parent's `project_key`. A failure to
/// write is logged and swallowed: losing a subagent transcript must never fail
/// the run that produced it.
async fn write_subagent_transcript(
    target: &SubagentTranscriptTarget,
    run_id: uuid::Uuid,
    context: &Context,
) {
    let Some(parent_store) = &target.parent_store else {
        return;
    };
    let provider_connection_and_model = target
        .engine_config
        .as_ref()
        .map(|config| (config.provider_connection.as_str(), config.model.as_str()));
    match parent_store
        .write_subagent_transcript_for_run_store(
            run_id,
            provider_connection_and_model,
            context.messages(),
        )
        .await
    {
        Ok(subagent_store) => {
            let subagent_session_id = subagent_store.session_id();
            tracing::debug!(%run_id, parent_session_id = %parent_store.session_id(), %subagent_session_id, "persisted subagent transcript");
            if let Some(utility_engine) = &target.utility_engine {
                let _ = TitleAttempt::default()
                    .trigger_with_engine(subagent_store, utility_engine.clone());
            }
        }

        Err(error) => {
            tracing::warn!(%run_id, parent_session_id = %parent_store.session_id(), %error, "failed to persist subagent transcript");
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
    profile.engine_name_for(agent_type)
}

/// Return the deterministic catalog shown to a root model when it can spawn
/// subagents. The built-in `default` type remains available even for profiles
/// constructed in memory without an explicit `[agents.default]` table.
fn configured_agent_types(profile: &ProfileConfig) -> Vec<AgentTypeInfo> {
    let mut agent_types: Vec<_> = profile
        .agents
        .iter()
        .map(|(name, config)| AgentTypeInfo {
            name: name.clone(),
            description: config.description.clone(),
        })
        .collect();
    if !agent_types
        .iter()
        .any(|agent_type| agent_type.name == crate::config::DEFAULT_PROFILE_NAME)
    {
        agent_types.push(AgentTypeInfo {
            name: crate::config::DEFAULT_PROFILE_NAME.to_string(),
            description: None,
        });
    }
    agent_types.sort_by(|left, right| left.name.cmp(&right.name));
    agent_types
}

/// Tags a subagent's streamed text and thinking with the subagent's own `run_id`
/// before the update enters the shared subagent channel. The root then
/// attributes each fragment directly, so several subagents can stream at once.
fn attribute_subagent_update(run_id: uuid::Uuid, update: UiUpdate) -> UiUpdate {
    match update {
        UiUpdate::AppendText(text) => UiUpdate::SubagentToken {
            run_id,
            text,
            is_thinking: false,
        },
        UiUpdate::AppendThinking(text) => UiUpdate::SubagentToken {
            run_id,
            text,
            is_thinking: true,
        },
        other => other,
    }
}

#[expect(clippy::too_many_arguments)]
async fn drive_subagent_to_completion(
    mut subagent_loop: AgentLoop,
    mut subagent_ui_rx: mpsc::Receiver<UiUpdate>,
    mut subagent_finish_rx: mpsc::Receiver<FinishResult>,
    task: String,
    run_id: uuid::Uuid,
    call_id: uuid::Uuid,
    cancel: CancellationToken,
    finish_tx: mpsc::UnboundedSender<FinishResult>,
    forward_ui_tx: mpsc::UnboundedSender<UiUpdate>,
    transcript_target: SubagentTranscriptTarget,
) {
    let forward_task = tokio::spawn(
        async move {
            while let Some(update) = subagent_ui_rx.recv().await {
                let _ = forward_ui_tx.send(attribute_subagent_update(run_id, update));
            }
        }
        .in_current_span(),
    );

    tracing::debug!(%run_id, tool_call_id = %call_id, "run_subagent: starting subagent AgentLoop");
    let handle = subagent_loop.run();
    subagent_loop
        .send_command(crate::agent::AgentCommand::Prompt(task))
        .await;

    let result = handle.await;
    tracing::debug!(%run_id, tool_call_id = %call_id, ok = result.is_ok(), "run_subagent: subagent AgentLoop::run() returned");
    // `handle.await` resolving only guarantees the subagent's `run()` future
    // completed — i.e. its own `ui_tx.send(...)` calls were enqueued, not
    // that `forward_task` has already popped and forwarded all of them. In
    // particular the subagent's terminal `agent_finish` `ToolDone` can still be
    // sitting unforwarded in `subagent_ui_rx` at this point. Aborting
    // `forward_task` here would race that drain and could drop the update,
    // leaving the forwarded bubble stuck in `Running` in the parent's UI.
    // Instead, drop `subagent_loop` (its last `ui_tx` clone) so `subagent_ui_rx`
    // closes once drained, and await `forward_task` so it only exits after
    // forwarding everything buffered.
    drop(subagent_loop);
    let _ = forward_task.await;
    tracing::debug!(%run_id, tool_call_id = %call_id, "run_subagent: forward_task drained and closed");

    if cancel.is_cancelled() {
        tracing::debug!(%run_id, tool_call_id = %call_id, "run_subagent: parent already cancelled this run, not reporting");
        // The parent already handled this cancellation synchronously
        // (`SubagentManager::cancel_all`); nothing left to report.
        return;
    }

    match result {
        Ok(Ok(context)) => {
            write_subagent_transcript(&transcript_target, run_id, &context).await;
            if subagent_context_called_finish(&context) {
                // The `agent_finish` tool buffered its result in
                // `subagent_finish_rx`; release it only now that its ToolDone has been
                // forwarded, so the parent never applies the finish first.
                if let Ok(agent_finish_result) = subagent_finish_rx.try_recv() {
                    let _ = finish_tx.send(agent_finish_result);
                }
                tracing::debug!(%run_id, tool_call_id = %call_id, "run_subagent: agent_finish reported result, not sending fallback");
                return;
            }
            let text = extract_last_assistant_text(&context);
            tracing::debug!(%run_id, tool_call_id = %call_id, "run_subagent: subagent never called agent_finish, sending unstructured fallback");
            let _ = finish_tx.send(FinishResult {
                run_id,
                call_id,
                origin: FinishResultOrigin::Runtime,
                status: "unstructured".to_string(),
                result: Some(text),
                error: None,
                sources: vec![],
            });
        }
        Ok(Err(error)) => {
            let _ = finish_tx.send(FinishResult {
                run_id,
                call_id,
                origin: FinishResultOrigin::Runtime,
                status: "failure".to_string(),
                result: None,
                error: Some(error.to_string()),
                sources: vec![],
            });
        }
        Err(join_error) => {
            let _ = finish_tx.send(FinishResult {
                run_id,
                call_id,
                origin: FinishResultOrigin::Runtime,
                status: "failure".to_string(),
                result: None,
                error: Some(format!("subagent task panicked: {join_error}")),
                sources: vec![],
            });
        }
    }
}

/// Whether the subagent's final context contains a completed `agent_finish` tool
/// call — if so, its own `AgentFinishTool::run` already sent a `FinishResult` and
/// the caller must not send a second, "unstructured" one.
fn subagent_context_called_finish(context: &Context) -> bool {
    context.messages().iter().any(|message| {
        matches!(
            message,
            Message::ToolResult {
                value: crate::agent::ToolResult { name, .. },
            } if name == crate::agent::tools::agent_finish::AgentFinishTool::NAME
        )
    })
}

/// The last `Message::Assistant` text in `context`, or an empty string when
/// none exists. Used to build the "unstructured fallback" `FinishResult`
/// when a subagent never called `agent_finish`.
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
mod tests;
