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

use super::{AssembledToolCall, ToolAttachment};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
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

/// A clonable, single-use reply for the runtime's session replacement barrier.
/// Equality compares the reply identity, not whether it has already been sent.
#[derive(Debug, Clone)]
pub struct SessionSwitchReply(Arc<Mutex<Option<tokio::sync::oneshot::Sender<bool>>>>);

impl From<tokio::sync::oneshot::Sender<bool>> for SessionSwitchReply {
    fn from(sender: tokio::sync::oneshot::Sender<bool>) -> Self {
        Self(Arc::new(Mutex::new(Some(sender))))
    }
}

impl PartialEq for SessionSwitchReply {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SessionSwitchReply {}

impl SessionSwitchReply {
    pub(super) fn send(self, accepted: bool) {
        if let Some(sender) = self.0.lock().expect("session switch reply poisoned").take() {
            let _ = sender.send(accepted);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    Prompt(String),
    /// Summarize a completed prefix while retaining the latest explicit task.
    Compact,
    /// Freeze mutations if the receiving root runtime is authoritatively idle.
    /// Repeating this command rechecks quiescence while keeping the freeze.
    PrepareSessionSwitch {
        reply: SessionSwitchReply,
    },
    /// Release a previously accepted replacement barrier.
    CancelSessionSwitch,
    /// Exit only after an accepted replacement barrier remains quiescent.
    Shutdown,
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
    /// Routes `command` to the subagent identified by `run_id` instead of
    /// the receiving loop's own state. Only a `Role::Root` loop's dispatch
    /// handles this variant: it looks up `run_id` in its
    /// `SubagentManager` and forwards `*command` verbatim into that
    /// subagent's own command channel — the subagent's own loop logic never sees
    /// `ForSubagent` itself, only the plain, unwrapped `command`. If `run_id`
    /// is stale (the subagent already finished/was cancelled), the command is
    /// dropped and a warning is logged; this can't cause a hang, since a
    /// subagent that's already gone isn't waiting on anything.
    ForSubagent {
        run_id: Uuid,
        command: Box<AgentCommand>,
    },
    /// `/clear`: drop the conversation and continue in a
    /// brand-new persisted session.
    ///
    /// Handled here rather than in the TUI Driver because the context ownership
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
    /// Also recheck the live engines' compaction capacity asynchronously,
    /// without waiting for the next conversation request.
    RecordModelChanged {
        provider_connection: String,
        model: String,
    },
}

pub(super) fn agent_command_name(command: &AgentCommand) -> &'static str {
    match command {
        AgentCommand::Prompt(_) => "prompt",
        AgentCommand::Compact => "compact",
        AgentCommand::PrepareSessionSwitch { .. } => "prepare_session_switch",
        AgentCommand::CancelSessionSwitch => "cancel_session_switch",
        AgentCommand::Shutdown => "shutdown",
        AgentCommand::Stop { .. } => "stop",
        AgentCommand::ApproveTool { .. } => "approve_tool",
        AgentCommand::DenyTool { .. } => "deny_tool",
        AgentCommand::AnswerClarify(_) => "answer_clarify",
        AgentCommand::AbortClarify => "abort_clarify",
        AgentCommand::ForSubagent { .. } => "for_subagent",
        AgentCommand::ClearContext => "clear_context",
        AgentCommand::RecordModelChanged { .. } => "record_model_changed",
    }
}

pub(super) fn log_agent_command(
    command: &AgentCommand,
    mode: AgentMode,
    has_stream: bool,
    has_tool: bool,
) {
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
        AgentCommand::ForSubagent { run_id, command } => {
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
        AgentCommand::AbortClarify
        | AgentCommand::ClearContext
        | AgentCommand::Compact
        | AgentCommand::PrepareSessionSwitch { .. }
        | AgentCommand::CancelSessionSwitch
        | AgentCommand::Shutdown => {
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

#[derive(Debug, Clone)]
pub enum AgentEvent {
    StreamDone {
        partial: String,
    },
    StreamCancelled {
        partial: String,
    },
    StreamError(String),
    ContextWindowRejected(String),
    ContextWindowChanged,
    PreflightNotice {
        text: String,
        acknowledgement: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    },
    ToolCallLimitExceeded {
        limit: usize,
    },
    ToolCall {
        calls: Vec<AssembledToolCall>,
    },
    ToolDone {
        id: Uuid,
        name: String,
        result: String,
        requires_complete_delivery: bool,
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
        output: Option<String>,
        is_output_truncated: bool,
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
    /// Background summary request activity, independent of the agent mode.
    CompactionActivity {
        run_id: Option<Uuid>,
        request_id: Uuid,
        active: bool,
    },
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
        /// `Some(run_id)` identifies which subagent raised it (see
        /// `AgentLoop::own_run_id`). See `UiUpdate::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A one-shot engine-generated notice (for example a profile reasoning
    /// effort the pre-chat catalog validation dropped), surfaced as a
    /// transcript notice via `AppState` rather than as an error line.
    Notice {
        text: String,
        /// See `UiUpdate::Error::run_id`.
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
        /// identifies which subagent raised it (see `AgentLoop::own_run_id`).
        /// Drivers use this to route the resulting `ApproveTool`/`DenyTool`
        /// command back to the correct agent loop via `AgentCommand::ForSubagent`.
        run_id: Option<Uuid>,
    },
    /// A still-running call reports an interim status update without
    /// transitioning to a terminal state — the bubble stays `Running`
    /// (spinner keeps going). Used by `spawn_subagent`: the call is not
    /// "done" merely because the subagent was accepted/queued, only once the
    /// subagent's `agent_finish` result (or cancellation) actually arrives does a
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
        /// identifies which subagent raised it (see `AgentLoop::own_run_id`).
        /// Today no driver ever routes a `Prompt` to a subagent (only
        /// `ApproveTool`/`DenyTool`/`AnswerClarify`/`AbortClarify` cross
        /// `AgentCommand::ForSubagent`), so `Some` is unreachable in practice —
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
    /// A subagent actually started running (dequeued from
    /// `SubagentManager`'s FIFO, or started immediately). Emitted by
    /// `SubagentManager::start`. Drives the drill-in conversation model
    /// (Phase 6.10): the driver creates/updates the spawn-bubble and tracks
    /// `run_id` as the current subagent conversation, but does **not** switch
    /// `active_view` — the user stays in the parent view until they
    /// navigate manually or an interactive pause (`ToolPending`/
    /// `ClarifyPending` carrying this same `run_id`) forces the switch.
    SubagentStarted {
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
    /// The subagent engine's provider-connection/model identity, emitted immediately after
    /// the subagent has been constructed so the TUI can label its conversation
    /// independently from the root engine.
    SubagentEngineConfigured {
        run_id: Uuid,
        engine: crate::llm::engine::EngineDisplayInfo,
    },
    /// A fragment of `run_id`'s own assistant text/thinking output,
    /// forwarded from that subagent's `AppendText`/`AppendThinking` and
    /// rendered into that subagent's own conversation transcript only — never
    /// mixed into the parent's own `partial_assistant_text`, nor into a
    /// different subagent's transcript (`run_id` disambiguates which one,
    /// since a queued subagent can start streaming while an earlier subagent's
    /// transcript is still being viewed).
    SubagentToken {
        run_id: Uuid,
        text: String,
        /// `true` when this fragment came from `AppendThinking` (extended
        /// reasoning), `false` for `AppendText` (visible reply text) — lets
        /// the subagent's own `TurnBuffer` route it to the right accumulator,
        /// mirroring how the root distinguishes `StreamChunk` from
        /// `StreamThinkingChunk`.
        is_thinking: bool,
    },
    /// The subagent run identified by `run_id` has finished
    /// (successfully, with an error, or via the "unstructured" fallback).
    /// `status`/`result` mirror `crate::subagent_manager::FinishResult`.
    /// If the driver had auto-switched into `run_id`'s subagent view for an
    /// interactive pause, it auto-returns to the parent view now.
    SubagentDone {
        run_id: Uuid,
        status: String,
        result: Option<String>,
    },
    /// An estimated size against the active context window for the root run.
    /// `source` distinguishes a serialized request estimate from the model-view
    /// estimate reported after compaction changes that view.
    ContextUsage {
        estimated_tokens: usize,
        context_length: usize,
        source: ContextUsageSource,
    },
    /// An estimate against the active context window for one subagent run. The
    /// parent loop forwards this update unchanged; the TUI keeps it keyed by
    /// `run_id` so background subagents cannot replace the root footer value.
    SubagentContextUsage {
        run_id: Uuid,
        estimated_tokens: usize,
        context_length: usize,
        source: ContextUsageSource,
    },
    /// A compaction snapshot was applied to the model view this run: the
    /// transcript keeps every message, and the TUI marks where the verbatim
    /// tail begins. `entry_index` counts the chat entries built from the
    /// messages before the cut; `summarized_messages` is how many transcript
    /// messages the snapshot summarizes (rebuilt system prompts excluded).
    /// `run_id` scopes the marker to the owning root or subagent transcript.
    CompactionApplied {
        run_id: Option<Uuid>,
        entry_index: usize,
        summarized_messages: usize,
    },
}

/// Identifies which request/model-view representation a token estimate covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextUsageSource {
    /// Estimate of the exact serialized provider request.
    SerializedRequest,
    /// Estimate of the model view plus active tool definitions, used by
    /// compaction budgeting.
    ModelView,
}

/// A raw tool-call fragment emitted by a provider, to be assembled by the
/// agent's inner streaming task, never by the provider.
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

#[cfg(test)]
mod tests;
