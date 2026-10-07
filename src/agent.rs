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
pub mod tools;

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
        /// `Some(run_id)` identifies which child raised it (see
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
    /// An estimated size against the active context window for the root run.
    /// `source` distinguishes a serialized request estimate from the model-view
    /// estimate reported after compaction changes that view.
    ContextUsage {
        estimated_tokens: usize,
        context_length: usize,
        source: ContextUsageSource,
    },
    /// An estimate against the active context window for one child run. The
    /// parent loop forwards this update unchanged; the TUI keeps it keyed by
    /// `run_id` so background children cannot replace the root footer value.
    ChildContextUsage {
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
    /// `run_id` scopes the marker to the owning root or child transcript.
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

/// Tell the UI where a freshly applied compaction sits in the transcript it
/// replays, so the marker lands at the same entry the next resume would put
/// it at.
async fn send_compaction_applied(
    ui_tx: &mpsc::Sender<UiUpdate>,
    context: &Context,
    boundary: usize,
    run_id: Option<Uuid>,
) {
    let entry_index =
        crate::tui_reducer_state::project_messages(&context.messages()[..boundary]).len();
    let summarized_messages = boundary.saturating_sub(context.leading_system_len());
    let _ = ui_tx
        .send(UiUpdate::CompactionApplied {
            run_id,
            entry_index,
            summarized_messages,
        })
        .await;
}

/// Run one compaction sweep at a safe boundary and report the outcome: the
/// compaction lifecycle notices and a transcript marker when that sweep
/// applied a snapshot the transcript has not shown yet.
///
/// The replayed marker comes from the persisted snapshot instead (the TUI
/// reads it before the loop ever runs), so only a compaction applied *here*
/// emits this update — `boundary_before`/`boundary_after` make the sweep's
/// decision visible to the caller.
async fn sweep_compaction_and_report(
    compaction: &mut compaction::CompactionRuntime,
    context: &mut Context,
    engine: &dyn Engine,
    tools: &[ToolSchema],
    generation: u64,
    run_id: Option<Uuid>,
    ui_tx: &mpsc::Sender<UiUpdate>,
) -> bool {
    let boundary_before = context.compaction().map(|snapshot| snapshot.boundary);
    let result = compaction.sweep(context, engine, tools, generation);
    if result.summary_applied {
        let _ = ui_tx
            .send(UiUpdate::Notice {
                text: "Background context compaction completed. Older context was summarized; \
                       the full transcript is preserved. If compaction becomes frequent, \
                       consider narrowing the task or starting a new conversation with a \
                       concise handoff, as repeated compaction may lose detail."
                    .to_string(),
                run_id,
            })
            .await;
    }
    if result.prefix_dropped {
        let _ = ui_tx
            .send(UiUpdate::Notice {
                text: "Compacted context synchronously to fit the context window. \
                       Older context was removed from the model view; some details may no \
                       longer be available to the model. The full transcript is preserved."
                    .to_string(),
                run_id,
            })
            .await;
    }
    if result.summary_started {
        let _ = ui_tx
            .send(UiUpdate::Notice {
                text: "Started background context compaction.".to_string(),
                run_id,
            })
            .await;
    }
    if result.prefix_dropped || result.summary_applied {
        context.persist_model_projection();
        send_context_usage_estimate(
            ui_tx,
            context.estimated_model_tokens(tools, engine),
            engine.effective_context_length(),
            run_id,
            ContextUsageSource::ModelView,
        )
        .await;
    }
    if !compaction.enabled() {
        return result.prefix_dropped;
    }
    if let Some(boundary_after) = context.compaction().map(|snapshot| snapshot.boundary) {
        if boundary_before != Some(boundary_after) {
            send_compaction_applied(ui_tx, context, boundary_after, run_id).await;
        }
    }
    result.prefix_dropped || result.summary_applied
}

async fn send_context_usage_estimate(
    ui_tx: &mpsc::Sender<UiUpdate>,
    estimated_tokens: usize,
    context_length: usize,
    run_id: Option<Uuid>,
    source: ContextUsageSource,
) {
    tracing::debug!(
        run_id = ?run_id,
        estimated_tokens,
        context_length,
        usage_ratio = if context_length == 0 {
            0.0
        } else {
            estimated_tokens as f64 / context_length as f64
        },
        source = ?source,
        "context usage estimate reported"
    );
    let update = match run_id {
        Some(run_id) => UiUpdate::ChildContextUsage {
            run_id,
            estimated_tokens,
            context_length,
            source,
        },
        None => UiUpdate::ContextUsage {
            estimated_tokens,
            context_length,
            source,
        },
    };
    let _ = ui_tx.send(update).await;
}

#[allow(clippy::too_many_arguments)]
fn resume_child_after_compaction(
    context: &mut Context,
    compaction: &mut compaction::CompactionRuntime,
    mode: &mut AgentMode,
    current_cancel: &mut Option<CancellationToken>,
    engine: &Arc<dyn Engine>,
    registry: &Arc<ToolRegistry>,
    ui_tx: &mpsc::Sender<UiUpdate>,
    agent_tx: &mpsc::Sender<(u64, AgentEvent)>,
    generation: u64,
    own_run_id: Option<Uuid>,
    session_id: Option<Uuid>,
) {
    *mode = AgentMode::Streaming;
    *current_cancel = Some(start_request(
        context,
        Some(compaction),
        engine.clone(),
        registry.schemas(),
        ui_tx.clone(),
        agent_tx.clone(),
        generation,
        own_run_id,
        session_id,
    ));
}

async fn report_compaction_failure(
    ui_tx: &mpsc::Sender<UiUpdate>,
    run_id: Option<Uuid>,
    reason: &str,
) {
    let _ = ui_tx
        .send(UiUpdate::Notice {
            text: format!("Background context compaction failed: {reason}."),
            run_id,
        })
        .await;
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
/// must parse and the per-response tool-call limit. The described JSON matches the schema derived from
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
    format!(
        "{lead}\n{shape}\n\nA single assistant response may contain at most {MAX_TOOL_CALLS_PER_RESPONSE} distinct tool calls. Keep each batch within this limit; if more work is needed, continue in a later response.\n\nRequest small batches: only the first {MAX_EXECUTED_TOOL_CALLS_PER_BATCH} tool calls of a response are executed. Remaining calls, and calls whose results would not fit the remaining context budget, are not executed and return an error; resubmit only those skipped calls in a smaller batch."
    )
});

/// Priority tier of a message in the model-facing view (blueprint §4C).
///
/// Tiers describe how the *model view* treats a message when the context
/// window fills. The transcript itself is tier-agnostic: it keeps every
/// message regardless of what the model view later drops or summarizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageTier {
    /// Tier 0: the core identity prompt — never dropped or summarized.
    System,
    /// Tier 1: stable instructions (tool protocol, persona, briefing) —
    /// never dropped initially.
    InitialContext,
    /// Tier 2: ordinary conversation turns — dropped last.
    Recent,
    /// Tier 3: previously summarized buckets — dropped before `Recent`.
    Compacted,
}

/// Bookkeeping cached for one transcript message when it enters the context:
/// its tier in the model-facing view and its approximate token count.
///
/// Held index-parallel to [`Context::messages`] (the blueprint's
/// `TieredMessage` bookkeeping) so the transcript itself stays a plain
/// `Vec<Message>` for replay, persistence and cloning, and so no message text
/// is stored twice.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextEntry {
    tier: MessageTier,
    token_count: usize,
}

/// The tier a message occupies at `index`: the leading identity prompt is
/// the `System` tier, every other system message (tool protocol, persona,
/// briefing, child budget) is stable `InitialContext` instructions, and
/// everything else is conversation `Recent`. [`MessageTier::Compacted`] is
/// only ever assigned to the synthetic summary entry compaction projects
/// into the model view, never to a transcript message.
fn tier_for(index: usize, message: &Message) -> MessageTier {
    match message {
        Message::System { .. } if index == 0 => MessageTier::System,
        Message::System { .. } => MessageTier::InitialContext,
        _ => MessageTier::Recent,
    }
}

/// Approximate token count of one message's model-visible text.
///
/// Counts the payload text only (content, tool-call arguments, tool result
/// and error text); role framing and message separators are ignored, which
/// is well inside the approximation's tolerance (blueprint §4C: counts are
/// advisory, the `context_length × 0.9` margin absorbs the error).
pub(crate) fn estimate_message_tokens(
    estimator: crate::llm::TokenEstimator,
    message: &Message,
) -> usize {
    let payload = match message {
        Message::System { content } => estimator.estimate_tokens(content),
        Message::User { content, .. } => estimator.estimate_tokens(content),
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let mut tokens = estimator.estimate_tokens(content);
            for tool_call in tool_calls {
                tokens += estimator.estimate_tokens(&tool_call.name);
                tokens += estimator.estimate_tokens(&tool_call.id.to_string()) + 8;
                tokens += estimator.estimate_tokens(&tool_call.arguments.to_string());
            }
            tokens
        }
        Message::ToolResult { value } => {
            estimator.estimate_tokens(&value.name)
                + estimator.estimate_tokens(&value.id.to_string())
                + estimator.estimate_tokens(
                    &ToolEnvelope {
                        ok: true,
                        result: Some(value.result.clone()),
                        error: None,
                        result_metadata: value.result_metadata.clone(),
                        result_metadata_schema: value.result_metadata_schema.clone(),
                    }
                    .to_string_checked(),
                )
        }
        Message::ToolError {
            id, name, error, ..
        } => {
            estimator.estimate_tokens(name)
                + estimator.estimate_tokens(&id.to_string())
                + estimator.estimate_tokens(
                    &ToolEnvelope {
                        ok: false,
                        result: None,
                        error: Some(error.clone()),
                        result_metadata: None,
                        result_metadata_schema: None,
                    }
                    .to_string_checked(),
                )
        }
    };
    payload + 8
}

#[cfg(test)]
fn tool_schema_token_count(tools: &[ToolSchema]) -> usize {
    let estimator = crate::llm::TokenEstimator::Tiktoken;
    tools
        .iter()
        .map(|tool| {
            estimator.estimate_tokens(&tool.name)
                + estimator.estimate_tokens(&tool.description)
                + estimator.estimate_tokens(&tool.parameters.to_string())
        })
        .sum()
}

#[derive(Debug, Default)]
struct ToolSchemaTokenCache {
    inputs: Vec<String>,
    token_count: usize,
}

impl ToolSchemaTokenCache {
    fn count(&mut self, tools: &[ToolSchema]) -> usize {
        let inputs: Vec<String> = tools
            .iter()
            .flat_map(|tool| {
                [
                    tool.name.clone(),
                    tool.description.clone(),
                    tool.parameters.to_string(),
                ]
            })
            .collect();
        if self.inputs != inputs {
            self.token_count = inputs
                .iter()
                .map(|text| crate::llm::TokenEstimator::Tiktoken.estimate_tokens(text))
                .sum();
            self.inputs = inputs;
        }
        self.token_count
    }
}

/// The conversation as the agent loop records it: every committed message, in
/// order, exactly as replayed by the TUI and persisted to the session file.
///
/// The model-facing view is built separately (see [`Context::model_messages`])
/// so compaction can replace an old prefix without touching this record.
#[derive(Debug, Default)]
pub struct Context {
    messages: Vec<Message>,
    /// Bookkeeping for `messages`, index-aligned and maintained exclusively
    /// by [`Context::push_entry`] (i.e. by `commit` and `hydrate`): tier plus
    /// the token count cached when the message entered the context.
    entries: Vec<ContextEntry>,
    /// Token estimator used to (re)build `entries`' counts. Chosen from the
    /// active engine's provider connection and refreshed when the model
    /// switches.
    estimator: crate::llm::TokenEstimator,
    tool_schema_token_cache: Mutex<ToolSchemaTokenCache>,
    /// The latest applied compaction: `messages[..boundary]` is represented
    /// in the model view by its marked summary. The transcript keeps every
    /// summarized message untouched (blueprint §4C *record vs view*).
    compaction: Option<crate::agent::compaction::CompactionSnapshot>,
    /// First transcript index included in the model view after the leading
    /// system block. `0` means "nothing dropped"; [`Context::drop_model_view_to`]
    /// and [`Context::set_compaction`] advance it, never rewind it. Like
    /// `compaction`, this shapes the model view only — the transcript is
    /// never rewritten.
    view_start: usize,
    /// Handoff and task anchor standing in for content a lossy recovery
    /// removed from the model view; superseded by a summary that covers its
    /// boundary.
    recovery: Option<crate::agent::compaction::recovery::RecoveryProjection>,
    model_overrides: HashMap<usize, Message>,
    summary_override: Option<String>,
    retry_after_context_rejection: bool,
    projection_revision: u64,
    projection_dirty: bool,
    compaction_request_id: Option<Uuid>,
    compaction_trigger: &'static str,
    batch_admitted_calls: usize,
    batch_skipped_calls: usize,
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
        let mut context = Context {
            store: None,
            entries: Vec::new(),
            estimator: crate::llm::TokenEstimator::default(),
            tool_schema_token_cache: Mutex::default(),
            compaction: None,
            view_start: 0,
            recovery: None,
            model_overrides: HashMap::new(),
            summary_override: None,
            retry_after_context_rejection: false,
            projection_revision: 0,
            projection_dirty: false,
            compaction_request_id: None,
            compaction_trigger: "request_boundary",
            batch_admitted_calls: 0,
            batch_skipped_calls: 0,
            messages: Vec::new(),
        };
        context.push_entry(Message::System {
            content: system_prompt.into(),
        });
        context.push_entry(Message::System {
            content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
        });
        context
    }

    /// Append a message and its bookkeeping together, without forwarding to
    /// the session store — hydration replays messages the store already
    /// holds, while [`Context::commit`] persists first and then lands here.
    fn push_entry(&mut self, message: Message) {
        let index = self.messages.len();
        let entry = ContextEntry {
            tier: tier_for(index, &message),
            token_count: estimate_message_tokens(self.estimator, &message),
        };
        self.messages.push(message);
        self.entries.push(entry);
        debug_assert_eq!(
            self.messages.len(),
            self.entries.len(),
            "transcript and bookkeeping stay index-aligned"
        );
    }

    /// Attach a session store, so every message committed from here on is
    /// also appended to the session file.
    ///
    /// The two system prompts installed by [`Context::new`] or
    /// [`Context::with_system_prompt`] are deliberately not persisted: they
    /// are rebuilt from the running binary on every resume, so storing them
    /// would pin a stale copy into the transcript.
    pub fn set_store(&mut self, store: Store) {
        match Store::read_session_state(store.path()) {
            Ok(state) => {
                self.hydrate(state.messages);
                // Rebuild the model view from the latest snapshot plus the
                // verbatim tail; the transcript above already carries every
                // original message, and re-applying is a read — it never
                // writes the event back.
                if let Some(mut snapshot) = state.compaction {
                    if let Some(boundary) = state.compaction_transcript_boundary {
                        snapshot.boundary = boundary + self.leading_system_len();
                    }
                    self.set_compaction(snapshot);
                }
                if let Some(projection) = state.model_projection {
                    self.restore_model_projection(projection);
                }
            }
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
        for message in messages {
            self.push_entry(message);
        }

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
                self.push_entry(Message::ToolError {
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
        self.push_entry(message);
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

    /// Number of leading contiguous system messages: the rebuilt identity
    /// prompt and tool-protocol prompt (plus any instructions injected ahead
    /// of the first user turn). Compaction neither summarizes nor drops them
    /// (blueprint §4C *system tiers*).
    pub fn leading_system_len(&self) -> usize {
        self.messages
            .iter()
            .take_while(|message| matches!(message, Message::System { .. }))
            .count()
    }

    /// First transcript index included in the model view after the leading
    /// system block.
    fn model_view_start(&self) -> usize {
        self.view_start.max(self.leading_system_len())
    }

    /// Tokens of the model view that are *not* part of the verbatim tail:
    /// the leading system block plus, when compacted, the projected summary.
    fn model_view_head_tokens(&self) -> usize {
        self.model_view_base_tokens()
            + self
                .recovery
                .as_ref()
                .map_or(0, |recovery| recovery.token_count)
            + self.projected_task_anchor().map_or(0, |message| {
                estimate_message_tokens(self.estimator, message)
            })
    }

    fn projected_task_anchor(&self) -> Option<&Message> {
        if self
            .recovery
            .as_ref()
            .is_some_and(|recovery| recovery.anchor.is_some())
        {
            return None;
        }
        self.messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| {
                matches!(
                    message,
                    Message::User {
                        kind: UserMessageKind::Explicit,
                        ..
                    }
                )
            })
            .filter(|(index, _)| *index < self.model_view_start())
            .map(|(index, message)| self.model_overrides.get(&index).unwrap_or(message))
    }

    /// Leading system block plus the projected summary, without any
    /// recovery projection.
    fn model_view_base_tokens(&self) -> usize {
        let leading_system_len = self.leading_system_len();
        let mut tokens: usize = self.entries[..leading_system_len]
            .iter()
            .map(|entry| entry.token_count)
            .sum();
        if let Some(snapshot) = &self.compaction {
            tokens += estimate_message_tokens(self.estimator, &self.projected_summary(snapshot));
        }
        tokens
    }

    /// The model-facing view of the conversation for the next request.
    ///
    /// The transcript stays intact no matter what the model sees: the
    /// leading system instructions, then the marked summary of the compacted
    /// prefix (when one is applied), then the verbatim tail after the
    /// boundary (blueprint §4C). Without a compaction snapshot — and before
    /// the hard fallback drops anything — this is simply the whole transcript
    /// in order.
    pub fn model_messages(&self) -> Vec<Message> {
        self.model_messages_from(self.model_view_start())
    }

    fn model_messages_from(&self, view_start: usize) -> Vec<Message> {
        let leading_system_len = self.leading_system_len();
        let mut messages: Vec<Message> = self.messages[..leading_system_len].to_vec();
        if let Some(snapshot) = &self.compaction {
            messages.push(self.projected_summary(snapshot));
        }
        if let Some(recovery) = &self.recovery {
            messages.push(recovery.handoff.clone());
            messages.extend(recovery.anchor.clone());
        }
        messages.extend(self.projected_task_anchor().cloned());
        messages.extend(
            (view_start.max(leading_system_len)..self.messages.len()).map(|index| {
                self.model_overrides
                    .get(&index)
                    .unwrap_or(&self.messages[index])
                    .clone()
            }),
        );
        messages
    }

    fn projected_summary(&self, snapshot: &compaction::CompactionSnapshot) -> Message {
        self.summary_override.as_ref().map_or_else(
            || snapshot.summary_message(),
            |content| Message::User {
                content: content.clone(),
                kind: UserMessageKind::ImplicitToolNote,
            },
        )
    }

    fn limit_last_model_message(&mut self, allowance: usize) {
        if let Some(index) = self.messages.len().checked_sub(1) {
            self.limit_model_message(index, allowance);
        }
    }

    fn limit_model_message(&mut self, index: usize, allowance: usize) -> bool {
        let message = self
            .model_overrides
            .get(&index)
            .unwrap_or(&self.messages[index]);
        if let Some(projected) =
            compaction::recovery::project_body(self.estimator, message, allowance)
        {
            self.model_overrides.insert(index, projected);
            self.projection_dirty = true;
            return true;
        }
        false
    }

    fn persist_model_projection(&mut self) {
        if !self.projection_dirty {
            return;
        }
        let Some(store) = self.store() else {
            return;
        };
        self.projection_dirty = false;
        self.projection_revision += 1;
        let leading = self.leading_system_len();
        let mut message_overrides: Vec<(usize, Message)> = self
            .model_overrides
            .iter()
            .map(|(index, message)| (index.saturating_sub(leading), message.clone()))
            .collect();
        message_overrides.sort_by_key(|(index, _)| *index);
        store.record_model_projection(compaction::ModelProjectionSnapshot {
            projection_revision: self.projection_revision,
            transcript_start: self.model_view_start().saturating_sub(leading),
            message_overrides,
            summary_override: self.summary_override.clone(),
            handoff: self
                .recovery
                .as_ref()
                .map(|recovery| recovery.handoff.clone()),
            anchor: self
                .recovery
                .as_ref()
                .and_then(|recovery| recovery.anchor.clone()),
        });
    }

    fn restore_model_projection(&mut self, projection: compaction::ModelProjectionSnapshot) {
        self.projection_revision = projection.projection_revision;
        let valid_handoff = projection.handoff.as_ref().is_none_or(|message| {
            matches!(
                message,
                Message::User {
                    kind: UserMessageKind::ImplicitToolNote,
                    ..
                }
            )
        });
        let valid_anchor = projection.anchor.as_ref().is_none_or(|message| {
            matches!(
                message,
                Message::User {
                    kind: UserMessageKind::Explicit,
                    ..
                }
            )
        });
        if !valid_handoff
            || !valid_anchor
            || (projection.anchor.is_some() && projection.handoff.is_none())
        {
            tracing::warn!("ignoring invalid persisted recovery message types");
            return;
        }
        let leading = self.leading_system_len();
        let boundary = projection.transcript_start.saturating_add(leading);
        if boundary < self.model_view_start()
            || boundary > self.messages.len()
            || !compaction::safe_boundaries_from(&self.messages, leading).contains(&boundary)
        {
            tracing::warn!(
                boundary,
                "ignoring invalid persisted model projection boundary"
            );
            return;
        }
        self.view_start = self.view_start.max(boundary);
        self.summary_override = projection.summary_override;
        for (index, message) in projection.message_overrides {
            let index = index.saturating_add(leading);
            let valid =
                self.messages
                    .get(index)
                    .is_some_and(|original| match (original, &message) {
                        (
                            Message::ToolResult { value: original },
                            Message::ToolResult { value: projected },
                        ) => original.id == projected.id && original.name == projected.name,
                        (
                            Message::ToolError {
                                id: original,
                                name: original_name,
                                ..
                            },
                            Message::ToolError {
                                id: projected,
                                name: projected_name,
                                ..
                            },
                        ) => original == projected && original_name == projected_name,
                        (
                            Message::User { kind: original, .. },
                            Message::User {
                                kind: projected, ..
                            },
                        ) => original == projected,
                        _ => false,
                    });
            if valid {
                self.model_overrides.insert(index, message);
            } else {
                tracing::warn!(index, "ignoring invalid persisted model message override");
            }
        }
        if let Some(handoff) = projection.handoff {
            let token_count = estimate_message_tokens(self.estimator, &handoff)
                + projection
                    .anchor
                    .as_ref()
                    .map_or(0, |anchor| estimate_message_tokens(self.estimator, anchor));
            self.recovery = Some(compaction::recovery::RecoveryProjection {
                boundary,
                handoff,
                anchor: projection.anchor,
                token_count,
            });
        }
        self.projection_dirty = false;
    }

    /// Tier of the message at `index` in the model-facing view.
    pub fn tier(&self, index: usize) -> Option<MessageTier> {
        self.entries.get(index).map(|entry| entry.tier)
    }

    /// The token count cached when the message at `index` entered the
    /// context, in the estimator active at that time.
    pub fn token_count(&self, index: usize) -> Option<usize> {
        self.entries.get(index).map(|entry| entry.token_count)
    }

    /// Sum of every cached token count: the approximate size of the whole
    /// transcript, in tokens. The model view can be smaller — see
    /// [`Context::model_view_token_count`].
    pub fn approximate_token_count(&self) -> usize {
        self.entries.iter().map(|entry| entry.token_count).sum()
    }

    /// Approximate model-view size including the active tool definitions, but
    /// excluding API/JSON transport framing.
    pub fn estimated_model_tokens(&self, tools: &[ToolSchema], _engine: &dyn Engine) -> usize {
        self.model_view_token_count() + self.tool_schema_token_count(tools)
    }

    fn tool_schema_token_count(&self, tools: &[ToolSchema]) -> usize {
        self.tool_schema_token_cache
            .lock()
            .expect("tool schema token cache poisoned")
            .count(tools)
    }

    /// Approximate tokens available for one additional model-facing result
    /// while reserving space for a normal reply and compaction safety margin.
    pub fn available_result_token_budget(
        &self,
        tools: &[ToolSchema],
        context_length: usize,
    ) -> usize {
        let current_tokens = self.model_view_token_count() + self.tool_schema_token_count(tools);
        crate::agent::compaction::safe_request_ceiling(context_length)
            .saturating_sub(current_tokens)
    }

    /// Approximate payload-only size of the model view. Retained for callers
    /// that need transcript message accounting.
    pub fn model_view_token_count(&self) -> usize {
        self.model_view_token_count_from(self.model_view_start())
    }

    fn model_view_token_count_from(&self, view_start: usize) -> usize {
        let tail_tokens: usize = self.entries[view_start..]
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                self.model_overrides
                    .get(&(view_start + offset))
                    .map_or(entry.token_count, |message| {
                        estimate_message_tokens(self.estimator, message)
                    })
            })
            .sum();
        self.model_view_head_tokens() + tail_tokens
    }

    /// Number of messages projected into the current model view, including
    /// the leading system block and the synthetic summary message, if any.
    pub(crate) fn model_view_message_count(&self) -> usize {
        self.leading_system_len()
            + usize::from(self.compaction.is_some())
            + usize::from(self.projected_task_anchor().is_some())
            + self
                .recovery
                .as_ref()
                .map_or(0, |recovery| 1 + usize::from(recovery.anchor.is_some()))
            + self.messages.len().saturating_sub(self.model_view_start())
    }

    /// Approximate share (`0.0`–`1.0+`) of the supplied context window the
    /// model view occupies. `context_length` is the active engine's effective
    /// window ([`crate::llm::Engine::effective_context_length`]), so a
    /// `/models` switch re-evaluates every threshold derived from this.
    pub fn estimated_usage_ratio(&self, context_length: usize) -> f64 {
        if context_length == 0 {
            return 0.0;
        }
        self.model_view_token_count() as f64 / context_length as f64
    }

    /// The latest applied compaction snapshot, if any.
    pub fn compaction(&self) -> Option<&crate::agent::compaction::CompactionSnapshot> {
        self.compaction.as_ref()
    }

    /// Install a prefix summary as the model view's compaction snapshot:
    /// `messages[..boundary]` is replaced by its marked summary, the tail
    /// after `boundary` stays verbatim, and the transcript keeps everything.
    ///
    /// Returns `false` when the snapshot does not advance past the boundary
    /// already applied — boundaries only ever move forward, so an older (or
    /// replayed) result can never resurrect an already summarized prefix.
    pub fn set_compaction(
        &mut self,
        snapshot: crate::agent::compaction::CompactionSnapshot,
    ) -> bool {
        let leading_system_len = self.leading_system_len();
        let clamped_boundary = snapshot
            .boundary
            .clamp(leading_system_len, self.messages.len());
        // Snap forward to a complete-turn cut: a persisted boundary must
        // never re-split a tool call from its one result after a resume
        // (blueprint §4C *tool protocol constraints*).
        let Some(boundary) =
            crate::agent::compaction::first_safe_boundary_from(&self.messages, clamped_boundary)
        else {
            return false;
        };
        if boundary < self.view_start {
            return false;
        }
        if let Some(current) = &self.compaction {
            if boundary <= current.boundary {
                return false;
            }
        }
        self.summary_override = None;
        self.model_overrides.retain(|index, _| *index >= boundary);
        tracing::debug!(
            boundary,
            summary_chars = snapshot.summary.chars().count(),
            "applying compaction snapshot to the model view"
        );
        self.view_start = self.view_start.max(boundary);
        if self
            .recovery
            .as_ref()
            .is_some_and(|recovery| boundary >= recovery.boundary)
        {
            self.recovery = None;
        }
        self.compaction = Some(crate::agent::compaction::CompactionSnapshot {
            boundary,
            summary: snapshot.summary,
        });
        self.projection_dirty = true;
        true
    }

    /// Oldest-first token budget the prefix a new summary must cover so the
    /// model view lands at `target_tokens`: the smallest prefix (counted from
    /// the first message in the visible tail) whose removal
    /// reaches the target, snapped forward to a cut that keeps every
    /// tool-call/result group intact.
    ///
    /// `None` when no summarization is useful: the view already fits the
    /// target, or there is no non-system prefix to summarize at all. The
    /// summary's own size is unknown when this runs, so reaching the target
    /// is best-effort — a later sweep trims the remainder.
    pub fn summary_boundary_for_target(&self, target_tokens: usize) -> Option<usize> {
        let view_start = self.model_view_start();
        if self.compaction.is_some()
            && self.entries[view_start..]
                .iter()
                .map(|entry| entry.token_count)
                .sum::<usize>()
                < 128
        {
            return None;
        }
        let tokens_to_remove = self.model_view_token_count().saturating_sub(target_tokens);
        if tokens_to_remove == 0 {
            return None;
        }
        let mut accumulated = 0usize;
        for index in view_start..self.entries.len() {
            accumulated += self.entries[index].token_count;
            if accumulated >= tokens_to_remove {
                return crate::agent::compaction::first_safe_boundary_from(
                    &self.messages,
                    index + 1,
                )
                .or_else(|| {
                    compaction::safe_boundaries_from(&self.messages, view_start)
                        .into_iter()
                        .rfind(|boundary| *boundary <= index + 1)
                })
                .filter(|boundary| *boundary > view_start);
            }
        }
        (self.entries.len() > view_start)
            .then(|| {
                crate::agent::compaction::first_safe_boundary_from(
                    &self.messages,
                    self.entries.len(),
                )
            })
            .flatten()
            .filter(|boundary| *boundary > view_start)
    }

    /// Find the oldest safe prefix whose removal brings model-visible content
    /// at or below `target_tokens`. The generated summary is not available yet,
    /// so it is excluded from this projection.
    pub(crate) fn summary_boundary_for_model_target(
        &self,
        target_tokens: usize,
        tools: &[ToolSchema],
        _engine: &dyn Engine,
    ) -> Option<usize> {
        let _ = _engine;
        let minimum_boundary = self.summary_boundary_for_target(
            target_tokens.saturating_sub(self.tool_schema_token_count(tools)),
        )?;
        compaction::safe_boundaries_from(&self.messages, self.model_view_start())
            .into_iter()
            .find(|boundary| {
                if *boundary < minimum_boundary {
                    return false;
                }
                let latest_request = self.messages.iter().rposition(|message| {
                    matches!(
                        message,
                        Message::User {
                            kind: UserMessageKind::Explicit,
                            ..
                        }
                    )
                });
                (self.model_view_start()..*boundary)
                    .filter(|index| Some(*index) != latest_request)
                    .map(|index| self.entries[index].token_count)
                    .sum::<usize>()
                    >= 128
            })
    }

    /// Synchronously drop model-view messages — oldest first — until the view
    /// fits `target_tokens` (blueprint §4C hard fallback).
    ///
    /// Only complete turns are cut: every candidate position keeps each
    /// `tool_call_id` together with its one result, the leading system
    /// instructions and any applied summary stay, and the transcript itself
    /// is untouched. Returns `true` when the target is met; `false` when even
    /// the empty tail would not fit (nothing is dropped then).
    pub fn drop_model_view_to(&mut self, target_tokens: usize) -> bool {
        let from = self.model_view_start();
        let head_tokens = self.model_view_head_tokens();
        for cut in crate::agent::compaction::safe_boundaries_from(&self.messages, from) {
            let tail_tokens: usize = self.entries[cut..]
                .iter()
                .map(|entry| entry.token_count)
                .sum();
            if head_tokens + tail_tokens <= target_tokens {
                self.view_start = self.view_start.max(cut);
                return true;
            }
        }
        false
    }

    /// Lossy, continuity-preserving recovery of the model view.
    ///
    /// Drops the oldest complete tool groups until the view (tool schemas
    /// included) fits `target_tokens`; when the target is unattainable it
    /// retains as much recent content as fits `ceiling_tokens`, and when even
    /// that is impossible it keeps only the projection. Every drop installs a
    /// [`RecoveryProjection`](crate::agent::compaction::recovery::RecoveryProjection):
    /// a handoff of what was removed plus the newest explicit user request,
    /// so a continuation never starts from system messages alone.
    pub(crate) fn recover_model_view(
        &mut self,
        target_tokens: usize,
        ceiling_tokens: usize,
        tools: &[ToolSchema],
    ) -> crate::agent::compaction::recovery::RecoveryReport {
        self.recover_projection(target_tokens, ceiling_tokens, tools)
    }

    fn recover_projection(
        &mut self,
        target_tokens: usize,
        ceiling_tokens: usize,
        tools: &[ToolSchema],
    ) -> compaction::recovery::RecoveryReport {
        use compaction::recovery::{
            build_projection, marked_text, ProjectionBudget, RecoveryReport,
            REQUEST_OMISSION_MARKER, SUMMARY_OMISSION_MARKER,
        };
        let schema_tokens = self.tool_schema_token_count(tools);
        let target = target_tokens.saturating_sub(schema_tokens);
        let ceiling = ceiling_tokens.saturating_sub(schema_tokens);
        let tokens_before = self.model_view_token_count();
        let from = self.model_view_start();
        let latest_request = self.messages.iter().rposition(|message| {
            matches!(
                message,
                Message::User {
                    kind: UserMessageKind::Explicit,
                    ..
                }
            )
        });
        let newest_batch = (from..self.messages.len()).rev().find(|index| matches!(
            &self.messages[*index], Message::Assistant { tool_calls, .. } if !tool_calls.is_empty()
        ));
        let retained_start =
            newest_batch.unwrap_or_else(|| latest_request.unwrap_or(from).max(from));
        let mut changed = false;
        let mut stage = 0;
        let mut clipped_results = 0;
        let body_allowance = (target / 16).max(MINIMUM_USEFUL_RESULT_TOKENS);
        if self.model_view_token_count() > target {
            stage = 1;
            for index in from..retained_start {
                if self.limit_model_message(index, body_allowance) {
                    changed = true;
                    clipped_results += 1;
                }
            }
        }
        if self.model_view_token_count() > target {
            stage = 2;
            let immutable_batch_tokens = newest_batch.map_or(0, |index| {
                self.messages[index..]
                    .iter()
                    .map(|message| {
                        let projected =
                            compaction::recovery::project_body(self.estimator, message, 64);
                        estimate_message_tokens(
                            self.estimator,
                            projected.as_ref().unwrap_or(message),
                        )
                    })
                    .sum::<usize>()
            });
            let keep_newest =
                immutable_batch_tokens + self.model_view_base_tokens() + 192 < ceiling;
            let cuts = compaction::safe_boundaries_from(&self.messages, from);
            for cut in cuts
                .into_iter()
                .filter(|cut| *cut > from && (!keep_newest || *cut <= retained_start))
            {
                let projection = build_projection(
                    self.estimator,
                    &self.messages,
                    from,
                    cut,
                    self.recovery.as_ref(),
                    ProjectionBudget {
                        handoff_tokens: (target / 8).max(128),
                        anchor_tokens: usize::MAX,
                    },
                );
                self.view_start = cut;
                self.recovery = Some(projection);
                self.model_overrides.retain(|index, _| *index >= cut);
                changed = true;
                if self.model_view_token_count() <= target {
                    break;
                }
            }
        }
        if self.model_view_token_count() > target {
            stage = 3;
            let result_indices: Vec<usize> = (self.model_view_start()..self.messages.len())
                .filter(|index| {
                    matches!(
                        &self.messages[*index],
                        Message::ToolResult { .. }
                            | Message::ToolError { .. }
                            | Message::User {
                                kind: UserMessageKind::ImplicitToolNote,
                                ..
                            }
                    )
                })
                .collect();
            let allowance = (target / (result_indices.len() + 4)).max(64);
            for index in result_indices {
                if self.limit_model_message(index, allowance) {
                    changed = true;
                    clipped_results += 1;
                }
            }
        }
        if self.model_view_token_count() > target {
            stage = 4;
            if let Some(snapshot) = &self.compaction {
                if let Message::User { content, .. } = self.projected_summary(snapshot) {
                    let shortened =
                        marked_text(self.estimator, &content, SUMMARY_OMISSION_MARKER, 128);
                    if shortened != content {
                        self.summary_override = Some(shortened);
                        changed = true;
                    }
                }
            }
            if let Some(recovery) = &mut self.recovery {
                if let Message::User { content, .. } = &mut recovery.handoff {
                    let shortened =
                        marked_text(self.estimator, content, SUMMARY_OMISSION_MARKER, 128);
                    changed |= shortened != *content;
                    *content = shortened;
                }
                recovery.token_count = estimate_message_tokens(self.estimator, &recovery.handoff)
                    + recovery.anchor.as_ref().map_or(0, |message| {
                        estimate_message_tokens(self.estimator, message)
                    });
            }
        }
        // Missing the target alone is not a reason to shorten the task.
        if self.model_view_token_count() > ceiling {
            stage = 5;
            let anchor_in_recovery = latest_request
                .is_some_and(|index| index < self.model_view_start())
                && self
                    .recovery
                    .as_ref()
                    .is_some_and(|recovery| recovery.anchor.is_some());
            let anchor = if anchor_in_recovery {
                self.recovery
                    .as_ref()
                    .and_then(|recovery| recovery.anchor.clone())
            } else {
                latest_request.map(|index| {
                    self.model_overrides
                        .get(&index)
                        .unwrap_or(&self.messages[index])
                        .clone()
                })
            };
            if let Some(Message::User { content, kind }) = anchor {
                let other_tokens = self
                    .model_view_token_count()
                    .saturating_sub(self.estimator.estimate_tokens(&content) + 8);
                let allowance = ceiling.saturating_sub(other_tokens + 8);
                let shortened =
                    marked_text(self.estimator, &content, REQUEST_OMISSION_MARKER, allowance);
                changed |= shortened != content;
                let message = Message::User {
                    content: shortened,
                    kind,
                };
                if let Some(recovery) = &mut self.recovery {
                    if anchor_in_recovery {
                        recovery.anchor = Some(message.clone());
                        recovery.token_count =
                            estimate_message_tokens(self.estimator, &recovery.handoff)
                                + estimate_message_tokens(self.estimator, &message);
                    } else if let Some(index) = latest_request {
                        self.model_overrides.insert(index, message);
                    }
                } else if let Some(index) = latest_request {
                    self.model_overrides.insert(index, message);
                }
            }
        }
        let estimated_tokens_after = self.model_view_token_count();
        self.projection_dirty |= changed;
        let model_messages = self.model_messages();
        let has_task = model_messages.iter().rev().find(|message| matches!(
            message, Message::User { kind: UserMessageKind::Explicit, .. }
        )).is_some_and(|message| matches!(
            message, Message::User { content, kind: UserMessageKind::Explicit }
                if !content.trim().is_empty()
                    && content.strip_suffix(REQUEST_OMISSION_MARKER).is_none_or(|prefix| !prefix.trim().is_empty())
        ));
        tracing::info!(
            trigger = self.compaction_trigger,
            request_id = ?self.compaction_request_id,
            safe_boundary = self.model_view_start(),
            stage,
            clipped_results,
            estimated_tokens_before = tokens_before + schema_tokens,
            estimated_tokens_after = estimated_tokens_after + schema_tokens,
            target_tokens,
            ceiling_tokens,
            projected_messages = self.model_view_message_count(),
            admitted_calls = self.batch_admitted_calls,
            skipped_calls = self.batch_skipped_calls,
            batch_cap = MAX_EXECUTED_TOOL_CALLS_PER_BATCH,
            remaining_result_allowance = ceiling.saturating_sub(estimated_tokens_after),
            terminal_reason = if estimated_tokens_after <= ceiling && has_task {
                "fits"
            } else {
                "irreducible"
            },
            "compaction recovery projection evaluated"
        );
        RecoveryReport {
            changed,
            estimated_tokens_after,
            fits_target: estimated_tokens_after <= target,
            fits_ceiling: estimated_tokens_after <= ceiling && has_task,
            stage,
            clipped_results,
        }
    }

    /// The estimator used to cache token counts.
    pub fn token_estimator(&self) -> crate::llm::TokenEstimator {
        self.estimator
    }

    /// Switch to another estimator (a different provider connection) and
    /// refresh every cached count in one pass. A no-op for the estimator
    /// already in use, so repeated model switches on the same connection
    /// never pay for a recount.
    pub fn set_token_estimator(&mut self, estimator: crate::llm::TokenEstimator) {
        if self.estimator == estimator {
            return;
        }
        self.estimator = estimator;
        for (entry, message) in self.entries.iter_mut().zip(self.messages.iter()) {
            entry.token_count = estimate_message_tokens(estimator, message);
        }
        if let Some(recovery) = &mut self.recovery {
            recovery.token_count = estimate_message_tokens(estimator, &recovery.handoff)
                + recovery
                    .anchor
                    .as_ref()
                    .map_or(0, |message| estimate_message_tokens(estimator, message));
        }
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

/// Default execution timeout for file-system tools (blueprint §4D).
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
        _max_output_tokens: Option<usize>,
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

fn is_context_window_rejection(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("context_length_exceeded")
        || message.contains("maximum context length")
        || message.contains("context window exceeded")
        || message.contains("exceeds the context window")
        || message.contains("prompt is too long")
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
    /// Set via [`AgentLoop::with_compaction_engine`] from the profile's
    /// `[compaction].engine`: the engine background prefix summaries run on.
    /// `None` (no compact engine configured) disables compaction for this loop.
    compaction_engine: Option<Arc<dyn Engine>>,
    /// Wall-clock budget configured by `[compaction].timeout_sec` for
    /// requests made by `compaction_engine`.
    compaction_timeout: Duration,
    /// Framing instruction for background summary requests: the resolved
    /// `COMPACTION.md` layered like `SOUL.md`, or its built-in default.
    compaction_summary_prompt: String,
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
            compaction_timeout: Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
            compaction_summary_prompt: crate::config::compaction_prompt::DEFAULT_COMPACTION_PROMPT
                .to_string(),
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

    /// Set the engine background prefix summaries run on (the profile's
    /// `[compaction].engine`). Applied to root and child loops.
    pub fn with_compaction_engine(mut self, engine: Arc<dyn Engine>) -> Self {
        self.compaction_engine = Some(engine);
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
        let max_tool_iterations = self.max_tool_iterations;
        #[cfg(test)]
        let batch_cap = self.test_batch_cap;
        #[cfg(not(test))]
        let batch_cap = MAX_EXECUTED_TOOL_CALLS_PER_BATCH;
        let compaction_engine = self.compaction_engine.take();
        let compaction_timeout = self.compaction_timeout;
        let compaction_summary_prompt = std::mem::take(&mut self.compaction_summary_prompt);
        let agent_type = self.agent_type.clone();
        let store = self.store.clone();
        let mut session_id = self.session_id;
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
            // Pick the common local estimate before hydration, so counts
            // cached while replaying use the same baseline.
            context.set_token_estimator(crate::llm::TokenEstimator::Tiktoken);
            if let Some(store) = store {
                context.set_store(store);
            }
            let (mut compaction, mut compaction_rx) = compaction::CompactionRuntime::new(
                compaction_engine.clone(),
                compaction_summary_prompt,
                compaction_timeout,
            );
            compaction.set_activity_sender(ui_tx.clone(), own_run_id);
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
                    "You have a budget of at most {max_tool_iterations} tool-use \
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
                                            &mut context,
                                            Some(&mut compaction),
                                            engine.clone(),
                                            registry.schemas(),
                                            ui_tx.clone(),
                                            agent_tx.clone(),
                                            generation,
                                            own_run_id,
                                            session_id,
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
                                            max_tool_iterations,
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
                                                &mut context,
                                                Some(&mut compaction),
                                                engine.clone(),
                                                registry.schemas(),
                                                ui_tx.clone(),
                                                agent_tx.clone(),
                                                generation,
                                                own_run_id,
                                                session_id,
                                            ));
                                        }
                                    } else {
                                        mode = AgentMode::Idle;
                                    }
                                    if mode == AgentMode::Idle && pending_tools.is_empty() {
                                        let prefix_dropped = sweep_compaction_and_report(
                                            &mut compaction,
                                            &mut context,
                                            engine.as_ref(),
                                            &registry.schemas(),
                                            generation,
                                            own_run_id,
                                            &ui_tx,
                                        )
                                        .await;
                                        if is_child && prefix_dropped {
                                            resume_child_after_compaction(
                                                &mut context,
                                                &mut compaction,
                                                &mut mode,
                                                &mut current_cancel,
                                                &engine,
                                                &registry,
                                                &ui_tx,
                                                &agent_tx,
                                                generation,
                                                own_run_id,
                                                session_id,
                                            );
                                        }
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
                                AgentEvent::PreflightNotice { text, acknowledgement } => {
                                    let _ = ui_tx.send(UiUpdate::Notice { text, run_id: own_run_id }).await;
                                    if let Some(acknowledgement) = acknowledgement.lock().expect("preflight acknowledgement lock").take() {
                                        let _ = acknowledgement.send(());
                                    }
                                }
                                AgentEvent::StreamError(error) => {
                                    pending_steers.clear();
                                    current_cancel = None;
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
                                AgentEvent::ContextWindowRejected(error) => {
                                    tracing::warn!(
                                        estimated_tokens = context.estimated_model_tokens(&registry.schemas(), engine.as_ref()),
                                        context_length = engine.effective_context_length(),
                                        reason = %error,
                                        "provider rejected context; retrying once with request-local extra margin"
                                    );
                                    context.retry_after_context_rejection = true;
                                    current_cancel = Some(start_request(
                                        &mut context, Some(&mut compaction), engine.clone(),
                                        registry.schemas(), ui_tx.clone(), agent_tx.clone(),
                                        generation, own_run_id, session_id,
                                    ));
                                }
                                AgentEvent::ToolCallLimitExceeded { limit } => {
                                    current_cancel = None;
                                    pending_steers.clear();
                                    let notice = format!(
                                        "The previous assistant response exceeded the limit of {limit} tool calls. None of those calls were executed. Continue the task using no more than {limit} tool calls in one response."
                                    );
                                    tracing::warn!(limit, "assistant response exceeded tool-call limit");
                                    context.push_async_harness_notice(notice);
                                    if !bump_and_check_iteration_cap(
                                        &mut iterations,
                                        max_tool_iterations,
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
                                            &mut context,
                                            Some(&mut compaction),
                                            engine.clone(),
                                            registry.schemas(),
                                            ui_tx.clone(),
                                            agent_tx.clone(),
                                            generation,
                                            own_run_id,
                                            session_id,
                                        ));
                                    }
                                }
                                AgentEvent::ToolCall { calls } => {
                                    let mut requests = Vec::with_capacity(calls.len());
                                    let mut unknown_tools = Vec::new();
                                    let mut skipped_calls = Vec::new();
                                    for (position, assembled) in calls.into_iter().enumerate() {
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
                                            Some(call)
                                                if position < batch_cap =>
                                            {
                                                pending_tools.push_back(PendingTool { call });
                                            }
                                            Some(call) => {
                                                skipped_calls.push((
                                                    assembled.id,
                                                    assembled.name.clone(),
                                                    call.title_arguments(),
                                                    call.pending_content_sections(),
                                                ));
                                            }
                                            None if position >= batch_cap => {
                                                skipped_calls.push((assembled.id, assembled.name.clone(), Vec::new(), Vec::new()));
                                            }
                                            None => {
                                                let error =
                                                    format!("Unknown tool '{}'", assembled.name);
                                                unknown_tools.push((assembled.id, assembled.name, error));
                                            }
                                        }
                                    }
                                    context.batch_admitted_calls = requests.len().saturating_sub(unknown_tools.len());
                                    context.batch_skipped_calls = unknown_tools.len();
                                    context.push_assistant_tool_call(
                                        String::new(),
                                        AssistantStatus::Complete,
                                        requests,
                                    );
                                    let unknown_only_round = (!unknown_tools.is_empty()
                                        || !skipped_calls.is_empty())
                                        && pending_tools.is_empty();
                                    for (id, name, title_arguments, content_sections) in skipped_calls {
                                        let reason = batch_limit_skip_reason(&name, batch_cap);
                                        resolve_skipped_tool_call(
                                            &mut context,
                                            &ui_tx,
                                            own_run_id,
                                            id,
                                            name,
                                            title_arguments,
                                            content_sections,
                                            reason,
                                        )
                                        .await;
                                    }
                                    // Registry failures are synchronous results, not
                                    // completions of the tool currently executing.
                                    for (id, name, error) in unknown_tools {
                                        tracing::error!(tool = %name, tool_call_id = %id, error = %error, "tool failed");
                                        context.push_tool_error(
                                            id, name, error.clone(), None,
                                            std::time::Duration::ZERO, vec![],
                                        );
                                        let _ = ui_tx.send(UiUpdate::ToolError {
                                            id, error, duration: std::time::Duration::ZERO,
                                            content_sections: vec![], run_id: own_run_id,
                                        }).await;
                                    }
                                    mode = AgentMode::ToolPending;
                                    advance_tool_queue(
                                        &mut pending_tools,
                                        &mut context,
                                        &mut mode,
                                        &mut current_cancel,
                                        &mut current_tool_cancel,
                                        &mut iterations,
                                        max_tool_iterations,
                                        unknown_only_round,
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
                                        session_id,
                                        Some(&mut compaction),
                                    )
                                    .await;
                                }
                                AgentEvent::ToolDone { id, name, result, display_result, result_metadata, result_metadata_schema, summary, duration, content_sections, attachments } => {
                                    current_tool_cancel = None;
                                    let allowance = tool_output_token_limit(
                                        &name, &context, &registry.schemas(),
                                        engine.effective_context_length(),
                                        &queued_tool_names(&pending_tools, 0),
                                    );
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
                                    if let Some(allowance) = allowance {
                                        context.limit_last_model_message(allowance);
                                    }
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
                                    if compaction
                                        .schedule_summary(&context, engine.as_ref(), &registry.schemas(), generation)
                                    {
                                        let _ = ui_tx
                                            .send(UiUpdate::Notice {
                                                text: "Started background context compaction."
                                                    .to_string(),
                                                run_id: own_run_id,
                                            })
                                            .await;
                                    }
                                    advance_tool_queue(
                                        &mut pending_tools,
                                        &mut context,
                                        &mut mode,
                                        &mut current_cancel,
                                        &mut current_tool_cancel,
                                        &mut iterations,
                                        max_tool_iterations,
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
                                        session_id,
                                        Some(&mut compaction),
                                    )
                                    .await;
                                }
                                AgentEvent::ToolError { id, name, error, partial_output, duration, content_sections } => {
                                    current_tool_cancel = None;
                                    let allowance = tool_output_token_limit(
                                        &name, &context, &registry.schemas(),
                                        engine.effective_context_length(),
                                        &queued_tool_names(&pending_tools, 0),
                                    );
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
                                    if let Some(allowance) = allowance {
                                        context.limit_last_model_message(allowance);
                                    }
                                    let _ = ui_tx.send(UiUpdate::ToolError {
                                        id,
                                        error,
                                        duration,
                                        content_sections,
                                        run_id: own_run_id,
                                    }).await;
                                    if compaction
                                        .schedule_summary(&context, engine.as_ref(), &registry.schemas(), generation)
                                    {
                                        let _ = ui_tx
                                            .send(UiUpdate::Notice {
                                                text: "Started background context compaction."
                                                    .to_string(),
                                                run_id: own_run_id,
                                            })
                                            .await;
                                    }
                                    advance_tool_queue(
                                        &mut pending_tools,
                                        &mut context,
                                        &mut mode,
                                        &mut current_cancel,
                                        &mut current_tool_cancel,
                                        &mut iterations,
                                        max_tool_iterations,
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
                                        session_id,
                                        Some(&mut compaction),
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
                            if mode == AgentMode::Idle && pending_tools.is_empty() {
                                // Apply any completed prefix summary or hard
                                // fallback before deciding whether this child
                                // report itself needs summarization.
                                sweep_compaction_and_report(
                                    &mut compaction,
                                    &mut context,
                                    engine.as_ref(),
                                    &registry.schemas(),
                                    generation,
                                    own_run_id,
                                    &ui_tx,
                                )
                                .await;
                            }
                            let mut parent_result = finish_result.result.clone();
                            let result_token_budget = context.available_result_token_budget(
                                &registry.schemas(),
                                engine.effective_context_length(),
                            );
                            let original_parent_result = parent_result.clone();
                            if let Some(result) = parent_result.as_deref() {
                                let result_estimator = crate::llm::TokenEstimator::Tiktoken;
                                if result_estimator.estimate_tokens(result) > result_token_budget {
                                    parent_result = Some(compaction::recovery::marked_text(
                                        result_estimator, result,
                                        compaction::recovery::RESULT_OMISSION_MARKER,
                                        result_token_budget / 2,
                                    ));
                                }
                            }
                            // The LLM-facing notice stays structured JSON (the
                            // model reads correlation/lifecycle fields as well as
                            // `status`/`result`/`error`/`sources`); only the TUI
                            // bubble renders result content as Markdown sections.
                            let mut completion_event = finish_result
                                .to_completion_json(&label, outstanding_count);
                            completion_event["result"] = parent_result
                                .clone()
                                .map(serde_json::Value::String)
                                .unwrap_or(serde_json::Value::Null);
                            completion_event["result_summarized"] = serde_json::Value::Bool(false);
                            completion_event["result_reduced"] = serde_json::Value::Bool(
                                original_parent_result != parent_result,
                            );
                            let result_text =
                                serde_json::to_string(&completion_event).unwrap_or_default();
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
                                     all outstanding child results are available. This is expected, \
                                     normal operation — do not reason about scheduler/concurrency \
                                     internals or speculate on timing. Continue only genuinely \
                                     independent work. If none remains, end this response \
                                     immediately, without waiting narration. Do not keep thinking \
                                     while waiting for results. Ending this response does not \
                                     cancel the subagents or complete the user's task."
                                )
                            } else {
                                "All spawned subagent runs are now terminal. Combine this result with \
                                 earlier completion events in the conversation and produce the \
                                 answer for the user."
                                    .to_string()
                            };
                            let projected_notice = format!(
                                "Subagent completion event; the structured JSON is harness metadata, \
                                 not a user-facing message. Do not assume tool results or subagent \
                                 responses are visible to the user. When all_done is true, use the \
                                 relevant findings, sources, and limitations from these results to \
                                 produce a self-contained answer to the user's request. Preserve \
                                 the user's requested format, including tables. Present the actual \
                                 findings, not just a completion acknowledgement or a reference to \
                                 results 'above'. Report failures or missing findings explicitly; \
                                 do not invent details omitted from a reduced result. Do not quote \
                                 the harness JSON verbatim:\n{result_text}\n\
                                 {next_step_note}",
                            );
                            let mut canonical_event = finish_result.to_completion_json(&label, outstanding_count);
                            canonical_event["result_summarized"] = serde_json::Value::Bool(false);
                            canonical_event["result_reduced"] = serde_json::Value::Bool(false);
                            context.push_async_harness_notice(projected_notice.replace(&result_text, &canonical_event.to_string()));
                            if original_parent_result != parent_result {
                                context.model_overrides.insert(context.messages.len() - 1, Message::User {
                                    content: format!("{HARNESS_NOTICE_MARKER}\n{projected_notice}"),
                                    kind: UserMessageKind::ImplicitToolNote,
                                });
                                context.projection_dirty = true;
                            }
                            context.limit_last_model_message(result_token_budget);
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
                                    max_tool_iterations,
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
                                        &mut context,
                                        Some(&mut compaction),
                                        engine.clone(),
                                        registry.schemas(),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                        generation,
                                        own_run_id,
                                        session_id,
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
                                update @ UiUpdate::ChildContextUsage { .. } => {
                                    // Child readouts are already scoped to their
                                    // run; forward them so the TUI can retain each
                                    // child independently of the parent view.
                                    let _ = ui_tx.send(update).await;
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
                        outcome = compaction_rx.recv() => {
                            // A background summary finished (or failed). The
                            // loop itself applies it — never the summarizer —
                            // and only at a safe boundary: idle with an empty
                            // serial tool queue. Otherwise it stays pending
                            // until the next completed turn.
                            if let Some(outcome) = outcome {
                                let summary_failure = match &outcome {
                                    compaction::CompactionOutcome::SummaryFailed { reason, .. } => {
                                        Some(reason.clone())
                                    }
                                    compaction::CompactionOutcome::SummaryReady { .. } => None,
                                };
                                if !compaction.on_outcome(outcome, generation) {
                                    continue;
                                }
                                let waiting_continuation = compaction.take_waiting_continuation();
                                if (mode == AgentMode::Idle || waiting_continuation) && pending_tools.is_empty() {
                                    let prefix_dropped = sweep_compaction_and_report(
                                        &mut compaction,
                                        &mut context,
                                        engine.as_ref(),
                                        &registry.schemas(),
                                        generation,
                                        own_run_id,
                                        &ui_tx,
                                    )
                                    .await;
                                    if waiting_continuation || (is_child && prefix_dropped) {
                                        resume_child_after_compaction(
                                            &mut context,
                                            &mut compaction,
                                            &mut mode,
                                            &mut current_cancel,
                                            &engine,
                                            &registry,
                                            &ui_tx,
                                            &agent_tx,
                                            generation,
                                            own_run_id,
                                            session_id,
                                        );
                                    }
                                } else if waiting_continuation {
                                    advance_tool_queue(
                                        &mut pending_tools, &mut context, &mut mode,
                                        &mut current_cancel, &mut current_tool_cancel,
                                        &mut iterations, max_tool_iterations, false,
                                        engine.clone(), registry.clone(), ui_tx.clone(), agent_tx.clone(),
                                        generation, &mut clarify_started_at, &mut pending_steers,
                                        &mut child_agent_manager, &mut spawn_content_sections, own_run_id,
                                        &mut pending_child_finish_reprompt, &mut iteration_cap_exceeded,
                                        session_id, Some(&mut compaction),
                                    ).await;
                                }
                                if let Some(reason) = summary_failure {
                                    report_compaction_failure(&ui_tx, own_run_id, &reason).await;
                                }
                            }
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
                                        // A fresh turn is a safe boundary too:
                                        // applying (or forcing) compaction here
                                        // keeps the request that follows inside
                                        // the active context window.
                                        sweep_compaction_and_report(
                                            &mut compaction,
                                            &mut context,
                                            engine.as_ref(),
                                            &registry.schemas(),
                                            generation,
                                            own_run_id,
                                            &ui_tx,
                                        )
                                        .await;
                                        iterations = 1;
                                        context.push_user(text);
                                        mode = AgentMode::Streaming;
                                        current_cancel = Some(start_request(
                                            &mut context,
                                            Some(&mut compaction),
                                            engine.clone(),
                                            registry.schemas(),
                                            ui_tx.clone(),
                                            agent_tx.clone(),
                                            generation,
                                            own_run_id,
                                            session_id,
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
                                        let allowance = tool_output_token_limit(
                                            pending_tools.front().expect("checked front").call.name(),
                                            &context,
                                            &registry.schemas(),
                                            engine.effective_context_length(),
                                            &queued_tool_names(&pending_tools, 1),
                                        );
                                        if allowance.is_some_and(|tokens| tokens < MINIMUM_USEFUL_RESULT_TOKENS) {
                                            advance_tool_queue(
                                                &mut pending_tools, &mut context, &mut mode,
                                                &mut current_cancel, &mut current_tool_cancel,
                                                &mut iterations, max_tool_iterations, true,
                                                engine.clone(), Arc::clone(&registry), ui_tx.clone(),
                                                agent_tx.clone(), generation, &mut clarify_started_at,
                                                &mut pending_steers, &mut child_agent_manager,
                                                &mut spawn_content_sections, own_run_id,
                                                &mut pending_child_finish_reprompt,
                                                &mut iteration_cap_exceeded, session_id,
                                                Some(&mut compaction),
                                            ).await;
                                            continue;
                                        }
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
                                                max_tool_iterations,
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
                                                session_id,
                                                Some(&mut compaction),
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
                                            let max_output_tokens = tool_output_token_limit(
                                                &name,
                                                &context,
                                                &registry.schemas(),
                                                engine.effective_context_length(),
                                                &queued_tool_names(&pending_tools, 0),
                                            );
                                            current_tool_cancel = Some(PendingTool::execute(
                                                pending,
                                                agent_tx.clone(),
                                                generation,
                                                max_output_tokens,
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
                                            max_tool_iterations,
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
                                            session_id,
                                            Some(&mut compaction),
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
                                            max_tool_iterations,
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
                                            session_id,
                                            Some(&mut compaction),
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
                                            max_tool_iterations,
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
                                            session_id,
                                            Some(&mut compaction),
                                        )
                                        .await;
                                    }
                                }
                                AgentCommand::Stop { reason } => {
                                    compaction.reset().await;
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
                                    // A summary (pending or in flight) belongs to
                                    // the conversation being cleared; its result
                                    // must never shape the replacement's model
                                    // view. Abort the request and clear its activity.
                                    compaction.reset().await;
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
                                            agent_type.as_deref(),
                                        ) {
                                            Ok(new_store) => fresh_context.set_store(new_store),
                                            Err(error) => tracing::warn!(
                                                %error,
                                                "could not start a new session file; the cleared conversation is not persisted"
                                            ),
                                        }
                                    }
                                    context = fresh_context;
                                    session_id = context.store().map(|store| store.session_id());
                                    *current_store
                                        .lock()
                                        .expect("current store mutex is not poisoned") =
                                        context.store();
                                }
                                AgentCommand::RecordModelChanged {
                                    provider_connection,
                                    model,
                                } => {
                                    // The common local baseline does not change
                                    // across provider/model switches.
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
                    if compaction.summary_pending() {
                        continue;
                    }
                    if compaction.enabled() && context.compaction().is_none() {
                        let prefix_dropped = sweep_compaction_and_report(
                            &mut compaction,
                            &mut context,
                            engine.as_ref(),
                            &registry.schemas(),
                            generation,
                            own_run_id,
                            &ui_tx,
                        )
                        .await;
                        if prefix_dropped {
                            resume_child_after_compaction(
                                &mut context,
                                &mut compaction,
                                &mut mode,
                                &mut current_cancel,
                                &engine,
                                &registry,
                                &ui_tx,
                                &agent_tx,
                                generation,
                                own_run_id,
                                session_id,
                            );
                            continue;
                        }
                        if compaction.summary_pending() {
                            continue;
                        }
                    }
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

/// Whether a tool's model-facing result is subject to the shared runtime
/// context allowance.
fn is_result_budgeted(tool_name: &str) -> bool {
    tool_name != tools::agent_finish::AgentFinishTool::NAME
}

/// Runtime token allowance for the result of `tool_name`, or `None` for tools
/// whose results are not context-budgeted.
///
/// The remaining allowance of the request ceiling is reduced by the error
/// framing reserved for every `queued_after` call still waiting, then shared
/// evenly among this call and the budgeted calls queued behind it, so one
/// early result cannot starve the rest of the batch.
fn tool_output_token_limit(
    tool_name: &str,
    context: &Context,
    tools: &[ToolSchema],
    context_length: usize,
    queued_after: &[&str],
) -> Option<usize> {
    if !is_result_budgeted(tool_name) {
        return None;
    }
    let available = context
        .available_result_token_budget(tools, context_length)
        .saturating_sub(ERROR_RESPONSE_FRAMING_TOKENS * queued_after.len());
    let unresolved_budgeted = 1 + queued_after
        .iter()
        .filter(|name| is_result_budgeted(name))
        .count();
    Some(available / unresolved_budgeted)
}

fn queued_tool_names(pending_tools: &VecDeque<PendingTool>, skip: usize) -> Vec<&str> {
    pending_tools
        .iter()
        .skip(skip)
        .map(|pending| pending.call.name())
        .collect()
}

/// Resolve one requested call that will not be executed: exactly one tool
/// error for the model plus a visible denied bubble for the user.
#[expect(clippy::too_many_arguments)]
async fn resolve_skipped_tool_call(
    context: &mut Context,
    ui_tx: &mpsc::Sender<UiUpdate>,
    own_run_id: Option<Uuid>,
    id: Uuid,
    name: String,
    title_arguments: Vec<(String, Value)>,
    content_sections: Vec<crate::agent::tools::Section>,
    reason: String,
) {
    context.batch_admitted_calls = context.batch_admitted_calls.saturating_sub(1);
    context.batch_skipped_calls += 1;
    tracing::info!(tool = %name, tool_call_id = %id, %reason, "tool call skipped, not executed");
    context.push_tool_error(
        id,
        name.clone(),
        reason.clone(),
        None,
        std::time::Duration::ZERO,
        content_sections.clone(),
    );
    context.limit_last_model_message(ERROR_RESPONSE_FRAMING_TOKENS);
    let _ = ui_tx
        .send(UiUpdate::ToolAutoDenied {
            id,
            name,
            title_arguments,
            content_sections,
            reason,
            run_id: own_run_id,
        })
        .await;
}

fn batch_limit_skip_reason(name: &str, batch_cap: usize) -> String {
    format!(
        "Tool '{name}' was not executed: the response requested more than \
         {batch_cap} tool calls and only the first \
         {batch_cap} are executed. Resubmit only the skipped \
         calls in smaller batches."
    )
}

fn context_budget_skip_reason(name: &str) -> String {
    format!(
        "Tool '{name}' was not executed: the remaining context budget cannot hold its \
         result alongside the other pending calls. Resubmit only the skipped calls in \
         smaller batches."
    )
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
/// the loop's effective `max_tool_iterations` is exceeded — but only for a
/// child loop
/// (`own_run_id.is_some()`); a root loop's own hit is reported to the human
/// directly (an `Interrupted` assistant bubble) and never sets this. The
/// caller (`AgentLoop::run`'s `is_child && mode == AgentMode::Idle` tail)
/// checks it to return `Err` instead of `Ok(context)` — so
/// `drive_child_to_completion` reports a clear `status: "failure"` instead of
/// folding this into the generic "unstructured" fallback used when a child
/// simply goes idle without ever calling `agent_finish`. See docs/features.md
/// → *spawn_subagent*/*agent_finish* for the distinction.
/// Increments `*iterations` and, if that now exceeds `max_tool_iterations`
/// (the loop's effective limit; see [`AgentLoop::with_max_tool_iterations`]),
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
    max_tool_iterations: usize,
    context: &mut Context,
    mode: &mut AgentMode,
    ui_tx: &mpsc::Sender<UiUpdate>,
    own_run_id: Option<Uuid>,
    iteration_cap_exceeded: &mut Option<String>,
) -> bool {
    *iterations += 1;
    tracing::debug!(iterations = *iterations, "advancing tool queue");
    if *iterations <= max_tool_iterations {
        return false;
    }
    tracing::warn!(
        iterations = *iterations,
        max_tool_iterations,
        run_id = ?own_run_id,
        "max tool-use iterations exceeded"
    );
    let message = format!("Max tool-use iterations ({max_tool_iterations}) exceeded.");
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
    max_tool_iterations: usize,
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
    session_id: Option<Uuid>,
    mut compaction: Option<&mut compaction::CompactionRuntime>,
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
    // against the loop's `max_tool_iterations`. Shadow the parameter as mutable so
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
                        max_tool_iterations,
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
                if let Some(compaction) = compaction.as_deref_mut() {
                    sweep_compaction_and_report(
                        compaction,
                        context,
                        engine.as_ref(),
                        &registry.schemas(),
                        generation,
                        own_run_id,
                        &ui_tx,
                    )
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
                    compaction,
                    engine,
                    registry.schemas(),
                    ui_tx,
                    agent_tx,
                    generation,
                    own_run_id,
                    session_id,
                ));
                return;
            }
            Some(front) => {
                let result_allowance = tool_output_token_limit(
                    front.call.name(),
                    context,
                    &registry.schemas(),
                    engine.effective_context_length(),
                    &queued_tool_names(pending_tools, 1),
                );
                if let Some(allowance) = result_allowance {
                    if allowance < MINIMUM_USEFUL_RESULT_TOKENS {
                        if compaction.as_deref_mut().is_some_and(|runtime| {
                            runtime.defer_request(context, engine.as_ref(), &registry.schemas())
                        }) {
                            let _ = ui_tx.send(UiUpdate::Notice {
                                text: "Paused not-yet-started tools for the existing context summary; running tools are not cancelled.".to_string(),
                                run_id: own_run_id,
                            }).await;
                            return;
                        }
                        let skipped = pending_tools.pop_front().expect("checked front");
                        let name = skipped.call.name().to_string();
                        resolve_skipped_tool_call(
                            context,
                            &ui_tx,
                            own_run_id,
                            skipped.call.id(),
                            name.clone(),
                            skipped.call.title_arguments(),
                            skipped.call.pending_content_sections(),
                            context_budget_skip_reason(&name),
                        )
                        .await;
                        // Skipped rounds still consume the iteration allowance.
                        increment_iterations = true;
                        continue;
                    }
                }
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
                        let max_output_tokens = result_allowance;
                        *current_tool_cancel = Some(PendingTool::execute(
                            pending,
                            agent_tx.clone(),
                            generation,
                            max_output_tokens,
                        ));
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

async fn send_preflight_notice(
    sender: &mpsc::Sender<(u64, AgentEvent)>,
    generation: u64,
    text: String,
) -> bool {
    let (acknowledgement, received) = tokio::sync::oneshot::channel();
    let acknowledgement = Arc::new(Mutex::new(Some(acknowledgement)));
    if sender
        .send((
            generation,
            AgentEvent::PreflightNotice {
                text,
                acknowledgement,
            },
        ))
        .await
        .is_err()
    {
        return false;
    }
    received.await.is_ok()
}

/// Spawns the inner streaming task for one request, built from an immutable
/// snapshot of the conversation history. Returns the `CancellationToken` that
/// the outer loop uses to abort this request.
///
/// This task owns the [`ToolCallAccumulator`]s and assembles raw
/// [`StreamEvent::Delta`] fragments into a batch of [`AssembledToolCall`]s
/// (blueprint §4A refinement); the outer loop stays the sole writer of
/// `Context`, and this task never writes it.
#[expect(clippy::too_many_arguments)]
fn start_request(
    context: &mut Context,
    compaction: Option<&mut compaction::CompactionRuntime>,
    engine: Arc<dyn Engine>,
    tools: Vec<ToolSchema>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<(u64, AgentEvent)>,
    generation: u64,
    own_run_id: Option<Uuid>,
    session_id: Option<Uuid>,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let mut notices = Vec::new();
    let send_notices = |notices: Vec<String>| {
        let sender = agent_tx.clone();
        tokio::spawn(async move {
            for notice in notices {
                if !send_preflight_notice(&sender, generation, notice).await {
                    return;
                }
            }
        });
    };
    if let Some(compaction) = compaction {
        if compaction.defer_request(context, engine.as_ref(), &tools) {
            send_notices(vec!["Waiting for the running context summary within its original deadline; queued work has not been replayed.".to_string()]);
            return cancel;
        }
        let result = compaction.sweep(context, engine.as_ref(), &tools, generation);
        if result.summary_applied || result.prefix_dropped {
            let text = if result.summary_applied {
                "Background context compaction completed. Older context was summarized; the full transcript is preserved."
            } else {
                "Reduced context lossily before continuation. The task anchor and full transcript are preserved."
            };
            notices.push(text.to_string());
        }
        if compaction.defer_request(context, engine.as_ref(), &tools) {
            notices.push(
                "Waiting for the running context summary within its original deadline.".to_string(),
            );
            send_notices(notices);
            return cancel;
        }
    }
    let retrying = std::mem::take(&mut context.retry_after_context_rejection);
    let ceiling = compaction::safe_request_ceiling(engine.effective_context_length());
    let ceiling = if retrying { ceiling * 3 / 4 } else { ceiling };
    if context.estimated_model_tokens(&tools, engine.as_ref()) > ceiling {
        context.compaction_trigger = if retrying {
            "provider_context_rejection"
        } else {
            "request_boundary"
        };
        let report = context.recover_model_view(
            (engine.effective_context_length() as f64 * engine.config().compact_target_ratio)
                as usize,
            ceiling,
            &tools,
        );
        let sender = agent_tx.clone();
        if !report.fits_ceiling {
            tracing::error!(
                estimated_tokens = report.estimated_tokens_after,
                ceiling,
                "no bounded continuation can fit"
            );
            tokio::spawn(async move {
                for notice in notices {
                    if !send_preflight_notice(&sender, generation, notice).await {
                        return;
                    }
                }
                let _ = sender.send((generation, AgentEvent::StreamError(
                    "The context window cannot hold the system instructions, tool definitions and a valid task continuation. Select a model with a larger context window or start a new conversation with a shorter task.".to_string()
                ))).await;
            });
            return cancel;
        }
        notices.push("Reduced the model view lossily to fit the context window. The task anchor remains; omitted output and the full transcript are preserved.".to_string());
    }
    let messages = context.model_messages();
    context.persist_model_projection();
    let request_tools = tools.clone();
    let usage_update_sender = ui_tx.clone();
    let on_request_estimate = Box::new(move |estimated_tokens, request_context_length| {
        let usage_update_sender = usage_update_sender.clone();
        Box::pin(async move {
            send_context_usage_estimate(
                &usage_update_sender,
                estimated_tokens,
                request_context_length,
                own_run_id,
                ContextUsageSource::SerializedRequest,
            )
            .await;
        }) as futures::future::BoxFuture<'static, ()>
    });
    let request_dump_failure_handler = crate::http::RequestDumpFailureHandler::default();
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
            for notice in notices {
                if !send_preflight_notice(&agent_tx, generation, notice).await {
                    return;
                }
            }
            if inner_cancel.is_cancelled() {
                let _ = ui_tx.send(UiUpdate::StreamCancelled).await;
                let _ = agent_tx
                    .send((
                        generation,
                        AgentEvent::StreamCancelled {
                            partial: String::new(),
                        },
                    ))
                    .await;
                return;
            }
            let mut stream = match engine
                .chat_with_session_id_and_diagnostics(
                    messages,
                    request_tools,
                    inner_cancel.clone(),
                    session_id,
                    on_request_estimate,
                    Some(request_dump_failure_handler),
                )
                .await
            {
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
                        let event = if !retrying && is_context_window_rejection(&message) {
                            AgentEvent::ContextWindowRejected(message)
                        } else {
                            AgentEvent::StreamError(message)
                        };
                        let _ = agent_tx.send((generation, event)).await;
                    }
                    return;
                }
            };
            // `engine.chat` completed the one-shot catalog hydration, so any
            // notices it queued (e.g. a dropped profile reasoning effort) are
            // ready — surface them before the stream events so they render
            // above this turn's response.
            for text in engine.take_pending_notices() {
                let _ = ui_tx
                    .send(UiUpdate::Notice {
                        text,
                        run_id: own_run_id,
                    })
                    .await;
            }
            let _ = ui_tx.send(UiUpdate::StreamStarted).await;
            let mut partial = String::new();
            let mut accumulators: std::collections::BTreeMap<usize, ToolCallAccumulator> =
                std::collections::BTreeMap::new();
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
                        if !accumulators.contains_key(&delta.tool_call_index)
                            && accumulators.len() >= MAX_TOOL_CALLS_PER_RESPONSE
                        {
                            inner_cancel.cancel();
                            let _ = agent_tx
                                .send((
                                    generation,
                                    AgentEvent::ToolCallLimitExceeded {
                                        limit: MAX_TOOL_CALLS_PER_RESPONSE,
                                    },
                                ))
                                .await;
                            break;
                        }
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
                        let event = if !retrying
                            && partial.is_empty()
                            && accumulators.is_empty()
                            && is_context_window_rejection(&message)
                        {
                            AgentEvent::ContextWindowRejected(message)
                        } else {
                            AgentEvent::StreamError(message)
                        };
                        let _ = agent_tx.send((generation, event)).await;
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

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        ))
    }

    fn test_engine_with_context_length(context_length: usize) -> Arc<dyn Engine> {
        Arc::new(EngineImpl::new(
            Arc::new(MockProvider::new(vec![], Duration::from_millis(0))),
            EngineConfig {
                provider_connection: "mock".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: Some(context_length),
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.4,
                soft_compaction_threshold: 0.5,
                hard_compaction_threshold: 0.8,
            },
        ))
    }

    struct ToolResultTrackingProvider {
        requests: Arc<Mutex<Vec<Vec<Message>>>>,
        request_received: Arc<tokio::sync::Notify>,
        reproduce_unknown_tool_overlap: bool,
    }

    impl Provider for ToolResultTrackingProvider {
        fn name(&self) -> &str {
            "tool-result-tracking"
        }

        fn chat<'a>(
            &'a self,
            messages: Vec<Message>,
            _tools: Vec<ToolSchema>,
            _cancel: CancellationToken,
        ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            let request_index = {
                let mut requests = self
                    .requests
                    .lock()
                    .expect("request tracking lock is not poisoned");
                let request_index = requests.len();
                requests.push(messages);
                request_index
            };
            self.request_received.notify_one();
            let reproduce_unknown_tool_overlap = self.reproduce_unknown_tool_overlap;
            Box::pin(async move {
                let (stream_tx, stream_rx) = mpsc::channel(4);
                tokio::spawn(async move {
                    if request_index == 0 {
                        if reproduce_unknown_tool_overlap {
                            for tool_call_index in [1, 2] {
                                let _ = stream_tx
                                    .send(StreamEvent::Delta(DeltaChunk {
                                        tool_call_index,
                                        tool_call_id: Some(format!(
                                            "unknown-grep-{tool_call_index}"
                                        )),
                                        function_name: Some("grep".to_string()),
                                        arguments_fragment: Some("{}".to_string()),
                                    }))
                                    .await;
                            }
                        }
                        let _ = stream_tx
                            .send(StreamEvent::Delta(DeltaChunk {
                                tool_call_index: 0,
                                tool_call_id: Some("read-large-context".to_string()),
                                function_name: Some("read".to_string()),
                                arguments_fragment: Some(
                                    if reproduce_unknown_tool_overlap {
                                        r#"{"path":"src/agent.rs","mode":"lines","limit":1}"#
                                    } else {
                                        r#"{"path":"src/agent.rs","mode":"lines","limit":1200}"#
                                    }
                                    .to_string(),
                                ),
                            }))
                            .await;
                    } else if request_index == 1 && reproduce_unknown_tool_overlap {
                        let _ = stream_tx
                            .send(StreamEvent::Delta(DeltaChunk {
                                tool_call_index: 0,
                                tool_call_id: Some("approved-bash".to_string()),
                                function_name: Some("bash".to_string()),
                                arguments_fragment: Some(
                                    r#"{"command":"printf regression-approved"}"#.to_string(),
                                ),
                            }))
                            .await;
                    } else {
                        let _ = stream_tx
                            .send(StreamEvent::Text("Read result incorporated.".to_string()))
                            .await;
                    }
                    let _ = stream_tx.send(StreamEvent::Done).await;
                });
                Ok(stream_rx)
            })
        }

        fn chat_with_session_id_and_estimate<'a>(
            &'a self,
            messages: Vec<Message>,
            tools: Vec<ToolSchema>,
            cancel: CancellationToken,
            _session_id: Option<Uuid>,
            on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
        ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            let estimator = crate::llm::TokenEstimator::Tiktoken;
            let estimated_input_tokens = messages
                .iter()
                .map(|message| estimate_message_tokens(estimator, message))
                .sum::<usize>()
                + tool_schema_token_count(&tools);
            let context_length = 10_000;
            Box::pin(async move {
                on_estimate(estimated_input_tokens, context_length).await;
                self.chat(messages, tools, cancel).await
            })
        }

        fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    #[test]
    fn unknown_only_batch_counts_once_and_respects_iteration_cap() {
        crate::test_support::run_locked(async {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(ToolResultTrackingProvider {
                    requests: Arc::clone(&requests),
                    request_received: Arc::new(tokio::sync::Notify::new()),
                    reproduce_unknown_tool_overlap: true,
                }),
                test_engine().config(),
            ));
            let (agent_loop, mut ui_rx) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
            let mut agent_loop = agent_loop.with_max_tool_iterations(1);
            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt("Run the tools.".to_string()))
                .await;
            let mut unknown_tool_errors = 0;
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match ui_rx.recv().await.expect("agent channel stays open") {
                        UiUpdate::ToolError { .. } => unknown_tool_errors += 1,
                        UiUpdate::Error { message, .. } => {
                            assert!(message.contains("Max tool-use iterations (1) exceeded."));
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("all-unknown batch reaches iteration cap");
            drop(agent_loop);
            let context = tokio::time::timeout(Duration::from_secs(3), handle)
                .await
                .expect("agent stops")
                .expect("agent task succeeds")
                .expect("root returns context");
            assert_eq!(unknown_tool_errors, 3);
            assert_eq!(requests.lock().expect("requests lock").len(), 1);
            assert_eq!(context.messages().iter().filter(|message| matches!(message, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty())).count(), 1);
        });
    }

    #[test]
    fn unknown_tool_does_not_leave_bash_approval_idle() {
        crate::test_support::run_locked(async {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let request_received = Arc::new(tokio::sync::Notify::new());
            let release_read = Arc::new(tokio::sync::Notify::new());
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(ToolResultTrackingProvider {
                    requests: Arc::clone(&requests),
                    request_received: Arc::clone(&request_received),
                    reproduce_unknown_tool_overlap: true,
                }),
                EngineConfig {
                    provider_connection: "tool-result-tracking".to_string(),
                    model: "test-model".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                    compact_target_ratio: 0.4,
                    soft_compaction_threshold: 0.5,
                    hard_compaction_threshold: 0.8,
                },
            ));
            let mut registry =
                tools::default_registry(test_sandbox(), tools::ToolRegistryConfig::default());
            registry.register(Arc::new(GatedReadTool {
                inner: tools::read::ReadTool::new(SandboxContext::new(test_sandbox())),
                release: Arc::clone(&release_read),
            }));
            let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine, Arc::new(registry));
            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt("Run the tools.".to_string()))
                .await;
            request_received.notified().await;
            let mut premature_follow_up = false;
            let bash_id = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match ui_rx.recv().await.expect("agent channel stays open") {
                        UiUpdate::ToolStart { name, .. } if name == "read" => {
                            premature_follow_up = tokio::time::timeout(
                                Duration::from_millis(100),
                                request_received.notified(),
                            )
                            .await
                            .is_ok();
                            if !premature_follow_up {
                                release_read.notify_one();
                            }
                        }
                        UiUpdate::ToolPending { id, name, .. } if name == "bash" => break id,
                        _ => {}
                    }
                }
            })
            .await
            .expect("bash approval arrives");
            if premature_follow_up {
                release_read.notify_one();
                tokio::time::timeout(Duration::from_secs(3), async {
                    while !matches!(ui_rx.recv().await, Some(UiUpdate::StreamDone)) {}
                })
                .await
                .expect("premature stream finishes while bash awaits approval");
            }
            agent_loop
                .send_command(AgentCommand::ApproveTool { id: bash_id })
                .await;
            let completed = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if matches!(ui_rx.recv().await, Some(UiUpdate::ToolDone { id, .. }) if id == bash_id) {
                        break;
                    }
                }
            }).await.is_ok();
            drop(agent_loop);
            let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
            assert!(
                completed,
                "approved bash must execute rather than be ignored in Idle"
            );
            let requests = requests.lock().expect("requests lock");
            assert_eq!(requests.len(), 3, "one follow-up per completed tool batch");
            assert_eq!(
                requests[1].iter().filter(|message| matches!(message, Message::ToolError { name, .. } if name == "grep")).count(),
                2,
                "unknown-tool results reach the model in the same completed batch",
            );
            assert!(requests[1].iter().any(
                |message| matches!(message, Message::ToolResult { value } if value.name == "read")
            ));
            assert!(requests[2].iter().any(
                |message| matches!(message, Message::ToolResult { value } if value.name == "bash")
            ));
        });
    }

    struct GatedReadTool {
        inner: tools::read::ReadTool,
        release: Arc<tokio::sync::Notify>,
    }

    impl tools::Tool for GatedReadTool {
        fn name(&self) -> &str {
            "read"
        }
        fn description(&self) -> &str {
            "Gated read regression fixture"
        }
        fn input_schema(&self) -> schemars::schema::RootSchema {
            tools::Tool::input_schema(&self.inner)
        }
        fn needs_approval(&self, _: &serde_json::Value) -> crate::sandbox::ApprovalCheck {
            crate::sandbox::ApprovalCheck::AutoApproved
        }
        fn timeout_secs(&self) -> u64 {
            10
        }
        fn non_default_redacted_arguments(
            &self,
            arguments: &serde_json::Value,
        ) -> Vec<(String, serde_json::Value)> {
            tools::Tool::non_default_redacted_arguments(&self.inner, arguments)
        }
        fn create_call(&self, id: Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
            Box::new(GatedReadCall {
                inner: tools::Tool::create_call(&self.inner, id, arguments),
                release: Arc::clone(&self.release),
            })
        }
    }

    struct GatedReadCall {
        inner: Box<dyn ToolCall>,
        release: Arc<tokio::sync::Notify>,
    }

    impl ToolCall for GatedReadCall {
        fn id(&self) -> Uuid {
            self.inner.id()
        }
        fn name(&self) -> &str {
            self.inner.name()
        }
        fn arguments(&self) -> &serde_json::Value {
            self.inner.arguments()
        }
        fn timeout_secs(&self) -> u64 {
            10
        }
        fn argument_order(&self, key: &str) -> usize {
            self.inner.argument_order(key)
        }
        fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
            self.inner.non_default_redacted_arguments()
        }
        fn run(
            self: Box<Self>,
        ) -> BoxFuture<'static, Result<Box<dyn tools::ToolCallResult>, tools::ToolError>> {
            Box::pin(async move {
                self.release.notified().await;
                self.inner.run().await
            })
        }
    }

    #[test]
    fn large_read_result_is_capped_to_remaining_context_before_follow_up_request() {
        crate::test_support::run_locked(async {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let request_received = Arc::new(tokio::sync::Notify::new());
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(ToolResultTrackingProvider {
                    requests: Arc::clone(&requests),
                    request_received: Arc::clone(&request_received),
                    reproduce_unknown_tool_overlap: false,
                }),
                EngineConfig {
                    provider_connection: "tool-result-tracking".to_string(),
                    model: "test-model".to_string(),
                    temperature: None,
                    context_length: Some(10_000),
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                    compact_target_ratio: 0.4,
                    soft_compaction_threshold: 0.5,
                    hard_compaction_threshold: 0.8,
                },
            ));
            let registry = Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig {
                    allowed_tools: vec![tools::read::ReadTool::NAME.to_string()],
                    auto_approve: std::collections::HashSet::from([tools::AutoApproveTool::Read]),
                    ..Default::default()
                },
            ));
            let tool_schemas = registry.schemas();
            let (agent_loop, mut ui_rx) = AgentLoop::new(engine, registry);
            let mut agent_loop =
                agent_loop.with_compaction_engine(test_engine_with_context_length(10_000));
            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt(
                    "Read the requested source file.".to_string(),
                ))
                .await;

            let first_context_usage = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let UiUpdate::ContextUsage {
                        estimated_tokens,
                        context_length,
                        ..
                    } = ui_rx.recv().await.expect("agent update channel stays open")
                    {
                        break (estimated_tokens, context_length);
                    }
                }
            })
            .await
            .expect("request estimate arrives before stream events");
            request_received.notified().await;
            let expected_first_usage = {
                let first_request = requests
                    .lock()
                    .expect("request tracking lock is not poisoned")[0]
                    .clone();
                let estimator = crate::llm::TokenEstimator::Tiktoken;
                first_request
                    .iter()
                    .map(|message| estimate_message_tokens(estimator, message))
                    .sum::<usize>()
                    + tool_schema_token_count(&tool_schemas)
            };
            assert_eq!(first_context_usage, (expected_first_usage, 10_000));

            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if requests
                        .lock()
                        .expect("request tracking lock is not poisoned")
                        .len()
                        >= 2
                    {
                        break;
                    }
                    request_received.notified().await;
                }
            })
            .await
            .expect("follow-up request starts after the tool result is committed");

            let second_request = requests
                .lock()
                .expect("request tracking lock is not poisoned")[1]
                .clone();
            let read_result = second_request.iter().find_map(|message| match message {
                Message::ToolResult { value } if value.name == tools::read::ReadTool::NAME => {
                    Some(value)
                }
                _ => None,
            });
            let read_result = read_result.expect("bounded read result remains in the model view");
            let result_token_budget = 10_000usize
                .saturating_sub(10_000 / 10)
                .saturating_sub(10_000 / 20)
                .saturating_sub(512)
                .saturating_sub(first_context_usage.0);
            assert!(
                crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&read_result.result)
                    <= result_token_budget,
                "read result fits its execution-time output budget"
            );
            let estimate_messages = |messages: &[Message]| {
                let estimator = crate::llm::TokenEstimator::Tiktoken;
                messages
                    .iter()
                    .map(|message| estimate_message_tokens(estimator, message))
                    .sum::<usize>()
                    + tool_schema_token_count(&tool_schemas)
            };
            let expected_usage = estimate_messages(&second_request);
            let first_request = requests
                .lock()
                .expect("request tracking lock is not poisoned")[0]
                .clone();
            let initial_usage = estimate_messages(&first_request);
            let mut observed_usage = Vec::new();
            while observed_usage.last().copied() != Some(expected_usage) {
                let update = tokio::time::timeout(Duration::from_secs(3), ui_rx.recv())
                    .await
                    .expect("context usage arrives before the follow-up request")
                    .expect("agent update channel stays open");
                if let UiUpdate::ContextUsage {
                    estimated_tokens, ..
                } = update
                {
                    observed_usage.push(estimated_tokens);
                }
            }
            assert!(
                observed_usage.contains(&expected_usage) && expected_usage < 10_000,
                "footer reflects the bounded follow-up request: initial={initial_usage}, after={observed_usage:?}"
            );
            drop(agent_loop);
            let _ = tokio::time::timeout(Duration::from_secs(3), handle)
                .await
                .expect("agent loop terminates after drop");
        });
    }

    #[tokio::test]
    async fn compaction_marker_emission_preserves_the_owning_run() {
        let (ui_tx, mut ui_rx) = mpsc::channel(4);
        let mut context = Context::new();
        for message_index in 0..15 {
            context.push_user(format!("Message {message_index}"));
        }
        let boundary = context.messages().len();
        for run_id in [None, Some(Uuid::new_v4())] {
            send_compaction_applied(&ui_tx, &context, boundary, run_id).await;
            let update = ui_rx.recv().await.expect("marker was emitted");
            let UiUpdate::CompactionApplied {
                run_id: marker_run_id,
                entry_index,
                summarized_messages,
            } = update
            else {
                panic!("expected a compaction marker, got {update:?}");
            };
            assert_eq!(marker_run_id, run_id);
            assert_eq!(summarized_messages, 15);
            assert_eq!(
                entry_index,
                crate::tui_reducer_state::project_messages(context.messages()).len()
            );
        }
    }

    #[test]
    fn reports_background_compaction_start_in_the_transcript() {
        crate::test_support::run_locked(async {
            let (ui_tx, mut ui_rx) = mpsc::channel(4);
            let mut context = Context::new();
            context.push_user("word ".repeat(10_000));
            context.push_assistant("recorded answer ".repeat(100), AssistantStatus::Complete);
            let active_engine = test_engine_with_context_length(16_000);
            let compact_engine = test_engine();
            let (mut compaction, _outcome_rx) = compaction::CompactionRuntime::new(
                Some(compact_engine),
                compaction::COMPACTION_SUMMARY_PROMPT.to_string(),
                Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
            );

            sweep_compaction_and_report(
                &mut compaction,
                &mut context,
                active_engine.as_ref(),
                &[],
                0,
                None,
                &ui_tx,
            )
            .await;

            assert!(matches!(
                ui_rx.recv().await,
                Some(UiUpdate::Notice { text, run_id: None })
                    if text == "Started background context compaction."
            ));
        });
    }

    #[test]
    fn reports_summary_before_synchronous_drop_in_the_same_sweep() {
        crate::test_support::run_locked(async {
            let (ui_tx, mut ui_rx) = mpsc::channel(8);
            let mut context = Context::new();
            context.push_user("old context".to_string());
            let summary_boundary = context.messages().len();
            context.push_user("word ".repeat(20_000));
            context.push_user("current task".to_string());
            let active_engine = test_engine_with_context_length(16_000);
            let (mut compaction, _outcome_rx) = compaction::CompactionRuntime::new(
                Some(test_engine()),
                compaction::COMPACTION_SUMMARY_PROMPT.to_string(),
                Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
            );
            compaction.on_outcome(
                compaction::CompactionOutcome::SummaryReady {
                    generation: 0,
                    request_id: None,
                    boundary: summary_boundary,
                    summary: "Earlier work.".to_string(),
                },
                0,
            );
            sweep_compaction_and_report(
                &mut compaction,
                &mut context,
                active_engine.as_ref(),
                &[],
                0,
                None,
                &ui_tx,
            )
            .await;
            assert!(matches!(
                ui_rx.recv().await,
                Some(UiUpdate::Notice { text, .. })
                    if text.starts_with("Background context compaction completed.")
            ));
            assert!(matches!(
                ui_rx.recv().await,
                Some(UiUpdate::Notice { text, .. })
                    if text.starts_with("Compacted context synchronously")
            ));
        });
    }

    #[test]
    fn reports_background_compaction_completion_with_detail_loss_guidance() {
        crate::test_support::run_locked(async {
            let (ui_tx, mut ui_rx) = mpsc::channel(8);
            let mut context = Context::new();
            context.push_user("old context".to_string());
            let active_engine = test_engine_with_context_length(16_000);
            let (mut compaction, _outcome_rx) = compaction::CompactionRuntime::new(
                Some(test_engine()),
                compaction::COMPACTION_SUMMARY_PROMPT.to_string(),
                Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
            );
            let run_id = Some(Uuid::new_v4());
            compaction.on_outcome(
                compaction::CompactionOutcome::SummaryReady {
                    generation: 0,
                    request_id: None,
                    boundary: context.messages().len(),
                    summary: "Earlier work.".to_string(),
                },
                0,
            );
            sweep_compaction_and_report(
                &mut compaction,
                &mut context,
                active_engine.as_ref(),
                &[],
                0,
                run_id,
                &ui_tx,
            )
            .await;
            assert!(matches!(
                ui_rx.recv().await,
                Some(UiUpdate::Notice { text, run_id: notice_run_id })
                    if notice_run_id == run_id
                        && text.starts_with("Background context compaction completed.")
                        && text.contains("the full transcript is preserved")
                        && text.contains("If compaction becomes frequent")
                        && text.contains("narrowing the task")
                        && text.contains("concise handoff")
                        && text.contains("repeated compaction may lose detail")
            ));
        });
    }

    #[test]
    fn reports_compaction_failure_and_refreshes_model_view_usage() {
        crate::test_support::run_locked(async {
            let (ui_tx, mut ui_rx) = mpsc::channel(8);
            let mut context = Context::new();
            context.push_user("old context ".repeat(4_000));
            context.push_assistant("old answer".to_string(), AssistantStatus::Complete);
            context.push_user("current task".to_string());
            let active_engine = test_engine_with_context_length(16_000);
            let compact_engine = test_engine();
            let (mut compaction, mut outcome_rx) = compaction::CompactionRuntime::new(
                Some(compact_engine),
                compaction::COMPACTION_SUMMARY_PROMPT.to_string(),
                Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
            );
            ui_tx
                .send(UiUpdate::ContextUsage {
                    estimated_tokens: 12_000,
                    context_length: 16_000,
                    source: ContextUsageSource::SerializedRequest,
                })
                .await
                .expect("seed stale usage");
            // Drive the real reporting function's post-compaction path using
            // a forced fallback outcome at the next safe boundary.
            compaction.on_outcome(
                compaction::CompactionOutcome::SummaryFailed {
                    generation: 0,
                    request_id: None,
                    reason: "compaction summary timed out after 60 seconds".to_string(),
                },
                0,
            );
            sweep_compaction_and_report(
                &mut compaction,
                &mut context,
                active_engine.as_ref(),
                &[],
                0,
                None,
                &ui_tx,
            )
            .await;

            let updates = std::iter::from_fn(|| ui_rx.try_recv().ok()).collect::<Vec<_>>();
            assert!(updates.iter().any(|update| matches!(
                update,
                UiUpdate::Notice { text, run_id: None }
                    if text.contains("synchronously")
                        && text.contains("Older context was removed from the model view")
                        && text.contains("some details may no longer be available to the model")
                        && text.contains("The full transcript is preserved")
            )));
            assert!(updates.iter().any(|update| matches!(
                update,
                UiUpdate::ContextUsage {
                    source: ContextUsageSource::ModelView,
                    ..
                }
            )));
            let expected_usage = context.estimated_model_tokens(&[], active_engine.as_ref());
            let refreshed_usage = updates.iter().find_map(|update| match update {
                UiUpdate::ContextUsage {
                    estimated_tokens,
                    context_length,
                    source: ContextUsageSource::ModelView,
                } => Some((*estimated_tokens, *context_length)),
                _ => None,
            });
            assert!(
                refreshed_usage == Some((expected_usage, 16_000)),
                "compaction reports the current model-view estimate: expected {expected_usage}"
            );
            assert!(outcome_rx.try_recv().is_err());
        });
    }

    #[test]
    fn reports_compaction_failure_reason_as_transcript_notice() {
        crate::test_support::run_locked(async {
            let (ui_tx, mut ui_rx) = mpsc::channel(1);
            report_compaction_failure(
                &ui_tx,
                None,
                "compaction summary timed out after 60 seconds",
            )
            .await;

            assert!(matches!(
                ui_rx.recv().await,
                Some(UiUpdate::Notice { text, run_id: None })
                    if text.contains("failed") && text.contains("timed out after 60 seconds")
            ));
        });
    }

    #[test]
    fn request_estimate_updates_footer_with_the_sent_request_window() {
        crate::test_support::run_locked(async {
            let (ui_tx, mut ui_rx) = mpsc::channel(1);
            send_context_usage_estimate(
                &ui_tx,
                46_977,
                65_536,
                None,
                ContextUsageSource::SerializedRequest,
            )
            .await;

            assert!(matches!(
                ui_rx.recv().await,
                Some(UiUpdate::ContextUsage {
                    estimated_tokens: 46_977,
                    context_length: 65_536,
                    source: ContextUsageSource::SerializedRequest,
                })
            ));
        });
    }

    /// Engine double that reports one pending notice, for asserting the
    /// agent loop forwards engine notices ahead of the stream.
    struct NoticeEngine {
        inner: EngineImpl,
        pending: std::sync::Mutex<Vec<String>>,
    }

    impl Engine for NoticeEngine {
        fn chat<'a>(
            &'a self,
            messages: Vec<Message>,
            tools: Vec<ToolSchema>,
            cancel: CancellationToken,
        ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            self.inner.chat(messages, tools, cancel)
        }

        fn config(&self) -> EngineConfig {
            self.inner.config()
        }

        fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
            self.inner.list_models()
        }

        fn switch_model(&self, model: &str) -> anyhow::Result<()> {
            self.inner.switch_model(model)
        }

        fn take_pending_notices(&self) -> Vec<String> {
            std::mem::take(
                &mut *self
                    .pending
                    .lock()
                    .expect("the engine notice lock is poisoned"),
            )
        }
    }

    #[test]
    fn engine_notices_are_forwarded_before_the_stream_starts() {
        crate::test_support::run_locked(async {
            let engine: Arc<dyn Engine> = Arc::new(NoticeEngine {
                inner: EngineImpl::new(
                    Arc::new(MockProvider::new(
                        vec!["hi".to_string()],
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

                        compact_target_ratio: 0.5,
                        soft_compaction_threshold: 0.7,
                        hard_compaction_threshold: 0.9,
},
                ),
                pending: std::sync::Mutex::new(vec![
                    "Model mock/test-model does not advertise reasoning effort \"max\"; using the provider default.".to_string(),
                ]),
            });
            let (mut agent, mut ui_rx) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
            let handle = agent.run();
            agent
                .send_command(AgentCommand::Prompt("hello".to_string()))
                .await;

            let mut updates = Vec::new();
            while !updates
                .iter()
                .any(|update| matches!(update, UiUpdate::StreamStarted))
            {
                updates.push(
                    tokio::time::timeout(Duration::from_millis(500), ui_rx.recv())
                        .await
                        .expect("the stream starts after initial updates")
                        .expect("the UI channel stays open"),
                );
            }
            assert!(
                matches!(updates.first(), Some(UiUpdate::Notice { run_id: None, .. }))
                    && matches!(updates.first(), Some(UiUpdate::Notice { text, .. })
                        if text.contains("does not advertise reasoning effort")),
                "the engine notice must be forwarded before the stream: {updates:?}"
            );

            drop(agent);
            handle
                .await
                .expect("agent task should not panic")
                .expect("agent should stop cleanly after the channel closes");
        });
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

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
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

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
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
        assert!(TOOL_PROTOCOL_SYSTEM_PROMPT.contains(&format!(
            "at most {MAX_TOOL_CALLS_PER_RESPONSE} distinct tool calls"
        )));
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
            None,
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
    fn token_counts_are_cached_when_messages_enter_the_context() {
        let estimator = crate::llm::TokenEstimator::FourCharactersPerToken;
        let mut context = Context::new();
        context.set_token_estimator(estimator);
        context.push_user("hello world".to_string());
        context.push_assistant("hi".to_string(), AssistantStatus::Complete);
        context.push_tool_result(ToolResult {
            id: Uuid::new_v4(),
            name: "read".to_string(),
            result: "abcdefgh".to_string(),
            attachments: vec![],
            result_section_id: None,
            result_metadata: None,
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: Duration::ZERO,
        });

        for index in 0..context.messages().len() {
            let expected = estimate_message_tokens(estimator, &context.messages()[index]);
            assert_eq!(
                context.token_count(index),
                Some(expected),
                "the count is cached at insertion for message {index}"
            );
        }
        let sum: usize = (0..context.messages().len())
            .map(|index| context.token_count(index).expect("entry exists"))
            .sum();
        assert_eq!(context.approximate_token_count(), sum);
        assert!(
            context.approximate_token_count() > 0,
            "a non-empty context occupies some tokens"
        );
        assert!(context.tier(0).is_some());
        assert_eq!(context.token_count(context.messages().len()), None);
    }

    #[test]
    fn switching_the_token_estimator_refreshes_every_cached_count() {
        let tiktoken = crate::llm::TokenEstimator::Tiktoken;
        let heuristic = crate::llm::TokenEstimator::FourCharactersPerToken;
        let mut context = Context::new();
        assert_eq!(context.token_estimator(), tiktoken);
        context.push_user("plenty of words to estimate in this turn".to_string());
        for index in 0..context.messages().len() {
            assert_eq!(
                context.token_count(index),
                Some(estimate_message_tokens(
                    tiktoken,
                    &context.messages()[index]
                )),
                "counts start out under the default estimator"
            );
        }

        context.set_token_estimator(heuristic);
        for index in 0..context.messages().len() {
            assert_eq!(
                context.token_count(index),
                Some(estimate_message_tokens(
                    heuristic,
                    &context.messages()[index]
                )),
                "every cached count is recounted under the new estimator"
            );
        }

        let after_switch = context.approximate_token_count();
        context.set_token_estimator(heuristic);
        assert_eq!(
            context.approximate_token_count(),
            after_switch,
            "re-selecting the active estimator is a no-op"
        );
    }

    #[test]
    fn context_tiers_order_identity_instructions_and_conversation() {
        let mut context = Context::new();
        assert_eq!(context.tier(0), Some(MessageTier::System));
        assert_eq!(context.tier(1), Some(MessageTier::InitialContext));

        context.push_user("first question".to_string());
        assert_eq!(context.tier(2), Some(MessageTier::Recent));

        // Instructions injected later (a child briefing, the iteration
        // budget) stay stable `InitialContext` wherever they land.
        context.push_system("work within the sandbox".to_string());
        assert_eq!(context.tier(3), Some(MessageTier::InitialContext));
        assert_eq!(context.tier(4), None);

        assert_eq!(
            context.model_messages(),
            context.messages(),
            "without a compaction snapshot the model view is the whole transcript"
        );
    }

    #[test]
    fn hydration_populates_tiers_and_counts_for_replayed_messages() {
        let mut context = Context::new();
        context.hydrate(vec![
            Message::User {
                content: "a stored turn".to_string(),
                kind: UserMessageKind::Explicit,
            },
            Message::System {
                content: "stored instructions".to_string(),
            },
        ]);

        assert_eq!(context.tier(2), Some(MessageTier::Recent));
        assert_eq!(context.tier(3), Some(MessageTier::InitialContext));
        for index in 0..context.messages().len() {
            let expected =
                estimate_message_tokens(context.token_estimator(), &context.messages()[index]);
            assert_eq!(
                context.token_count(index),
                Some(expected),
                "replayed messages are counted too, for message {index}"
            );
        }
        assert!(context.approximate_token_count() > 0);
    }

    #[test]
    fn estimated_usage_ratio_uses_the_active_engines_effective_context_length() {
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        context.push_user("0123456789abcdef".to_string());

        let configured = EngineConfig {
            provider_connection: "ollama".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: Some(64),
            api_base_url: None,
            api_key: None,
            reasoning: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };
        assert_eq!(configured.effective_context_length(None), 64);
        let unconfigured = EngineConfig {
            context_length: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
            ..configured.clone()
        };
        assert_eq!(
            unconfigured.effective_context_length(Some(1024)),
            1024,
            "the provider catalog supplies the length when config does not"
        );
        assert_eq!(
            unconfigured.effective_context_length(None),
            crate::llm::DEFAULT_CONTEXT_LENGTH,
            "the built-in default is the last resort"
        );

        let tokens = context.approximate_token_count();
        let ratio = context.estimated_usage_ratio(configured.effective_context_length(None));
        assert!(
            (ratio - tokens as f64 / 64.0).abs() < f64::EPSILON,
            "usage is measured against the effective window: {ratio}"
        );
        assert_eq!(context.estimated_usage_ratio(0), 0.0);
    }

    /// Compact-engine stand-in that records every request it receives and
    /// either answers with a canned summary or fails the stream.
    struct ScriptedSummaryProvider {
        recorded: Arc<Mutex<Vec<Vec<Message>>>>,
        fail: bool,
    }

    impl crate::llm::Provider for ScriptedSummaryProvider {
        fn name(&self) -> &str {
            "scripted-summary"
        }

        fn list_models(
            &self,
        ) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>> {
            Box::pin(async { Ok(vec![]) })
        }

        fn chat<'a>(
            &'a self,
            messages: Vec<Message>,
            _tools: Vec<crate::llm::ToolSchema>,
            _cancel: CancellationToken,
        ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            self.recorded
                .lock()
                .expect("summary recording mutex is not poisoned")
                .push(messages);
            let fail = self.fail;
            Box::pin(async move {
                let (tx, rx) = mpsc::channel(4);
                tokio::spawn(async move {
                    if fail {
                        let _ = tx
                            .send(StreamEvent::Error(anyhow::anyhow!(
                                "compact engine unavailable"
                            )))
                            .await;
                    } else {
                        let _ = tx
                            .send(StreamEvent::Text(
                                "Decided to keep the transcript in the store.".to_string(),
                            ))
                            .await;
                        let _ = tx.send(StreamEvent::Done).await;
                    }
                });
                Ok(rx)
            })
        }
    }

    /// Drain `ui_rx` until `predicate` matches, then return everything read.
    ///
    /// Draining is mandatory (see `docs/known-test-issues.md`): the loop parks
    /// on `send().await` once the bounded update channel fills, which would
    /// stall both the turn and the compaction it is meant to trigger.
    async fn collect_updates_until<F>(
        ui_rx: &mut mpsc::Receiver<UiUpdate>,
        mut predicate: F,
    ) -> Vec<UiUpdate>
    where
        F: FnMut(&UiUpdate) -> bool,
    {
        let mut collected: Vec<UiUpdate> = Vec::new();
        for _ in 0..400 {
            while let Ok(update) = ui_rx.try_recv() {
                let matched = predicate(&update);
                collected.push(update);
                if matched {
                    return collected;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("condition not reached; collected {collected:?}");
    }

    /// Let already-spawned background tasks (the summary request and the
    /// loop's handling of its outcome) run to completion under paused time.
    async fn settle_background_tasks() {
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn compaction_test_tools() -> Vec<ToolSchema> {
        tools::default_registry(test_sandbox(), tools::ToolRegistryConfig::default()).schemas()
    }

    #[test]
    fn tool_schema_budget_reuses_counts_without_retokenizing() {
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        context.push_user("A bounded task.".to_string());
        let tools = compaction_test_tools();
        let expected_budget = context.available_result_token_budget(&tools, 64_000);
        let expected_tokens = context.model_view_token_count() + tool_schema_token_count(&tools);
        let tokenizer = tiktoken_rs::cl100k_base_singleton();
        let tokenizer_lock = tokenizer.lock();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let engine = test_engine();
            assert_eq!(
                context.estimated_model_tokens(&tools, engine.as_ref()),
                expected_tokens
            );
            assert!(context
                .summary_boundary_for_model_target(64_000, &tools, engine.as_ref())
                .is_none());
            assert!(
                context
                    .recover_model_view(64_000, 64_000, &tools)
                    .fits_ceiling
            );
            let budget = context.available_result_token_budget(&tools, 64_000);
            sender.send(budget).expect("send budget");
        });

        let result = receiver.recv_timeout(Duration::from_secs(1));
        drop(tokenizer_lock);
        worker.join().expect("budget worker");
        assert_eq!(
            result.expect("unchanged schemas must not wait for the tokenizer"),
            expected_budget
        );
    }

    #[test]
    fn tool_schema_budget_refreshes_when_definitions_change() {
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        let engine = test_engine();
        let mut tools = vec![ToolSchema {
            name: "read".to_string(),
            description: "Read a file.".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let check_estimate = |tools: &[ToolSchema]| {
            let expected_tokens = context.model_view_token_count() + tool_schema_token_count(tools);
            assert_eq!(
                context.estimated_model_tokens(tools, engine.as_ref()),
                expected_tokens
            );
            assert_eq!(
                context.available_result_token_budget(tools, 64_000),
                compaction::safe_request_ceiling(64_000).saturating_sub(expected_tokens)
            );
        };

        check_estimate(&tools);
        tools[0].name = "a different tool name ".repeat(10);
        check_estimate(&tools);
        tools[0].description = "A changed description with more details. ".repeat(20);
        check_estimate(&tools);
        tools[0].parameters = serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string", "description": "A file path."}},
        });
        check_estimate(&tools);
        let parameters = tools[0].parameters.as_object_mut().expect("parameters");
        let parameter_type = parameters.shift_remove("type").expect("type");
        parameters.insert("type".to_string(), parameter_type);
        check_estimate(&tools);
        assert_eq!(
            context
                .tool_schema_token_cache
                .lock()
                .expect("tool schema cache")
                .inputs[2],
            tools[0].parameters.to_string(),
            "JSON field order is part of the tokenizer input"
        );
        tools.push(tools[0].clone());
        check_estimate(&tools);
        tools.remove(0);
        check_estimate(&tools);
        check_estimate(&[]);
        check_estimate(&tools);
    }

    #[test]
    fn cached_tool_schema_budget_tracks_message_and_window_changes() {
        let mut context = Context::new();
        let tools = compaction_test_tools();
        let initial_budget = context.available_result_token_budget(&tools, 64_000);
        context.push_user("Additional context ".repeat(100));
        let updated_budget = context.available_result_token_budget(&tools, 64_000);
        assert!(updated_budget < initial_budget);
        let smaller_window = 32_000;
        assert_eq!(
            context.available_result_token_budget(&tools, smaller_window),
            compaction::safe_request_ceiling(smaller_window)
                .saturating_sub(context.model_view_token_count() + tool_schema_token_count(&tools))
        );
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        assert_eq!(
            context.available_result_token_budget(&tools, smaller_window),
            compaction::safe_request_ceiling(smaller_window)
                .saturating_sub(context.model_view_token_count() + tool_schema_token_count(&tools))
        );
    }

    /// Build the first turn of a conversation and pick a context window in
    /// which it sits inside the soft-threshold band (`≈80%`), with the
    /// rebuilt system prompts alone well under the compaction target so the
    /// drop fallback has somewhere to go.
    fn first_turn_window(prompt: &str, target_usage: f64) -> usize {
        let mut probe = Context::new();
        probe.set_token_estimator(crate::llm::TokenEstimator::Tiktoken);
        let tools = compaction_test_tools();
        let engine = test_engine();
        let system_tokens = probe.estimated_model_tokens(&tools, engine.as_ref());
        probe.push_user(prompt.to_string());
        let total_tokens = probe.estimated_model_tokens(&tools, engine.as_ref());
        assert!(
            total_tokens * 10 >= system_tokens * 19,
            "the prompt must carry enough tokens that the system prompts \
             alone fit the compaction target: system={system_tokens} \
             total={total_tokens}"
        );
        let context_length = (total_tokens as f64 / target_usage) as usize;
        assert!(
            (system_tokens as f64) <= 0.5 * context_length as f64,
            "the system prompts alone must fit the compaction target"
        );
        context_length
    }

    /// Grow a prompt until it carries enough tokens for
    /// [`first_turn_window`]'s budget assertions.
    fn sized_prompt() -> String {
        let tools = compaction_test_tools();
        let engine = test_engine();
        let mut prompt = "note ".repeat(64);
        loop {
            let mut probe = Context::new();
            probe.set_token_estimator(crate::llm::TokenEstimator::Tiktoken);
            let system_tokens = probe.estimated_model_tokens(&tools, engine.as_ref());
            probe.push_user(prompt.clone());
            if probe.estimated_model_tokens(&tools, engine.as_ref()) * 10 >= system_tokens * 19 {
                return prompt;
            }
            prompt = prompt.repeat(2);
            assert!(prompt.len() < 4_000_000, "prompt sizing must converge");
        }
    }

    #[test]
    fn model_projections_restore_exactly_on_resume_and_clone_with_rebuilt_systems() {
        crate::test_support::run_locked(async {
            let directory = tempfile::tempdir().expect("directory");
            let chats = directory.path().join("chats");
            let session_id = Uuid::new_v4();
            let store = Store::create(&chats, directory.path(), session_id, "mock", "model", None)
                .expect("store");
            let mut context = Context::new();
            context.set_store(store.clone());
            context.push_user("keep this exact task".to_string());
            let id = Uuid::new_v4();
            context.push_assistant_tool_call(
                String::new(),
                AssistantStatus::Complete,
                vec![ToolCallRequest {
                    id,
                    name: "read".to_string(),
                    arguments: Value::Null,
                    persisted_title_arguments: Vec::new(),
                }],
            );
            context.push_tool_error(
                id,
                "read".to_string(),
                "long error ".repeat(4_000),
                Some(&"partial output ".repeat(4_000)),
                Duration::ZERO,
                Vec::new(),
            );
            let report = context.recover_model_view(2_000, 4_000, &[]);
            assert!(report.fits_ceiling);
            context.persist_model_projection();
            let boundary = context.model_view_start();
            context.limit_last_model_message(128);
            context.persist_model_projection();
            assert_eq!(context.model_view_start(), boundary);
            store.flush().await;
            let expected = context.model_messages()[context.leading_system_len()..].to_vec();
            let canonical = context.messages()[context.leading_system_len()..].to_vec();
            let state = Store::read_session_state(store.path()).expect("state");
            let mut stale = state.model_projection.expect("projection");
            assert_eq!(stale.projection_revision, 2);
            stale.projection_revision = 1;
            stale.transcript_start += 100;
            store.record_model_projection(stale);
            store.flush().await;
            drop(context);
            drop(store);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let resumed = Store::resume(&chats, directory.path(), session_id).expect("resume");
            let mut replay = Context::new();
            replay.push_system("additional rebuilt instructions".to_string());
            replay.set_store(resumed.clone());
            assert_eq!(
                &replay.model_messages()[replay.leading_system_len()..],
                expected
            );
            assert_eq!(&replay.messages()[replay.leading_system_len()..], canonical);
            drop(replay);
            drop(resumed);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let cloned =
                Store::clone_session(&chats, directory.path(), session_id, None).expect("clone");
            let mut clone_context = Context::new();
            clone_context.set_store(cloned);
            assert_eq!(
                &clone_context.model_messages()[clone_context.leading_system_len()..],
                expected
            );
            assert_eq!(
                &clone_context.messages()[clone_context.leading_system_len()..],
                canonical
            );
        });
    }

    #[test]
    fn invalid_projections_and_late_summaries_cannot_resurrect_or_break_tool_groups() {
        let mut context = Context::new();
        context.push_user("continue the exact task".to_string());
        let id = Uuid::new_v4();
        context.push_assistant_tool_call(
            String::new(),
            AssistantStatus::Complete,
            vec![ToolCallRequest {
                id,
                name: "read".to_string(),
                arguments: Value::Null,
                persisted_title_arguments: Vec::new(),
            }],
        );
        context.push_tool_error(
            id,
            "read".to_string(),
            "failure ".repeat(4_000),
            None,
            Duration::ZERO,
            Vec::new(),
        );
        let original = context.model_messages();
        context.restore_model_projection(compaction::ModelProjectionSnapshot {
            projection_revision: 9,
            transcript_start: 0,
            message_overrides: vec![(
                2,
                Message::ToolError {
                    id: Uuid::new_v4(),
                    name: "read".to_string(),
                    error: "wrong correlation".to_string(),
                    duration: Duration::ZERO,
                    content_sections: Vec::new(),
                },
            )],
            summary_override: None,
            handoff: None,
            anchor: None,
        });
        assert_eq!(context.model_messages(), original);
        assert_eq!(context.projection_revision, 9);
        context.restore_model_projection(compaction::ModelProjectionSnapshot {
            projection_revision: 10,
            transcript_start: 0,
            message_overrides: Vec::new(),
            summary_override: None,
            handoff: Some(Message::System {
                content: "invalid recovery".to_string(),
            }),
            anchor: None,
        });
        assert_eq!(context.model_messages(), original);
        let boundary = context.leading_system_len();
        assert!(context.recover_model_view(1_000, 2_000, &[]).fits_ceiling);
        let recovered = context.model_messages();
        assert!(!context.set_compaction(compaction::CompactionSnapshot {
            boundary,
            summary: "late, obsolete summary".to_string(),
        }));
        assert_eq!(context.model_messages(), recovered);
        assert!(orphaned_tool_call_ids(&recovered).is_empty());
    }

    #[test]
    fn recovery_budgets_markers_utf8_envelopes_and_a_shrunken_window() {
        let mut context = Context::new();
        context.push_user("retain the task".to_string());
        let id = Uuid::new_v4();
        context.push_assistant_tool_call(
            String::new(),
            AssistantStatus::Complete,
            vec![ToolCallRequest {
                id,
                name: "bash".to_string(),
                arguments: Value::Null,
                persisted_title_arguments: Vec::new(),
            }],
        );
        context.push_tool_error(
            id,
            "bash".to_string(),
            "失败 αβ \n\\\" ".repeat(4_000),
            Some(&"partial \nαβ ".repeat(4_000)),
            Duration::ZERO,
            Vec::new(),
        );
        let canonical = context.messages().to_vec();
        context.limit_last_model_message(256);
        let projection = context.model_messages().last().expect("error").clone();
        assert!(estimate_message_tokens(context.estimator, &projection) <= 256);
        assert!(
            matches!(projection, Message::ToolError { error, .. } if error.contains(compaction::recovery::RESULT_OMISSION_MARKER))
        );
        assert_eq!(context.messages(), canonical);
        assert!(context.available_result_token_budget(&[], 64_000) > 0);
        assert_eq!(context.available_result_token_budget(&[], 100), 0);
        let report = context.recover_model_view(2_000, 4_000, &[]);
        assert!(report.fits_ceiling);
        assert!(report.stage <= 5);
        assert_eq!(
            report.estimated_tokens_after,
            context.model_view_token_count()
        );
        let mut schemas = compaction_test_tools();
        schemas[0].description = "huge schema ".repeat(10_000);
        assert_eq!(context.available_result_token_budget(&schemas, 4_000), 0);
        assert!(
            !context
                .recover_model_view(2_000, 4_000, &schemas)
                .fits_ceiling
        );
    }

    #[test]
    fn each_recovery_stage_is_measured_without_shortening_a_ceiling_safe_task() {
        let make_context = || {
            let mut context = Context::new();
            context.messages.clear();
            context.entries.clear();
            context.push_system("instructions".to_string());
            context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
            context
        };
        let append_result = |context: &mut Context, text: String| {
            let id = Uuid::new_v4();
            context.push_assistant_tool_call(
                String::new(),
                AssistantStatus::Complete,
                vec![ToolCallRequest {
                    id,
                    name: "read".to_string(),
                    arguments: Value::Null,
                    persisted_title_arguments: Vec::new(),
                }],
            );
            context.push_tool_error(
                id,
                "read".to_string(),
                text,
                None,
                Duration::ZERO,
                Vec::new(),
            );
        };
        let mut unchanged = make_context();
        unchanged.push_user("task".to_string());
        assert_eq!(unchanged.recover_model_view(1_000, 2_000, &[]).stage, 0);

        let mut old_results = make_context();
        old_results.push_user("task".to_string());
        append_result(&mut old_results, "older output ".repeat(3_000));
        append_result(&mut old_results, "newest result".to_string());
        let report = old_results.recover_model_view(1_000, 2_000, &[]);
        assert_eq!(report.stage, 1);
        assert_eq!(report.clipped_results, 1);
        assert!(report.fits_target && report.fits_ceiling);

        let mut old_groups = make_context();
        old_groups.push_user("task".to_string());
        old_groups.push_assistant("older answer ".repeat(3_000), AssistantStatus::Complete);
        append_result(&mut old_groups, "newest result".to_string());
        let report = old_groups.recover_model_view(1_000, 2_000, &[]);
        assert_eq!(report.stage, 2);
        assert!(report.fits_target && report.fits_ceiling);
        assert!(old_groups.recovery.is_some());
        assert!(orphaned_tool_call_ids(&old_groups.model_messages()).is_empty());

        let mut retained_results = make_context();
        retained_results.push_user("task".to_string());
        append_result(&mut retained_results, "newest output ".repeat(3_000));
        let report = retained_results.recover_model_view(1_000, 2_000, &[]);
        assert_eq!(report.stage, 3);
        assert!(report.fits_ceiling);

        let mut summary = make_context();
        summary.push_user("earlier task".to_string());
        assert!(summary.set_compaction(compaction::CompactionSnapshot {
            boundary: summary.messages.len(),
            summary: "older summary ".repeat(3_000),
        }));
        let task = "retain this request ".repeat(100);
        summary.push_user(task.clone());
        let report = summary.recover_model_view(128, 2_000, &[]);
        assert_eq!(report.stage, 4);
        assert!(!report.fits_target && report.fits_ceiling);
        assert!(summary
            .summary_override
            .as_ref()
            .expect("shortened summary")
            .contains(compaction::recovery::SUMMARY_OMISSION_MARKER));
        assert!(summary.model_messages().iter().any(|message| matches!(
            message, Message::User { content, kind: UserMessageKind::Explicit } if content == &task
        )));

        let mut oversized_task = make_context();
        oversized_task.push_user("task ".repeat(4_000));
        let report = oversized_task.recover_model_view(1_000, 2_000, &[]);
        assert_eq!(report.stage, 5);
        assert!(report.fits_ceiling);
        assert_eq!(
            report.estimated_tokens_after,
            oversized_task.model_view_token_count()
        );
    }

    #[test]
    fn an_oversized_new_task_shortens_the_newest_anchor_not_the_previous_one() {
        let mut context = Context::new();
        context.push_user("original task".to_string());
        context.push_assistant("older context ".repeat(5_000), AssistantStatus::Complete);
        context.recover_model_view(2_000, 4_000, &[]);
        context.push_user(format!("new task {}", "request ".repeat(10_000)));
        let report = context.recover_model_view(2_000, 4_000, &[]);
        assert!(report.fits_ceiling);
        assert!(context.model_messages().iter().any(|message| matches!(
            message, Message::User { content, kind: UserMessageKind::Explicit }
                if content.starts_with("new task") && content.contains(compaction::recovery::REQUEST_OMISSION_MARKER)
        )));
    }

    #[tokio::test(start_paused = true)]
    async fn a_completed_turn_above_the_soft_threshold_compacts_the_model_view() {
        let prompt = sized_prompt();
        let context_length = first_turn_window(&prompt, 0.75);
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let active_config = EngineConfig {
            provider_connection: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: Some(context_length),
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };
        let active: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::new(
                vec!["recorded answer ".repeat(100)],
                Duration::ZERO,
            )),
            active_config.clone(),
        ));
        let compact_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(ScriptedSummaryProvider {
                recorded: Arc::clone(&recorded),
                fail: false,
            }),
            active_config,
        ));
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let store = crate::store::Store::create(
            &temporary_directory.path().join("chats"),
            temporary_directory.path(),
            Uuid::new_v4(),
            "mock",
            "test-model",
            None,
        )
        .expect("store created");

        let (agent_loop, mut ui_rx) = AgentLoop::new(
            active,
            Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            )),
        );
        let mut agent_loop = agent_loop
            .with_compaction_engine(compact_engine)
            .with_store(store.clone());
        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt(prompt.clone()))
            .await;
        let _updates =
            collect_updates_until(&mut ui_rx, |update| matches!(update, UiUpdate::StreamDone))
                .await;
        settle_background_tasks().await;
        let post_compaction_usage =
            std::iter::from_fn(|| ui_rx.try_recv().ok()).find_map(|update| match update {
                UiUpdate::ContextUsage {
                    estimated_tokens,
                    context_length,
                    source: ContextUsageSource::ModelView,
                } => Some((estimated_tokens, context_length)),
                _ => None,
            });
        assert!(
            post_compaction_usage.is_some(),
            "applying the background summary refreshes the footer with the model-view estimate"
        );

        let requests = recorded
            .lock()
            .expect("summary recording mutex is not poisoned")
            .clone();
        assert_eq!(
            requests.len(),
            1,
            "the completed turn above the soft threshold starts exactly one background summary"
        );
        let request = &requests[0];
        assert!(
            matches!(
                request.first(),
                Some(Message::System { content })
                    if content.starts_with(compaction::COMPACTION_SUMMARY_PROMPT)
                        && content.contains("is the signal to produce the summary")
            ),
            "the summarizer gets its framing instruction first: {request:?}"
        );
        assert!(
            request.iter().any(|message| matches!(
                message,
                Message::User { content, .. } if content.as_str() == prompt.as_str()
            )),
            "the summarized prefix contains the original turn: {request:?}"
        );

        drop(agent_loop);
        let context = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("loop returns")
            .expect("loop task ok")
            .expect("loop returns a context");

        assert!(
            context.compaction().is_some(),
            "the background summary was applied at a safe boundary"
        );
        let snapshot = context.compaction().expect("compaction applied");
        assert_eq!(
            snapshot.summary,
            "Decided to keep the transcript in the store."
        );
        let model_messages = context.model_messages();
        let leading_system_len = context.leading_system_len();
        assert!(
            snapshot.boundary > leading_system_len && snapshot.boundary <= context.messages().len(),
            "the boundary sits inside the transcript: {}",
            snapshot.boundary
        );
        assert!(
            model_messages.iter().any(|message| matches!(
                message,
                Message::User { content, .. } if content.as_str() == prompt.as_str()
            )),
            "the explicit task anchor survives summarization: {model_messages:?}"
        );
        assert_eq!(
            &model_messages[..leading_system_len],
            &context.messages()[..leading_system_len],
            "the leading system instructions stay verbatim"
        );
        assert!(
            matches!(
                &model_messages[leading_system_len],
                Message::User { content, .. }
                    if content.starts_with(compaction::COMPACTION_SUMMARY_MARKER)
            ),
            "the summary is projected as a marked user message: {model_messages:?}"
        );
        assert_eq!(
            &model_messages[leading_system_len + 2..],
            &context.messages()[snapshot.boundary..],
            "the tail after the boundary stays verbatim"
        );
        assert!(
            context.messages().iter().any(|message| matches!(
                message,
                Message::User { content, .. } if content.as_str() == prompt.as_str()
            )),
            "the transcript keeps every original message"
        );

        store.flush().await;
        let events = crate::store::Store::read_events(store.path()).expect("events readable");
        let compactions: Vec<(usize, String)> = events
            .iter()
            .filter_map(|event| match event {
                crate::store::StoreEvent::Compaction {
                    boundary, summary, ..
                } => Some((*boundary, summary.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            compactions,
            vec![(
                snapshot.boundary,
                "Decided to keep the transcript in the store.".to_string()
            )],
            "the applied snapshot is appended to the session file"
        );
        assert_eq!(
            crate::store::Store::read_messages(store.path())
                .expect("messages readable")
                .iter()
                .filter(|message| matches!(message, Message::User { .. }))
                .count(),
            1,
            "restoring or applying the snapshot never writes the user turn twice"
        );

        let mut announced_boundary: Option<(usize, usize)> = None;
        while let Ok(update) = ui_rx.try_recv() {
            if let UiUpdate::CompactionApplied {
                run_id,
                entry_index,
                summarized_messages,
            } = update
            {
                assert_eq!(run_id, None, "root compaction retains its identity");
                announced_boundary = Some((entry_index, summarized_messages));
            }
        }
        let (entry_index, summarized_messages) = announced_boundary
            .expect("applying a snapshot announces the marker position to the TUI");
        assert_eq!(
            summarized_messages,
            snapshot.boundary - leading_system_len,
            "the marker counts transcript messages, not the rebuilt system prompts"
        );
        let store_messages =
            crate::store::Store::read_messages(store.path()).expect("messages readable");
        assert_eq!(
            entry_index,
            crate::tui_reducer_state::project_messages(&store_messages[..summarized_messages])
                .len(),
            "the emitted index matches what a resume-time replay computes"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_background_summary_falls_back_to_dropping_at_the_next_boundary() {
        let prompt = sized_prompt();
        let context_length = first_turn_window(&prompt, 0.75);
        let active_config = EngineConfig {
            provider_connection: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: Some(context_length),
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let active: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::new(
                vec!["recorded answer ".repeat(100)],
                Duration::ZERO,
            )),
            active_config.clone(),
        ));
        let compact_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(ScriptedSummaryProvider {
                recorded: Arc::clone(&recorded),
                fail: true,
            }),
            active_config,
        ));

        let (agent_loop, mut ui_rx) = AgentLoop::new(
            active,
            Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            )),
        );
        let mut agent_loop = agent_loop.with_compaction_engine(compact_engine);
        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt(prompt.clone()))
            .await;
        collect_updates_until(&mut ui_rx, |update| matches!(update, UiUpdate::StreamDone)).await;
        settle_background_tasks().await;

        assert_eq!(
            recorded
                .lock()
                .expect("summary recording mutex is not poisoned")
                .len(),
            1,
            "the summary attempt is made before the fallback runs"
        );

        drop(agent_loop);
        let context = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("loop returns")
            .expect("loop task ok")
            .expect("loop returns a context");

        assert!(
            context.compaction().is_none(),
            "a failed summary never becomes a snapshot"
        );
        assert!(
            context.model_messages().iter().any(|message| matches!(
                message,
                Message::User { content, .. }
                    if content == &prompt || content.contains(compaction::recovery::REQUEST_OMISSION_MARKER)
            )),
            "failed summarization preserves a task anchor instead of a system-only view"
        );
        assert!(
            matches!(
                context.model_messages().first(),
                Some(Message::System { .. })
            ),
            "the leading system instructions are never dropped"
        );
        assert!(
            context.messages().iter().any(|message| matches!(
                message,
                Message::User { content, .. } if content.as_str() == prompt.as_str()
            )),
            "the transcript keeps every original message"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_above_the_hard_threshold_drops_without_starting_a_summary() {
        let prompt = sized_prompt();
        let context_length = first_turn_window(&prompt, 0.95);
        let active_config = EngineConfig {
            provider_connection: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: Some(context_length),
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let active: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::new(vec!["ok".to_string()], Duration::ZERO)),
            active_config.clone(),
        ));
        let compact_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(ScriptedSummaryProvider {
                recorded: Arc::clone(&recorded),
                fail: false,
            }),
            active_config,
        ));

        let (agent_loop, mut ui_rx) = AgentLoop::new(
            active,
            Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            )),
        );
        let mut agent_loop = agent_loop.with_compaction_engine(compact_engine);
        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt(prompt.clone()))
            .await;
        let mut updates = collect_updates_until(&mut ui_rx, |update| {
            matches!(update, UiUpdate::Notice { text, .. }
                if text.contains("lossily"))
        })
        .await;
        settle_background_tasks().await;
        updates.extend(std::iter::from_fn(|| ui_rx.try_recv().ok()));

        assert!(
            recorded
                .lock()
                .expect("summary recording mutex is not poisoned")
                .is_empty(),
            "above the hard threshold the loop drops synchronously instead of waiting \
             for a background summary"
        );
        assert!(
            updates
                .iter()
                .any(|update| matches!(update, UiUpdate::Notice { text, .. }
                if text.contains("lossily"))),
            "the synchronous fallback is announced"
        );
        assert_eq!(
            updates
                .iter()
                .filter(|update| matches!(update, UiUpdate::StreamDone))
                .count(),
            1,
            "compaction does not start another conversation request"
        );
        drop(agent_loop);
        let context = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("loop returns")
            .expect("loop task ok")
            .expect("loop returns a context");

        assert!(context.compaction().is_none(), "no summary was produced");
        assert!(
            context.model_messages().iter().any(|message| matches!(
                message,
                Message::User { content, .. }
                    if content.contains(compaction::recovery::REQUEST_OMISSION_MARKER)
            )),
            "the oversized task is shortened with disclosure, not removed"
        );
        let active_tools = compaction_test_tools();
        let active_engine = test_engine();
        let trimmed_usage = context.estimated_model_tokens(&active_tools, active_engine.as_ref())
            as f64
            / context_length as f64;
        assert!(
            (trimmed_usage - 0.5).abs() < 0.4,
            "the trimmed view is near the compaction target: {trimmed_usage}"
        );
        assert!(
            context.messages().iter().any(|message| matches!(
                message,
                Message::User { content, .. } if content.as_str() == prompt.as_str()
            )),
            "the transcript keeps every original message"
        );
    }

    #[test]
    fn set_compaction_replaces_the_prefix_and_only_moves_forward() {
        let mut context = Context::new();
        context.push_user("first turn".to_string());
        context.push_assistant("first answer".to_string(), AssistantStatus::Complete);
        context.push_user("second turn".to_string());
        let transcript_len = context.messages().len();

        assert!(
            context.set_compaction(compaction::CompactionSnapshot {
                boundary: 3,
                summary: "the first turn, summarized".to_string(),
            }),
            "the first snapshot applies"
        );

        let model_messages = context.model_messages();
        let leading_system_len = context.leading_system_len();
        assert_eq!(
            model_messages.len(),
            leading_system_len + 1 + (transcript_len - 3),
            "system block + the summary + the verbatim tail after the boundary"
        );
        assert!(
            matches!(
                &model_messages[leading_system_len],
                Message::User { content, .. }
                    if content.starts_with(compaction::COMPACTION_SUMMARY_MARKER)
            ),
            "the summary is marked, not a system message: {model_messages:?}"
        );
        assert_eq!(
            &model_messages[leading_system_len + 1..],
            &context.messages()[3..],
            "the tail after the boundary is verbatim"
        );
        assert_eq!(
            context.messages().len(),
            transcript_len,
            "compaction never rewrites the transcript"
        );

        assert!(
            !context.set_compaction(compaction::CompactionSnapshot {
                boundary: 3,
                summary: "the same boundary again".to_string(),
            }),
            "an identical boundary must not supersede the applied snapshot"
        );
        assert!(
            !context.set_compaction(compaction::CompactionSnapshot {
                boundary: 2,
                summary: "an older boundary".to_string(),
            }),
            "boundaries only ever move forward"
        );
        assert_eq!(
            context.compaction().expect("snapshot kept").summary,
            "the first turn, summarized"
        );

        assert!(
            context.set_compaction(compaction::CompactionSnapshot {
                boundary: transcript_len,
                summary: "everything, summarized".to_string(),
            }),
            "a newer, larger boundary supersedes the applied one"
        );
        assert_eq!(
            context.model_messages().len(),
            leading_system_len + 2,
            "the whole prefix is represented by the summary and the preserved task anchor"
        );
    }

    #[test]
    fn drop_model_view_to_cuts_only_at_complete_turn_boundaries() {
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        let call_id = Uuid::new_v4();
        context.push_user("do the work".to_string());
        context.push_assistant_tool_call(
            String::new(),
            AssistantStatus::Complete,
            vec![ToolCallRequest {
                id: call_id,
                name: "read".to_string(),
                arguments: Value::Null,
                persisted_title_arguments: Vec::new(),
            }],
        );
        context.push_tool_result(ToolResult {
            id: call_id,
            name: "read".to_string(),
            result: "x".repeat(8_000),
            attachments: vec![],
            result_section_id: None,
            result_metadata: None,
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: Duration::ZERO,
        });
        context.push_user("and now this".to_string());

        let leading_system_len = context.leading_system_len();
        let system_tokens: usize = context.entries[..leading_system_len]
            .iter()
            .map(|entry| entry.token_count)
            .sum();
        let final_turn_tokens = context.entries[context.messages().len() - 1].token_count;
        // Room for the system prompts plus the final turn, but nowhere near
        // enough for the 8 000-character tool result that precedes it: the
        // only cut that fits must leave the result behind, never split it
        // from its call.
        let target_tokens = system_tokens + final_turn_tokens + 10;

        assert!(
            context.drop_model_view_to(target_tokens),
            "the target is reachable without the oversized result"
        );
        let model_messages = context.model_messages();
        assert_eq!(
            model_messages.len(),
            leading_system_len + 1,
            "the view keeps the system block and the final turn: {model_messages:?}"
        );
        assert_eq!(
            model_messages.last(),
            context.messages().last(),
            "the final turn stays verbatim"
        );
        assert_eq!(
            context.messages().len(),
            6,
            "the transcript keeps the dropped messages"
        );
        assert_eq!(
            context.model_view_token_count(),
            system_tokens + final_turn_tokens
        );

        assert!(
            !context.drop_model_view_to(1),
            "a target below the system prompts alone cannot be met"
        );
    }

    #[test]
    fn summary_boundary_advances_past_a_synchronously_dropped_prefix() {
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        context.push_user("old source ".repeat(2_000));
        context.push_assistant("old answer".to_string(), AssistantStatus::Complete);
        let dropped_boundary = context.messages().len();
        context.push_user("new source ".repeat(100));
        assert!(context.drop_model_view_to(context.model_view_token_count_from(dropped_boundary)));
        let boundary = context
            .summary_boundary_for_target(context.model_view_token_count() - 10)
            .expect("visible tail can be summarized");
        assert!(boundary > dropped_boundary);
    }

    #[test]
    fn summary_boundary_does_not_resummarize_an_empty_visible_tail() {
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut context = Context::new();
        context.push_user("old source".to_string());
        assert!(context.set_compaction(compaction::CompactionSnapshot {
            boundary: context.messages().len(),
            summary: "large summary ".repeat(100),
        }));
        assert_eq!(context.summary_boundary_for_target(1), None);
    }

    #[test]
    fn summary_boundary_for_target_never_cuts_a_tool_call_result_group() {
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        let call_id = Uuid::new_v4();
        context.push_user("do the work".to_string());
        context.push_assistant_tool_call(
            String::new(),
            AssistantStatus::Complete,
            vec![ToolCallRequest {
                id: call_id,
                name: "read".to_string(),
                arguments: Value::Null,
                persisted_title_arguments: Vec::new(),
            }],
        );
        context.push_tool_result(ToolResult {
            id: call_id,
            name: "read".to_string(),
            result: "y".repeat(8_000),
            attachments: vec![],
            result_section_id: None,
            result_metadata: None,
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: Duration::ZERO,
        });
        context.push_user("follow-up".to_string());

        // A target that the raw accumulation would reach in the middle of the
        // tool-call/result pair (right after the assistant message).
        let target_tokens = context
            .model_view_token_count()
            .saturating_sub(context.token_count(2).expect("result entry") + 100);

        let boundary = context
            .summary_boundary_for_target(target_tokens)
            .expect("a prefix exists to summarize");
        assert!(
            compaction::first_safe_boundary_from(context.messages(), boundary) == Some(boundary),
            "the chosen boundary {boundary} must be a complete-turn cut"
        );
        assert!(
            boundary >= 4,
            "the cut has to include the whole tool round, not just its call: {boundary}"
        );
    }

    /// Tool-call ids in `messages` whose result or error is not present —
    /// the pairing invariant every model view must satisfy.
    fn orphaned_tool_call_ids(messages: &[Message]) -> Vec<Uuid> {
        let mut open: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        for message in messages {
            match message {
                Message::Assistant { tool_calls, .. } => {
                    open.extend(tool_calls.iter().map(|tool_call| tool_call.id));
                }
                Message::ToolResult { value } => {
                    open.remove(&value.id);
                }
                Message::ToolError { id, .. } => {
                    open.remove(id);
                }
                _ => {}
            }
        }
        open.into_iter().collect()
    }

    #[tokio::test]
    async fn resuming_restores_the_latest_compaction_without_rewriting_the_session() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let chats_root = temporary_directory.path().join("chats");
        let session_id = Uuid::new_v4();
        let store = crate::store::Store::create(
            &chats_root,
            temporary_directory.path(),
            session_id,
            "ollama",
            "llama3",
            None,
        )
        .expect("store created");
        store.record_message(&Message::User {
            content: "first turn".to_string(),
            kind: UserMessageKind::Explicit,
        });
        store.record_message(&Message::Assistant {
            content: "first answer".to_string(),
            status: AssistantStatus::Complete,
            tool_calls: Vec::new(),
        });
        store.record_message(&Message::User {
            content: "second turn".to_string(),
            kind: UserMessageKind::Explicit,
        });
        // Absolute transcript index: the two rebuilt system prompts occupy
        // `[..2]` in the live context and again after a resume.
        store.record_compaction(4, 2, "first turn, summarized");
        store.flush().await;
        let path = store.path().to_path_buf();
        let before = std::fs::read(&path).expect("session file readable");

        let mut context = Context::new();
        context.set_store(store.clone());
        store.flush().await;
        let after = std::fs::read(&path).expect("session file readable");
        assert_eq!(
            before, after,
            "hydration plus snapshot restore must not append a second event"
        );

        assert_eq!(context.messages().len(), 5, "2 system + 3 stored messages");
        let snapshot = context.compaction().expect("snapshot restored");
        assert_eq!(snapshot.boundary, 4);
        assert_eq!(snapshot.summary, "first turn, summarized");

        let model_messages = context.model_messages();
        let leading_system_len = context.leading_system_len();
        assert_eq!(
            model_messages.len(),
            leading_system_len + 2,
            "system block + restored summary + tail containing the latest task"
        );
        assert!(
            matches!(
                &model_messages[leading_system_len],
                Message::User { content, .. }
                    if content.starts_with(compaction::COMPACTION_SUMMARY_MARKER)
            ),
            "the restored summary keeps the provider-safe projection: {model_messages:?}"
        );
        assert_eq!(
            &model_messages[leading_system_len + 1..],
            &context.messages()[4..],
            "the tail after the restored boundary is verbatim"
        );
        assert!(
            !model_messages.iter().any(|message| matches!(
                message,
                Message::User { content, .. } if content == "first turn"
            )),
            "the summarized prefix stays out of the model view"
        );
        assert!(
            context.messages().iter().any(|message| matches!(
                message,
                Message::User { content, .. } if content == "first turn"
            )),
            "the transcript keeps every original message"
        );
    }

    #[tokio::test]
    async fn resuming_a_snapshot_never_splits_a_tool_call_from_its_result() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let chats_root = temporary_directory.path().join("chats");
        let session_id = Uuid::new_v4();
        let store = crate::store::Store::create(
            &chats_root,
            temporary_directory.path(),
            session_id,
            "ollama",
            "llama3",
            None,
        )
        .expect("store created");
        let call_id = Uuid::new_v4();
        store.record_message(&Message::User {
            content: "do the work".to_string(),
            kind: UserMessageKind::Explicit,
        });
        store.record_message(&Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id: call_id,
                name: "read".to_string(),
                arguments: Value::Null,
                persisted_title_arguments: Vec::new(),
            }],
        });
        store.record_message(&Message::ToolResult {
            value: ToolResult {
                id: call_id,
                name: "read".to_string(),
                result: "file contents".to_string(),
                attachments: vec![],
                result_section_id: None,
                result_metadata: None,
                result_metadata_schema: None,
                summary: vec![],
                content_sections: vec![],
                duration: Duration::ZERO,
            },
        });
        store.record_message(&Message::User {
            content: "second turn".to_string(),
            kind: UserMessageKind::Explicit,
        });
        // Deliberately splits the round: the assistant tool call lands in the
        // summarized prefix while its result stays in the tail.
        store.record_compaction(4, 2, "the tool round, summarized");
        store.flush().await;

        let mut context = Context::new();
        context.set_store(store.clone());
        store.flush().await;

        let snapshot = context.compaction().expect("snapshot restored");
        assert_eq!(
            snapshot.boundary, 5,
            "the restored boundary snaps forward past the tool call's result"
        );
        let model_messages = context.model_messages();
        assert_eq!(
            orphaned_tool_call_ids(&model_messages),
            Vec::<Uuid>::new(),
            "the model view never carries a call without its result: {model_messages:?}"
        );
        assert_eq!(
            orphaned_tool_call_ids(context.messages()),
            Vec::<Uuid>::new(),
            "nor does the transcript"
        );
        assert_eq!(
            model_messages.last(),
            context.messages().last(),
            "the verbatim tail keeps the final turn"
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
            let _cancel = pending.execute(agent_tx, 0, None);
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
        let max_tool_iterations = DEFAULT_MAX_TOOL_USE_ITERATIONS;
        let mut clarify_started_at = None;
        let mut pending_steers = VecDeque::new();

        advance_tool_queue(
            &mut pending_tools,
            &mut context,
            &mut mode,
            &mut current_cancel,
            &mut current_tool_cancel,
            &mut iterations,
            max_tool_iterations,
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
            None,
            None,
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
        let max_tool_iterations = DEFAULT_MAX_TOOL_USE_ITERATIONS;
        let mut clarify_started_at = None;
        let mut pending_steers = VecDeque::new();

        advance_tool_queue(
            &mut pending_tools,
            &mut context,
            &mut mode,
            &mut current_cancel,
            &mut current_tool_cancel,
            &mut iterations,
            max_tool_iterations,
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
            None,
            None,
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
        let max_tool_iterations = DEFAULT_MAX_TOOL_USE_ITERATIONS;
        let mut clarify_started_at = None;
        let mut pending_steers = VecDeque::from(["steered while tool ran".to_string()]);

        advance_tool_queue(
            &mut pending_tools,
            &mut context,
            &mut mode,
            &mut current_cancel,
            &mut current_tool_cancel,
            &mut iterations,
            max_tool_iterations,
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
            None,
            None,
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

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
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
            let completion_notice = context
                .model_messages()
                .into_iter()
                .find_map(|message| match message {
                    Message::User {
                        content,
                        kind: UserMessageKind::ImplicitToolNote,
                    } if content.contains("Subagent completion event") => Some(content),
                    _ => None,
                })
                .expect("completion guidance reaches the root model");
            assert!(completion_notice.contains("self-contained answer"));
            assert!(completion_notice.contains("Preserve the user's requested format"));
            assert!(completion_notice.contains(
                "Do not assume tool results or subagent responses are visible to the user"
            ));
            assert!(!completion_notice.contains("bubbles"));
            assert!(!completion_notice.contains("do not repeat or quote it"));

            // Drain the ui updates; just ensure the channel didn't panic/drop.
            while ui_rx.try_recv().is_ok() {}
        });
    }

    #[test]
    fn oversized_child_result_preserves_canonical_output_and_bounds_parent_projection() {
        crate::test_support::run_locked(async {
            use crate::agent::tools::{default_registry, ToolRegistryConfig};
            use crate::child_agent_manager::{ChildAgentManager, FinishResult};

            let root_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::with_script(
                    vec![MockEvent::Text("I will combine the results.".to_string())],
                    Duration::ZERO,
                )),
                EngineConfig {
                    provider_connection: "mock".to_string(),
                    model: "root-model".to_string(),
                    temperature: None,
                    context_length: Some(20_000),
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                    compact_target_ratio: 0.9,
                    soft_compaction_threshold: 0.95,
                    hard_compaction_threshold: 0.99,
                },
            ));
            let recorded_summaries = Arc::new(Mutex::new(Vec::new()));
            let summary_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(ScriptedSummaryProvider {
                    recorded: Arc::clone(&recorded_summaries),
                    fail: false,
                }),
                EngineConfig {
                    provider_connection: "mock".to_string(),
                    model: "summary-model".to_string(),
                    temperature: None,
                    context_length: Some(16_000),
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
                },
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
            let (_parent_subagent_ui_tx, parent_subagent_ui_rx) = mpsc::channel(8);
            let (agent_loop, _ui_rx) = AgentLoop::new(
                root_engine,
                Arc::new(default_registry(
                    test_sandbox(),
                    ToolRegistryConfig::default(),
                )),
            );
            let mut agent_loop = agent_loop
                .with_child_agent_manager(manager, finish_rx, parent_subagent_ui_rx)
                .with_compaction_engine(summary_engine);
            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt("Use the child findings.".to_string()))
                .await;

            tokio::time::sleep(Duration::from_millis(20)).await;
            finish_tx
                .send(FinishResult {
                    run_id: Uuid::new_v4(),
                    call_id: Uuid::new_v4(),
                    origin: crate::child_agent_manager::FinishResultOrigin::Agent,
                    status: "success".to_string(),
                    result: Some("verbose child report ".repeat(6_000)),
                    error: None,
                    sources: vec!["src/store.rs".to_string()],
                })
                .await
                .expect("parent completion receiver remains open");
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(agent_loop);
            let context = tokio::time::timeout(Duration::from_secs(2), handle)
                .await
                .expect("root loop stops")
                .expect("root task joins")
                .expect("root loop succeeds");

            let completion_notice = context.messages().iter().find_map(|message| match message {
                Message::User {
                    content,
                    kind: UserMessageKind::ImplicitToolNote,
                } if content.contains("Subagent completion event") => Some(content),
                _ => None,
            });
            let completion_notice = completion_notice.expect("completion event reaches parent");
            assert!(completion_notice.contains("verbose child report"));
            let projected_notice = context
                .model_messages()
                .into_iter()
                .find_map(|message| match message {
                    Message::User {
                        content,
                        kind: UserMessageKind::ImplicitToolNote,
                    } if content.contains("Subagent completion event") => Some(content),
                    _ => None,
                })
                .expect("projected child event");
            let event: Value = serde_json::from_str(
                projected_notice
                    .lines()
                    .find(|line| line.starts_with('{'))
                    .expect("structured event"),
            )
            .expect("valid completion JSON");
            assert!(event["result"]
                .as_str()
                .expect("result")
                .contains(compaction::recovery::RESULT_OMISSION_MARKER));
            assert_eq!(event["result_reduced"], true);
            assert!(projected_notice.len() < completion_notice.len());
            for notice in [completion_notice, &projected_notice] {
                assert!(notice.contains("self-contained answer"));
                assert!(notice.contains("Preserve the user's requested format"));
                assert!(notice.contains(
                    "Do not assume tool results or subagent responses are visible to the user"
                ));
                assert!(!notice.contains("bubbles"));
                assert!(!notice.contains("do not repeat or quote it"));
            }
            assert!(
                recorded_summaries
                    .lock()
                    .expect("summary requests")
                    .is_empty(),
                "report reduction does not block the loop on an extra model request"
            );
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

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
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
    fn clear_context_records_the_root_agent_type_in_the_replacement_session() {
        crate::test_support::run_locked(async {
            // `/clear` starts a brand-new session file; its `meta` header must
            // carry the root agent type this loop runs under, so a cleared
            // conversation keeps advertising the type instead of degrading to
            // a header with no type at all.
            let temporary_directory = tempfile::tempdir().expect("temporary directory");
            let chats_root = temporary_directory.path().join("chats");
            let original_session_id = Uuid::new_v4();
            let store = crate::store::Store::create(
                &chats_root,
                temporary_directory.path(),
                original_session_id,
                "mock",
                "test-model",
                None,
            )
            .expect("store created");

            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::with_script(
                    vec![MockEvent::Text("ok".to_string())],
                    Duration::from_millis(10),
                )),
                EngineConfig {
                    provider_connection: "mock".to_string(),
                    model: "test-model".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
                },
            ));
            let (agent_loop, mut ui_rx) = AgentLoop::new(
                engine,
                Arc::new(tools::default_registry(
                    test_sandbox(),
                    tools::ToolRegistryConfig::default(),
                )),
            );
            let mut agent_loop = agent_loop.with_agent_type("researcher").with_store(store);

            let handle = agent_loop.run();
            agent_loop.send_command(AgentCommand::ClearContext).await;

            let replacement = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let sessions = crate::store::Store::list_sessions(
                        &chats_root,
                        &crate::store::project_key(temporary_directory.path()),
                    )
                    .expect("sessions are listable");
                    if let Some(session) = sessions
                        .into_iter()
                        .find(|session| session.session_id != original_session_id)
                    {
                        break session;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the replacement session appears on disk");

            let events =
                crate::store::Store::read_events(&replacement.path).expect("events readable");
            let crate::store::StoreEvent::Meta(meta) = &events[0] else {
                panic!("first line must be the meta header, got {:?}", events[0]);
            };
            assert_eq!(
                meta.agent_type.as_deref(),
                Some("researcher"),
                "the replacement session records the loop's root agent type"
            );
            assert_eq!(
                meta.parent_session, None,
                "a /clear rollover is not a child run"
            );

            drop(agent_loop);
            let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
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

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
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
                session_id: None,
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
                Message::System { content } if content.contains("tool-use") && content.contains(&DEFAULT_MAX_TOOL_USE_ITERATIONS.to_string())
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

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
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

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
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
            agent_loop = agent_loop
                .with_max_tool_iterations(3)
                .with_child_role(ChildRoleConfig {
                    run_id,
                    call_id,
                    session_id: None,
                    briefing: Some("do the task".to_string()),
                    parent_cancel: CancellationToken::new(),
                    subagent_ui_tx,
                    finish_tx,
                });

            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt("child task".into()))
                .await;

            // Drain `ui_rx` *while* `run()` is in flight, exactly like the
            // TUI/CLI drivers do. The channel is bounded (capacity 100) and
            // every tool round emits several `UiUpdate`s, so an undrained
            // receiver fills it long before `DEFAULT_MAX_TOOL_USE_ITERATIONS` rounds
            // elapse — `ui_tx.send().await` then parks the whole loop, the
            // iteration cap never trips, and `run()` never returns.
            let mut handle = handle;
            let mut collected_updates: Vec<UiUpdate> = Vec::new();
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    tokio::select! {
                        biased;
                        joined = &mut handle => break joined,
                        update = ui_rx.recv() => match update {
                            Some(update) => collected_updates.push(update),
                            // The loop itself keeps a sender alive for the
                            // whole test, so the channel never closes here;
                            // park on `handle` instead of spinning.
                            None => std::future::pending::<()>().await,
                        },
                    }
                }
            })
            .await
            .expect("child run() should return promptly once the iteration cap trips")
            .expect("run() task should not panic");

            // Collect anything `run()` posted just before returning that the
            // loop above had not polled yet (both branches were ready, and
            // `biased` preferred `handle`).
            while let Ok(update) = ui_rx.try_recv() {
                collected_updates.push(update);
            }

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
            for update in &collected_updates {
                if let UiUpdate::Error {
                    message,
                    run_id: got_run_id,
                } = update
                {
                    assert_eq!(got_run_id, &Some(run_id));
                    assert!(message.contains("iterations"));
                }
            }
            // Drain the subagent ui channel too; just ensure nothing panics.
            while subagent_ui_rx.try_recv().is_ok() {}
        });
    }

    #[tokio::test]
    async fn a_budgeted_call_without_a_useful_allowance_is_skipped_not_executed() {
        let registry = Arc::new(tools::default_registry(
            test_sandbox(),
            tools::ToolRegistryConfig::default(),
        ));
        let call_id = Uuid::new_v4();
        let mut pending_tools = VecDeque::from([PendingTool {
            call: registry
                .create_call("read", call_id, serde_json::json!({ "path": "Cargo.toml" }))
                .expect("read tool registered"),
        }]);
        let (ui_tx, mut ui_rx) = mpsc::channel(8);
        let (agent_tx, _agent_rx) = mpsc::channel(8);
        let mut context = Context::new();
        let mut mode = AgentMode::ToolPending;
        let mut iterations = 0usize;

        advance_tool_queue(
            &mut pending_tools,
            &mut context,
            &mut mode,
            &mut None,
            &mut None,
            &mut iterations,
            DEFAULT_MAX_TOOL_USE_ITERATIONS,
            false,
            test_engine_with_context_length(100),
            registry,
            ui_tx,
            agent_tx,
            0,
            &mut None,
            &mut VecDeque::new(),
            &mut None,
            &mut HashMap::new(),
            None,
            &mut false,
            &mut None,
            None,
            None,
        )
        .await;

        assert!(pending_tools.is_empty());
        assert_eq!(iterations, 1, "a skipped round still consumes an iteration");
        assert!(matches!(
            context.messages().last(),
            Some(Message::ToolError { id, error, .. })
                if *id == call_id && error.contains("not executed")
        ));
        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::ToolAutoDenied { id, .. }) if id == call_id
        ));
    }

    #[test]
    fn recovery_after_a_large_tool_batch_keeps_the_task_and_pairs_results() {
        let mut context = Context::new();
        context.push_user("summarize the ten pages".to_string());
        let calls: Vec<ToolCallRequest> = (0..10)
            .map(|index| ToolCallRequest {
                id: Uuid::new_v4(),
                name: "web_fetch".to_string(),
                arguments: serde_json::json!({ "url": format!("https://example.com/{index}") }),
                persisted_title_arguments: Vec::new(),
            })
            .collect();
        context.push_assistant_tool_call(String::new(), AssistantStatus::Complete, calls.clone());
        for call in &calls {
            context.push_tool_result(ToolResult {
                id: call.id,
                name: call.name.clone(),
                result: "page text ".repeat(2_000),
                attachments: Vec::new(),
                result_section_id: None,
                result_metadata: None,
                result_metadata_schema: None,
                summary: Vec::new(),
                duration: std::time::Duration::ZERO,
                content_sections: Vec::new(),
            });
        }

        let report = context.recover_model_view(4_000, 6_000, &[]);

        assert!(report.changed);
        assert!(report.fits_ceiling);
        let view = context.model_messages();
        assert!(
            view.iter().any(|message| matches!(
                message,
                Message::User { content, .. } if content == "summarize the ten pages"
            )),
            "the task anchor survives recovery"
        );
        assert!(
            view.iter().any(|message| matches!(
                message,
                Message::User { content, .. }
                    if content.contains(compaction::recovery::RECOVERY_HANDOFF_MARKER)
            )),
            "the handoff replaces the removed outcomes"
        );
        let mut open_calls = std::collections::HashSet::new();
        for message in &view {
            match message {
                Message::Assistant { tool_calls, .. } => {
                    open_calls.extend(tool_calls.iter().map(|call| call.id));
                }
                Message::ToolResult { value } => assert!(
                    open_calls.remove(&value.id),
                    "tool result without its call in the view"
                ),
                _ => {}
            }
        }
        assert!(open_calls.is_empty(), "tool call without its result");
        assert_eq!(context.messages().len(), 14, "the transcript is untouched");
    }

    #[test]
    fn recovery_of_a_view_that_already_fits_changes_nothing() {
        let mut context = Context::new();
        context.push_user("only task".to_string());
        let report = context.recover_model_view(100_000, 200_000, &[]);
        assert!(!report.changed);
        assert!(report.fits_target);
    }

    struct ResubmittingBatchProvider {
        operation_count: usize,
        requests: Arc<Mutex<Vec<Vec<Message>>>>,
        skipped_write_path: Option<String>,
    }

    struct RejectingRequestProvider {
        requests: Arc<Mutex<Vec<Vec<Message>>>>,
        failures: usize,
        context_rejection: bool,
        emit_partial: bool,
    }

    impl Provider for RejectingRequestProvider {
        fn name(&self) -> &str {
            "rejecting-request"
        }
        fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn chat<'a>(
            &'a self,
            messages: Vec<Message>,
            _tools: Vec<ToolSchema>,
            _cancel: CancellationToken,
        ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            let index = {
                let mut requests = self.requests.lock().expect("requests");
                let index = requests.len();
                requests.push(messages);
                index
            };
            let fails = index < self.failures;
            let emit_partial = self.emit_partial;
            let reason = if self.context_rejection {
                "context_length_exceeded"
            } else {
                "connection failed"
            };
            Box::pin(async move {
                if fails && !emit_partial {
                    anyhow::bail!(reason);
                }
                let (sender, receiver) = mpsc::channel(4);
                tokio::spawn(async move {
                    if fails {
                        sender
                            .send(StreamEvent::Text("partial".to_string()))
                            .await
                            .expect("partial");
                        sender
                            .send(StreamEvent::Error(anyhow::anyhow!(reason)))
                            .await
                            .expect("error");
                    } else {
                        sender
                            .send(StreamEvent::Text("complete".to_string()))
                            .await
                            .expect("complete");
                        let _ = sender.send(StreamEvent::Done).await;
                    }
                });
                Ok(receiver)
            })
        }
    }

    #[test]
    fn context_rejections_retry_once_before_deltas_with_a_smaller_projection() {
        crate::test_support::run_locked(async {
            for (failures, context_rejection, emit_partial, expected_requests) in [
                (1, true, false, 2),
                (2, true, false, 2),
                (1, false, false, 1),
                (1, true, true, 1),
            ] {
                let requests = Arc::new(Mutex::new(Vec::new()));
                let prompt = sized_prompt();
                let mut configuration = test_engine().config();
                configuration.context_length = Some(first_turn_window(&prompt, 0.78));
                let engine = Arc::new(EngineImpl::new(
                    Arc::new(RejectingRequestProvider {
                        requests: requests.clone(),
                        failures,
                        context_rejection,
                        emit_partial,
                    }),
                    configuration,
                ));
                let registry = Arc::new(tools::default_registry(
                    test_sandbox(),
                    tools::ToolRegistryConfig::default(),
                ));
                let (mut agent, mut updates) = AgentLoop::new(engine, registry);
                let handle = agent.run();
                agent
                    .send_command(AgentCommand::Prompt(prompt.clone()))
                    .await;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while let Some(update) = updates.recv().await {
                        if matches!(update, UiUpdate::Error { .. }) ||
                            matches!(update, UiUpdate::AppendText(ref text) if text == "complete") { break; }
                    }
                }).await.expect("request terminates");
                {
                    let recorded = requests.lock().expect("requests");
                    assert_eq!(recorded.len(), expected_requests);
                    if expected_requests == 2 {
                        let estimate = |request: &[Message]| {
                            request
                                .iter()
                                .map(|message| {
                                    estimate_message_tokens(
                                        crate::llm::TokenEstimator::Tiktoken,
                                        message,
                                    )
                                })
                                .sum::<usize>()
                        };
                        assert!(estimate(&recorded[1]) < estimate(&recorded[0]));
                        assert!(recorded[1].iter().any(|message| matches!(message,
                            Message::User { kind: UserMessageKind::Explicit, content }
                                if content.contains(compaction::recovery::REQUEST_OMISSION_MARKER)
                        )));
                    }
                }
                drop(agent);
                let context = handle.await.expect("joins").expect("context");
                assert!(context.messages().iter().any(|message| matches!(message,
                    Message::User { kind: UserMessageKind::Explicit, content } if content == &prompt
                )));
            }
        });
    }

    impl Provider for ResubmittingBatchProvider {
        fn name(&self) -> &str {
            "resubmitting-batch"
        }
        fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn chat<'a>(
            &'a self,
            messages: Vec<Message>,
            _tools: Vec<ToolSchema>,
            _cancel: CancellationToken,
        ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            let mut operations = HashMap::new();
            let mut completed = std::collections::HashSet::new();
            for message in &messages {
                match message {
                    Message::Assistant { tool_calls, .. } => {
                        for call in tool_calls {
                            if let Some(operation) = call.arguments["operation"].as_u64() {
                                operations.insert(call.id, operation as usize);
                            }
                        }
                    }
                    Message::ToolResult { value } => {
                        if let Some(operation) = operations.get(&value.id) {
                            completed.insert(*operation);
                        }
                    }
                    _ => {}
                }
            }
            let request_index = {
                let mut requests = self.requests.lock().expect("batch requests");
                let index = requests.len();
                requests.push(messages);
                index
            };
            let skipped_write_path = self.skipped_write_path.clone();
            let remaining: Vec<usize> = (0..self.operation_count)
                .filter(|operation| !completed.contains(operation))
                .filter(|_| skipped_write_path.is_none() || request_index == 0)
                .collect();
            Box::pin(async move {
                let (sender, receiver) = mpsc::channel(16);
                tokio::spawn(async move {
                    if remaining.is_empty() {
                        sender
                            .send(StreamEvent::Text("complete".to_string()))
                            .await
                            .expect("text");
                    } else {
                        for (index, operation) in remaining.into_iter().enumerate() {
                            let (name, arguments) = if let Some(path) = &skipped_write_path {
                                match operation {
                                    0 | 6 => {
                                        ("unknown", serde_json::json!({ "operation": operation }))
                                    }
                                    1 => (
                                        "read",
                                        serde_json::json!({ "path": 12, "operation": operation }),
                                    ),
                                    2 => (
                                        "bash",
                                        serde_json::json!({ "command": "printf denied", "operation": operation }),
                                    ),
                                    7 => (
                                        "write",
                                        serde_json::json!({ "path": path, "content": "must not be written", "operation": operation }),
                                    ),
                                    _ => (
                                        "read",
                                        serde_json::json!({ "path": "Cargo.toml", "mode": "lines", "limit": 1, "operation": operation }),
                                    ),
                                }
                            } else {
                                (
                                    "read",
                                    serde_json::json!({ "path": "Cargo.toml", "mode": "lines", "limit": 1, "operation": operation }),
                                )
                            };
                            sender
                                .send(StreamEvent::Delta(DeltaChunk {
                                    tool_call_index: index,
                                    tool_call_id: None,
                                    function_name: Some(name.to_string()),
                                    arguments_fragment: Some(arguments.to_string()),
                                }))
                                .await
                                .expect("call");
                        }
                    }
                    let _ = sender.send(StreamEvent::Done).await;
                });
                Ok(receiver)
            })
        }
    }

    #[test]
    fn controlled_batch_caps_measure_round_trips_skips_and_task_continuity() {
        crate::test_support::run_locked(async {
            for (batch_cap, operation_count, expected_requests, expected_skips) in [
                (6, 0, 1, 0),
                (6, 1, 2, 0),
                (6, 6, 2, 0),
                (6, 7, 3, 1),
                (6, 12, 3, 6),
                (6, 10, 3, 4),
                (4, 10, 4, 8),
            ] {
                let requests = Arc::new(Mutex::new(Vec::new()));
                let engine = Arc::new(EngineImpl::new(
                    Arc::new(ResubmittingBatchProvider {
                        operation_count,
                        requests: requests.clone(),
                        skipped_write_path: None,
                    }),
                    test_engine().config(),
                ));
                let registry = Arc::new(tools::default_registry(
                    test_sandbox(),
                    tools::ToolRegistryConfig::default(),
                ));
                let (mut agent, mut updates) = AgentLoop::new(engine, registry);
                agent.test_batch_cap = batch_cap;
                let handle = agent.run();
                agent
                    .send_command(AgentCommand::Prompt("retain this task".to_string()))
                    .await;
                let mut started = 0;
                let mut skipped = 0;
                let mut lossy = 0;
                tokio::time::timeout(Duration::from_secs(10), async {
                    while let Some(update) = updates.recv().await {
                        match update {
                            UiUpdate::ToolStart { .. } => started += 1,
                            UiUpdate::ToolAutoDenied { .. } => skipped += 1,
                            UiUpdate::Notice { text, .. } if text.contains("lossily") => lossy += 1,
                            UiUpdate::Error { message, .. } => panic!("batch failed: {message}"),
                            UiUpdate::AppendText(text) if text == "complete" => break,
                            _ => {}
                        }
                    }
                })
                .await
                .expect("batch completes");
                assert_eq!(started, operation_count);
                assert_eq!(skipped, expected_skips);
                assert_eq!(lossy, 0);
                {
                    let recorded = requests.lock().expect("batch requests");
                    assert_eq!(recorded.len(), expected_requests);
                    assert!(recorded
                        .iter()
                        .all(|request| request.iter().any(|message| matches!(
                            message, Message::User { content, kind: UserMessageKind::Explicit }
                                if content == "retain this task"
                        ))));
                }
                drop(agent);
                handle.await.expect("batch joins").expect("batch succeeds");
            }
        });
    }

    #[test]
    fn invalid_denied_and_unknown_slots_do_not_backfill_or_execute_an_excess_write() {
        crate::test_support::run_locked(async {
            let directory = tempfile::tempdir().expect("directory");
            let write_path = directory.path().join("skipped.txt");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let engine = Arc::new(EngineImpl::new(
                Arc::new(ResubmittingBatchProvider {
                    operation_count: 8,
                    requests: requests.clone(),
                    skipped_write_path: Some(write_path.display().to_string()),
                }),
                test_engine().config(),
            ));
            let registry = Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            ));
            let (mut agent, mut updates) = AgentLoop::new(engine, registry);
            let handle = agent.run();
            agent
                .send_command(AgentCommand::Prompt("perform the batch".to_string()))
                .await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(update) = updates.recv().await {
                    match update {
                        UiUpdate::ToolPending { id, .. } => {
                            agent
                                .send_command(AgentCommand::DenyTool {
                                    id,
                                    reason: "deliberate denial".to_string(),
                                })
                                .await;
                        }
                        UiUpdate::AppendText(text) if text == "complete" => break,
                        UiUpdate::Error { message, .. } => panic!("batch failed: {message}"),
                        _ => {}
                    }
                }
            })
            .await
            .expect("mixed batch terminates");
            assert!(!write_path.exists(), "excess write never executes");
            {
                let recorded = requests.lock().expect("requests");
                assert_eq!(recorded.len(), 2);
                let continuation = &recorded[1];
                assert_eq!(
                    continuation
                        .iter()
                        .filter(|message| matches!(message, Message::ToolResult { .. }))
                        .count(),
                    3
                );
                assert_eq!(
                    continuation
                        .iter()
                        .filter(|message| matches!(message, Message::ToolError { .. }))
                        .count(),
                    5
                );
                assert!(continuation.iter().any(|message| matches!(message,
                Message::ToolError { name, error, .. } if name == "unknown" && error.contains("not executed")
            )));
                assert!(continuation.iter().any(|message| matches!(message,
                Message::ToolError { name, error, .. } if name == "write" && error.contains("not executed")
            )));
            }
            drop(agent);
            handle.await.expect("joins").expect("context");
        });
    }

    #[test]
    fn preflight_notice_precedes_dispatch_and_a_discarded_notice_prevents_dispatch() {
        crate::test_support::run_locked(async {
            for (acknowledge, cancel_before_ack) in [(false, false), (true, false), (true, true)] {
                let requests = Arc::new(Mutex::new(Vec::new()));
                let mut configuration = test_engine().config();
                configuration.context_length = Some(10_000);
                let engine = Arc::new(EngineImpl::new(
                    Arc::new(RejectingRequestProvider {
                        requests: requests.clone(),
                        failures: 0,
                        context_rejection: false,
                        emit_partial: false,
                    }),
                    configuration,
                ));
                let mut context = Context::new();
                context.push_user("task ".repeat(10_000));
                let (sender, mut events) = mpsc::channel(4);
                let (updates, mut updates_receiver) = mpsc::channel(10);
                let cancel = start_request(
                    &mut context,
                    None,
                    engine,
                    Vec::new(),
                    updates,
                    sender.clone(),
                    7,
                    None,
                    None,
                );
                drop(sender);
                let (generation, event) =
                    tokio::time::timeout(Duration::from_secs(1), events.recv())
                        .await
                        .expect("notice arrives")
                        .expect("notice");
                assert_eq!(generation, 7);
                assert!(
                    requests.lock().expect("requests").is_empty(),
                    "notice is processed before provider contact"
                );
                let AgentEvent::PreflightNotice {
                    text,
                    acknowledgement,
                } = event
                else {
                    panic!("notice first")
                };
                assert!(text.contains("lossily"));
                if cancel_before_ack {
                    cancel.cancel();
                }
                if acknowledge {
                    acknowledgement
                        .lock()
                        .expect("acknowledgement")
                        .take()
                        .expect("sender")
                        .send(())
                        .expect("ack");
                    let completed = tokio::time::timeout(Duration::from_secs(1), events.recv())
                        .await
                        .expect("request completes");
                    if cancel_before_ack {
                        assert!(matches!(
                            completed,
                            Some((7, AgentEvent::StreamCancelled { .. }))
                        ));
                        assert!(matches!(
                            updates_receiver.recv().await,
                            Some(UiUpdate::StreamCancelled)
                        ));
                        assert!(requests.lock().expect("requests").is_empty());
                    } else {
                        assert!(matches!(
                            completed,
                            Some((7, AgentEvent::StreamDone { .. }))
                        ));
                        assert_eq!(requests.lock().expect("requests").len(), 1);
                    }
                } else {
                    drop(acknowledgement);
                    assert!(tokio::time::timeout(Duration::from_secs(1), events.recv())
                        .await
                        .expect("stale request exits")
                        .is_none());
                    assert!(
                        requests.lock().expect("requests").is_empty(),
                        "superseded request never dispatches"
                    );
                }
            }
        });
    }

    #[test]
    fn irreducible_request_overhead_fails_visibly_without_contacting_the_provider() {
        crate::test_support::run_locked(async {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let mut configuration = test_engine().config();
            configuration.context_length = Some(100);
            let engine = Arc::new(EngineImpl::new(
                Arc::new(RejectingRequestProvider {
                    requests: requests.clone(),
                    failures: 0,
                    context_rejection: false,
                    emit_partial: false,
                }),
                configuration,
            ));
            let registry = Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            ));
            let (mut agent, mut updates) = AgentLoop::new(engine, registry);
            let handle = agent.run();
            agent
                .send_command(AgentCommand::Prompt("retain this request".to_string()))
                .await;
            let message = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(UiUpdate::Error { message, .. }) = updates.recv().await {
                        break message;
                    }
                }
            })
            .await
            .expect("visible failure");
            assert!(message.contains("larger context window"));
            assert!(requests.lock().expect("requests").is_empty());
            drop(agent);
            handle.await.expect("joins").expect("context");
        });
    }

    #[test]
    fn only_the_first_six_calls_of_a_response_are_executed() {
        crate::test_support::run_locked(async {
            let script: Vec<MockEvent> = (0..8)
                .map(|index| {
                    MockEvent::Delta(DeltaChunk {
                        tool_call_index: index,
                        tool_call_id: Some(format!("call_{index}")),
                        function_name: Some("read".to_string()),
                        arguments_fragment: Some(
                            serde_json::json!({ "path": "Cargo.toml" }).to_string(),
                        ),
                    })
                })
                .collect();
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
                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
                },
            ));
            let registry = Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            ));
            let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine, registry);
            let handle = agent_loop.run();
            agent_loop
                .send_command(AgentCommand::Prompt("go".into()))
                .await;

            let mut starts = 0;
            let mut skipped = 0;
            tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(update) = ui_rx.recv().await {
                    match update {
                        UiUpdate::ToolStart { .. } => starts += 1,
                        UiUpdate::ToolAutoDenied { reason, .. } => {
                            assert!(reason.contains("smaller batches"));
                            skipped += 1;
                        }
                        UiUpdate::StreamDone if skipped > 0 => break,
                        _ => {}
                    }
                    if skipped >= 2 && starts >= 6 {
                        break;
                    }
                }
            })
            .await
            .expect("first round resolves");
            assert_eq!(skipped, 2, "calls beyond the sixth are not executed");
            assert_eq!(starts, 6, "the first six calls are executed");
            drop(agent_loop);
            handle.abort();
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
