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

//! Events state and operations.

use super::{ContextUsageSource, HistoryDirection, ResultSection};
use crate::llm::engine::{EngineDisplayInfo, ModelSelection};
use crossterm::event::KeyEvent;
use std::time::Instant;
use uuid::Uuid;

// ─── TuiMode ─────────────────────────────────────────────────────────────────

/// TUI-local control-flow state that mirrors the subset of [`crate::agent::AgentMode`]
/// the Presenter needs. The Driver converts `UiUpdate` events into
/// [`DomainEvent`]s that drive transitions here; the Presenter never imports
/// from `agent.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiMode {
    /// No agent activity; ready to accept user input.
    Idle,
    /// Prompt sent to the provider; waiting for `StreamStarted`.
    Pending,
    /// Provider is streaming a response.
    Streaming,
    /// Waiting for the user to answer a `clarify` tool question.
    ClarifyPending {
        id: Uuid,
        question: String,
        context: Option<String>,
        choices: Option<Vec<String>>,
    },
    /// `AgentCommand::Stop` has been sent; waiting for `StreamCancelled` or
    /// the agent to become idle.
    Aborting,
    /// A first confirm-key press (Ctrl+C or Esc) has been received; waiting
    /// for a second press of the *same* key within the confirm window to
    /// actually quit/stop, or for the window to expire.
    DoublePressAbort {
        pressed_at: Instant,
        /// The mode to restore when the confirm window expires.
        return_to: Box<TuiMode>,
        /// Which key armed this confirm window. Only a second press of the
        /// *same* key confirms; a press of the other key starts its own
        /// fresh confirm window instead (so e.g. Esc-then-Ctrl+C never
        /// quits, and Ctrl+C-then-Esc never confirms Ctrl+C's quit).
        armed_by: ConfirmKey,
    },
}

impl TuiMode {
    /// Short, stable variant tag for logging state transitions — omits the
    /// `ClarifyPending`/`DoublePressAbort` payloads (question text, timers).
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            TuiMode::Idle => "Idle",
            TuiMode::Pending => "Pending",
            TuiMode::Streaming => "Streaming",
            TuiMode::ClarifyPending { .. } => "ClarifyPending",
            TuiMode::Aborting => "Aborting",
            TuiMode::DoublePressAbort { .. } => "DoublePressAbort",
        }
    }
}

/// The key that armed a pending [`TuiMode::DoublePressAbort`] confirm window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKey {
    CtrlC,
    Escape,
}

// ─── Focus ────────────────────────────────────────────────────────────────────

/// Keyboard and mouse routing target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    /// Normal typing and history navigation.
    MainInput,
    /// Modal help overlay; captures all keyboard and mouse input.
    HelpPopup,
    /// Tool-approval banner; captures keyboard only (non-modal).
    /// Tool arguments are **not** stored here — they live solely in the
    /// corresponding `ToolBubble` chat entry.
    ToolApproval { tool_id: Uuid, tool_name: String },
}

impl Focus {
    /// Returns `true` only for modal focus variants (currently only
    /// [`Focus::HelpPopup`]).
    pub fn is_modal(&self) -> bool {
        matches!(self, Focus::HelpPopup)
    }
}

// ─── InputEvent ───────────────────────────────────────────────────────────────

/// Typed user-input event surface for the Presenter. The Driver converts
/// [`crate::events::AppAction`] into `InputEvent` variants so the Presenter
/// never imports `AppAction` or raw crossterm types for routing decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// F1 pressed — toggle help popup.
    ShowHelp,
    /// OS SIGINT (external `kill -INT`): interrupt immediately, no confirm.
    SigInt,
    /// Ctrl+C terminal key event; follows double-press confirm logic.
    CtrlC,
    /// Esc key; interpreted based on current [`Focus`] and [`TuiMode`].
    Escape,
    /// Any other key event forwarded verbatim.
    KeyInput(KeyEvent),
    /// Bracketed paste text.
    Paste(String),
}

// ─── DomainEvent ──────────────────────────────────────────────────────────────

/// Agent-domain events the Presenter reacts to. The Driver converts each
/// [`crate::agent::UiUpdate`] variant into the matching `DomainEvent` before
/// calling `presenter::update()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainEvent {
    CompactionActivity {
        run_id: Option<Uuid>,
        request_id: Uuid,
        active: bool,
    },
    /// Provider started streaming (first token / acknowledgement).
    StreamStarted,
    /// A text chunk arrived from the provider.
    StreamChunk(String),
    /// The stream ended cleanly.
    StreamDone,
    /// The stream was cancelled (e.g. after `AgentCommand::Stop`).
    StreamCancelled,
    /// The agent transitioned to idle from any path (error, cancel, done).
    /// Resets `TuiMode` to [`TuiMode::Idle`] and `Focus` to
    /// [`Focus::MainInput`] if needed.
    AgentIdle,
    /// A tool call is waiting for user approval.
    ToolPending {
        id: Uuid,
        name: String,
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
        /// `None` for the root's own pending call; `Some(run_id)` when it
        /// belongs to the currently-tracked child (see `UiUpdate::ToolPending`).
        /// Drives which transcript (`chat_entries` vs `child_messages`) the
        /// resulting bubble is created in and whether the drill-in view
        /// auto-switches to `ConversationView::Child`.
        run_id: Option<Uuid>,
    },
    /// The model is asking the user to clarify something.
    ClarifyPending {
        id: Uuid,
        question: String,
        context: Option<String>,
        choices: Option<Vec<String>>,
        /// See `DomainEvent::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A tool call started executing (approved).
    ToolStart {
        id: Uuid,
        name: String,
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
        /// See `DomainEvent::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A still-running tool call reports an interim status update; the
    /// bubble stays `Running` (spinner keeps going) — only its
    /// `title_arguments` are patched. See `UiUpdate::ToolProgress`.
    ToolProgress {
        id: Uuid,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
    /// A tool call completed successfully.
    ToolDone {
        id: Uuid,
        name: String,
        result_section: ResultSection,
        summary: Vec<(String, String)>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
        /// See `DomainEvent::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A tool call was killed (e.g. via Stop).
    ToolKilled {
        id: Uuid,
        /// See `DomainEvent::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A tool call was denied by the user.
    ToolDenied { id: Uuid },
    /// An error occurred in the agent — in its own loop when `run_id` is
    /// `None`, or in the child run identified by `run_id` (e.g. that
    /// child's own max-tool-use-iterations guard tripped). See
    /// `DomainEvent::ToolPending::run_id`.
    Error {
        message: String,
        run_id: Option<Uuid>,
    },
    /// The double-press confirm window expired; revert to `return_to` mode.
    ConfirmExpired,
    /// A steer was queued while the agent was busy. See
    /// `UiUpdate::SteerQueued::run_id`.
    SteerQueued { run_id: Option<Uuid> },
    /// All queued steers have been injected into the LLM context. See
    /// `UiUpdate::SteerQueued::run_id`.
    SteerApplied { run_id: Option<Uuid> },
    /// A fragment of extended-thinking / reasoning-token output arrived.
    StreamThinkingChunk(String),
    /// A child agent actually started running. See `UiUpdate::ChildAgentStarted`.
    ChildAgentStarted {
        run_id: Uuid,
        /// The `spawn_subagent` tool-call id (== the root `ToolBubble`'s
        /// `id`) this run belongs to. See `ChildRunInfo::bubble_id`.
        call_id: Uuid,
        agent_type: String,
        task: String,
        label: String,
    },
    /// The provider-connection/model identity of a constructed child engine.
    ChildAgentEngineConfigured {
        run_id: Uuid,
        engine: EngineDisplayInfo,
    },
    /// A fragment of `run_id`'s own text/thinking output.
    /// See `UiUpdate::ChildAgentToken`.
    ChildAgentToken {
        run_id: Uuid,
        text: String,
        is_thinking: bool,
    },
    /// The child run's estimate against its own active context window. See
    /// `UiUpdate::ChildContextUsage` for the estimate source.
    ChildContextUsage {
        run_id: Uuid,
        estimated_tokens: usize,
        context_length: usize,
        source: ContextUsageSource,
    },
    /// The child agent run identified by `run_id` has finished.
    /// See `UiUpdate::ChildAgentDone`.
    ChildAgentDone {
        run_id: Uuid,
        status: String,
        result: Option<String>,
    },
    /// `DOUBLE_CLICK_WINDOW` elapsed after a single click on a bubble's
    /// title row without a confirming second click arriving — applies the
    /// deferred collapse/expand toggle that click was waiting to commit
    /// (see `AppState::last_bubble_click`). A no-op if a second click (or a
    /// click on a different bubble) already resolved/cleared it first.
    BubbleClickWindowExpired,
    /// The terminal flushed the lightweight frame acknowledging an expansion.
    BubbleExpansionFeedbackShown,
    /// Estimated size against the active context window; see [`super::ContextUsage`]
    /// and `UiUpdate::ContextUsage` for the estimate source.
    ContextUsage {
        estimated_tokens: usize,
        context_length: usize,
        source: ContextUsageSource,
    },
    /// A compaction snapshot was applied this run; see
    /// `UiUpdate::CompactionApplied` and [`super::ChatEntry::CompactionMarker`].
    CompactionApplied {
        run_id: Option<Uuid>,
        entry_index: usize,
        summarized_messages: usize,
    },
}

impl DomainEvent {
    /// Short, stable variant tag for logging state transitions — deliberately
    /// omits payloads (chunk text, tool arguments/content) so per-event
    /// logging stays cheap and never leaks tool output into the log.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            DomainEvent::CompactionActivity { .. } => "CompactionActivity",
            DomainEvent::StreamStarted => "StreamStarted",
            DomainEvent::StreamChunk(_) => "StreamChunk",
            DomainEvent::StreamDone => "StreamDone",
            DomainEvent::StreamCancelled => "StreamCancelled",
            DomainEvent::AgentIdle => "AgentIdle",
            DomainEvent::ToolPending { .. } => "ToolPending",
            DomainEvent::ClarifyPending { .. } => "ClarifyPending",
            DomainEvent::ToolStart { .. } => "ToolStart",
            DomainEvent::ToolProgress { .. } => "ToolProgress",
            DomainEvent::ToolDone { .. } => "ToolDone",
            DomainEvent::ToolKilled { .. } => "ToolKilled",
            DomainEvent::ToolDenied { .. } => "ToolDenied",
            DomainEvent::Error { .. } => "Error",
            DomainEvent::ConfirmExpired => "ConfirmExpired",
            DomainEvent::SteerQueued { .. } => "SteerQueued",
            DomainEvent::SteerApplied { .. } => "SteerApplied",
            DomainEvent::StreamThinkingChunk(_) => "StreamThinkingChunk",
            DomainEvent::ChildAgentStarted { .. } => "ChildAgentStarted",
            DomainEvent::ChildAgentEngineConfigured { .. } => "ChildAgentEngineConfigured",
            DomainEvent::ChildAgentToken { .. } => "ChildAgentToken",
            DomainEvent::ChildContextUsage { .. } => "ChildContextUsage",
            DomainEvent::ChildAgentDone { .. } => "ChildAgentDone",
            DomainEvent::BubbleClickWindowExpired => "BubbleClickWindowExpired",
            DomainEvent::BubbleExpansionFeedbackShown => "BubbleExpansionFeedbackShown",
            DomainEvent::ContextUsage { .. } => "ContextUsage",
            DomainEvent::CompactionApplied { .. } => "CompactionApplied",
        }
    }

    /// The tool-call/clarify `id` this event correlates to, if any — the one
    /// piece of payload worth logging alongside `kind()`, since it is what
    /// lets a `ToolStart`/`ToolProgress`/`ToolDone` (or `ClarifyPending`)
    /// sequence for the *same* call be followed across log lines.
    pub(crate) fn correlation_id(&self) -> Option<Uuid> {
        match self {
            DomainEvent::ToolPending { id, .. }
            | DomainEvent::ClarifyPending { id, .. }
            | DomainEvent::ToolStart { id, .. }
            | DomainEvent::ToolProgress { id, .. }
            | DomainEvent::ToolDone { id, .. }
            | DomainEvent::ToolKilled { id, .. }
            | DomainEvent::ToolDenied { id } => Some(*id),
            _ => None,
        }
    }
}
// ─── Effect ───────────────────────────────────────────────────────────────────

/// I/O command emitted by the Presenter for the Driver to execute.
/// The Presenter is pure; all actual I/O (channel sends, textarea mutations)
/// happens in `execute_effects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Send the current textarea contents as a prompt.
    SubmitPrompt { steer: bool },
    /// Replace the textarea with an empty, freshly-configured default.
    ClearTextarea,
    /// Park the message-draft input state: snapshot the textarea lines and
    /// the current slash overlay into `AppState::pending_input_snapshot` as
    /// one [`super::InputSnapshot`], clear the textarea, and hide the overlay for
    /// the duration of the pause (the approval footer / clarify banner owns
    /// the rows above the input).
    SaveInputStateAndClear,
    /// Give back what `SaveInputStateAndClear` parked: restore the textarea
    /// lines and the slash overlay from `AppState::pending_input_snapshot`.
    RestoreInputState,
    /// Replace the textarea content with the given text.
    SetInputText(String),
    /// Feed a key event into the textarea.
    TextareaKey(KeyEvent),
    /// Feed a pasted string (dedented) into the textarea.
    Paste(String),
    /// Send `AgentCommand::ApproveTool { id }` to the agent.
    ApproveTool { id: Uuid },
    /// Send `AgentCommand::DenyTool { id, reason }` to the agent.
    /// `reason` is the user-typed denial text; empty when no reason was given.
    DenyTool { id: Uuid, reason: String },
    /// Send `AgentCommand::AnswerClarify(answer)` to the agent.
    AnswerClarify(String),
    /// Send `AgentCommand::AbortClarify` to the agent (user cancelled without answering).
    AbortClarify,
    /// Send `AgentCommand::Stop` to the agent.
    StopAgent,
    /// Quit the TUI event loop.
    Quit,
    /// Load a history entry into the textarea (driver replaces textarea content).
    LoadHistory { direction: HistoryDirection },
    /// Exit history browsing mode (driver restores live draft).
    ExitBrowsing,
    /// Place the textarea cursor at the given terminal cell coordinates.
    PlaceCursorAtMousePos { column: u16, row: u16 },
    /// Start a textarea text selection at the current cursor position.
    StartInputSelection,
    /// Extend the textarea text selection to the given terminal cell.
    ExtendInputSelection { column: u16, row: u16 },
    /// Copy the given text to the OS clipboard.
    CopyToClipboard(String),
    /// Finalize and copy the current textarea text selection.
    FinalizeInputSelection,
    /// `/clear` — drop the transcript and start a new persisted session.
    ClearConversation,
    /// `/tools` — show the tools the running agent may call.
    ShowTools,
    /// Read a replay-discovered child transcript before entering its view.
    LoadChildTranscript { run_id: Uuid },
    /// `/models` — fetch the provider's model list for the picker,
    /// optionally narrowed by a typed model filter.
    ListModels { filter: Option<String> },
    /// A provider-connection/model was picked: activate it and confirm in the transcript.
    SwitchModel(ModelSelection),
}
