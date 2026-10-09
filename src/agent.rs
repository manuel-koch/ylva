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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use uuid::Uuid;

use crate::llm::engine::Engine;
use crate::llm::{ToolEnvelope, ToolSchema};
use crate::store::Store;

pub mod child_agent_manager;
pub mod compaction;
mod context;
mod message;
mod protocol;
mod request;
mod runtime;
mod tool_execution;
mod tool_queue;
use request::start_request;
#[cfg(test)]
use request::ToolCallAccumulator;
#[cfg(test)]
use runtime::compaction_updates::{
    report_compaction_failure, send_compaction_applied, send_context_usage_estimate,
    sweep_compaction_and_report,
};
pub use tool_execution::PendingTool;
use tool_queue::spawn::resolve_spawn_subagent;
use tool_queue::{
    advance_tool_queue, bump_and_check_iteration_cap, inject_pending_steers, queued_tool_names,
    tool_output_token_limit,
};

pub mod tools;
pub(crate) use context::estimate_message_tokens;
#[cfg(test)]
use context::tool_schema_token_count;
pub use context::{Context, MessageTier};
pub use message::{
    AssistantStatus, Message, ToolAttachment, ToolCallRequest, ToolResult, UserMessageKind,
};
use protocol::log_agent_command;
pub use protocol::{
    AgentCommand, AgentEvent, AgentMode, ContextUsageSource, DeltaChunk, StreamEvent, UiUpdate,
};

use tools::{ToolCall, ToolRegistry};

/// Default number of LLM requests the tool-use loop starts within a single
/// user turn before the guard stops re-prompting. Each loop tracks its own
/// `iterations` counter independently, so a child tripping this guard neither
/// counts against nor is masked by the root's own counter. See
/// `UiUpdate::Error::run_id` for how the resulting error is attributed back
/// to whichever loop (root or child) actually exceeded it.
///
/// This is the *default* only: an agent type's `max_tool_iterations`
/// (`ProfileConfig::max_tool_iterations_for`) overrides it per loop, root and
/// child alike, through `AgentLoop::with_max_tool_iterations`.
pub const DEFAULT_MAX_TOOL_USE_ITERATIONS: usize = 50;

/// Maximum distinct tool calls accumulated while parsing one assistant response.
/// Exceeding this backstop rejects the whole response before execution; it does
/// not increase the separate six-position execution cap.
pub const MAX_TOOL_CALLS_PER_RESPONSE: usize = 12;

/// Maximum tool calls the harness executes from one assistant response, for
/// root and subagent runs and all tools alike. Only the first
/// `MAX_EXECUTED_TOOL_CALLS_PER_BATCH` requested positions are eligible; the
/// excess is answered with an explicit not-executed error. Distinct from
/// [`MAX_TOOL_CALLS_PER_RESPONSE`], which rejects an abusive response whole.
pub const MAX_EXECUTED_TOOL_CALLS_PER_BATCH: usize = 6;

/// Smallest per-call result allowance, in estimated tokens, worth executing a
/// context-budgeted tool for. A call whose share of the remaining allowance is
/// smaller is skipped instead of started.
pub const MINIMUM_USEFUL_RESULT_TOKENS: usize = 256;

/// Estimated tokens reserved for the error response of every still-queued
/// call, so skipping them later can never overflow the request ceiling.
const ERROR_RESPONSE_FRAMING_TOKENS: usize = 192;
/// Prefix marking an asynchronous, out-of-band harness notice (e.g. "a
/// spawned subagent finished") sent via `Context::push_async_harness_notice`
/// as a `role:"user"` message rather than `role:"system"`. Not every
/// provider/model tolerates a `system`-role message appearing after the
/// conversation's leading system-prompt block — many open-weight chat
/// templates (Mistral-family included) only support `system` as the very
/// first message and reject/mis-render one injected mid-conversation (seen
/// in practice as an HTTP 400 "Invalid request" from `mistralai/mistral-nemo`
/// via OpenRouter's "Io Net" backend). `role:"user"` is safe everywhere, so
/// this marker lets the model tell the harness's own notice apart from
/// something the human actually typed.
pub const HARNESS_NOTICE_MARKER: &str = "[harness notice — not from the user]";

/// System-prompt snippet, injected immediately after the persona/initial
/// system prompt, that documents the uniform tool-response envelope a model
/// must parse and the per-response tool-call limit. The described JSON matches the schema derived from
/// [`crate::llm::ToolEnvelope`] — the very struct used to serialize every
/// `role:"tool"` message — so the instruction can never drift from the wire
/// shape. It is a
/// `LazyLock` because the schema is generated at first use.
pub static TOOL_PROTOCOL_SYSTEM_PROMPT: LazyLock<String> = LazyLock::new(|| {
    let mut schema_json =
        serde_json::to_value(schemars::schema_for!(ToolEnvelope)).unwrap_or_default();
    // The struct's doc comment is the document: schemars mirrors it into the
    // schema's `description`, which we lift out as the leading phrase, then
    // drop so the embedded schema stays a pure shape (no comment text inside).
    let lead = schema_json
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if let Some(object) = schema_json.as_object_mut() {
        object.remove("description");
    }
    let shape = serde_json::to_string_pretty(&schema_json).unwrap_or_default();
    format!(
        "{lead}\n{shape}\n\nA single assistant response may contain at most {MAX_TOOL_CALLS_PER_RESPONSE} distinct tool calls. Keep each batch within this limit; if more work is needed, continue in a later response.\n\nRequest small batches: only the first {MAX_EXECUTED_TOOL_CALLS_PER_BATCH} tool calls of a response are executed. Remaining calls, and calls whose results would not fit the remaining context budget, are not executed and return an error; resubmit only those skipped calls in a smaller batch."
    )
});

/// A fully assembled, validated tool call, ready for the outer loop to queue
/// for serial (one-at-a-time) execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledToolCall {
    pub id: Uuid,
    pub name: String,
    pub arguments: Value,
}

/// Default execution timeout for file-system tools.
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 5;

/// Non-blocking agent loop.
///
/// State machine: `Idle` receives a `Prompt` and starts a stream; further
/// `Prompt`s while busy are queued as **steers** and injected at the next
/// natural point — they never abort a live stream. A stream that assembles
/// tool calls transitions to `ToolPending`, drains the pending queue
/// serially (each call awaiting `ApproveTool`/`DenyTool`), and re-prompts the
/// model once the queue empties. Only `Stop` aborts, via the current request's
/// or current tool's `CancellationToken`.
///
/// Approval protocol is uniform across all tools: every call pauses at
/// `ToolPending` and waits for an approve/deny command — the loop holds no
/// approval policy, the driver decides.
///
/// Indicates whether an `AgentLoop` is acting as the root (top-level) agent
/// or as a child agent spawned by a root agent.
pub enum Role {
    /// The top-level agent for a session.
    Root,
    /// A child agent spawned by a root agent, with the wiring back to its
    /// parent: an optional briefing, the parent-side cancellation token that
    /// cascades a parent stop into this child, the channel for forwarding
    /// live tokens up to the parent, and the channel the `agent_finish` tool (the
    /// only tool a child is guaranteed to have) uses to report its outcome.
    Child(ChildRoleConfig),
}

/// Fields carried by [`Role::Child`]; consumed by `AgentFactory::create` to
/// wire the child's `AgentLoop` and its `agent_finish` tool.
pub struct ChildRoleConfig {
    /// Identifies this child run to the parent's `ChildAgentManager`.
    pub run_id: Uuid,
    /// The parent's `spawn_subagent` tool-call id this run is tied to.
    pub call_id: Uuid,
    /// Persisted parent conversation associated with this child's requests.
    pub session_id: Option<Uuid>,
    /// Optional context/background injected as an additional system message
    /// ahead of the child's task.
    pub briefing: Option<String>,
    /// Cancelled by the parent's `ChildAgentManager::cancel_all` (e.g. on
    /// `AgentCommand::Stop`); cascades into this child's own cancellation.
    pub parent_cancel: CancellationToken,
    /// Forwards this child's `UiUpdate`s up to the parent, for live-token and
    /// context-usage display in the child drill-in view.
    pub subagent_ui_tx: mpsc::Sender<UiUpdate>,
    /// Sent to by this child's `agent_finish` tool when called.
    pub finish_tx: mpsc::Sender<crate::child_agent_manager::FinishResult>,
}

/// The outer loop task is the sole writer of `Context` (`single-writer`
/// rule): no `Arc<Mutex<Context>>` anywhere. `run()` takes the loop's channel
/// receivers out of `self`; the caller keeps `self` (and thus `cmd_tx`) alive
/// and shuts the loop down by **dropping the `AgentLoop`**, after which the
/// task returns the final committed `Context` from `handle.await`.
pub struct AgentLoop {
    engine: Arc<dyn Engine>,
    /// The configured profile engine name for a directly constructed engine.
    /// Routers report their current active name dynamically instead.
    configured_engine_name: Option<String>,
    inherits_parent_runtime: bool,
    registry: Arc<ToolRegistry>,
    cmd_tx: mpsc::Sender<AgentCommand>,
    cmd_rx: Option<mpsc::Receiver<AgentCommand>>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<(u64, AgentEvent)>,
    agent_rx: Option<mpsc::Receiver<(u64, AgentEvent)>>,
    /// Set via [`AgentLoop::with_child_role`] for a `Role::Child` agent;
    /// firing it cascades a parent stop into this loop.
    parent_cancel: Option<CancellationToken>,
    /// Set via [`AgentLoop::with_child_role`]; injected as an extra system
    /// message ahead of the child's task.
    briefing: Option<String>,
    /// Root-only initial system message loaded from `SOUL.md`. Child loops
    /// leave this unset and retain [`Context::new`]'s compatibility prompt.
    system_prompt: Option<String>,
    /// Set via [`AgentLoop::with_child_role`] from `config.run_id` for a
    /// `Role::Child` agent; `None` for `Role::Root`. Stamped onto every
    /// `UiUpdate::ToolPending`/`ClarifyPending` this loop emits so the root
    /// (which forwards a child's `UiUpdate`s up to its own driver) and the
    /// driver can tell which agent a pending approval belongs to and route
    /// the verdict back via `AgentCommand::ForChild`.
    own_run_id: Option<Uuid>,
    /// Set via [`AgentLoop::with_child_agent_manager`] for a `Role::Root`
    /// agent whose `AgentConfig::can_spawn` is `true`.
    child_agent_manager: Option<crate::child_agent_manager::ChildAgentManager>,
    finish_rx: Option<mpsc::Receiver<crate::child_agent_manager::FinishResult>>,
    subagent_ui_rx: Option<mpsc::Receiver<UiUpdate>>,
    /// Set via [`AgentLoop::with_max_tool_iterations`]: the effective
    /// tool-use iteration limit for this loop, applied by the shared
    /// iteration guard. Defaults to [`DEFAULT_MAX_TOOL_USE_ITERATIONS`].
    max_tool_iterations: usize,
    /// Engine used for background prefix summaries. Factory-built loops bind
    /// implicit selection to their conversation Engine runtime and explicit selection
    /// to its configured named engine. `None` disables compaction.
    compaction_engine: Option<Arc<dyn Engine>>,
    compaction_engine_name: Option<String>,
    compaction_follows_conversation_engine: bool,
    /// Wall-clock budget configured by `[compaction].timeout_sec` for
    /// requests made by `compaction_engine`.
    compaction_timeout: Duration,
    /// Framing instruction for background summary requests: the resolved
    /// `COMPACTION.md` layered like `SOUL.md`, or its built-in default.
    compaction_summary_prompt: String,
    startup_compaction_checks: Vec<compaction::AgentCompactionCapacityCheck>,
    /// Set via [`AgentLoop::with_agent_type`]: the root agent type this loop
    /// was created for. Stamped into the `meta` header of the replacement
    /// session `/clear` starts, so a cleared conversation keeps advertising
    /// the type it runs under. `None` for loops built outside the factory
    /// (tests, the legacy `run_cli` path).
    agent_type: Option<String>,
    /// Set via [`AgentLoop::with_store`] when persistence is enabled: the
    /// session file this loop's committed messages are appended to.
    store: Option<Store>,
    /// Session identity attached to model requests, including child requests
    /// that are attributed to their persisted parent conversation.
    session_id: Option<Uuid>,
    /// Shared with the running loop so drivers observe the successor store
    /// after `AgentCommand::ClearContext` rolls the session over.
    current_store: Arc<Mutex<Option<Store>>>,
    #[cfg(test)]
    test_batch_cap: usize,
}

impl AgentLoop {
    pub fn new(
        engine: Arc<dyn Engine>,
        registry: Arc<ToolRegistry>,
    ) -> (Self, mpsc::Receiver<UiUpdate>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>(10);
        let (ui_tx, ui_rx) = mpsc::channel::<UiUpdate>(100);
        let (agent_tx, agent_rx) = mpsc::channel::<(u64, AgentEvent)>(10);
        let current_store = Arc::new(Mutex::new(None));
        let agent = AgentLoop {
            engine,
            configured_engine_name: None,
            inherits_parent_runtime: false,
            #[cfg(test)]
            test_batch_cap: MAX_EXECUTED_TOOL_CALLS_PER_BATCH,
            registry,
            cmd_tx,
            cmd_rx: Some(cmd_rx),
            ui_tx,
            agent_tx,
            agent_rx: Some(agent_rx),
            parent_cancel: None,
            briefing: None,
            system_prompt: None,
            own_run_id: None,
            child_agent_manager: None,
            finish_rx: None,
            subagent_ui_rx: None,
            max_tool_iterations: DEFAULT_MAX_TOOL_USE_ITERATIONS,
            compaction_engine: None,
            compaction_engine_name: None,
            compaction_follows_conversation_engine: false,
            compaction_timeout: Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
            compaction_summary_prompt: crate::config::compaction_prompt::DEFAULT_COMPACTION_PROMPT
                .to_string(),
            startup_compaction_checks: Vec::new(),
            agent_type: None,
            store: None,
            session_id: None,
            current_store,
        };
        (agent, ui_rx)
    }

    /// Record the profile engine name for display when this loop wraps a
    /// directly constructed (non-router) engine.
    pub fn with_configured_engine_name(mut self, engine_name: impl Into<String>) -> Self {
        self.configured_engine_name = Some(engine_name.into());
        self
    }

    /// Mark this loop as sharing its conversation Engine runtime with its
    /// parent agent. Display projections use this to reflect later parent
    /// engine switches in a drilled-in child conversation.
    pub fn with_inherited_parent_runtime(mut self, inherited: bool) -> Self {
        self.inherits_parent_runtime = inherited;
        self
    }

    /// Wires this loop as a `Role::Child` agent: `config.parent_cancel`
    /// cascades a parent stop into this loop, and `config.briefing` (if any)
    /// is injected as an extra system message ahead of the child's task.
    /// `config.run_id` is retained as `own_run_id`, stamped onto this loop's
    /// `ToolPending`/`ClarifyPending` updates (see `own_run_id`).
    /// `call_id`/`subagent_ui_tx`/`finish_tx` are consumed by the
    /// caller to build the child's `agent_finish` tool and are not otherwise held
    /// on the loop itself.
    pub fn with_child_role(mut self, config: ChildRoleConfig) -> Self {
        self.parent_cancel = Some(config.parent_cancel);
        self.briefing = config.briefing;
        self.own_run_id = Some(config.run_id);
        self.session_id = config.session_id;
        self
    }

    /// Set the root agent's initial system message.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    /// Set the engine background prefix summaries run on.
    pub fn with_compaction_engine(mut self, engine: Arc<dyn Engine>) -> Self {
        self.compaction_engine_name = engine.active_engine_name();
        self.compaction_engine = Some(engine);
        self
    }

    /// Set the summary engine and record whether it follows the active
    /// conversation Engine runtime or is explicitly bound to a named engine.
    pub fn with_compaction_engine_selection(
        mut self,
        engine: Arc<dyn Engine>,
        engine_name: Option<String>,
        follows_conversation: bool,
    ) -> Self {
        self.compaction_engine = Some(engine);
        self.compaction_engine_name = engine_name;
        self.compaction_follows_conversation_engine = follows_conversation;
        self
    }

    /// Set the wall-clock budget for summary requests made by the configured
    /// compaction engine.
    pub fn with_compaction_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.compaction_timeout = Duration::from_secs(timeout_secs);
        self
    }

    /// Set the framing instruction background summary requests carry (the
    /// loaded `COMPACTION.md`). Defaults to the built-in prompt, so loops
    /// built outside the factory still summarize with a sane instruction.
    pub fn with_compaction_summary_prompt(mut self, summary_prompt: impl Into<String>) -> Self {
        self.compaction_summary_prompt = summary_prompt.into();
        self
    }

    pub(crate) fn with_startup_compaction_checks(
        mut self,
        checks: Vec<compaction::AgentCompactionCapacityCheck>,
    ) -> Self {
        self.startup_compaction_checks = checks;
        self
    }

    /// Record the root agent type this loop was created for; see the
    /// `agent_type` field.
    pub fn with_agent_type(mut self, agent_type: impl Into<String>) -> Self {
        self.agent_type = Some(agent_type.into());
        self
    }

    /// Set this loop's effective tool-use iteration limit (the per-agent
    /// `max_tool_iterations` resolved from the profile). A limit of `0` is
    /// rejected by profile validation; here it falls back to the default so
    /// a programmatically built loop can never start with a dead budget.
    pub fn with_max_tool_iterations(mut self, max_tool_iterations: usize) -> Self {
        self.max_tool_iterations = if max_tool_iterations == 0 {
            DEFAULT_MAX_TOOL_USE_ITERATIONS
        } else {
            max_tool_iterations
        };
        self
    }

    /// This loop's tool registry, as resolved by `AgentFactory::build` (per-
    /// `agent_type` `allowed_tools`, `include_spawn_subagent`, etc.).
    /// `pub(crate)` because the only readers are same-crate: the TUI Driver
    /// (which captures the resolved tool names once at startup for the
    /// `/tools` slash command) and tests inspecting which tools ended up
    /// registered.
    pub(crate) fn registry(&self) -> &Arc<ToolRegistry> {
        &self.registry
    }

    /// Wires this loop as a `Role::Root` agent that can spawn children.
    /// `finish_rx`/`subagent_ui_rx` are the receiving ends of the channels
    /// whose sending ends `manager` hands to each child it spawns.
    pub fn with_child_agent_manager(
        mut self,
        manager: crate::child_agent_manager::ChildAgentManager,
        finish_rx: mpsc::Receiver<crate::child_agent_manager::FinishResult>,
        subagent_ui_rx: mpsc::Receiver<UiUpdate>,
    ) -> Self {
        self.child_agent_manager = Some(manager);
        self.finish_rx = Some(finish_rx);
        self.subagent_ui_rx = Some(subagent_ui_rx);
        self
    }

    /// The engine this loop runs on. Used by the driver to answer engine-level
    /// queries (the model list) without reaching through the factory.
    pub fn engine(&self) -> Arc<dyn Engine> {
        self.engine.clone()
    }

    #[cfg(test)]
    pub(crate) fn compaction_engine_config(&self) -> Option<crate::llm::EngineConfig> {
        self.compaction_engine
            .as_ref()
            .map(|engine| engine.config())
    }

    #[cfg(test)]
    pub(crate) fn compaction_timeout(&self) -> Duration {
        self.compaction_timeout
    }

    /// Snapshot the engine identity currently used by this agent.
    ///
    /// A router's active named engine takes precedence over the configured
    /// initial name so a `/models` connection switch is reflected immediately.
    pub fn engine_display_info(&self) -> crate::llm::EngineDisplayInfo {
        let mut display = crate::llm::EngineDisplayInfo::from_engine(self.engine.as_ref());
        if display.engine_name.is_empty() {
            display.engine_name = self.configured_engine_name.clone().unwrap_or_default();
        }
        display.inherits_parent_runtime = self.inherits_parent_runtime;
        if let Some(compaction_engine) = &self.compaction_engine {
            let compaction_config = compaction_engine.config();
            let compaction_engine_name = if self.compaction_follows_conversation_engine {
                display.engine_name.clone()
            } else {
                self.compaction_engine_name
                    .clone()
                    .or_else(|| compaction_engine.active_engine_name())
                    .unwrap_or_default()
            };
            let shares_conversation = self.compaction_follows_conversation_engine
                || Arc::ptr_eq(&self.engine, compaction_engine);
            display.compaction_engine = Some(crate::llm::CompactionEngineDisplayInfo {
                engine_name: compaction_engine_name,
                provider_connection: compaction_config.provider_connection,
                model: compaction_config.model,
                context_length: compaction_engine.effective_context_length(),
                follows_conversation: self.compaction_follows_conversation_engine,
                shares_conversation,
            });
        }
        display
    }

    /// Enables persistence for this loop: every message the outer loop
    /// commits to its `Context` is also appended to `store`'s session file.
    pub fn with_store(mut self, store: Store) -> Self {
        *self
            .current_store
            .lock()
            .expect("current store mutex is not poisoned") = Some(store.clone());
        self.session_id = Some(store.session_id());
        self.store = Some(store);
        self
    }

    /// The session store this loop persists to, when persistence is enabled.
    /// Cloning is cheap: every clone shares one writer task (see [`Store`]).
    pub fn store(&self) -> Option<Store> {
        self.current_store
            .lock()
            .expect("current store mutex is not poisoned")
            .clone()
    }

    pub async fn send_command(&self, cmd: AgentCommand) {
        let _ = self.cmd_tx.send(cmd).await;
    }

    /// Clone of the command channel sender, for consumers (e.g. the TUI
    /// driver and the SIGINT relay) that send commands without a `&self`.
    pub fn command_sender(&self) -> mpsc::Sender<AgentCommand> {
        self.cmd_tx.clone()
    }
}

#[cfg(test)]
mod tests;
