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
pub mod tools;

use tools::{ToolCall, ToolRegistry};

/// Maximum number of LLM requests the tool-use loop starts within a single user
/// turn before the guard stops re-prompting. Applies identically to the root
/// loop and to every child `AgentLoop` spawned via `spawn_subagent` — each
/// loop tracks its own `iterations` counter independently, so a child
/// tripping this guard neither counts against nor is masked by the root's own
/// counter. See `UiUpdate::Error::run_id` for how the resulting error is
/// attributed back to whichever loop (root or child) actually exceeded it.
/// Per-engine configurability is a later (Phase 5) concern; this
/// module-level default is the Phase 3 SSOT.
pub const MAX_TOOL_USE_ITERATIONS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    Idle,
    /// Prompt sent to the provider; waiting for the first `StreamStarted`
    /// event. The agent is busy — a second message typed now is a steer.
    Pending,
    Streaming,
    ToolPending,
    /// A tool has been approved and is actively executing.
    ToolRunning,
    ClarifyPending,
    /// User requested abort (double Ctrl+C); `AgentCommand::Stop` has been
    /// sent. The TUI waits for the resulting `StreamCancelled` / `Idle`
    /// transition before quitting.
    Aborting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    Prompt(String),
    Stop {
        reason: String,
    },
    ApproveTool {
        id: Uuid,
    },
    DenyTool {
        id: Uuid,
        reason: String,
    },
    AnswerClarify(String),
    /// Abort the pending clarify call without an answer.
    /// The agent records a tool error and continues, so the LLM can react.
    AbortClarify,
    /// Routes `command` to the child agent identified by `run_id` instead of
    /// the receiving loop's own state. Only a `Role::Root` loop's dispatch
    /// handles this variant: it looks up `run_id` in its
    /// `ChildAgentManager` and forwards `*command` verbatim into that
    /// child's own command channel — the child's own loop logic never sees
    /// `ForChild` itself, only the plain, unwrapped `command`. If `run_id`
    /// is stale (the child already finished/was cancelled), the command is
    /// dropped and a warning is logged; this can't cause a hang, since a
    /// child that's already gone isn't waiting on anything.
    ForChild {
        run_id: Uuid,
        command: Box<AgentCommand>,
    },
    /// `/clear` (blueprint §6): drop the conversation and continue in a
    /// brand-new persisted session.
    ///
    /// Handled here rather than in the TUI Driver because the §4C ownership
    /// rule makes the outer loop the only mutator of `Context` — and, through
    /// it, the only writer to the session store.
    ClearContext,
    /// Persist a structured `model_changed` record directly to the session
    /// store (bypassing `Context`'s conversation-message path). This is the
    /// machine-readable history entry a resumed session (or any tool reading
    /// the JSONL) relies on for `/models`' switch. The human-readable
    /// confirmation is shown only as a TUI notice (`AppState::push_notice`)
    /// — it is deliberately never added to the conversation: a `role:
    /// "system"` message injected mid-conversation is rejected by some
    /// providers/chat templates (see `HARNESS_NOTICE_MARKER`'s doc comment),
    /// and `model_changed` already gives replay/resume everything it needs.
    RecordModelChanged {
        provider_connection: String,
        model: String,
    },
}

fn agent_command_name(command: &AgentCommand) -> &'static str {
    match command {
        AgentCommand::Prompt(_) => "prompt",
        AgentCommand::Stop { .. } => "stop",
        AgentCommand::ApproveTool { .. } => "approve_tool",
        AgentCommand::DenyTool { .. } => "deny_tool",
        AgentCommand::AnswerClarify(_) => "answer_clarify",
        AgentCommand::AbortClarify => "abort_clarify",
        AgentCommand::ForChild { .. } => "for_child",
        AgentCommand::ClearContext => "clear_context",
        AgentCommand::RecordModelChanged { .. } => "record_model_changed",
    }
}

fn log_agent_command(command: &AgentCommand, mode: AgentMode, has_stream: bool, has_tool: bool) {
    let command_name = agent_command_name(command);
    match command {
        AgentCommand::Prompt(text) | AgentCommand::AnswerClarify(text) => {
            tracing::debug!(
                command = command_name,
                ?mode,
                has_stream,
                has_tool,
                text_len = text.len(),
                "agent command received"
            );
        }
        AgentCommand::Stop { reason } => {
            tracing::debug!(
                command = command_name,
                ?mode,
                has_stream,
                has_tool,
                reason_len = reason.len(),
                "agent command received"
            );
        }
        AgentCommand::ApproveTool { id } => {
            tracing::debug!(
                command = command_name,
                ?mode,
                has_stream,
                has_tool,
                tool_call_id = %id,
                "agent command received"
            );
        }
        AgentCommand::DenyTool { id, reason } => {
            tracing::debug!(
                command = command_name,
                ?mode,
                has_stream,
                has_tool,
                tool_call_id = %id,
                reason_len = reason.len(),
                "agent command received"
            );
        }
        AgentCommand::ForChild { run_id, command } => {
            tracing::debug!(
                command = command_name,
                ?mode,
                has_stream,
                has_tool,
                run_id = %run_id,
                nested_command = agent_command_name(command),
                "agent command received"
            );
        }
        AgentCommand::RecordModelChanged {
            provider_connection,
            model,
        } => {
            tracing::debug!(
                command = command_name,
                ?mode,
                has_stream,
                has_tool,
                provider_connection_len = provider_connection.len(),
                model_len = model.len(),
                "agent command received"
            );
        }
        AgentCommand::AbortClarify | AgentCommand::ClearContext => {
            tracing::debug!(
                command = command_name,
                ?mode,
                has_stream,
                has_tool,
                "agent command received"
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    StreamDone {
        partial: String,
    },
    StreamCancelled {
        partial: String,
    },
    StreamError(String),
    ToolCall {
        calls: Vec<AssembledToolCall>,
    },
    ToolDone {
        id: Uuid,
        name: String,
        result: String,
        display_result: Option<String>,
        result_metadata: Option<serde_json::Value>,
        result_metadata_schema: Option<serde_json::Value>,
        summary: Vec<(String, String)>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
        attachments: Vec<ToolAttachment>,
    },
    ToolError {
        id: Uuid,
        name: String,
        error: String,
        partial_output: Option<String>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
    },
    ToolKilled {
        id: Uuid,
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiUpdate {
    AppendText(String),
    StreamStarted,
    StreamDone,
    StreamCancelled,
    /// The agent is idle after a command or terminal event that did not
    /// produce a stream or tool-specific completion update.
    AgentIdle,
    Error {
        message: String,
        /// `None` when this error originates in the root's own loop;
        /// `Some(run_id)` identifies which child raised it (see
        /// `AgentLoop::own_run_id`). See `UiUpdate::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    ToolStart {
        name: String,
        id: Uuid,
        arguments: Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
        /// See `UiUpdate::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    ToolPending {
        name: String,
        id: Uuid,
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
        /// `None` when this is the root's own pending tool call; `Some(run_id)`
        /// identifies which child raised it (see `AgentLoop::own_run_id`).
        /// Drivers use this to route the resulting `ApproveTool`/`DenyTool`
        /// command back to the correct agent loop via `AgentCommand::ForChild`.
        run_id: Option<Uuid>,
    },
    /// A still-running call reports an interim status update without
    /// transitioning to a terminal state — the bubble stays `Running`
    /// (spinner keeps going). Used by `spawn_subagent`: the call is not
    /// "done" merely because the child was accepted/queued, only once the
    /// child's `agent_finish` result (or cancellation) actually arrives does a
    /// `ToolDone` follow. Patches `title_arguments` on the existing
    /// `Running` bubble state; a no-op if the bubble isn't `Running`.
    ToolProgress {
        id: Uuid,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
    ClarifyPending {
        id: Uuid,
        question: String,
        context: Option<String>,
        choices: Option<Vec<String>>,
        /// See `UiUpdate::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    ToolDone {
        id: Uuid,
        name: String,
        result_section: crate::tui_reducer_state::ResultSection,
        summary: Vec<(String, String)>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
        /// See `UiUpdate::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    ToolError {
        id: Uuid,
        error: String,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
        /// See `UiUpdate::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A tool call was denied automatically by the sandbox before it started.
    /// Creates a `Denied` bubble directly — no `ToolStart` is ever sent.
    ToolAutoDenied {
        id: Uuid,
        name: String,
        title_arguments: Vec<(String, serde_json::Value)>,
        content_sections: Vec<crate::agent::tools::Section>,
        reason: String,
        /// See `UiUpdate::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    ToolKilled {
        id: Uuid,
        /// See `UiUpdate::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A steer (user message while busy) was queued. The TUI driver creates
    /// the pending bubble itself before sending the command.
    SteerQueued {
        /// `None` when this loop's own root steer queue changed; `Some(run_id)`
        /// identifies which child raised it (see `AgentLoop::own_run_id`).
        /// Today no driver ever routes a `Prompt` to a child (only
        /// `ApproveTool`/`DenyTool`/`AnswerClarify`/`AbortClarify` cross
        /// `AgentCommand::ForChild`), so `Some` is unreachable in practice —
        /// carried anyway so this can never silently misattribute to the
        /// root if that changes. See `UiUpdate::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// All previously queued steers have been injected into the LLM context.
    SteerApplied {
        /// See `UiUpdate::SteerQueued::run_id`.
        run_id: Option<Uuid>,
    },
    /// A fragment of extended-thinking / reasoning-token output arrived.
    /// Appended to `partial_thinking_text`, never to the assistant-text
    /// accumulator; flushed into a `ChatEntry::ThinkingMessage` bubble ahead
    /// of the assistant bubble on `StreamDone`.
    AppendThinking(String),
    /// A child agent actually started running (dequeued from
    /// `ChildAgentManager`'s FIFO, or started immediately). Emitted by
    /// `ChildAgentManager::start`. Drives the drill-in conversation model
    /// (Phase 6.10): the driver creates/updates the spawn-bubble and tracks
    /// `run_id` as the current child conversation, but does **not** switch
    /// `active_view` — the user stays in the parent view until they
    /// navigate manually or an interactive pause (`ToolPending`/
    /// `ClarifyPending` carrying this same `run_id`) forces the switch.
    ChildAgentStarted {
        run_id: Uuid,
        /// The `spawn_subagent` tool-call id this run belongs to (== the
        /// root `ToolBubble`'s `id`) — lets the TUI correlate this run to
        /// its specific bubble even when multiple `spawn_subagent` bubbles
        /// (active + queued, or already finished) coexist.
        call_id: Uuid,
        agent_type: String,
        task: String,
        label: String,
    },
    /// The child engine's provider-connection/model identity, emitted immediately after
    /// the child has been constructed so the TUI can label its conversation
    /// independently from the root engine.
    ChildAgentEngineConfigured {
        run_id: Uuid,
        engine: crate::llm::engine::EngineDisplayInfo,
    },
    /// A fragment of `run_id`'s own assistant text/thinking output,
    /// forwarded from that child's `AppendText`/`AppendThinking` and
    /// rendered into that child's own conversation transcript only — never
    /// mixed into the parent's own `partial_assistant_text`, nor into a
    /// different child's transcript (`run_id` disambiguates which one,
    /// since a queued child can start streaming while an earlier child's
    /// transcript is still being viewed).
    ChildAgentToken {
        run_id: Uuid,
        text: String,
        /// `true` when this fragment came from `AppendThinking` (extended
        /// reasoning), `false` for `AppendText` (visible reply text) — lets
        /// the child's own `TurnBuffer` route it to the right accumulator,
        /// mirroring how the root distinguishes `StreamChunk` from
        /// `StreamThinkingChunk`.
        is_thinking: bool,
    },
    /// The child agent run identified by `run_id` has finished
    /// (successfully, with an error, or via the "unstructured" fallback).
    /// `status`/`result` mirror `crate::child_agent_manager::FinishResult`.
    /// If the driver had auto-switched into `run_id`'s child view for an
    /// interactive pause, it auto-returns to the parent view now.
    ChildAgentDone {
        run_id: Uuid,
        status: String,
        result: Option<String>,
    },
}

/// A raw tool-call fragment emitted by a provider, to be assembled by the
/// agent's inner streaming task, never by the provider (blueprint §4A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaChunk {
    pub tool_call_index: usize,
    pub tool_call_id: Option<String>,
    pub function_name: Option<String>,
    pub arguments_fragment: Option<String>,
}

#[derive(Debug)]
pub enum StreamEvent {
    Text(String),
    /// A fragment of extended-thinking / reasoning-token output. Distinct
    /// from `Text` so it never contaminates the assistant text accumulator;
    /// it is purely a model capability (models must have extended-thinking
    /// / reasoning enabled to produce it) and is provider-agnostic.
    Thinking(String),
    Delta(DeltaChunk),
    Done,
    Cancelled,
    Error(anyhow::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantStatus {
    Complete,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallRequest {
    pub id: Uuid,
    pub name: String,
    pub arguments: Value,
    /// Tool-specific title arguments captured when the call is created.
    ///
    /// An empty list means that the call has no title arguments.
    #[serde(rename = "title_arguments", default)]
    pub persisted_title_arguments: Vec<(String, Value)>,
}

impl ToolCallRequest {
    /// Return the captured tool-specific title arguments.
    pub fn title_arguments(&self) -> Vec<(String, Value)> {
        self.persisted_title_arguments.clone()
    }
}

/// Provider-neutral media returned by a successful tool call.
///
/// The descriptor deliberately does not own the encoded bytes. It refers to
/// the containing successful [`ToolResult::result`] value, which remains the
/// canonical Base64 payload for persistence, replay, and text-only fallback.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolAttachment {
    Image {
        media_type: String,
    },
    File {
        media_type: String,
        /// A basename safe to send to a provider as the source filename.
        filename: String,
    },
}

/// Persisted data for one completed tool call.
///
/// This is the payload of [`Message::ToolResult`] so callers can pass the
/// complete result through the context without expanding it into another
/// positional argument list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolResult {
    pub id: Uuid,
    pub name: String,
    pub result: String,
    /// Media descriptors that reference this result's Base64 `result` value.
    /// The descriptors never store a second copy of that payload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ToolAttachment>,
    /// Optional UI result-section identifier. `None` means the standard
    /// plain-result section; specialized tools such as `clarify` use a
    /// different identifier so replay can preserve the live rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_section_id: Option<String>,
    /// Structured metadata emitted by the tool for this specific result.
    #[serde(default)]
    pub result_metadata: Option<serde_json::Value>,
    /// JSON Schema for the fields present in `result_metadata`.
    #[serde(default)]
    pub result_metadata_schema: Option<serde_json::Value>,
    /// Key/value metadata displayed in the completed tool title.
    #[serde(default)]
    pub summary: Vec<(String, String)>,
    /// Structured content rendered in the completed tool bubble.
    #[serde(default)]
    pub content_sections: Vec<crate::agent::tools::Section>,
    /// Elapsed execution time displayed in the completed tool title.
    #[serde(default)]
    pub duration: std::time::Duration,
}

/// Distinguishes user messages that were typed explicitly by the user from
/// those injected by the agent harness as model hints.
///
/// `Explicit` messages are rendered in the TUI conversation list and the
/// headless CLI output. `ImplicitToolNote` messages are sent to the model
/// as regular `role: "user"` turns but are **never rendered** in any UI —
/// they exist solely to give the model richer context (e.g. a denial reason).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserMessageKind {
    #[default]
    Explicit,
    ImplicitToolNote,
}

/// Persisted as one JSONL line by `crate::store` — hence the `serde`
/// derives. The discriminant is serialized as a `role` field
/// (`#[serde(tag = "role")]`) and **no** `deny_unknown_fields` is used, so a
/// session file written by an older or newer build still deserializes
/// (blueprint §4H decision 4).
///
/// The tool-result variant intentionally stays inline so the persisted
/// `#[serde(flatten)]` shape and the single-writer context API remain direct.
#[expect(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
        #[serde(default)]
        kind: UserMessageKind,
    },
    Assistant {
        content: String,
        status: AssistantStatus,
        #[serde(default)]
        tool_calls: Vec<ToolCallRequest>,
    },
    ToolResult {
        #[serde(flatten)]
        value: ToolResult,
    },
    ToolError {
        id: Uuid,
        name: String,
        error: String,
        #[serde(default)]
        duration: std::time::Duration,
        /// Structured content produced before the tool failed.
        #[serde(default)]
        content_sections: Vec<crate::agent::tools::Section>,
    },
}

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
/// must parse. The described JSON matches the schema derived from
/// [`crate::llm::ToolEnvelope`] — the very struct used to serialize every
/// `role:"tool"` message — so the instruction can never drift from the wire
/// shape (blueprint §4D: schema from typed data, never hand-written). It is a
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
    format!("{lead}\n{shape}")
});

#[derive(Debug, Default)]
pub struct Context {
    messages: Vec<Message>,
    /// Session store this context appends its committed messages to, when
    /// persistence is enabled (blueprint §4H). `None` for child agents and
    /// for runs started without a store.
    store: Option<Store>,
}

impl Context {
    pub fn new() -> Self {
        Self::with_system_prompt(crate::config::soul::DEFAULT_SOUL)
    }

    /// Construct a context with a caller-supplied initial system message.
    pub fn with_system_prompt(system_prompt: impl Into<String>) -> Self {
        Context {
            store: None,
            messages: vec![
                Message::System {
                    content: system_prompt.into(),
                },
                Message::System {
                    content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
                },
            ],
        }
    }

    /// Attach a session store, so every message committed from here on is
    /// also appended to the session file.
    ///
    /// The two system prompts installed by [`Context::new`] or
    /// [`Context::with_system_prompt`] are deliberately not persisted: they
    /// are rebuilt from the running binary on every resume, so storing them
    /// would pin a stale copy into the transcript.
    pub fn set_store(&mut self, store: Store) {
        match Store::read_messages(store.path()) {
            Ok(messages) => self.hydrate(messages),
            Err(error) => tracing::warn!(
                %error,
                path = %store.path().display(),
                "could not replay persisted messages"
            ),
        }
        self.store = Some(store);
    }

    /// Load committed messages into this context without writing them back.
    /// Existing context messages remain at the front; persisted messages are
    /// appended in store order. Malformed store lines are handled by the store's
    /// tolerant read path.
    pub fn hydrate(&mut self, messages: Vec<Message>) {
        self.messages.extend(messages);

        let mut completed_tool_calls = std::collections::HashSet::new();
        let mut tool_calls = Vec::new();
        for message in &self.messages {
            match message {
                Message::Assistant {
                    tool_calls: calls, ..
                } => {
                    tool_calls.extend(calls.iter().map(|call| (call.id, call.name.clone())));
                }
                Message::ToolResult { value } => {
                    completed_tool_calls.insert(value.id);
                }
                Message::ToolError { id, .. } => {
                    completed_tool_calls.insert(*id);
                }
                _ => {}
            }
        }
        for (id, name) in tool_calls {
            if !completed_tool_calls.contains(&id)
                && name != tools::spawn_subagent::SpawnSubagentTool::NAME
            {
                self.messages.push(Message::ToolError {
                    id,
                    name,
                    error: "Tool call did not complete before the session ended.".to_string(),
                    duration: std::time::Duration::ZERO,
                    content_sections: vec![],
                });
            }
        }
    }

    /// The session store this context currently appends to, when persistence
    /// is enabled. Cloning is cheap (see [`Store`]). Tracks `/clear`
    /// rollovers correctly — unlike a store handle captured once at loop
    /// startup — because it reads whichever store `set_store` last attached.
    pub fn store(&self) -> Option<Store> {
        self.store.clone()
    }

    /// The single commit point for the conversation: every message enters the
    /// context here, and is forwarded to the session store from here.
    ///
    /// Keeping the forward in one place upholds the ownership
    /// rule — only the outer loop mutates `Context`, so only the outer loop
    /// can produce a store event, and the on-disk order is the commit order.
    fn commit(&mut self, message: Message) {
        if let Some(store) = &self.store {
            store.record_message(&message);
        }
        self.messages.push(message);
    }

    pub fn push_user(&mut self, text: String) {
        self.commit(Message::User {
            content: text,
            kind: UserMessageKind::Explicit,
        });
    }

    /// Push an additional system message. Used to inject a `Role::Child`
    /// agent's `briefing` ahead of its task, after the standard system
    /// prompts `Context::new` already installed.
    pub fn push_system(&mut self, text: String) {
        self.commit(Message::System { content: text });
    }

    /// Push a model hint injected by the agent harness. The message is sent
    /// to the model as a regular `role: "user"` turn but is never rendered in
    /// any UI (TUI conversation list or headless CLI output).
    pub fn push_implicit_user(&mut self, text: String) {
        self.commit(Message::User {
            content: text,
            kind: UserMessageKind::ImplicitToolNote,
        });
    }

    /// Push an asynchronous, out-of-band harness notice (e.g. "a spawned
    /// subagent finished/was cancelled") that arrives mid-conversation, well
    /// after the leading system-prompt block. Sent as `push_implicit_user`
    /// (`role:"user"`, hidden from the UI) rather than `push_system`, and
    /// prefixed with `HARNESS_NOTICE_MARKER` so the model can tell it apart
    /// from a real user message — see that constant's doc comment for why
    /// `role:"system"` is unsafe here.
    pub fn push_async_harness_notice(&mut self, text: String) {
        self.push_implicit_user(format!("{HARNESS_NOTICE_MARKER}\n{text}"));
    }

    pub fn push_assistant(&mut self, content: String, status: AssistantStatus) {
        self.commit(Message::Assistant {
            content,
            status,
            tool_calls: Vec::new(),
        });
    }

    pub fn push_assistant_tool_call(
        &mut self,
        content: String,
        status: AssistantStatus,
        tool_calls: Vec<ToolCallRequest>,
    ) {
        self.commit(Message::Assistant {
            content,
            status,
            tool_calls,
        });
    }

    pub fn push_tool_result(&mut self, value: ToolResult) {
        self.commit(Message::ToolResult { value });
    }

    pub fn push_tool_error(
        &mut self,
        id: Uuid,
        name: String,
        error: String,
        partial_output: Option<&str>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
    ) {
        let llm_error = match partial_output.filter(|s| !s.is_empty()) {
            Some(output) => format!(
                "{error}\n\nThe tool produced the following output before being terminated:\n\
                 {output}"
            ),
            None => error,
        };
        self.commit(Message::ToolError {
            id,
            name,
            error: llm_error,
            duration,
            content_sections,
        });
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

/// A fully assembled, validated tool call, ready for the outer loop to queue
/// for serial (one-at-a-time) execution (blueprint §4A/§4B).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledToolCall {
    pub id: Uuid,
    pub name: String,
    pub arguments: Value,
}

/// A queued tool call awaiting approval, with its tool-defined timeout.
///
/// Once approved, [`PendingTool::execute`] runs the tool in its own task and
/// reports completion through `agent_tx` so the loop never blocks on a tool.
pub struct PendingTool {
    pub call: Box<dyn ToolCall>,
}

impl std::fmt::Debug for PendingTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingTool")
            .field("name", &self.call.name())
            .field("id", &self.call.id())
            .finish()
    }
}

/// Default execution timeout for file-system tools (blueprint §4D: 5s).
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 5;

impl PendingTool {
    /// Spawn the tool in its own task, bounded by `timeout_secs` (or the
    /// default) and a per-tool `CancellationToken`. Completion is reported via
    /// `agent_tx` (`ToolDone`/`ToolError`/`ToolKilled`); returns the token the
    /// outer loop stores as the running tool's cancel.
    pub fn execute(
        self,
        agent_tx: mpsc::Sender<(u64, AgentEvent)>,
        generation: u64,
    ) -> CancellationToken {
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let id = self.call.id();
        let name = self.call.name().to_string();
        let timeout_secs = self.call.timeout_secs();
        // Captured before `run()` consumes the call so failure events can
        // lead with the same body sections the pending/running bubble showed
        // (e.g. bash's `Label`/`Command`), keeping replay identical to live.
        let pending_content_sections = self.call.pending_content_sections();
        let span = tracing::info_span!("tool_execute", tool = %name, tool_call_id = %id);
        tokio::spawn(
            async move {
                let outcome = tokio::select! {
                    biased;
                    _ = run_cancel.cancelled() => {
                        let _ = agent_tx
                            .send((
                                generation,
                                AgentEvent::ToolKilled {
                                    id,
                                    name: name.clone(),
                                },
                            ))
                            .await;
                        return;
                    }
                    result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
                        let start = std::time::Instant::now();
                        // TODO: remove artificial delay
                        #[cfg(feature = "artificial-tool-delay")]
                        {
                            let delay_secs = 2
                                + (std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .subsec_nanos()
                                    % 3) as u64;
                            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                        }
                        // END TODO
                        let result = self.call.run().await;
                        let elapsed = start.elapsed();
                        (result, elapsed)
                    }) => result,
                };
                match outcome {
                    Ok((Ok(tool_output), duration)) => {
                        let result_metadata = tool_output.metadata().and_then(|m| {
                            if m.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                                None
                            } else {
                                Some(m)
                            }
                        });
                        let result_metadata_schema = result_metadata.as_ref().and_then(|_| {
                            tool_output
                                .metadata_schema()
                                .and_then(|schema| serde_json::to_value(schema).ok())
                        });
                        let result = tool_output.text();
                        let display_result = tool_output.display_text();
                        let summary = tool_output.display_summary();
                        let content_sections = tool_output.content_sections();
                        let attachments = tool_output.attachments();
                        let _ = agent_tx
                            .send((
                                generation,
                                AgentEvent::ToolDone {
                                    id,
                                    name,
                                    result,
                                    display_result,
                                    result_metadata,
                                    result_metadata_schema,
                                    summary,
                                    duration,
                                    content_sections,
                                    attachments,
                                },
                            ))
                            .await;
                    }
                    Ok((Err(error), duration)) => {
                        let partial_output = error
                            .partial_output()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                        let mut content_sections = pending_content_sections;
                        if let Some(output) = &partial_output {
                            content_sections.push(crate::agent::tools::Section::Content(
                                crate::agent::tools::ContentSection {
                                    label: Some("Partial result".to_string()),
                                    content: output.clone(),
                                    file_extension: None,
                                    line_number_offset: Some(0),
                                },
                            ));
                        }
                        let _ = agent_tx
                            .send((
                                generation,
                                AgentEvent::ToolError {
                                    id,
                                    name,
                                    error: error.to_string(),
                                    partial_output,
                                    duration,
                                    content_sections,
                                },
                            ))
                            .await;
                    }
                    Err(_elapsed) => {
                        let message = format!("Tool '{name}' timed out after {timeout_secs}s");
                        let _ = agent_tx
                            .send((
                                generation,
                                AgentEvent::ToolError {
                                    id,
                                    name,
                                    error: message,
                                    partial_output: None,
                                    duration: Duration::from_secs(timeout_secs),
                                    content_sections: pending_content_sections,
                                },
                            ))
                            .await;
                    }
                }
            }
            .instrument(span),
        );
        cancel
    }
}

/// Non-blocking agent loop (blueprint §4B).
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
/// approval policy, the driver decides (blueprint §4E, §4B).
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
    /// Optional context/background injected as an additional system message
    /// ahead of the child's task.
    pub briefing: Option<String>,
    /// Cancelled by the parent's `ChildAgentManager::cancel_all` (e.g. on
    /// `AgentCommand::Stop`); cascades into this child's own cancellation.
    pub parent_cancel: CancellationToken,
    /// Forwards this child's `UiUpdate`s up to the parent, for live-token
    /// display (blueprint §4G; rendering itself is Phase 6.10).
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
    /// Set via [`AgentLoop::with_store`] when persistence is enabled: the
    /// session file this loop's committed messages are appended to.
    store: Option<Store>,
    /// Shared with the running loop so drivers observe the successor store
    /// after `AgentCommand::ClearContext` rolls the session over.
    current_store: Arc<Mutex<Option<Store>>>,
}

/// Awaits the next `FinishResult` when `rx` is `Some`, else never resolves.
/// Lets `AgentLoop::run`'s `select!` treat "no `ChildAgentManager` wired" the
/// same as "channel open but empty" without an `Option`-shaped branch.
async fn recv_finish(
    rx: &mut Option<mpsc::Receiver<crate::child_agent_manager::FinishResult>>,
) -> Option<crate::child_agent_manager::FinishResult> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Awaits the next forwarded child `UiUpdate` when `rx` is `Some`, else
/// never resolves. See [`recv_finish`].
async fn recv_subagent_ui(rx: &mut Option<mpsc::Receiver<UiUpdate>>) -> Option<UiUpdate> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Awaits cancellation of `token` when `token` is `Some`, else never
/// resolves. See [`recv_finish`].
async fn cancelled_or_pending(token: &Option<CancellationToken>) {
    match token {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
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
            store: None,
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
        self
    }

    /// Set the root agent's initial system message.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
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

    /// Snapshot the engine identity currently used by this agent.
    ///
    /// A router's active named engine takes precedence over the configured
    /// initial name so a `/models` connection switch is reflected immediately.
    pub fn engine_display_info(&self) -> crate::llm::EngineDisplayInfo {
        let mut display = crate::llm::EngineDisplayInfo::from_engine(self.engine.as_ref());
        if display.engine_name.is_empty() {
            display.engine_name = self.configured_engine_name.clone().unwrap_or_default();
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

    pub fn run(&mut self) -> tokio::task::JoinHandle<anyhow::Result<Context>> {
        let engine = self.engine.clone();
        let registry = self.registry.clone();
        let ui_tx = self.ui_tx.clone();
        let agent_tx = self.agent_tx.clone();
        let mut cmd_rx = self
            .cmd_rx
            .take()
            .expect("AgentLoop run() must not be called more than once");
        let mut agent_rx = self
            .agent_rx
            .take()
            .expect("AgentLoop run() must not be called more than once");
        let parent_cancel = self.parent_cancel.take();
        let briefing = self.briefing.take();
        let system_prompt = self.system_prompt.clone();
        let own_run_id = self.own_run_id.take();
        let mut child_agent_manager = self.child_agent_manager.take();
        let mut finish_rx = self.finish_rx.take();
        let mut subagent_ui_rx = self.subagent_ui_rx.take();
        let store = self.store.clone();
        let current_store = Arc::clone(&self.current_store);
        // Retained past `Context::set_store` so `AgentCommand::ClearContext`
        // can roll the session over to a fresh file on the same store root.
        let store_for_commands = store.clone();
        // Stashes each in-flight `spawn_subagent` call's task/briefing content
        // sections (shown while pending/running) keyed by call id, so they
        // can be carried forward into the eventual real `ToolDone` — sent
        // once the child actually finishes or is cancelled — alongside its
        // result. Entries are removed as soon as that terminal `ToolDone` is
        // sent; see `resolve_spawn_subagent`, the `finish_rx` arm below, and
        // the `AgentCommand::Stop` handler's cancellation cleanup.
        let mut spawn_content_sections: HashMap<Uuid, Vec<crate::agent::tools::Section>> =
            HashMap::new();
        // Only a `Role::Child` loop has `parent_cancel` set; such a loop must
        // return as soon as it goes idle instead of waiting forever for
        // commands nobody will send (see the end of the loop body below).
        let is_child = parent_cancel.is_some();

        tokio::spawn(async move {
            let mut context = match system_prompt.as_deref() {
                Some(system_prompt) => Context::with_system_prompt(system_prompt),
                None => Context::new(),
            };
            if let Some(store) = store {
                context.set_store(store);
            }
            if let Some(briefing) = briefing {
                context.push_system(briefing);
            }
            if is_child {
                // Give the child a chance to budget its own tool calls: it
                // has no visibility into `advance_tool_queue`'s guard (it
                // cannot tell "was cut off" from "chose to stop"), so a smart
                // model can only avoid the cap by knowing about it up front
                // and wrapping up (calling `agent_finish`) well before it
                // hits.
                context.push_system(format!(
                    "You have a budget of at most {MAX_TOOL_USE_ITERATIONS} tool-use \
                     round(s) to complete this task. Work efficiently — batch related \
                     tool calls where possible — and call `agent_finish` with your \
                     result as soon as you have an answer, well before you run out of \
                     rounds. If you exhaust this budget without calling `agent_finish`, \
                     your work will be reported as failed and none of your findings will \
                     reach the caller."
                ));
            }
            let mut mode = AgentMode::Idle;
            let mut current_cancel: Option<CancellationToken> = None;
            let mut current_tool_cancel: Option<CancellationToken> = None;
            let mut pending_steers: VecDeque<String> = VecDeque::new();
            let mut pending_tools: VecDeque<PendingTool> = VecDeque::new();
            let mut iterations = 0usize;
            let mut clarify_started_at: Option<std::time::Instant> = None;
            // Bumped by `AgentCommand::ClearContext` so any `AgentEvent`
            // produced by a stream/tool started *before* the clear (and
            // still in flight when it happened — cancellation is
            // cooperative, not synchronous) is recognised as stale and
            // dropped when it eventually arrives, instead of being applied
            // to the unrelated `fresh_context` the clear installed. Every
            // `start_request`/`PendingTool::execute` call stamps the
            // `generation` active at spawn time onto its events; the
            // `agent_rx` arm below only processes an event whose stamped
            // generation still matches this counter.
            let mut generation: u64 = 0;
            // Populated by `AgentCommand::Stop`/`AgentCommand::ClearContext`
            // right after `child_agent_manager.cancel_all()`: the `run_id`s
            // of children cancelled but not yet confirmed terminal. A
            // cancelled child's task may still be mid-flight and send a real
            // `FinishResult` after cancellation was requested; `finish_rx`'s
            // arm consults this set to drop such stale results instead of
            // pushing their content into a `Context` (possibly a brand-new
            // one, post-`ClearContext`) they no longer belong to.
            let mut cancelled_run_ids: std::collections::HashSet<Uuid> =
                std::collections::HashSet::new();
            // Set by the `finish_rx` arm when the final outstanding child's
            // result lands while the root is mid-stream (`mode != Idle`): the
            // result is already in `context` (see the `finish_rx` arm's
            // comment), but the root can't react to it until its *own*
            // in-flight turn completes.
            // `StreamDone` below is that turn's completion point, so it must
            // check this flag and force a re-prompt — otherwise the root goes
            // Idle believing its just-streamed text is the final answer, the
            // driver (which tracks the same child via `pending_spawn_call_ids`)
            // sees zero pending children and ends the conversation, and the
            // child's result silently never reaches the model.
            let mut pending_child_finish_reprompt = false;
            // Set (only for a child loop) when `advance_tool_queue`'s
            // max-tool-use-iterations guard trips — see that function's doc
            // comment. Checked below at the `is_child && mode == Idle` tail
            // to report a clear `Err` (→ `status: "failure"`) instead of the
            // generic "unstructured" `Ok(context)` fallback.
            let mut iteration_cap_exceeded: Option<String> = None;
            // A child has no caller that can recover from a provider/stream
            // failure after the current request ends. Preserve that
            // authoritative error so the child returns `Err` instead of
            // being misclassified as an unstructured successful answer.
            let mut stream_error: Option<String> = None;

            loop {
                tokio::select! {
                    biased;
                    Some((event_generation, event)) = agent_rx.recv() => {
                        if event_generation != generation {
                            tracing::debug!(
                                event_generation,
                                generation,
                                ?event,
                                "dropping stale agent event from a superseded generation \
                                 (likely produced before an AgentCommand::ClearContext)"
                            );
                            continue;
                        }
                        match event {
                            AgentEvent::StreamDone { partial } => {
                                context.push_assistant(partial, AssistantStatus::Complete);
                                // Signal the driver only after the completed
                                // assistant message has entered Context and
                                // the session store. This ordering lets
                                // out-of-band consumers safely inspect the
                                // committed turn.
                                let _ = ui_tx.send(UiUpdate::StreamDone).await;
                                let steers_injected = inject_pending_steers(&mut pending_steers, &mut context);
                                if steers_injected {
                                    pending_child_finish_reprompt = false;
                                    let _ = ui_tx
                                        .send(UiUpdate::SteerApplied {
                                            run_id: own_run_id,
                                        })
                                        .await;
                                    mode = AgentMode::Streaming;
                                    current_cancel = Some(start_request(
                                        &context,
                                        engine.clone(),
                                        registry.schemas(),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                        generation,
                                        own_run_id,
                                    ));
                                } else if pending_child_finish_reprompt {
                                    pending_child_finish_reprompt = false;
                                    // Same iteration budget as any other
                                    // re-prompt — without this guard a model
                                    // whose replies never contain a tool call
                                    // (so this branch, not `advance_tool_queue`'s
                                    // queue-empty branch, is the one that
                                    // clears a finish notice deferred while it
                                    // was still streaming) could reprompt
                                    // forever, bypassing the cap entirely; see
                                    // `bump_and_check_iteration_cap`'s doc
                                    // comment.
                                    if !bump_and_check_iteration_cap(
                                        &mut iterations,
                                        &mut context,
                                        &mut mode,
                                        &ui_tx,
                                        own_run_id,
                                        &mut iteration_cap_exceeded,
                                    )
                                    .await
                                    {
                                        mode = AgentMode::Streaming;
                                        current_cancel = Some(start_request(
                                            &context,
                                            engine.clone(),
                                            registry.schemas(),
                                            ui_tx.clone(),
                                            agent_tx.clone(),
                                            generation,
                                            own_run_id,
                                        ));
                                    }
                                } else {
                                    mode = AgentMode::Idle;
                                }
                            }
                            AgentEvent::StreamCancelled { partial } => {
                                // Only commit the partial when this was a genuine
                                // interrupt; ignore stale cancellation from a replaced
                                // stream (current_cancel already Some again).
                                if current_cancel.is_none() {
                                    context.push_assistant(partial, AssistantStatus::Interrupted);
                                    mode = AgentMode::Idle;
                                }
                            }
                            AgentEvent::StreamError(error) => {
                                pending_steers.clear();
                                mode = AgentMode::Idle;
                                if is_child {
                                    stream_error = Some(error.clone());
                                }
                                tracing::error!(message = %error, "agent stream error");
                                let _ = ui_tx
                                    .send(UiUpdate::Error {
                                        message: error,
                                        run_id: own_run_id,
                                    })
                                    .await;
                            }
                            AgentEvent::ToolCall { calls } => {
                                let mut requests = Vec::with_capacity(calls.len());
                                for assembled in calls {
                                    let pending_call = registry.create_call(
                                        &assembled.name,
                                        assembled.id,
                                        assembled.arguments.clone(),
                                    );
                                    let persisted_title_arguments = pending_call
                                        .as_ref()
                                        .map_or_else(Vec::new, |call| call.title_arguments());
                                    requests.push(ToolCallRequest {
                                        id: assembled.id,
                                        name: assembled.name.clone(),
                                        arguments: assembled.arguments.clone(),
                                        persisted_title_arguments,
                                    });
                                    match pending_call {
                                        Some(call) => {
                                            pending_tools.push_back(PendingTool { call });
                                        }
                                        None => {
                                            let error =
                                                format!("Unknown tool '{}'", assembled.name);
                                            let _ = agent_tx
                                                .send((
                                                    generation,
                                                    AgentEvent::ToolError {
                                                        id: assembled.id,
                                                        name: assembled.name.clone(),
                                                        error,
                                                        partial_output: None,
                                                        duration: std::time::Duration::ZERO,
                                                        content_sections: vec![],
                                                    },
                                                ))
                                                .await;
                                        }
                                    }
                                }
                                context.push_assistant_tool_call(
                                    String::new(),
                                    AssistantStatus::Complete,
                                    requests,
                                );
                                mode = AgentMode::ToolPending;
                                advance_tool_queue(
                                    &mut pending_tools,
                                    &mut context,
                                    &mut mode,
                                    &mut current_cancel,
                                    &mut current_tool_cancel,
                                    &mut iterations,
                                    false,
                                    engine.clone(),
                                    Arc::clone(&registry),
                                    ui_tx.clone(),
                                    agent_tx.clone(),
                                    generation,
                                    &mut clarify_started_at,
                                    &mut pending_steers,
                                    &mut child_agent_manager,
                                    &mut spawn_content_sections,
                                    own_run_id,
                                    &mut pending_child_finish_reprompt,
                                    &mut iteration_cap_exceeded,
                                )
                                .await;
                            }
                            AgentEvent::ToolDone { id, name, result, display_result, result_metadata, result_metadata_schema, summary, duration, content_sections, attachments } => {
                                current_tool_cancel = None;
                                // `agent_finish` is only ever registered on a `Role::Child`
                                // registry; once it completes the child's ReAct loop
                                // exits unconditionally (exempt from the iteration
                                // cap) — the tool itself already reported the
                                // outcome to the parent via `finish_tx`.
                                let is_finish = name == tools::agent_finish::AgentFinishTool::NAME;
                                let result_section_id = if display_result.is_some() {
                                    None
                                } else {
                                    Some(
                                        crate::tui_reducer_state::result_section_ids::HIDDEN
                                            .to_string(),
                                    )
                                };
                                context.push_tool_result(
                                    ToolResult {
                                        id,
                                        name: name.clone(),
                                        result: result.clone(),
                                        attachments,
                                        result_section_id: result_section_id.clone(),
                                        result_metadata,
                                        result_metadata_schema,
                                        summary: summary.clone(),
                                        content_sections: content_sections.clone(),
                                        duration,
                                    },
                                );
                                let _ = ui_tx
                                    .send(UiUpdate::ToolDone {
                                        id,
                                        name,
                                        result_section: crate::tui_reducer_state::ResultSection {
                                            id: result_section_id
                                                .as_deref()
                                                .unwrap_or(
                                                    crate::tui_reducer_state::result_section_ids::RESULT,
                                                )
                                                .to_string(),
                                            content: display_result.unwrap_or_default(),
                                        },
                                        summary,
                                        duration,
                                        content_sections,
                                        run_id: own_run_id,
                                    })
                                    .await;
                                if is_finish {
                                    return Ok(context);
                                }
                                advance_tool_queue(
                                    &mut pending_tools,
                                    &mut context,
                                    &mut mode,
                                    &mut current_cancel,
                                    &mut current_tool_cancel,
                                    &mut iterations,
                                    true,
                                    engine.clone(),
                                    Arc::clone(&registry),
                                    ui_tx.clone(),
                                    agent_tx.clone(),
                                    generation,
                                    &mut clarify_started_at,
                                    &mut pending_steers,
                                    &mut child_agent_manager,
                                    &mut spawn_content_sections,
                                    own_run_id,
                                    &mut pending_child_finish_reprompt,
                                    &mut iteration_cap_exceeded,
                                )
                                .await;
                            }
                            AgentEvent::ToolError { id, name, error, partial_output, duration, content_sections } => {
                                current_tool_cancel = None;
                                tracing::error!(
                                    tool = %name,
                                    tool_call_id = %id,
                                    error = %error,
                                    "tool failed"
                                );
                                context.push_tool_error(
                                    id,
                                    name,
                                    error.clone(),
                                    partial_output.as_deref(),
                                    duration,
                                    content_sections.clone(),
                                );
                                let _ = ui_tx.send(UiUpdate::ToolError {
                                    id,
                                    error,
                                    duration,
                                    content_sections,
                                    run_id: own_run_id,
                                }).await;
                                advance_tool_queue(
                                    &mut pending_tools,
                                    &mut context,
                                    &mut mode,
                                    &mut current_cancel,
                                    &mut current_tool_cancel,
                                    &mut iterations,
                                    true,
                                    engine.clone(),
                                    Arc::clone(&registry),
                                    ui_tx.clone(),
                                    agent_tx.clone(),
                                    generation,
                                    &mut clarify_started_at,
                                    &mut pending_steers,
                                    &mut child_agent_manager,
                                    &mut spawn_content_sections,
                                    own_run_id,
                                    &mut pending_child_finish_reprompt,
                                    &mut iteration_cap_exceeded,
                                )
                                .await;
                            }
                            AgentEvent::ToolKilled { id, name: _ } => {
                                tracing::info!(%id, "tool killed; agent returning to idle");
                                current_tool_cancel = None;
                                pending_tools.clear();
                                let _ = ui_tx
                                    .send(UiUpdate::ToolKilled {
                                        id,
                                        run_id: own_run_id,
                                    })
                                    .await;
                                mode = AgentMode::Idle;
                            }
                        }
                    }
                    Some(finish_result) = recv_finish(&mut finish_rx) => {
                        // The tool-calling wire protocol allows exactly one
                        // `role:"tool"` response per `tool_call_id`; that slot
                        // was already spent on `spawn_subagent`'s immediate
                        // `{"status":"running"/"queued"}` result (see
                        // `resolve_spawn_subagent`). So the child's *real*
                        // outcome, arriving asynchronously and possibly after
                        // further assistant turns, is appended as a brand-new
                        // hidden harness notice instead of rewriting/duplicating
                        // that earlier tool result — this keeps history
                        // append-only (an honest record of what was known
                        // when) and avoids the duplicate-`tool_call_id`
                        // protocol violation entirely, rather than papering
                        // over it by mutating a past message in place. A
                        // completion is appended to context immediately, but
                        // the root is re-prompted only after all currently
                        // outstanding child runs are terminal. This avoids
                        // acknowledgement turns for intermediate results
                        // while preserving a single continuation point for
                        // the aggregate answer.
                        let run_id = finish_result.run_id;
                        if cancelled_run_ids.remove(&run_id) {
                            // This child was cancelled by a `Stop`/`ClearContext`
                            // that already resolved (or discarded) its
                            // tool bubble/context entry synchronously; the
                            // manager only *requested* cancellation (a
                            // `CancellationToken` is cooperative, not
                            // immediate), so the child's task can still race
                            // in a real `FinishResult` afterwards. Applying it
                            // now would duplicate an already-settled entry or
                            // — after `ClearContext` — push content into an
                            // unrelated, brand-new `Context`. Drop it.
                            tracing::debug!(
                                %run_id,
                                tool_call_id = %finish_result.call_id,
                                "dropping stale child finish result for a cancelled run"
                            );
                            continue;
                        }
                        let call_id = finish_result.call_id;
                        let status = finish_result.status.clone();
                        let label = child_agent_manager
                            .as_ref()
                            .and_then(|manager| manager.label_for(run_id))
                            .unwrap_or_default()
                            .to_string();
                        let outstanding_count = child_agent_manager
                            .as_ref()
                            .map(|manager| manager.outstanding_count_after_completion(run_id))
                            .unwrap_or(0);
                        let all_done = outstanding_count == 0;
                        // The LLM-facing notice stays structured JSON (the
                        // model reads correlation/lifecycle fields as well as
                        // `status`/`result`/`error`/`sources`); only the TUI
                        // bubble renders result content as Markdown sections.
                        let result_text =
                            serde_json::to_string(&finish_result.to_completion_json(
                                &label,
                                outstanding_count,
                            ))
                            .unwrap_or_default();
                        let mut content_sections = spawn_content_sections
                            .remove(&call_id)
                            .unwrap_or_default();
                        content_sections.extend(finish_result.content_sections());
                        tracing::info!(%run_id, tool_call_id = %call_id, %status, "child agent finished");
                        let next_step_note = if !all_done {
                            format!(
                                "{outstanding_count} other subagent run(s) remain outstanding. \
                                 This is an intermediate completion; do not produce an \
                                 acknowledgement or a user-facing progress update solely for \
                                 this event. The harness will provide one continuation after \
                                 all outstanding child results are available."
                            )
                        } else {
                            "All spawned subagent runs are now terminal. Combine this result with \
                             earlier completion events in the conversation and produce the \
                             answer for the user."
                                .to_string()
                        };
                        context.push_async_harness_notice(format!(
                            "Subagent completion event; the structured JSON is harness metadata, \
                             not a user-facing message. The child result is already shown to the \
                             user in the tool bubble's Result/Error/Sources sections; do not \
                             repeat or quote it. Do not quote the JSON verbatim:\n{result_text}\n\
                             {next_step_note}",
                        ));
                        // Computed before `on_child_finished` clears the running slot.
                        let duration = child_agent_manager
                            .as_ref()
                            .and_then(|manager| manager.running_duration(run_id))
                            .unwrap_or_default();
                        // Emitted before the `spawn_subagent` bubble's own
                        // `ToolDone` so the driver can auto-return from the
                        // child conversation view (if it auto-switched into
                        // it) ahead of the parent bubble settling.
                        let _ = ui_tx
                            .send(UiUpdate::ChildAgentDone {
                                run_id,
                                status: status.clone(),
                                result: finish_result.result.clone(),
                            })
                            .await;
                        let _ = ui_tx
                            .send(UiUpdate::ToolDone {
                                id: call_id,
                                name: tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
                                result_section: crate::tui_reducer_state::ResultSection::plain(String::new()),
                                summary: vec![("status".to_string(), status)],
                                duration,
                                content_sections,
                                run_id: None,
                            })
                            .await;
                        if let Some(manager) = child_agent_manager.as_mut() {
                            manager.record_child_result(&finish_result);
                            manager.on_child_finished(run_id);
                        }
                        tracing::debug!(
                            tool_call_id = %call_id,
                            mode = ?mode,
                            "finish_rx: deciding whether to reprompt root after child result"
                        );
                        if all_done && mode == AgentMode::Idle {
                            // Same iteration budget as any other re-prompt
                            // (`advance_tool_queue`'s queue-empty branch) —
                            // without this guard a model that just keeps
                            // calling `spawn_subagent` again on every finish
                            // notice, instead of ever answering, loops here
                            // forever: this branch used to call
                            // `start_request` unconditionally, bypassing the
                            // cap entirely (see `bump_and_check_iteration_cap`'s
                            // doc comment).
                            if bump_and_check_iteration_cap(
                                &mut iterations,
                                &mut context,
                                &mut mode,
                                &ui_tx,
                                own_run_id,
                                &mut iteration_cap_exceeded,
                            )
                            .await
                            {
                                tracing::info!(
                                    tool_call_id = %call_id,
                                    "finish_rx: root was idle but iteration cap already exceeded; not reprompting"
                                );
                            } else {
                                tracing::info!(tool_call_id = %call_id, "finish_rx: root was idle, forcing reprompt");
                                mode = AgentMode::Streaming;
                                current_cancel = Some(start_request(
                                    &context,
                                    engine.clone(),
                                    registry.schemas(),
                                    ui_tx.clone(),
                                    agent_tx.clone(),
                                    generation,
                                    own_run_id,
                                ));
                            }
                        } else if all_done {
                            pending_child_finish_reprompt = true;
                            tracing::debug!(
                                tool_call_id = %call_id,
                                mode = ?mode,
                                "finish_rx: root not idle, deferring to its own queue-drain re-prompt"
                            );
                        } else {
                            tracing::debug!(
                                tool_call_id = %call_id,
                                outstanding_count,
                                mode = ?mode,
                                "finish_rx: intermediate child result recorded without reprompt"
                            );
                        }
                    }
                    Some(update) = recv_subagent_ui(&mut subagent_ui_rx) => {
                        // Phase 6.10 drill-in conversation model: the
                        // child's own text/thinking tokens must never leak
                        // into the parent's own `partial_assistant_text`
                        // (they render in the dedicated child transcript
                        // instead), and the child's own stream lifecycle
                        // (`StreamStarted`/`StreamDone`/`StreamCancelled`)
                        // must never flip the parent's own `TuiMode` —
                        // only `ChildAgentStarted`/`ChildAgentDone` (see
                        // `ChildAgentManager::start` and this loop's
                        // `finish_rx` arm) mark the child conversation's
                        // lifecycle boundaries for the UI. Everything else
                        // (tool approvals/progress/errors, already stamped
                        // with the child's own `run_id`) is forwarded as-is.
                        match update {
                            UiUpdate::AppendText(text) => {
                                if let Some(run_id) = child_agent_manager
                                    .as_ref()
                                    .and_then(|manager| manager.running_run_id())
                                {
                                    let _ = ui_tx
                                        .send(UiUpdate::ChildAgentToken {
                                            run_id,
                                            text,
                                            is_thinking: false,
                                        })
                                        .await;
                                }
                            }
                            UiUpdate::AppendThinking(text) => {
                                if let Some(run_id) = child_agent_manager
                                    .as_ref()
                                    .and_then(|manager| manager.running_run_id())
                                {
                                    let _ = ui_tx
                                        .send(UiUpdate::ChildAgentToken {
                                            run_id,
                                            text,
                                            is_thinking: true,
                                        })
                                        .await;
                                }
                            }
                            UiUpdate::StreamStarted
                            | UiUpdate::StreamDone
                            | UiUpdate::StreamCancelled
                            | UiUpdate::AgentIdle => {
                                // Intentionally swallowed — see comment above.
                            }
                            other => {
                                let _ = ui_tx.send(other).await;
                            }
                        }
                    }
                    _ = cancelled_or_pending(&parent_cancel) => {
                        // The parent cancelled this child (e.g. it stopped
                        // or was torn down). Abort whatever is in flight and
                        // return immediately — nobody will drive this loop
                        // with further commands.
                        tracing::info!("child agent cancelled by parent");
                        if let Some(cancel_token) = current_cancel.take() {
                            cancel_token.cancel();
                        }
                        if let Some(tool_cancel) = current_tool_cancel.take() {
                            tool_cancel.cancel();
                        }
                        pending_tools.clear();
                        pending_steers.clear();
                        return Ok(context);
                    }
                    cmd = cmd_rx.recv() => {
                        let Some(cmd) = cmd else {
                            // AgentLoop dropped → cmd_tx closed → clean shutdown:
                            // return the final committed Context (read-only history).
                            return Ok(context);
                        };
                        log_agent_command(
                            &cmd,
                            mode,
                            current_cancel.is_some(),
                            current_tool_cancel.is_some(),
                        );
                        match cmd {
                            AgentCommand::Prompt(text) => {
                                if text.is_empty() {
                                    continue;
                                }
                                if mode == AgentMode::Idle {
                                    iterations = 1;
                                    context.push_user(text);
                                    mode = AgentMode::Streaming;
                                    current_cancel = Some(start_request(
                                        &context,
                                        engine.clone(),
                                        registry.schemas(),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                        generation,
                                        own_run_id,
                                    ));
                                } else {
                                    // Cooperative steering: never cancel the live
                                    // stream. Queued from ToolPending too; injection
                                    // is deferred (Phase 4 adds descope_pending_tools).
                                    pending_steers.push_back(text);
                                    let _ = ui_tx
                                        .send(UiUpdate::SteerQueued {
                                            run_id: own_run_id,
                                        })
                                        .await;
                                }
                            }
                            AgentCommand::ApproveTool { id } => {
                                // Only valid in `ToolPending`; ignored otherwise.
                                let is_front = mode == AgentMode::ToolPending
                                    && pending_tools
                                        .front()
                                        .is_some_and(|pending| pending.call.id() == id);
                                if is_front {
                                    let pending =
                                        pending_tools.pop_front().expect("checked front");
                                    if pending.call.name()
                                        == tools::spawn_subagent::SpawnSubagentTool::NAME
                                    {
                                        resolve_spawn_subagent(
                                            pending,
                                            &mut child_agent_manager,
                                            &mut context,
                                            &ui_tx,
                                            &mut spawn_content_sections,
                                        )
                                        .await;
                                        advance_tool_queue(
                                            &mut pending_tools,
                                            &mut context,
                                            &mut mode,
                                            &mut current_cancel,
                                            &mut current_tool_cancel,
                                            &mut iterations,
                                            true,
                                            engine.clone(),
                                            Arc::clone(&registry),
                                            ui_tx.clone(),
                                            agent_tx.clone(),
                                            generation,
                                            &mut clarify_started_at,
                                            &mut pending_steers,
                                            &mut child_agent_manager,
                                            &mut spawn_content_sections,
                                            own_run_id,
                                            &mut pending_child_finish_reprompt,
                                            &mut iteration_cap_exceeded,
                                        )
                                        .await;
                                    } else {
                                        let name = pending.call.name().to_string();
                                        let call_id = pending.call.id();
                                        let arguments = pending.call.arguments().clone();
                                        let content_sections =
                                            pending.call.pending_content_sections();
                                        let title_arguments = pending.call.title_arguments();
                                        tracing::info!(tool = %name, tool_call_id = %call_id, "tool approved");
                                        current_tool_cancel = Some(PendingTool::execute(
                                            pending,
                                            agent_tx.clone(),
                                            generation,
                                        ));
                                        let _ = ui_tx
                                            .send(UiUpdate::ToolStart {
                                                name: name.clone(),
                                                id: call_id,
                                                arguments: arguments.clone(),
                                                content_sections,
                                                title_arguments,
                                                run_id: own_run_id,
                                            })
                                            .await;
                                    }
                                }
                            }
                            AgentCommand::DenyTool { id, reason } => {
                                // Only valid in `ToolPending`; ignored otherwise.
                                let is_front = mode == AgentMode::ToolPending
                                    && pending_tools
                                        .front()
                                        .is_some_and(|pending| pending.call.id() == id);
                                if is_front {
                                    let denied =
                                        pending_tools.pop_front().expect("checked front");
                                    let name = denied.call.name().to_string();
                                    let call_id = denied.call.id();
                                    let denied_content_sections =
                                        denied.call.pending_content_sections();
                                    tracing::info!(tool = %name, tool_call_id = %call_id, "tool denied");
                                    let mut error = format!("Tool '{name}' was denied by the user");
                                    if !reason.is_empty() {
                                        error.push_str(&format!(
                                            ". Here is their reason for the denial:\n\n{reason}"
                                        ));
                                    }
                                    context.push_tool_error(
                                        call_id,
                                        name.clone(),
                                        error.clone(),
                                        None,
                                        std::time::Duration::ZERO,
                                        denied_content_sections.clone(),
                                    );
                                    // When the user supplied a reason, also push a
                                    // `user` role message so the model reads the
                                    // correction through the authoritative instruction
                                    // channel rather than just the tool-error envelope.
                                    if !reason.is_empty() {
                                        context.push_implicit_user(format!(
                                            "I denied the '{name}' tool call \
                                             ( tool_call_id={call_id} ). \
                                             Here is my reason for the denial:\n\n{reason}"
                                        ));
                                    }
                                    let _ = ui_tx
                                        .send(UiUpdate::ToolError {
                                            id: call_id,
                                            error,
                                            duration: std::time::Duration::ZERO,
                                            content_sections: denied_content_sections,
                                            run_id: own_run_id,
                                        })
                                        .await;
                                    advance_tool_queue(
                                        &mut pending_tools,
                                        &mut context,
                                        &mut mode,
                                        &mut current_cancel,
                                        &mut current_tool_cancel,
                                        &mut iterations,
                                        true,
                                        engine.clone(),
                                        Arc::clone(&registry),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                        generation,
                                        &mut clarify_started_at,
                                        &mut pending_steers,
                                        &mut child_agent_manager,
                                        &mut spawn_content_sections,
                                        own_run_id,
                                        &mut pending_child_finish_reprompt,
                                        &mut iteration_cap_exceeded,
                                    )
                                    .await;
                                }
                            }
                            AgentCommand::AnswerClarify(answer) => {
                                let is_front = mode == AgentMode::ClarifyPending
                                    && pending_tools
                                        .front()
                                        .is_some_and(|pending| pending.call.name() == "clarify");
                                if is_front {
                                    let pending = pending_tools.pop_front().expect("checked front");
                                    let call_id = pending.call.id();
                                    let choices_sections =
                                        tools::clarify::ClarifyTool::answer_display_sections(
                                            pending.call.arguments(),
                                        );
                                    let duration = clarify_started_at
                                        .take()
                                        .map(|started| started.elapsed())
                                        .unwrap_or(std::time::Duration::ZERO);
                                    context.push_tool_result(
                                        ToolResult {
                                            id: call_id,
                                            name: tools::clarify::ClarifyTool::NAME.to_string(),
                                            result: answer.clone(),
                                            attachments: vec![],
                                            result_section_id: Some(
                                                crate::tui_reducer_state::result_section_ids::USER_ANSWER
                                                    .to_string(),
                                            ),
                                            result_metadata: None,
                                            result_metadata_schema: None,
                                            summary: vec![],
                                            content_sections: choices_sections.clone(),
                                            duration,
                                        },
                                    );
                                    let _ = ui_tx
                                        .send(UiUpdate::ToolDone {
                                            id: call_id,
                                            name: tools::clarify::ClarifyTool::NAME.to_string(),
                                            result_section: crate::tui_reducer_state::ResultSection {
                                                id: crate::tui_reducer_state::result_section_ids::USER_ANSWER.to_string(),
                                                content: answer,
                                            },
                                            summary: vec![],
                                            duration,
                                            content_sections: choices_sections,
                                            run_id: own_run_id,
                                        })
                                        .await;
                                    mode = AgentMode::ToolPending;
                                    advance_tool_queue(
                                        &mut pending_tools,
                                        &mut context,
                                        &mut mode,
                                        &mut current_cancel,
                                        &mut current_tool_cancel,
                                        &mut iterations,
                                        true,
                                        engine.clone(),
                                        Arc::clone(&registry),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                        generation,
                                        &mut clarify_started_at,
                                        &mut pending_steers,
                                        &mut child_agent_manager,
                                        &mut spawn_content_sections,
                                        own_run_id,
                                        &mut pending_child_finish_reprompt,
                                        &mut iteration_cap_exceeded,
                                    )
                                    .await;
                                }
                            }
                            AgentCommand::AbortClarify => {
                                let is_front = mode == AgentMode::ClarifyPending
                                    && pending_tools
                                        .front()
                                        .is_some_and(|pending| pending.call.name() == "clarify");
                                if is_front {
                                    let pending = pending_tools.pop_front().expect("checked front");
                                    let call_id = pending.call.id();
                                    let error =
                                        "User aborted the clarify question without answering."
                                            .to_string();
                                    context.push_tool_error(
                                        call_id,
                                        tools::clarify::ClarifyTool::NAME.to_string(),
                                        error.clone(),
                                        None,
                                        std::time::Duration::ZERO,
                                        vec![],
                                    );
                                    let _ = ui_tx
                                        .send(UiUpdate::ToolError {
                                            id: call_id,
                                            error,
                                            duration: std::time::Duration::ZERO,
                                            content_sections: vec![],
                                            run_id: own_run_id,
                                        })
                                        .await;
                                    mode = AgentMode::ToolPending;
                                    advance_tool_queue(
                                        &mut pending_tools,
                                        &mut context,
                                        &mut mode,
                                        &mut current_cancel,
                                        &mut current_tool_cancel,
                                        &mut iterations,
                                        true,
                                        engine.clone(),
                                        Arc::clone(&registry),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                        generation,
                                        &mut clarify_started_at,
                                        &mut pending_steers,
                                        &mut child_agent_manager,
                                        &mut spawn_content_sections,
                                        own_run_id,
                                        &mut pending_child_finish_reprompt,
                                        &mut iteration_cap_exceeded,
                                    )
                                    .await;
                                }
                            }
                            AgentCommand::Stop { reason } => {
                                if let Some(manager) = child_agent_manager.as_mut() {
                                    let cancelled_spawns = manager.cancel_all();
                                    let cancelled_count = cancelled_spawns.len();
                                    for (index, cancelled) in cancelled_spawns.into_iter().enumerate() {
                                        // Store integration (child_result JSONL
                                        // record) lands in Phase 6.11; for now
                                        // the cancellation is only logged, but
                                        // the tool bubble/context still get a
                                        // proper terminal state below instead
                                        // of being left stuck mid-flight.
                                        tracing::info!(
                                            run_id = %cancelled.run_id,
                                            tool_call_id = %cancelled.call_id,
                                            "child agent cancelled on parent stop"
                                        );
                                        cancelled_run_ids.insert(cancelled.run_id);
                                        let mut content_sections = spawn_content_sections
                                            .remove(&cancelled.call_id)
                                            .unwrap_or_default();
                                        content_sections.push(tools::Section::Content(
                                            tools::ContentSection {
                                                label: Some("Result".to_string()),
                                                content: "Cancelled: the parent agent stopped before the child finished.".to_string(),
                                                file_extension: Some("md".to_string()),
                                                line_number_offset: None,
                                            },
                                        ));
                                        let cancellation_result = crate::child_agent_manager::FinishResult {
                                            run_id: cancelled.run_id,
                                            call_id: cancelled.call_id,
                                            origin: crate::child_agent_manager::FinishResultOrigin::Runtime,
                                            status: "cancelled".to_string(),
                                            result: None,
                                            error: Some(
                                                "The parent agent stopped before the child finished."
                                                    .to_string(),
                                            ),
                                            sources: vec![],
                                        };
                                        let outstanding_count =
                                            cancelled_count.saturating_sub(index + 1);
                                        let result_text = serde_json::to_string(
                                            &cancellation_result.to_completion_json(
                                                &cancelled.label,
                                                outstanding_count,
                                            ),
                                        )
                                        .unwrap_or_default();
                                        context.push_async_harness_notice(format!(
                                            "Subagent completion event for \
                                             `{tool_name}`; the structured JSON is harness \
                                             metadata, not a user-facing message. The parent \
                                             stop cancelled this child, so no continuation will \
                                             be started:\n{result_text}",
                                            tool_name = tools::spawn_subagent::SpawnSubagentTool::NAME,
                                        ));
                                        let _ = ui_tx
                                            .send(UiUpdate::ToolDone {
                                                id: cancelled.call_id,
                                                name: tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
                                                result_section: crate::tui_reducer_state::ResultSection::plain(String::new()),
                                                summary: vec![("status".to_string(), "cancelled".to_string())],
                                                duration: std::time::Duration::ZERO,
                                                content_sections,
                                                run_id: own_run_id,
                                            })
                                            .await;
                                    }
                                }
                                // Interrupt: hard-abort the current round.
                                pending_steers.clear();
                                if let Some(cancel_token) = current_cancel.take() {
                                    cancel_token.cancel();
                                }
                                if let Some(tool_cancel) = current_tool_cancel.take() {
                                    tool_cancel.cancel();
                                }
                                if mode == AgentMode::ClarifyPending {
                                    if let Some(pending) = pending_tools.pop_front() {
                                        let name = pending.call.name().to_string();
                                        let id = pending.call.id();
                                        let content_sections =
                                            pending.call.pending_content_sections();
                                        let mut error =
                                            format!("Tool '{name}' was denied by the user");
                                        if !reason.is_empty() {
                                            error.push_str(&format!(
                                                ". Here is their reason for the denial:\n\n{reason}"
                                            ));
                                        }
                                        context.push_tool_error(
                                            id,
                                            name,
                                            error.clone(),
                                            None,
                                            std::time::Duration::ZERO,
                                            content_sections.clone(),
                                        );
                                        let _ = ui_tx
                                            .send(UiUpdate::ToolError {
                                                id,
                                                error,
                                                duration: std::time::Duration::ZERO,
                                                content_sections,
                                                run_id: own_run_id,
                                            })
                                            .await;
                                    }
                                    for pending in pending_tools.iter() {
                                        let name = pending.call.name().to_string();
                                        let id = pending.call.id();
                                        let mut error =
                                            format!("Tool '{name}' was denied by the user");
                                        if !reason.is_empty() {
                                            error.push_str(&format!(
                                                ". Here is their reason for the denial:\n\n{reason}"
                                            ));
                                        }
                                        context.push_tool_error(
                                            id,
                                            name,
                                            error,
                                            None,
                                            std::time::Duration::ZERO,
                                            pending.call.pending_content_sections(),
                                        );
                                    }
                                    pending_tools.clear();
                                    mode = AgentMode::Idle;
                                } else if mode == AgentMode::ToolPending {
                                    // Record a denial result for every pending
                                    // tool so the model history stays consistent
                                    // (every tool call must have a result).
                                    // Only the front tool has a visible bubble;
                                    // the rest are queued without one yet.
                                    let mut first = true;
                                    for pending in pending_tools.iter() {
                                        let name = pending.call.name().to_string();
                                        let id = pending.call.id();
                                        let content_sections =
                                            pending.call.pending_content_sections();
                                        let mut error = format!("Tool '{name}' was denied by the user");
                                        if !reason.is_empty() {
                                            error.push_str(&format!(
                                                ". Here is their reason for the denial:\n\n{reason}"
                                            ));
                                        }
                                        context.push_tool_error(
                                            id,
                                            name,
                                            error.clone(),
                                            None,
                                            std::time::Duration::ZERO,
                                            content_sections.clone(),
                                        );
                                        if first {
                                            let _ = ui_tx
                                                .send(UiUpdate::ToolError {
                                                    id,
                                                    error,
                                                    duration: std::time::Duration::ZERO,
                                                    content_sections,
                                                    run_id: own_run_id,
                                                })
                                                .await;
                                            first = false;
                                        }
                                    }
                                    pending_tools.clear();
                                    mode = AgentMode::Idle;
                                }
                                if mode == AgentMode::Idle {
                                    let _ = ui_tx.send(UiUpdate::AgentIdle).await;
                                }
                            }
                            AgentCommand::ClearContext => {
                                // Interrupt whatever was in flight before
                                // swapping in `fresh_context`, mirroring
                                // `AgentCommand::Stop`'s cleanup — otherwise
                                // the old stream/tool/children keep running
                                // against a `Context` nobody can see anymore
                                // and their eventual `AgentEvent`s land in the
                                // new, unrelated conversation instead (see
                                // the `generation` bump below for the part
                                // cancellation alone can't cover).
                                generation = generation.wrapping_add(1);
                                pending_steers.clear();
                                pending_tools.clear();
                                if let Some(cancel_token) = current_cancel.take() {
                                    cancel_token.cancel();
                                }
                                if let Some(tool_cancel) = current_tool_cancel.take() {
                                    tool_cancel.cancel();
                                }
                                if let Some(manager) = child_agent_manager.as_mut() {
                                    for cancelled in manager.cancel_all() {
                                        tracing::info!(
                                            run_id = %cancelled.run_id,
                                            tool_call_id = %cancelled.call_id,
                                            "child agent cancelled on clear_context"
                                        );
                                        cancelled_run_ids.insert(cancelled.run_id);
                                        spawn_content_sections.remove(&cancelled.call_id);
                                    }
                                }
                                mode = AgentMode::Idle;
                                let mut fresh_context = match system_prompt.as_deref() {
                                    Some(system_prompt) => {
                                        Context::with_system_prompt(system_prompt)
                                    }
                                    None => Context::new(),
                                };
                                if let Some(store) = &store_for_commands {
                                    let config = engine.config();
                                    match store.start_new_session(
                                        &config.provider_connection,
                                        &config.model,
                                        None,
                                    ) {
                                        Ok(new_store) => fresh_context.set_store(new_store),
                                        Err(error) => tracing::warn!(
                                            %error,
                                            "could not start a new session file; the cleared conversation is not persisted"
                                        ),
                                    }
                                }
                                context = fresh_context;
                                *current_store
                                    .lock()
                                    .expect("current store mutex is not poisoned") =
                                    context.store();
                            }
                            AgentCommand::RecordModelChanged {
                                provider_connection,
                                model,
                            } => {
                                if let Some(store) = context.store() {
                                    store.record_model_changed(&provider_connection, &model);
                                }
                            }
                            AgentCommand::ForChild { run_id, command } => {
                                // Only a root loop's `child_agent_manager` can
                                // resolve `run_id` to a live child command
                                // channel; forward the inner command verbatim
                                // — the child's own loop never sees `ForChild`
                                // itself, only plain `ApproveTool`/`DenyTool`/
                                // `AnswerClarify`/`AbortClarify` on its own
                                // channel, exactly as if sent directly.
                                let sender = child_agent_manager
                                    .as_ref()
                                    .and_then(|manager| manager.running_command_sender(run_id));
                                match sender {
                                    Some(sender) => {
                                        let _ = sender.send(*command).await;
                                    }
                                    None => {
                                        tracing::warn!(
                                            %run_id,
                                            "AgentCommand::ForChild targets a run_id with no \
                                             running child (already finished/cancelled); dropping"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                // A `Role::Child` loop has nobody left to drive it once it
                // goes idle (no user, no more steers) — the "unstructured
                // fallback" case (the model stopped calling tools without
                // ever calling `agent_finish`). Returning here lets the caller
                // (the spawn wrapper) harvest the final `Context` and report
                // an "unstructured" outcome.
                //
                // Iteration-cap and stream-error cases are different and
                // reported as hard `Err`s instead: the child either never got
                // a chance to conclude naturally or the provider failed, so
                // there is no "final answer" to salvage. The
                // `drive_child_to_completion` `Ok(Err(error))` arm reports
                // exactly the right shape (`status: "failure"`, explicit
                // `error`, no fabricated `result`).
                if is_child && mode == AgentMode::Idle {
                    if let Some(message) = stream_error.take() {
                        return Err(anyhow::anyhow!(message));
                    }
                    if let Some(message) = iteration_cap_exceeded.take() {
                        return Err(anyhow::anyhow!(message));
                    }
                    return Ok(context);
                }
            }
        })
    }
}

/// Log the "tool approval requested" event for the given pending call using the
/// tool's own non-default redacted arguments so sensitive values are never
/// written to the log.
fn log_tool_approval_request(call: &dyn ToolCall) {
    let safe_args: serde_json::Map<String, serde_json::Value> =
        call.non_default_redacted_arguments().into_iter().collect();
    tracing::info!(
        tool = %call.name(),
        tool_call_id = %call.id(),
        args = %serde_json::to_string(&safe_args).unwrap_or_else(|_| "{}".to_string()),
        "tool approval requested"
    );
}

/// Resolve an approved `spawn_subagent` call immediately against
/// `child_agent_manager`: unlike ordinary tools, spawning never executes the
/// call's own `ToolCall::run` (which has no access to the manager) — it
/// starts (or enqueues) the child directly and pushes a structured admission
/// event as the call's tool result right away. This is *not* the call
/// finishing: `ChildAgentManager::spawn` reports that status as a
/// `UiUpdate::ToolProgress` (bubble stays `Running`), and `content_sections`
/// (the task/briefing shown to the user) are stashed in
/// `spawn_content_sections` so the eventual real `ToolDone` — sent only
/// once the child actually finishes, see `AgentLoop::run`'s `finish_rx`
/// handling — can still show them alongside the child's result.
async fn resolve_spawn_subagent(
    pending: PendingTool,
    child_agent_manager: &mut Option<crate::child_agent_manager::ChildAgentManager>,
    context: &mut Context,
    ui_tx: &mpsc::Sender<UiUpdate>,
    spawn_content_sections: &mut HashMap<Uuid, Vec<crate::agent::tools::Section>>,
) {
    let call_id = pending.call.id();
    let name = pending.call.name().to_string();
    let arguments = pending.call.arguments().clone();
    let content_sections = pending.call.pending_content_sections();
    let title_arguments = pending.call.title_arguments();
    let _ = ui_tx
        .send(UiUpdate::ToolStart {
            name: name.clone(),
            id: call_id,
            arguments: arguments.clone(),
            content_sections: content_sections.clone(),
            title_arguments,
            run_id: None,
        })
        .await;

    let task = arguments
        .get("task")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let briefing = arguments
        .get("briefing")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let agent_type = arguments
        .get("agent_type")
        .and_then(|value| value.as_str())
        .unwrap_or(tools::spawn_subagent::DEFAULT_CHILD_AGENT_TYPE)
        .to_string();
    let label = arguments
        .get("label")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();

    let (status_text, run_id, outstanding_count) = match child_agent_manager.as_mut() {
        Some(manager) => {
            let status = manager.spawn(task, briefing, agent_type, label.clone(), call_id);
            let outstanding_count = manager.outstanding_count();
            let (status_text, run_id) = match status {
                crate::child_agent_manager::SpawnStatus::Started(run_id) => ("running", run_id),
                crate::child_agent_manager::SpawnStatus::Queued(run_id) => ("queued", run_id),
            };
            (status_text, run_id, outstanding_count)
        }
        None => {
            // Structurally unreachable: `spawn_subagent` is only ever
            // registered when a `ChildAgentManager` is wired. Fail safe
            // rather than panic.
            let error =
                "spawn_subagent is unavailable: this agent cannot spawn children".to_string();
            tracing::error!(tool_call_id = %call_id, "spawn_subagent resolved with no ChildAgentManager wired");
            context.push_tool_error(
                call_id,
                name.clone(),
                error.clone(),
                None,
                std::time::Duration::ZERO,
                content_sections.clone(),
            );
            let _ = ui_tx
                .send(UiUpdate::ToolError {
                    id: call_id,
                    error,
                    duration: std::time::Duration::ZERO,
                    content_sections,
                    run_id: None,
                })
                .await;
            return;
        }
    };

    spawn_content_sections.insert(call_id, content_sections);
    tracing::info!(tool_call_id = %call_id, status = status_text, %run_id, "spawn_subagent resolved");
    let result_value = serde_json::json!({
        "event": "subagent_accepted",
        "tool_call_id": call_id.to_string(),
        "label": label,
        "status": status_text,
        "run_id": run_id.to_string(),
        "outstanding_count": outstanding_count,
        "all_done": false,
    });
    let result_text = serde_json::to_string(&result_value).unwrap_or_default();
    context.push_tool_result(ToolResult {
        id: call_id,
        name,
        result: result_text,
        attachments: vec![],
        result_section_id: None,
        result_metadata: None,
        result_metadata_schema: None,
        summary: vec![],
        content_sections: spawn_content_sections
            .get(&call_id)
            .cloned()
            .unwrap_or_default(),
        duration: std::time::Duration::ZERO,
    });
    // No `ToolDone` here: `ChildAgentManager::spawn` already reported the
    // "running"/"queued" status via `UiUpdate::ToolProgress`. The bubble
    // stays `Running` until the child truly finishes.
}

/// Advance the pending-tool queue. Processes auto-approved and auto-denied
/// tools immediately in a loop; stops at the first `UserApprovalRequired`
/// tool (sends `ToolPending`) or re-prompts the model when the queue is
/// empty.
///
/// `increment_iterations` is `false` when called immediately after a new
/// `ToolCall` batch arrives (the iteration counter was already set) and
/// `true` after a tool finishes executing (each completed tool round counts).
/// `own_run_id` is the calling loop's own `AgentLoop::own_run_id` (`None` for
/// the root, `Some(run_id)` for a child); it is stamped onto the
/// `UiUpdate::ToolPending`/`ClarifyPending` this function may emit so the
/// root (which forwards a child's `UiUpdate`s up to its own driver) and the
/// driver can route the resulting `ApproveTool`/`DenyTool`/`AnswerClarify`/
/// `AbortClarify` verdict back to the correct agent loop via
/// `AgentCommand::ForChild`. This has no effect on how the tool queue itself
/// is processed — children pause and wait exactly like the root; there is no
/// auto-deny special case.
///
/// `pending_child_finish_reprompt` is cleared whenever this function
/// re-prompts the model (the queue-empty branch): that re-prompt's `context`
/// already carries any deferred `spawn_subagent` child result (see
/// `AgentLoop::run`'s `finish_rx` arm), so the flag's job is done and a later
/// `StreamDone` must not force a redundant extra re-prompt for it.
///
/// `iteration_cap_exceeded` is set (to the guard's explanation message) when
/// `MAX_TOOL_USE_ITERATIONS` is exceeded — but only for a child loop
/// (`own_run_id.is_some()`); a root loop's own hit is reported to the human
/// directly (an `Interrupted` assistant bubble) and never sets this. The
/// caller (`AgentLoop::run`'s `is_child && mode == AgentMode::Idle` tail)
/// checks it to return `Err` instead of `Ok(context)` — so
/// `drive_child_to_completion` reports a clear `status: "failure"` instead of
/// folding this into the generic "unstructured" fallback used when a child
/// simply goes idle without ever calling `agent_finish`. See docs/features.md
/// → *spawn_subagent*/*agent_finish* for the distinction.
/// Increments `*iterations` and, if that now exceeds `MAX_TOOL_USE_ITERATIONS`,
/// reports the guard tripping and forces `*mode = AgentMode::Idle`: for the
/// root (`own_run_id: None`) a human reads the transcript directly, so a real
/// `Interrupted` assistant bubble is pushed; for a child, the explanation is
/// instead handed back via `*iteration_cap_exceeded` (never pushed as an
/// assistant message — `extract_last_assistant_text` in `agent_factory.rs`
/// would otherwise mistake this guard boilerplate for the child's own final
/// answer) for `AgentLoop::run`'s `is_child && mode == AgentMode::Idle` tail
/// to turn into an `Err`, producing an honest `status: "failure"` instead of
/// folding this into the generic "unstructured" fallback (see
/// `advance_tool_queue`'s doc comment and docs/features.md →
/// *spawn_subagent*/*agent_finish*).
///
/// Returns `true` when the cap was exceeded (the caller must not start a new
/// request this round) and `false` otherwise (still under budget, proceed as
/// normal). Shared by `advance_tool_queue`'s queue-empty branch and
/// `AgentLoop::run`'s `finish_rx` idle-reprompt branch — both are re-prompt
/// call sites, and both must enforce the same budget. Without this, a child
/// finishing while the root is idle re-prompts it directly via `start_request`,
/// bypassing `advance_tool_queue`'s iteration counter entirely; a model that
/// just keeps calling `spawn_subagent` again on every finish notice (rather
/// than ever answering) can then loop forever — one HTTP request every few
/// seconds, with no cap at all — instead of tripping the same guard as any
/// other tool-use round.
async fn bump_and_check_iteration_cap(
    iterations: &mut usize,
    context: &mut Context,
    mode: &mut AgentMode,
    ui_tx: &mpsc::Sender<UiUpdate>,
    own_run_id: Option<Uuid>,
    iteration_cap_exceeded: &mut Option<String>,
) -> bool {
    *iterations += 1;
    tracing::debug!(iterations = *iterations, "advancing tool queue");
    if *iterations <= MAX_TOOL_USE_ITERATIONS {
        return false;
    }
    tracing::warn!(
        iterations = *iterations,
        run_id = ?own_run_id,
        "max tool-use iterations exceeded"
    );
    let message = format!(
        "Max tool-use iterations ({}) exceeded.",
        MAX_TOOL_USE_ITERATIONS
    );
    match own_run_id {
        None => {
            context.push_assistant(message.clone(), AssistantStatus::Interrupted);
        }
        Some(_) => {
            *iteration_cap_exceeded = Some(message.clone());
        }
    }
    let _ = ui_tx
        .send(UiUpdate::Error {
            message,
            run_id: own_run_id,
        })
        .await;
    *mode = AgentMode::Idle;
    true
}

#[expect(clippy::too_many_arguments)]
async fn advance_tool_queue(
    pending_tools: &mut VecDeque<PendingTool>,
    context: &mut Context,
    mode: &mut AgentMode,
    current_cancel: &mut Option<CancellationToken>,
    current_tool_cancel: &mut Option<CancellationToken>,
    iterations: &mut usize,
    increment_iterations: bool,
    engine: Arc<dyn Engine>,
    registry: Arc<ToolRegistry>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<(u64, AgentEvent)>,
    generation: u64,
    clarify_started_at: &mut Option<std::time::Instant>,
    pending_steers: &mut VecDeque<String>,
    child_agent_manager: &mut Option<crate::child_agent_manager::ChildAgentManager>,
    spawn_content_sections: &mut HashMap<Uuid, Vec<crate::agent::tools::Section>>,
    own_run_id: Option<Uuid>,
    pending_child_finish_reprompt: &mut bool,
    iteration_cap_exceeded: &mut Option<String>,
) {
    // `spawn_subagent` and auto-denied tools resolve synchronously within
    // this same loop (unlike a normally-executing auto-approved tool, which
    // `return`s here and waits for a later `ToolDone`/`ToolError` event to
    // re-invoke this function with `increment_iterations: true`). Without
    // this, a round made up entirely of such synchronously-resolved tools
    // could reach the queue-empty re-prompt below still carrying whatever
    // `increment_iterations` this call was originally invoked with (`false`,
    // since the `ToolCall` handler always calls this function that way) —
    // silently completing a full tool-use round without ever counting
    // against `MAX_TOOL_USE_ITERATIONS`. Shadow the parameter as mutable so
    // resolving any such tool below can flip it to `true` for the rest of
    // this call.
    let mut increment_iterations = increment_iterations;
    loop {
        match pending_tools.front() {
            None => {
                // Queue empty: optionally increment and re-prompt.
                if increment_iterations
                    && bump_and_check_iteration_cap(
                        iterations,
                        context,
                        mode,
                        &ui_tx,
                        own_run_id,
                        iteration_cap_exceeded,
                    )
                    .await
                {
                    return;
                }
                // All tool results for this round are now in context — this is
                // the earliest safe point to inject queued steers (pending user
                // messages typed while tools were running), so they reach the
                // model on the very next request rather than waiting for a full
                // tool-free turn to complete via StreamDone.
                if inject_pending_steers(pending_steers, context) {
                    let _ = ui_tx
                        .send(UiUpdate::SteerApplied { run_id: own_run_id })
                        .await;
                }
                // This re-prompt's `context` already carries any child result
                // that arrived (and was deferred) while the root was mid-turn
                // — clear the flag so a later, unrelated `StreamDone` doesn't
                // force a redundant re-prompt for a result already handled.
                *pending_child_finish_reprompt = false;
                *mode = AgentMode::Streaming;
                *current_cancel = Some(start_request(
                    context,
                    engine,
                    registry.schemas(),
                    ui_tx,
                    agent_tx,
                    generation,
                    own_run_id,
                ));
                return;
            }
            Some(front) => {
                let approval = {
                    let _span = tracing::debug_span!(
                        "needs_approval",
                        tool = %front.call.name(),
                        tool_call_id = %front.call.id(),
                    )
                    .entered();
                    // Validate arguments against the tool's input schema first.
                    // Structurally invalid calls are denied immediately without an approval
                    // prompt, for all tools uniformly.
                    let validation = registry
                        .get(front.call.name())
                        .map(|tool| tool.validate_args(front.call.arguments()));
                    match validation {
                        Some(Err(reason)) => crate::sandbox::ApprovalCheck::AutoDenied { reason },
                        _ => registry
                            .get(front.call.name())
                            .map(|tool| tool.needs_approval(front.call.arguments()))
                            .unwrap_or(crate::sandbox::ApprovalCheck::UserApprovalRequired),
                    }
                };
                match approval {
                    crate::sandbox::ApprovalCheck::AutoApproved
                        if front.call.name() == tools::spawn_subagent::SpawnSubagentTool::NAME =>
                    {
                        let pending = pending_tools.pop_front().expect("checked front");
                        resolve_spawn_subagent(
                            pending,
                            child_agent_manager,
                            context,
                            &ui_tx,
                            spawn_content_sections,
                        )
                        .await;
                        // Resolves synchronously (unlike a normally-executing
                        // tool); continue the loop to process the next
                        // pending tool or re-prompt. This still counts as a
                        // completed tool-use round against the iteration cap.
                        increment_iterations = true;
                    }
                    crate::sandbox::ApprovalCheck::AutoApproved => {
                        let pending = pending_tools.pop_front().expect("checked front");
                        let name = pending.call.name().to_string();
                        let call_id = pending.call.id();
                        let arguments = pending.call.arguments().clone();
                        let content_sections = pending.call.pending_content_sections();
                        let title_arguments = pending.call.title_arguments();
                        tracing::info!(tool = %name, tool_call_id = %call_id, "tool auto-approved");
                        *current_tool_cancel =
                            Some(PendingTool::execute(pending, agent_tx.clone(), generation));
                        let _ = ui_tx
                            .send(UiUpdate::ToolStart {
                                name,
                                id: call_id,
                                arguments,
                                content_sections,
                                title_arguments,
                                run_id: own_run_id,
                            })
                            .await;
                        return; // wait for ToolDone/ToolError
                    }
                    crate::sandbox::ApprovalCheck::AutoDenied { reason } => {
                        let pending = pending_tools.pop_front().expect("checked front");
                        let name = pending.call.name().to_string();
                        let call_id = pending.call.id();
                        let title_arguments = pending.call.title_arguments();
                        let content_sections = pending.call.pending_content_sections();
                        tracing::info!(tool = %name, tool_call_id = %call_id, %reason, "tool auto-denied");
                        context.push_tool_error(
                            call_id,
                            name.clone(),
                            reason.clone(),
                            None,
                            std::time::Duration::ZERO,
                            content_sections.clone(),
                        );
                        let _ = ui_tx
                            .send(UiUpdate::ToolAutoDenied {
                                id: call_id,
                                name,
                                title_arguments,
                                content_sections,
                                reason,
                                run_id: own_run_id,
                            })
                            .await;
                        // Continue loop to handle next tool in queue. This
                        // still counts as a completed tool-use round against
                        // the iteration cap (see comment on
                        // `increment_iterations` above).
                        increment_iterations = true;
                    }
                    crate::sandbox::ApprovalCheck::UserApprovalRequired
                        if front.call.name() == tools::clarify::ClarifyTool::NAME =>
                    {
                        let args = front.call.arguments();
                        let question = args
                            .get("question")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_string();
                        let context = args
                            .get("context")
                            .and_then(|value| value.as_str())
                            .map(str::to_string);
                        let choices =
                            args.get("choices")
                                .and_then(|value| value.as_array())
                                .map(|values| {
                                    values
                                        .iter()
                                        .filter_map(|value| value.as_str().map(str::to_string))
                                        .collect()
                                });
                        let call_id = front.call.id();
                        *mode = AgentMode::ClarifyPending;
                        *clarify_started_at = Some(std::time::Instant::now());
                        let _ = ui_tx
                            .send(UiUpdate::ClarifyPending {
                                id: call_id,
                                question,
                                context,
                                choices,
                                run_id: own_run_id,
                            })
                            .await;
                        return;
                    }
                    crate::sandbox::ApprovalCheck::UserApprovalRequired => {
                        log_tool_approval_request(front.call.as_ref());
                        *mode = AgentMode::ToolPending;
                        let _ = ui_tx
                            .send(UiUpdate::ToolPending {
                                name: front.call.name().to_string(),
                                id: front.call.id(),
                                arguments: front.call.arguments().clone(),
                                content_sections: front.call.pending_content_sections(),
                                title_arguments: front.call.title_arguments(),
                                run_id: own_run_id,
                            })
                            .await;
                        return;
                    }
                }
            }
        }
    }
}

/// Spawns the inner streaming task for one request, built from an immutable
/// snapshot of the conversation history. Returns the `CancellationToken` that
/// the outer loop uses to abort this request.
///
/// This task owns the [`ToolCallAccumulator`]s and assembles raw
/// [`StreamEvent::Delta`] fragments into a batch of [`AssembledToolCall`]s
/// (blueprint §4A refinement); the outer loop stays the sole writer of
/// `Context`, and this task never writes it.
fn start_request(
    context: &Context,
    engine: Arc<dyn Engine>,
    tools: Vec<ToolSchema>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<(u64, AgentEvent)>,
    generation: u64,
    own_run_id: Option<Uuid>,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let messages = context.messages().to_vec();
    let inner_cancel = cancel.clone();
    // `run_id` is `None` for the root loop and `Some(...)` for a child loop
    // (see `AgentLoop::own_run_id`); attaching it to this span lets every
    // nested log emitted while this request is in flight — including the
    // provider-level "chat request started"/"stream ended with [DONE]"
    // lines in `llm::openai_compatible_provider` — identify which agent loop it
    // belongs to, without threading `run_id` through the provider itself.
    let span = tracing::info_span!("agent_stream", run_id = ?own_run_id);
    tokio::spawn(
        async move {
            let mut stream = match engine.chat(messages, tools, inner_cancel.clone()).await {
                Ok(stream) => stream,
                Err(error) => {
                    // If the cancel token was already fired (e.g. user aborted
                    // while the server was still thinking), treat the resulting
                    // error as a cancellation rather than a hard error so
                    // domain_aborting receives StreamCancelled and exits cleanly.
                    if inner_cancel.is_cancelled() {
                        tracing::debug!(
                            "engine chat failed after cancel; treating as StreamCancelled"
                        );
                        let _ = ui_tx.send(UiUpdate::StreamCancelled).await;
                        let _ = agent_tx
                            .send((
                                generation,
                                AgentEvent::StreamCancelled {
                                    partial: String::new(),
                                },
                            ))
                            .await;
                    } else {
                        let message = format!("{error}");
                        tracing::debug!(
                            message = %message,
                            "engine chat failed; forwarding stream failure"
                        );
                        let _ = agent_tx
                            .send((generation, AgentEvent::StreamError(message)))
                            .await;
                    }
                    return;
                }
            };
            let _ = ui_tx.send(UiUpdate::StreamStarted).await;
            let mut partial = String::new();
            let mut accumulators: HashMap<usize, ToolCallAccumulator> = HashMap::new();
            while let Some(event) = stream.recv().await {
                match event {
                    StreamEvent::Text(text) => {
                        partial.push_str(&text);
                        let _ = ui_tx.send(UiUpdate::AppendText(text)).await;
                    }
                    StreamEvent::Thinking(text) => {
                        // Never appended to `partial` — thinking content must
                        // not affect the assistant text or tool-call logic.
                        let _ = ui_tx.send(UiUpdate::AppendThinking(text)).await;
                    }
                    StreamEvent::Delta(delta) => {
                        let accumulator =
                            accumulators
                                .entry(delta.tool_call_index)
                                .or_insert_with(|| ToolCallAccumulator {
                                    id: Uuid::new_v4(),
                                    name: None,
                                    arguments: String::new(),
                                });
                        if let Some(name) = delta.function_name {
                            if accumulator.name.is_none() {
                                accumulator.name = Some(name);
                            }
                        }
                        if let Some(fragment) = delta.arguments_fragment {
                            accumulator.arguments.push_str(&fragment);
                        }
                    }
                    StreamEvent::Done => {
                        if accumulators.is_empty() {
                            let _ = agent_tx
                                .send((generation, AgentEvent::StreamDone { partial }))
                                .await;
                        } else {
                            let mut calls = Vec::new();
                            let mut error: Option<String> = None;
                            for (_, accumulator) in accumulators {
                                match accumulator.into_tool_call_checked() {
                                    Ok(call) => calls.push(call),
                                    Err(raw) => {
                                        error = Some(format!(
                                            "stream ended with incomplete tool-call JSON: {raw}"
                                        ));
                                        break;
                                    }
                                }
                            }
                            if let Some(error) = error {
                                let _ = agent_tx
                                    .send((generation, AgentEvent::StreamError(error)))
                                    .await;
                            } else {
                                let _ = agent_tx
                                    .send((generation, AgentEvent::ToolCall { calls }))
                                    .await;
                            }
                        }
                        break;
                    }
                    StreamEvent::Cancelled => {
                        if accumulators.is_empty() {
                            let _ = ui_tx.send(UiUpdate::StreamCancelled).await;
                        }
                        let _ = agent_tx
                            .send((generation, AgentEvent::StreamCancelled { partial }))
                            .await;
                        break;
                    }
                    StreamEvent::Error(error) => {
                        let message = format!("{error}");
                        let _ = agent_tx
                            .send((generation, AgentEvent::StreamError(message)))
                            .await;
                        break;
                    }
                }
            }
        }
        .instrument(span),
    );
    cancel
}

/// Buffers the fragmented pieces of one tool call (blueprint §4A).
///
/// Raw `Delta` fragments for a given `tool_call_index` are concatenated in
/// order; the name is captured from the first fragment that carries it and the
/// accumulated `arguments` JSON is only parsed once complete.
#[derive(Debug, Clone)]
struct ToolCallAccumulator {
    id: Uuid,
    name: Option<String>,
    arguments: String,
}

impl ToolCallAccumulator {
    /// Finalize this accumulator into a validated tool call, or return the raw
    /// argument fragment when the JSON is incomplete/unparseable.
    fn into_tool_call_checked(self) -> Result<AssembledToolCall, String> {
        let name = self.name.unwrap_or_default();
        if self.arguments.trim().is_empty() {
            return Ok(AssembledToolCall {
                id: self.id,
                name,
                arguments: Value::Null,
            });
        }
        match serde_json::from_str(&self.arguments) {
            Ok(arguments) => Ok(AssembledToolCall {
                id: self.id,
                name,
                arguments,
            }),
            Err(_) => Err(self.arguments),
        }
    }
}

/// Drains queued steers into context as user messages; returns true if any
/// were injected. Injecting is the only way a steer reaches the LLM — it
/// never cancels a live stream.
fn inject_pending_steers(pending_steers: &mut VecDeque<String>, context: &mut Context) -> bool {
    if pending_steers.is_empty() {
        return false;
    }
    while let Some(message) = pending_steers.pop_front() {
        context.push_user(message);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use std::path::PathBuf;

    use crate::test_support::capture_logs;
    use crate::{
        agent::tools::{clarify::ClarifyTool, write::WriteTool},
        llm::engine::{Engine, EngineConfig, EngineImpl},
        llm::{MockEvent, MockProvider, ModelInfo, Provider, ToolSchema},
        sandbox::{Sandbox, SandboxContext},
    };

    struct FailingProvider;

    impl Provider for FailingProvider {
        fn name(&self) -> &str {
            "failing"
        }

        fn chat<'a>(
            &'a self,
            _messages: Vec<Message>,
            _tools: Vec<ToolSchema>,
            _cancel: CancellationToken,
        ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            Box::pin(async {
                Err(anyhow::anyhow!(
                    "provider returned HTTP 404: model 'invalid' not found"
                ))
            })
        }

        fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
            Box::pin(async { Ok(vec![]) })
        }
    }

    fn test_engine() -> Arc<dyn Engine> {
        Arc::new(EngineImpl::new(
            Arc::new(MockProvider::new(vec![], Duration::from_millis(0))),
            EngineConfig {
                provider_connection: "mock".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,
            },
        ))
    }

    #[test]
    fn stream_failure_emits_one_user_visible_error() {
        crate::test_support::run_locked(async {
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(FailingProvider),
                EngineConfig {
                    provider_connection: "failing".to_string(),
                    model: "invalid".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                },
            ));
            let (mut agent, mut ui_rx) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
            let handle = agent.run();
            agent
                .send_command(AgentCommand::Prompt("trigger failure".to_string()))
                .await;

            let mut error_messages = Vec::new();
            while let Ok(Some(update)) =
                tokio::time::timeout(Duration::from_millis(100), ui_rx.recv()).await
            {
                if let UiUpdate::Error { message, .. } = update {
                    error_messages.push(message);
                }
            }

            assert_eq!(
                error_messages,
                vec!["provider returned HTTP 404: model 'invalid' not found".to_string()],
                "a stream failure must be rendered once even though the provider and \
                 agent-loop layers both observe it"
            );

            drop(agent);
            handle
                .await
                .expect("agent task should not panic")
                .expect("agent should stop cleanly after the channel closes");
        });
    }

    #[test]
    fn user_visible_tool_failures_are_written_to_the_log() {
        let log = capture_logs(async {
            let script = vec![MockEvent::Delta(DeltaChunk {
                tool_call_index: 0,
                tool_call_id: Some("call-1".to_string()),
                function_name: Some("missing_tool".to_string()),
                arguments_fragment: Some("{}".to_string()),
            })];
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::with_script(script, Duration::from_millis(0))),
                EngineConfig {
                    provider_connection: "mock".to_string(),
                    model: "test-model".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                },
            ));
            let (mut agent, mut ui_rx) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
            let handle = agent.run();
            agent
                .send_command(AgentCommand::Prompt("trigger tool failure".to_string()))
                .await;

            loop {
                let update = ui_rx
                    .recv()
                    .await
                    .expect("agent should emit a tool error update");
                if matches!(update, UiUpdate::ToolError { .. }) {
                    break;
                }
            }

            drop(agent);
            handle
                .await
                .expect("agent task should not panic")
                .expect("agent should stop cleanly after the channel closes");
        });

        assert!(
            log.contains("tool failed"),
            "user-visible tool failures must be logged: {log}"
        );
        assert!(
            log.contains("Unknown tool 'missing_tool'"),
            "the log must include the tool failure diagnostic: {log}"
        );
    }

    fn test_sandbox() -> std::sync::Arc<std::sync::RwLock<Sandbox>> {
        std::sync::Arc::new(std::sync::RwLock::new(Sandbox::with_root(PathBuf::from(
            env!("CARGO_MANIFEST_DIR"),
        ))))
    }

    #[test]
    fn agent_command_dispatch_logs_every_command_variant() {
        let log = capture_logs(async {
            tracing::callsite::rebuild_interest_cache();
            let registry = Arc::new(ToolRegistry::new());
            let (mut agent_loop, _ui_rx) = AgentLoop::new(test_engine(), registry);
            let handle = agent_loop.run();
            let tool_call_id = Uuid::nil();
            let child_run_id = Uuid::nil();

            agent_loop
                .send_command(AgentCommand::Prompt(String::new()))
                .await;
            agent_loop
                .send_command(AgentCommand::ApproveTool { id: tool_call_id })
                .await;
            agent_loop
                .send_command(AgentCommand::DenyTool {
                    id: tool_call_id,
                    reason: "private denial reason".to_string(),
                })
                .await;
            agent_loop
                .send_command(AgentCommand::AnswerClarify("private answer".to_string()))
                .await;
            agent_loop.send_command(AgentCommand::AbortClarify).await;
            agent_loop
                .send_command(AgentCommand::Stop {
                    reason: "private stop reason".to_string(),
                })
                .await;
            agent_loop.send_command(AgentCommand::ClearContext).await;
            agent_loop
                .send_command(AgentCommand::RecordModelChanged {
                    provider_connection: "private connection".to_string(),
                    model: "private model".to_string(),
                })
                .await;
            agent_loop
                .send_command(AgentCommand::ForChild {
                    run_id: child_run_id,
                    command: Box::new(AgentCommand::AbortClarify),
                })
                .await;

            drop(agent_loop);
            handle
                .await
                .expect("agent command logging task should not panic")
                .expect("agent command logging loop should shut down cleanly");
        });

        for command_name in [
            "prompt",
            "approve_tool",
            "deny_tool",
            "answer_clarify",
            "abort_clarify",
            "stop",
            "clear_context",
            "record_model_changed",
            "for_child",
        ] {
            assert!(
                log.contains(&format!("command=\"{command_name}\"")),
                "missing receipt log for {command_name}: {log}"
            );
        }
        assert_eq!(
            log.matches("agent command received").count(),
            9,
            "each command should produce exactly one receipt event: {log}"
        );
        assert!(
            !log.contains("private"),
            "command receipt logs must not include private payloads: {log}"
        );
    }

    #[test]
    fn constructs_message_variants() {
        let system = Message::System {
            content: "system".into(),
        };
        let user = Message::User {
            content: "user".into(),
            kind: UserMessageKind::Explicit,
        };
        let assistant = Message::Assistant {
            content: "assistant".into(),
            status: AssistantStatus::Complete,
            tool_calls: Vec::new(),
        };
        let tool_result = Message::ToolResult {
            value: ToolResult {
                id: Uuid::new_v4(),
                name: "tool".into(),
                result: "ok".into(),
                attachments: vec![],
                result_section_id: None,
                result_metadata: None,
                result_metadata_schema: None,
                summary: vec![],
                content_sections: vec![],
                duration: std::time::Duration::ZERO,
            },
        };
        let tool_error = Message::ToolError {
            id: Uuid::new_v4(),
            name: "tool".into(),
            error: "err".into(),
            duration: std::time::Duration::ZERO,
            content_sections: vec![],
        };
        assert!(matches!(system, Message::System { .. }));
        assert!(matches!(user, Message::User { .. }));
        assert!(matches!(assistant, Message::Assistant { .. }));
        assert!(matches!(tool_result, Message::ToolResult { .. }));
        assert!(matches!(tool_error, Message::ToolError { .. }));
    }

    #[test]
    fn tool_call_request_uses_uuid_id() {
        let id = Uuid::new_v4();
        let request = ToolCallRequest {
            id,
            name: "search".into(),
            arguments: serde_json::json!({ "query": "rust" }),
            persisted_title_arguments: vec![],
        };
        assert_eq!(request.id, id);
        assert_eq!(request.name, "search");
        assert_eq!(request.arguments, serde_json::json!({ "query": "rust" }));
    }

    #[test]
    fn tool_call_request_preserves_tool_specific_title_arguments() {
        let id = Uuid::new_v4();
        let title_arguments = vec![(
            "path".to_string(),
            serde_json::Value::String("src/example.rs".to_string()),
        )];
        let request = ToolCallRequest {
            id,
            name: "write".to_string(),
            arguments: serde_json::json!({
                "path": "src/example.rs",
                "content": "fn main() {}",
            }),
            persisted_title_arguments: title_arguments.clone(),
        };

        assert_eq!(request.title_arguments(), title_arguments);
        let serialized = serde_json::to_value(&request).expect("request serializes");
        assert_eq!(
            serialized["title_arguments"],
            serde_json::json!(title_arguments)
        );
    }

    #[test]
    fn session_begins_with_seeded_system_messages() {
        let context = Context::new();
        assert_eq!(
            context.messages(),
            &[
                Message::System {
                    content: crate::config::soul::DEFAULT_SOUL.to_string(),
                },
                Message::System {
                    content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
                },
            ]
        );
    }

    #[test]
    fn session_can_begin_with_a_caller_supplied_system_message() {
        let context = Context::with_system_prompt("project persona");

        assert_eq!(
            context.messages()[0],
            Message::System {
                content: "project persona".to_string(),
            }
        );
        assert!(matches!(context.messages()[1], Message::System { .. }));
    }

    #[test]
    fn push_messages_appends_in_order() {
        let mut context = Context::new();
        context.push_user("first user turn".to_string());
        context.push_assistant("first response".to_string(), AssistantStatus::Complete);
        context.push_user("second user turn".to_string());
        assert_eq!(context.messages().len(), 5);
        assert_eq!(
            context.messages()[2],
            Message::User {
                content: "first user turn".to_string(),
                kind: UserMessageKind::Explicit,
            }
        );
        assert_eq!(
            context.messages()[3],
            Message::Assistant {
                content: "first response".to_string(),
                status: AssistantStatus::Complete,
                tool_calls: Vec::new(),
            }
        );
        assert_eq!(
            context.messages()[4],
            Message::User {
                content: "second user turn".to_string(),
                kind: UserMessageKind::Explicit,
            }
        );
    }

    #[test]
    fn push_assistant_marks_interrupted_status() {
        let mut context = Context::new();
        context.push_assistant("partial".to_string(), AssistantStatus::Interrupted);
        assert_eq!(
            context.messages()[2],
            Message::Assistant {
                content: "partial".to_string(),
                status: AssistantStatus::Interrupted,
                tool_calls: Vec::new(),
            }
        );
    }

    #[test]
    fn push_assistant_tool_call_stores_calls() {
        let mut context = Context::new();
        let id = Uuid::new_v4();
        context.push_assistant_tool_call(
            String::new(),
            AssistantStatus::Complete,
            vec![ToolCallRequest {
                id,
                name: "read".into(),
                arguments: serde_json::json!({ "path": "a.txt" }),
                persisted_title_arguments: vec![],
            }],
        );
        assert_eq!(
            context.messages()[2],
            Message::Assistant {
                content: String::new(),
                status: AssistantStatus::Complete,
                tool_calls: vec![ToolCallRequest {
                    id,
                    name: "read".into(),
                    arguments: serde_json::json!({ "path": "a.txt" }),
                    persisted_title_arguments: vec![],
                }],
            }
        );
    }

    #[test]
    fn push_tool_result_and_error_append_messages() {
        let mut context = Context::new();
        let id = Uuid::new_v4();
        context.push_tool_result(ToolResult {
            id,
            name: "read".into(),
            result: "contents".into(),
            attachments: vec![],
            result_section_id: None,
            result_metadata: None,
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: std::time::Duration::ZERO,
        });
        context.push_tool_error(
            id,
            "write".into(),
            "denied".into(),
            None,
            std::time::Duration::ZERO,
            vec![],
        );
        assert_eq!(
            context.messages()[2],
            Message::ToolResult {
                value: ToolResult {
                    id,
                    name: "read".into(),
                    result: "contents".into(),
                    attachments: vec![],
                    result_section_id: None,
                    result_metadata: None,
                    result_metadata_schema: None,
                    summary: vec![],
                    content_sections: vec![],
                    duration: std::time::Duration::ZERO,
                },
            }
        );
        assert_eq!(
            context.messages()[3],
            Message::ToolError {
                id,
                name: "write".into(),
                error: "denied".into(),
                duration: std::time::Duration::ZERO,
                content_sections: vec![],
            }
        );
    }

    #[test]
    fn tool_messages_round_trip_replay_metadata_and_legacy_defaults() {
        let id = Uuid::new_v4();
        let content_section =
            crate::agent::tools::Section::Content(crate::agent::tools::ContentSection {
                label: Some("Output".to_string()),
                content: "contents".to_string(),
                file_extension: Some("txt".to_string()),
                line_number_offset: Some(4),
            });
        let result = Message::ToolResult {
            value: ToolResult {
                id,
                name: "read".to_string(),
                result: "contents".to_string(),
                attachments: vec![ToolAttachment::File {
                    media_type: "application/pdf".to_string(),
                    filename: "report.pdf".to_string(),
                }],
                result_section_id: None,
                result_metadata: Some(serde_json::json!({"bytes": 8})),
                result_metadata_schema: Some(serde_json::json!({"type": "object"})),
                summary: vec![("path".to_string(), "README.md".to_string())],
                content_sections: vec![content_section.clone()],
                duration: std::time::Duration::from_millis(37),
            },
        };
        let error = Message::ToolError {
            id,
            name: "write".to_string(),
            error: "denied".to_string(),
            duration: std::time::Duration::from_millis(11),
            content_sections: vec![content_section],
        };

        let serialized_result =
            serde_json::to_value(&result).expect("tool result serializes to a value");
        assert_eq!(serialized_result["role"], "tool_result");
        assert_eq!(serialized_result["id"], serde_json::json!(id));
        assert_eq!(serialized_result["result"], "contents");
        assert_eq!(serialized_result["attachments"][0]["type"], "file");
        assert_eq!(
            serialized_result["attachments"][0]["filename"],
            "report.pdf"
        );
        assert!(
            serialized_result.get("value").is_none(),
            "the canonical payload must remain flattened in the persisted schema"
        );

        for message in [result.clone(), error.clone()] {
            let serialized = serde_json::to_string(&message).expect("message serializes");
            let restored =
                serde_json::from_str::<Message>(&serialized).expect("message deserializes");
            assert_eq!(restored, message);
        }

        let legacy_result = serde_json::json!({
            "role": "tool_result",
            "id": id,
            "name": "read",
            "result": "old contents",
            "result_metadata": null,
            "result_metadata_schema": null,
        });
        assert_eq!(
            serde_json::from_value::<Message>(legacy_result).expect("legacy result deserializes"),
            Message::ToolResult {
                value: ToolResult {
                    id,
                    name: "read".to_string(),
                    result: "old contents".to_string(),
                    attachments: vec![],
                    result_section_id: None,
                    result_metadata: None,
                    result_metadata_schema: None,
                    summary: vec![],
                    content_sections: vec![],
                    duration: std::time::Duration::ZERO,
                },
            }
        );

        let legacy_error = serde_json::json!({
            "role": "tool_error",
            "id": id,
            "name": "write",
            "error": "old error",
            "duration": { "secs": 1, "nanos": 2 },
        });
        assert_eq!(
            serde_json::from_value::<Message>(legacy_error).expect("legacy error deserializes"),
            Message::ToolError {
                id,
                name: "write".to_string(),
                error: "old error".to_string(),
                duration: std::time::Duration::new(1, 2),
                content_sections: vec![],
            }
        );
    }

    #[test]
    fn hydrate_appends_after_system_prompts_and_synthesizes_orphaned_tool_error() {
        let id = Uuid::new_v4();
        let mut context = Context::new();
        context.hydrate(vec![
            Message::User {
                content: "resume this".to_string(),
                kind: UserMessageKind::Explicit,
            },
            Message::Assistant {
                content: String::new(),
                status: AssistantStatus::Complete,
                tool_calls: vec![ToolCallRequest {
                    id,
                    name: "read".to_string(),
                    arguments: serde_json::json!({"path": "README.md"}),
                    persisted_title_arguments: vec![],
                }],
            },
        ]);

        assert!(matches!(context.messages()[0], Message::System { .. }));
        assert!(matches!(context.messages()[1], Message::System { .. }));
        assert!(matches!(
            context.messages()[2],
            Message::User { ref content, .. } if content == "resume this"
        ));
        assert!(matches!(
            context.messages().last(),
            Some(Message::ToolError { id: error_id, name, error, duration, content_sections })
                if *error_id == id
                    && name == "read"
                    && error == "Tool call did not complete before the session ended."
                    && *duration == std::time::Duration::ZERO
                    && content_sections.is_empty()
        ));

        let wire_payload =
            crate::llm::openai_compatible_messages::serialize_messages(context.messages());
        assert!(wire_payload
            .as_array()
            .expect("wire payload is an array")
            .iter()
            .any(|message| {
                let content = message["content"]
                    .as_str()
                    .and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok());
                message["role"] == "tool"
                    && message["tool_call_id"] == id.to_string()
                    && content.is_some_and(|content| content["ok"] == false)
            }));
    }

    #[tokio::test]
    async fn attaching_a_store_hydrates_without_recommitting_messages() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let store = crate::store::Store::create(
            &temporary_directory.path().join("chats"),
            temporary_directory.path(),
            Uuid::new_v4(),
            "mock",
            "test-model",
        )
        .expect("store created");
        store.record_message(&Message::User {
            content: "persisted once".to_string(),
            kind: UserMessageKind::Explicit,
        });
        store.flush().await;

        let mut context = Context::new();
        context.set_store(store.clone());
        store.flush().await;

        let messages = crate::store::Store::read_messages(store.path()).expect("messages readable");
        assert_eq!(messages.len(), 1);
        assert_eq!(context.messages().len(), 3);
        assert!(matches!(
            &context.messages()[2],
            Message::User { content, .. } if content == "persisted once"
        ));
    }

    #[test]
    fn child_finish_notice_is_appended_as_implicit_user_message_not_a_second_tool_result() {
        // Regression test: `spawn_subagent`'s two-stage protocol (an
        // immediate "running"/"queued" `ToolResult`, later finalized once
        // the child actually finishes) must never leave two `role:"tool"`
        // messages tied to the same `tool_call_id` in the wire history —
        // that violates the one-call/one-response tool-calling protocol and
        // visibly confuses models. The fix is to never touch the original
        // `ToolResult` again; the child's real outcome is appended as a
        // brand-new implicit `user` message instead (see
        // `push_async_harness_notice` — not `role:"system"`, which some
        // providers/models reject mid-conversation), even with an assistant
        // message (the root's "running" reply) already in between.
        let mut context = Context::new();
        let id = Uuid::new_v4();
        context.push_tool_result(ToolResult {
            id,
            name: "spawn_subagent".into(),
            result: "{\"status\":\"spawned\"}".into(),
            attachments: vec![],
            result_section_id: None,
            result_metadata: None,
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: std::time::Duration::ZERO,
        });
        context.push_assistant(
            "I have spawned the child, standing by.".into(),
            AssistantStatus::Complete,
        );
        context.push_async_harness_notice(format!(
            "The subagent spawned via `spawn_subagent` (tool_call_id={id}) has finished. \
             Its final result:\n{{\"status\":\"success\",\"result\":\"done\"}}"
        ));

        let tool_result_count = context
            .messages()
            .iter()
            .filter(|message| {
                matches!(
                    message,
                    Message::ToolResult {
                        value: ToolResult {
                            id: message_id, ..
                        },
                    } if *message_id == id
                )
            })
            .count();
        assert_eq!(
            tool_result_count,
            1,
            "the original provisional ToolResult must remain untouched, got: {:?}",
            context.messages()
        );
        assert_eq!(
            context.messages()[2],
            Message::ToolResult {
                value: ToolResult {
                    id,
                    name: "spawn_subagent".into(),
                    result: "{\"status\":\"spawned\"}".into(),
                    attachments: vec![],
                    result_section_id: None,
                    result_metadata: None,
                    result_metadata_schema: None,
                    summary: vec![],
                    content_sections: vec![],
                    duration: std::time::Duration::ZERO,
                },
            },
            "the provisional result must never be rewritten"
        );
        assert!(
            matches!(
                context.messages().last(),
                Some(Message::User { content, kind: UserMessageKind::ImplicitToolNote })
                    if content.contains(HARNESS_NOTICE_MARKER) && content.contains("has finished")
            ),
            "the child's real outcome must be appended as a trailing implicit user message \
             carrying the harness-notice marker, got: {:?}",
            context.messages()
        );
    }

    #[test]
    fn messages_returns_immutable_read_back_slice() {
        let mut context = Context::new();
        context.push_user("hello".to_string());
        let read_back = context.messages();
        assert_eq!(read_back.len(), 3);
        assert!(matches!(read_back[0], Message::System { .. }));
        assert!(matches!(read_back[1], Message::System { .. }));
        assert_eq!(
            read_back[2],
            Message::User {
                content: "hello".into(),
                kind: UserMessageKind::Explicit,
            }
        );
    }

    #[test]
    fn construct_and_pattern_match_protocol_enums() {
        assert!(matches!(
            AgentCommand::Prompt("hello".into()),
            AgentCommand::Prompt(text) if text == "hello"
        ));
        assert!(matches!(
            AgentCommand::Stop {
                reason: String::new()
            },
            AgentCommand::Stop { .. }
        ));
        let id = Uuid::new_v4();
        assert!(matches!(
            AgentCommand::ApproveTool { id },
            AgentCommand::ApproveTool { id: got } if got == id
        ));
        assert!(matches!(
            AgentCommand::DenyTool { id, reason: "r".into() },
            AgentCommand::DenyTool { id: got, .. } if got == id
        ));
        assert!(matches!(
            AgentCommand::AnswerClarify("answer".into()),
            AgentCommand::AnswerClarify(answer) if answer == "answer"
        ));

        assert!(matches!(
            AgentEvent::StreamDone { partial: "done".into() },
            AgentEvent::StreamDone { partial } if partial == "done"
        ));
        assert!(matches!(
            AgentEvent::StreamCancelled { partial: "cancel".into() },
            AgentEvent::StreamCancelled { partial } if partial == "cancel"
        ));
        assert!(matches!(
            AgentEvent::StreamError("boom".into()),
            AgentEvent::StreamError(message) if message == "boom"
        ));

        assert!(
            matches!(UiUpdate::AppendText("tok".into()), UiUpdate::AppendText(text) if text == "tok")
        );
        assert!(matches!(
            UiUpdate::AppendThinking("hmm".into()),
            UiUpdate::AppendThinking(text) if text == "hmm"
        ));
        assert!(matches!(UiUpdate::StreamStarted, UiUpdate::StreamStarted));
        assert!(matches!(UiUpdate::StreamDone, UiUpdate::StreamDone));
        assert!(matches!(
            UiUpdate::StreamCancelled,
            UiUpdate::StreamCancelled
        ));
        assert!(matches!(
            UiUpdate::ClarifyPending {
                id,
                question: "question".into(),
                context: None,
                choices: None,
                run_id: None,
            },
            UiUpdate::ClarifyPending {
                id: got,
                question: got_question,
                context: _,
                choices: None,
                run_id: _,
            } if got == id && got_question == "question"
        ));
        assert!(matches!(
            UiUpdate::Error {
                message: "oops".into(),
                run_id: None,
            },
            UiUpdate::Error { message, run_id: None } if message == "oops"
        ));

        assert!(
            matches!(StreamEvent::Text("tok".into()), StreamEvent::Text(text) if text == "tok")
        );
        assert!(matches!(
            StreamEvent::Thinking("hmm".into()),
            StreamEvent::Thinking(text) if text == "hmm"
        ));
        assert!(matches!(StreamEvent::Done, StreamEvent::Done));
        assert!(matches!(StreamEvent::Cancelled, StreamEvent::Cancelled));
        assert!(matches!(
            StreamEvent::Error(anyhow::anyhow!("network")),
            StreamEvent::Error(error) if format!("{error}") == "network"
        ));
        assert!(matches!(
            StreamEvent::Delta(DeltaChunk {
                tool_call_index: 0,
                tool_call_id: None,
                function_name: Some("read".into()),
                arguments_fragment: Some("{\"path\":\"a.txt\"}".into()),
            }),
            StreamEvent::Delta(_)
        ));

        assert_eq!(AgentMode::Idle, AgentMode::Idle);
        assert_eq!(AgentMode::Streaming, AgentMode::Streaming);
        assert_eq!(AgentMode::ToolPending, AgentMode::ToolPending);
        assert_eq!(AgentMode::ClarifyPending, AgentMode::ClarifyPending);
    }

    #[test]
    fn accumulator_assembles_single_fragment_call() {
        let mut acc = ToolCallAccumulator {
            id: Uuid::new_v4(),
            name: None,
            arguments: String::new(),
        };
        acc.name = Some("read".to_string());
        acc.arguments.push_str(r#"{"path":"a.txt"}"#);
        let call = acc.into_tool_call_checked().expect("valid arguments");
        assert_eq!(call.name, "read");
        assert_eq!(call.arguments, serde_json::json!({ "path": "a.txt" }));
    }

    #[test]
    fn accumulator_concatenates_fragmented_arguments_across_indices() {
        let mut acc_0 = ToolCallAccumulator {
            id: Uuid::new_v4(),
            name: Some("read".into()),
            arguments: String::new(),
        };
        let mut acc_1 = ToolCallAccumulator {
            id: Uuid::new_v4(),
            name: Some("glob".into()),
            arguments: String::new(),
        };
        acc_0.arguments.push_str(r#"{"path""#);
        acc_1.arguments.push_str(r#"{"pattern""#);
        acc_0.arguments.push_str(r#":"a.txt"}"#);
        acc_1.arguments.push_str(r#":"*.rs"}"#);
        let call_0 = acc_0.into_tool_call_checked().expect("a.txt parses");
        let call_1 = acc_1.into_tool_call_checked().expect("glob parses");
        assert_eq!(call_0.arguments, serde_json::json!({ "path": "a.txt" }));
        assert_eq!(call_1.arguments, serde_json::json!({ "pattern": "*.rs" }));
    }

    #[test]
    fn accumulator_reports_incomplete_json_error_with_fragment() {
        let acc = ToolCallAccumulator {
            id: Uuid::new_v4(),
            name: Some("read".into()),
            arguments: r#"{"path":"unterminated"#.to_string(),
        };
        let err = acc.into_tool_call_checked().expect_err("incomplete JSON");
        assert_eq!(err, r#"{"path":"unterminated"#);
    }

    #[test]
    fn accumulator_empty_arguments_yields_null() {
        let acc = ToolCallAccumulator {
            id: Uuid::new_v4(),
            name: Some("glob".into()),
            arguments: String::new(),
        };
        let call = acc.into_tool_call_checked().expect("empty is valid");
        assert_eq!(call.arguments, Value::Null);
    }

    #[test]
    fn tool_execution_log_events_carry_spanned_tool_call_id() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let write_path = dir.path().join("out.txt");
        let log = capture_logs(async {
            let id = Uuid::new_v4();
            let (agent_tx, mut agent_rx) = mpsc::channel(8);
            let registry = tools::default_registry(
                std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new())),
                tools::ToolRegistryConfig::default(),
            );
            let pending = PendingTool {
                call: registry
                    .create_call(
                        "write",
                        id,
                        serde_json::json!({
                            "path": write_path.display().to_string(),
                            // Deliberately not lowercase-hex-only: a `Uuid::new_v4()`
                            // (used below as `tool_call_id`) is rendered into the log
                            // and consists solely of lowercase hex digits and hyphens,
                            // so a hex-only content literal (e.g. "abc") can rarely but
                            // genuinely collide with a random id substring and false-
                            // positive the "must not log content" assertion below.
                            "content": "XYZ",
                        }),
                    )
                    .expect("write tool registered"),
            };
            let _cancel = pending.execute(agent_tx, 0);
            tokio::time::timeout(Duration::from_secs(2), async {
                while let Some((_generation, event)) = agent_rx.recv().await {
                    if matches!(event, AgentEvent::ToolDone { .. }) {
                        break;
                    }
                }
            })
            .await
            .expect("tool completes in time");
        });
        let contents = log;
        assert!(
            contents.contains("tool_execute{tool=write"),
            "tool span with its tool field must be rendered: {contents}"
        );
        assert!(
            contents.contains("tool_call_id="),
            "tool call id must be attached to the tool's log events: {contents}"
        );
        assert!(
            contents.contains("wrote file"),
            "the write tool's log event must be inside the tool span: {contents}"
        );
        assert!(
            !contents.contains("XYZ"),
            "the write tool must not log the written content: {contents}"
        );
        let written = std::fs::read_to_string(&write_path).expect("read written file");
        assert_eq!(written, "XYZ");
    }

    #[tokio::test]
    async fn advance_tool_queue_emits_clarify_pending_for_clarify_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ClarifyTool));
        let registry = Arc::new(registry);
        let call_id = Uuid::new_v4();
        let mut pending_tools = VecDeque::from([PendingTool {
            call: registry
                .create_call(
                    "clarify",
                    call_id,
                    serde_json::json!({
                        "question": "Pick one",
                        "choices": ["a", "b"],
                    }),
                )
                .expect("clarify tool registered"),
        }]);
        let (ui_tx, mut ui_rx) = mpsc::channel(4);
        let (agent_tx, _agent_rx) = mpsc::channel(1);
        let mut context = Context::new();
        let mut mode = AgentMode::ToolPending;
        let mut current_cancel = None;
        let mut current_tool_cancel = None;
        let mut iterations = 1usize;
        let mut clarify_started_at = None;
        let mut pending_steers = VecDeque::new();

        advance_tool_queue(
            &mut pending_tools,
            &mut context,
            &mut mode,
            &mut current_cancel,
            &mut current_tool_cancel,
            &mut iterations,
            false,
            test_engine(),
            registry,
            ui_tx,
            agent_tx,
            0,
            &mut clarify_started_at,
            &mut pending_steers,
            &mut None,
            &mut HashMap::new(),
            None,
            &mut false,
            &mut None,
        )
        .await;

        assert_eq!(mode, AgentMode::ClarifyPending);
        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::ClarifyPending {
                id,
                question,
                context: _,
                choices: Some(choices),
                run_id: _,
            }) if id == call_id && question == "Pick one" && choices == vec!["a".to_string(), "b".to_string()]
        ));
    }

    #[tokio::test]
    async fn advance_tool_queue_emits_tool_pending_for_non_clarify_tool() {
        let sandbox = test_sandbox();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WriteTool::new(SandboxContext::new(sandbox))));
        let registry = Arc::new(registry);
        let call_id = Uuid::new_v4();
        let mut pending_tools = VecDeque::from([PendingTool {
            call: registry
                .create_call(
                    "write",
                    call_id,
                    serde_json::json!({
                        "path": "Cargo.toml",
                        "content": "ignored",
                    }),
                )
                .expect("write tool registered"),
        }]);
        let (ui_tx, mut ui_rx) = mpsc::channel(4);
        let (agent_tx, _agent_rx) = mpsc::channel(1);
        let mut context = Context::new();
        let mut mode = AgentMode::ToolPending;
        let mut current_cancel = None;
        let mut current_tool_cancel = None;
        let mut iterations = 1usize;
        let mut clarify_started_at = None;
        let mut pending_steers = VecDeque::new();

        advance_tool_queue(
            &mut pending_tools,
            &mut context,
            &mut mode,
            &mut current_cancel,
            &mut current_tool_cancel,
            &mut iterations,
            false,
            test_engine(),
            registry,
            ui_tx,
            agent_tx,
            0,
            &mut clarify_started_at,
            &mut pending_steers,
            &mut None,
            &mut HashMap::new(),
            None,
            &mut false,
            &mut None,
        )
        .await;

        assert_eq!(mode, AgentMode::ToolPending);
        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::ToolPending { id, name, .. }) if id == call_id && name == "write"
        ));
    }

    #[tokio::test]
    async fn advance_tool_queue_injects_pending_steers_before_starting_next_request() {
        // Regression test: queued steers (pending user messages typed while
        // tools were running) must reach the model on the very next request
        // once all tool results for this round are in context, rather than
        // waiting for a full tool-free turn to complete via StreamDone.
        let registry = Arc::new(ToolRegistry::new());
        let mut pending_tools: VecDeque<PendingTool> = VecDeque::new();
        let (ui_tx, mut ui_rx) = mpsc::channel(4);
        let (agent_tx, _agent_rx) = mpsc::channel(1);
        let mut context = Context::new();
        context.push_tool_result(ToolResult {
            id: Uuid::new_v4(),
            name: "write".to_string(),
            result: "ok".to_string(),
            attachments: vec![],
            result_section_id: None,
            result_metadata: None,
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: std::time::Duration::ZERO,
        });
        let mut mode = AgentMode::ToolPending;
        let mut current_cancel = None;
        let mut current_tool_cancel = None;
        let mut iterations = 1usize;
        let mut clarify_started_at = None;
        let mut pending_steers = VecDeque::from(["steered while tool ran".to_string()]);

        advance_tool_queue(
            &mut pending_tools,
            &mut context,
            &mut mode,
            &mut current_cancel,
            &mut current_tool_cancel,
            &mut iterations,
            true,
            test_engine(),
            registry,
            ui_tx,
            agent_tx,
            0,
            &mut clarify_started_at,
            &mut pending_steers,
            &mut None,
            &mut HashMap::new(),
            None,
            &mut false,
            &mut None,
        )
        .await;

        assert_eq!(mode, AgentMode::Streaming);
        assert!(pending_steers.is_empty(), "steer must be drained");
        assert_eq!(
            context.messages().last(),
            Some(&Message::User {
                content: "steered while tool ran".to_string(),
                kind: UserMessageKind::Explicit,
            })
        );
        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::SteerApplied { .. })
        ));
    }

    #[test]
    fn root_reprompts_after_child_finish_arrives_mid_stream() {
        crate::test_support::run_locked(async {
            // Regression test: a `spawn_subagent` child's `FinishResult` landing
            // while the root is mid-`Streaming` (not `Idle`) must force a
            // re-prompt once the root's current tool-free stream completes,
            // instead of silently going `Idle` and stranding the child's result
            // in `context` unseen by the model. Without the
            // `pending_child_finish_reprompt` flag this reproduces the bug where
            // the root's last streamed text claims it's "waiting for subagents"
            // and the conversation ends right there.
            use crate::agent::tools::{default_registry, ToolRegistryConfig};
            use crate::child_agent_manager::{ChildAgentManager, FinishResult};

            // Two text tokens with a real gap between them so there is a window
            // to inject the `FinishResult` while `mode == Streaming`, i.e. before
            // this turn's `StreamDone`.
            let script = vec![
                MockEvent::Text("Pending: waiting for subagents".to_string()),
                MockEvent::Text(" ...".to_string()),
            ];
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::with_script(script, Duration::from_millis(80))),
                EngineConfig {
                    provider_connection: "mock".to_string(),
                    model: "test-model".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                },
            ));

            let registry = Arc::new(default_registry(
                test_sandbox(),
                ToolRegistryConfig::default(),
            ));

            let (subagent_ui_tx, _subagent_ui_rx) = mpsc::channel(8);
            let manager = ChildAgentManager::new(
                subagent_ui_tx,
                Box::new(
                    |_request, _cancel, _ui_tx| crate::child_agent_manager::SpawnOutcome {
                        command_sender: None,
                        handle: tokio::spawn(async {}),
                    },
                ),
            );
            let (finish_tx, finish_rx) = mpsc::channel(1);
            let (parent_subagent_ui_tx, parent_subagent_ui_rx) = mpsc::channel(8);
            let _ = parent_subagent_ui_tx; // kept alive only to avoid closing the channel early

            let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine, registry);
            agent_loop =
                agent_loop.with_child_agent_manager(manager, finish_rx, parent_subagent_ui_rx);

            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt("do the task".into()))
                .await;

            // Let the first stream start (mode -> Streaming) before injecting.
            tokio::time::sleep(Duration::from_millis(20)).await;
            finish_tx
                .send(FinishResult {
                    run_id: Uuid::new_v4(),
                    call_id: Uuid::new_v4(),
                    origin: crate::child_agent_manager::FinishResultOrigin::Agent,
                    status: "success".to_string(),
                    result: Some("child done".to_string()),
                    error: None,
                    sources: vec![],
                })
                .await
                .expect("finish_rx must still be open");

            // Give the loop time to finish the first stream, observe the
            // deferred finish while not idle, and (if fixed) start a second,
            // tool-free re-prompt.
            tokio::time::sleep(Duration::from_millis(300)).await;

            // Dropping the loop closes `cmd_tx`, which is how `run()` returns the
            // final committed history (see `cli_driver.rs`'s identical pattern).
            drop(agent_loop);

            let context = tokio::time::timeout(Duration::from_secs(2), handle)
                .await
                .expect("run() should return promptly after being dropped")
                .expect("run() task should not panic")
                .expect("run() should not return an error");

            let assistant_turns = context
                .messages()
                .iter()
                .filter(|message| matches!(message, Message::Assistant { .. }))
                .count();
            assert!(
                assistant_turns >= 2,
                "root must be re-prompted after the child's result arrives mid-stream \
             instead of going idle after a single turn; got {assistant_turns} assistant turn(s)"
            );
            assert!(
            context.messages().iter().any(
                |message| matches!(message, Message::User { content, kind: UserMessageKind::ImplicitToolNote } if content.contains("Subagent completion event") && content.contains("\"event\":\"subagent_completed\""))
            ),
            "the child's finish result must be recorded in context as an implicit user message"
        );

            // Drain the ui updates; just ensure the channel didn't panic/drop.
            while ui_rx.try_recv().is_ok() {}
        });
    }

    #[test]
    fn clear_context_drops_stale_stream_event_from_a_superseded_generation() {
        crate::test_support::run_locked(async {
            // Regression test: `AgentCommand::ClearContext` must not let an
            // `AgentEvent` produced by the pre-clear stream leak into the
            // brand-new `Context` it installs. Cancellation is cooperative —
            // the old stream's producer task only notices `current_cancel`
            // was fired the next time it's polled, so it can still emit a
            // `StreamCancelled`/`StreamDone` after the clear has already run.
            // Without the `generation` guard, `StreamCancelled`'s existing
            // `current_cancel.is_none()` check (designed to reattach a
            // genuinely interrupted partial to the *same* conversation, as
            // `AgentCommand::Stop` does) mistakes this stale event for a
            // fresh interrupt and pushes the old partial into the new,
            // unrelated context.
            let script = vec![
                MockEvent::Text("Pending: partial from before clear".to_string()),
                MockEvent::Text(" ...".to_string()),
            ];
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::with_script(script, Duration::from_millis(80))),
                EngineConfig {
                    provider_connection: "mock".to_string(),
                    model: "test-model".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                },
            ));

            let (mut agent_loop, mut ui_rx) = AgentLoop::new(
                engine,
                Arc::new(tools::default_registry(
                    test_sandbox(),
                    tools::ToolRegistryConfig::default(),
                )),
            );
            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt("do the task".into()))
                .await;

            // Let the first stream start (mode -> Streaming, current_cancel ->
            // Some) before clearing.
            tokio::time::sleep(Duration::from_millis(20)).await;
            agent_loop.send_command(AgentCommand::ClearContext).await;

            // Give the old stream's producer task time to notice the
            // cancellation and emit its (now stale) `StreamCancelled`.
            tokio::time::sleep(Duration::from_millis(300)).await;

            drop(agent_loop);
            let context = tokio::time::timeout(Duration::from_secs(2), handle)
                .await
                .expect("run() should return promptly after being dropped")
                .expect("run() task should not panic")
                .expect("run() should not return an error");

            assert!(
                !context.messages().iter().any(|message| matches!(
                    message,
                    Message::Assistant { content, .. } if content.contains("Pending: partial from before clear")
                )),
                "a stale stream event from before ClearContext must not be \
                 committed into the fresh context; got {:?}",
                context.messages()
            );

            while ui_rx.try_recv().is_ok() {}
        });
    }

    #[test]
    fn child_role_run_returns_immediately_after_finish_tool_is_called() {
        crate::test_support::run_locked(async {
            // Regression/integration test for Phase 6.9: a `Role::Child` agent
            // whose model calls `agent_finish` must have its context carry the tool
            // result, must send the `FinishResult` through `finish_tx`, and must
            // return from `run()` without waiting for a tool-free `StreamDone`.
            use crate::agent::tools::{
                agent_finish::AgentFinishToolConfig, default_registry, ToolRegistryConfig,
            };

            let script = vec![MockEvent::Delta(DeltaChunk {
                tool_call_index: 0,
                tool_call_id: Some("call_1".into()),
                function_name: Some(tools::agent_finish::AgentFinishTool::NAME.to_string()),
                arguments_fragment: Some(
                    "{\"status\":\"success\",\"result\":\"done\"}".to_string(),
                ),
            })];
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::with_script(script, Duration::from_millis(0))),
                EngineConfig {
                    provider_connection: "mock".to_string(),
                    model: "test-model".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                },
            ));

            let (finish_tx, mut finish_rx) = mpsc::channel(1);
            let (subagent_ui_tx, _subagent_ui_rx) = mpsc::channel(8);
            let run_id = Uuid::new_v4();
            let call_id = Uuid::new_v4();

            let registry = Arc::new(default_registry(
                test_sandbox(),
                ToolRegistryConfig {
                    allowed_tools: vec![],
                    agent_finish: Some(AgentFinishToolConfig {
                        finish_tx: finish_tx.clone(),
                        run_id,
                        call_id,
                    }),
                    ..Default::default()
                },
            ));

            let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine, registry);
            agent_loop = agent_loop.with_child_role(ChildRoleConfig {
                run_id,
                call_id,
                briefing: Some("do the task".to_string()),
                parent_cancel: CancellationToken::new(),
                subagent_ui_tx,
                finish_tx,
            });

            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt("child task".into()))
                .await;

            let context = tokio::time::timeout(Duration::from_secs(2), handle)
                .await
                .expect("child run() should return promptly after finish")
                .expect("run() task should not panic")
                .expect("run() should not return an error");

            assert!(
                context.messages().iter().any(|message| matches!(
                    message,
                    Message::ToolResult {
                        value: ToolResult { name, .. },
                    } if name == tools::agent_finish::AgentFinishTool::NAME
                )),
                "context should carry the finish tool's result"
            );
            assert!(
            context.messages().iter().any(|message| matches!(
                message,
                Message::System { content } if content.contains("tool-use") && content.contains(&MAX_TOOL_USE_ITERATIONS.to_string())
            )),
            "a child must receive a heads-up system message stating its \
             tool-use iteration budget, so it can wrap up before hitting it"
        );

            let finish_result = tokio::time::timeout(Duration::from_secs(1), finish_rx.recv())
                .await
                .expect("finish_rx should receive promptly")
                .expect("finish_tx should not be dropped without sending");
            assert_eq!(finish_result.run_id, run_id);
            assert_eq!(finish_result.call_id, call_id);
            assert_eq!(finish_result.status, "success");
            assert_eq!(finish_result.result.as_deref(), Some("done"));

            // Drain the ui updates emitted along the way; we only assert that a
            // ToolDone for "agent_finish" was sent among them.
            let mut saw_finish_tool_done = false;
            while let Ok(Some(update)) =
                tokio::time::timeout(Duration::from_millis(50), ui_rx.recv()).await
            {
                if matches!(update, UiUpdate::ToolDone { ref name, .. } if name == tools::agent_finish::AgentFinishTool::NAME)
                {
                    saw_finish_tool_done = true;
                }
            }
            assert!(
                saw_finish_tool_done,
                "expected a ToolDone update for finish"
            );
        });
    }

    #[test]
    fn root_role_does_not_receive_the_child_iteration_budget_heads_up() {
        crate::test_support::run_locked(async {
            // The heads-up message is only meaningful for a `Role::Child` loop
            // (only it can be silently cut off with no one to explain things to);
            // a root loop's `Interrupted` assistant bubble already tells a human
            // what happened, so it must not get this extra system message too.
            let registry = Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            ));
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::new(
                    vec!["answer".to_string()],
                    Duration::from_millis(0),
                )),
                EngineConfig {
                    provider_connection: "mock".to_string(),
                    model: "test-model".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                },
            ));

            let (mut agent_loop, _ui_rx) = AgentLoop::new(engine, registry);
            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt("hi".into()))
                .await;
            drop(agent_loop);

            let context = tokio::time::timeout(Duration::from_secs(2), handle)
                .await
                .expect("root run() should return promptly after being dropped")
                .expect("run() task should not panic")
                .expect("run() should not return an error");

            assert!(
            !context
                .messages()
                .iter()
                .any(|message| matches!(message, Message::System { content } if content.contains("tool-use"))),
            "a root loop must not receive the child-only iteration-budget heads-up"
        );
        });
    }

    #[test]
    fn child_role_iteration_cap_exceeded_returns_err_not_unstructured_ok() {
        crate::test_support::run_locked(async {
            // Regression test: a `Role::Child` loop whose model keeps calling
            // tools without ever calling `agent_finish` must, once the iteration
            // cap trips, report a clear `Err` (→ `drive_child_to_completion`'s
            // `Ok(Err(error))` arm, `status: "failure"`) rather than the
            // "unstructured" `Ok(context)` fallback that would otherwise mistake
            // the guard's boilerplate message for a genuine final answer.
            // The mock replays this single scripted tool call on every fresh
            // request, so the child keeps calling "read" (auto-approved inside
            // the sandbox root, so no approval round-trip is needed) forever
            // without ever calling `agent_finish`, tripping the iteration guard.
            let script = vec![MockEvent::Delta(DeltaChunk {
                tool_call_index: 0,
                tool_call_id: Some("call_1".into()),
                function_name: Some("read".to_string()),
                arguments_fragment: Some(
                    serde_json::json!({
                        "path": "Cargo.toml",
                    })
                    .to_string(),
                ),
            })];
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::with_script(script, Duration::from_millis(0))),
                EngineConfig {
                    provider_connection: "mock".to_string(),
                    model: "test-model".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                },
            ));

            let registry = Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            ));

            let (finish_tx, _finish_rx) = mpsc::channel(1);
            let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::channel(16);
            let run_id = Uuid::new_v4();
            let call_id = Uuid::new_v4();

            let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine, registry);
            agent_loop = agent_loop.with_child_role(ChildRoleConfig {
                run_id,
                call_id,
                briefing: Some("do the task".to_string()),
                parent_cancel: CancellationToken::new(),
                subagent_ui_tx,
                finish_tx,
            });

            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt("child task".into()))
                .await;

            let result = tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .expect("child run() should return promptly once the iteration cap trips")
                .expect("run() task should not panic");

            let error = result.expect_err(
                "a child hitting the iteration cap must return Err, not Ok(context) \
             (which would be mistaken for an 'unstructured' final answer)",
            );
            assert!(
                error.to_string().contains("iterations"),
                "error should explain the iteration cap was hit; got: {error}"
            );

            // No boilerplate assistant bubble should have been pushed to the
            // child's own context (that message is reserved for a human reading
            // the root's transcript directly).
            while let Ok(update) = ui_rx.try_recv() {
                if let UiUpdate::Error {
                    message,
                    run_id: got_run_id,
                } = update
                {
                    assert_eq!(got_run_id, Some(run_id));
                    assert!(message.contains("iterations"));
                }
            }
            // Drain the subagent ui channel too; just ensure nothing panics.
            while subagent_ui_rx.try_recv().is_ok() {}
        });
    }

    #[tokio::test]
    async fn resolve_spawn_subagent_pushes_spawned_status_and_starts_child() {
        use crate::agent::tools::spawn_subagent::SpawnSubagentTool;
        use crate::agent::tools::Tool;
        use crate::child_agent_manager::ChildAgentManager;
        use std::sync::Mutex;

        let call_id = Uuid::new_v4();
        let tool = SpawnSubagentTool::new(true);
        let call = tool.create_call(
            call_id,
            serde_json::json!({
                "label": "Do the thing",
                "task": "do the thing",
                "briefing": "context"
            }),
        );
        let pending = PendingTool { call };

        let started: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let started_clone = started.clone();
        let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::channel(8);
        let manager = ChildAgentManager::new(
            subagent_ui_tx,
            Box::new(move |request, _cancel, _ui_tx| {
                started_clone.lock().unwrap().push(request.task.clone());
                crate::child_agent_manager::SpawnOutcome {
                    command_sender: None,
                    handle: tokio::spawn(async {}),
                }
            }),
        );
        let mut child_agent_manager = Some(manager);

        let (ui_tx, mut ui_rx) = mpsc::channel(8);
        let mut context = Context::new();
        let mut spawn_content_sections: HashMap<Uuid, Vec<crate::agent::tools::Section>> =
            HashMap::new();

        resolve_spawn_subagent(
            pending,
            &mut child_agent_manager,
            &mut context,
            &ui_tx,
            &mut spawn_content_sections,
        )
        .await;

        assert_eq!(
            started.lock().unwrap().as_slice(),
            &["do the thing".to_string()],
            "the stub spawn_fn should have been invoked with the task"
        );
        assert!(
            child_agent_manager.as_ref().unwrap().is_running(),
            "manager should report a running child"
        );

        let tool_result = context
            .messages()
            .iter()
            .find_map(|message| match message {
                Message::ToolResult {
                    value:
                        ToolResult {
                            id, name, result, ..
                        },
                } if *id == call_id && name == "spawn_subagent" => Some(result.clone()),
                _ => None,
            })
            .expect("spawn_subagent tool result should be pushed into context");
        let parsed: serde_json::Value = serde_json::from_str(&tool_result).unwrap();
        assert_eq!(parsed["event"], "subagent_accepted");
        assert_eq!(parsed["tool_call_id"], call_id.to_string());
        assert_eq!(parsed["label"], "Do the thing");
        assert_eq!(parsed["status"], "running");
        assert_eq!(parsed["outstanding_count"], 1);
        assert_eq!(parsed["all_done"], false);

        // Only `ToolStart` comes back on the caller's own `ui_tx` — resolving
        // the spawn is not the call finishing, so no `ToolDone` is sent here.
        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::ToolStart { name, .. }) if name == "spawn_subagent"
        ));

        // `ChildAgentManager::start` first reports `ChildAgentStarted` (the
        // child-lifecycle boundary for the drill-in view), then the
        // "running" status as a `ToolProgress` on the manager's own
        // `subagent_ui_tx` (forwarded to the parent's `ui_tx` in production
        // by `AgentLoop::run`'s `recv_subagent_ui` arm — bypassed here since
        // this test calls `resolve_spawn_subagent` directly).
        assert!(matches!(
            subagent_ui_rx.recv().await,
            Some(UiUpdate::ChildAgentStarted { agent_type, task, .. })
                if agent_type == "default" && task == "do the thing"
        ));
        match subagent_ui_rx.recv().await {
            Some(UiUpdate::ToolProgress {
                id,
                title_arguments,
            }) => {
                assert_eq!(id, call_id);
                let status = title_arguments
                    .iter()
                    .find(|(key, _)| key == "status")
                    .map(|(_, value)| value.clone());
                assert_eq!(status, Some(serde_json::json!("running")));
            }
            other => panic!("expected a ToolProgress update, got: {other:?}"),
        }

        // The task/briefing content sections shown while pending approval
        // must be stashed so the eventual real `ToolDone` (sent once the
        // child actually finishes) can still show them alongside the result.
        let stashed = spawn_content_sections
            .get(&call_id)
            .expect("task/briefing sections should be stashed for this call id");
        let labels: Vec<Option<String>> = stashed
            .iter()
            .map(|section| match section {
                crate::agent::tools::Section::Content(content) => content.label.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(
            labels,
            vec![Some("Task".to_string()), Some("Briefing".to_string())]
        );
    }
}
