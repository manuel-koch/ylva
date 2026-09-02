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

//! Pure TUI state model for the reducer pattern.
//!
//! This module defines the types consumed by `presenter::update()`:
//! - [`AppState`] — the authoritative view-model.
//! - [`TuiMode`] — agent-side control flow state.
//! - [`Focus`] — keyboard/mouse routing.
//! - [`InputEvent`] / [`DomainEvent`] — the typed event surface of the Presenter.
//! - [`Effect`] — the typed I/O commands the Driver executes.
//! - [`AppLayout`] — screen-space geometry with hit-test helpers.
//!
//! **No `use ratatui::backend`**, no channel types, no `UiUpdate` imports.
//! Only [`ratatui::layout::Rect`] is allowed from ratatui here.

use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use uuid::Uuid;

use crate::agent::AgentMode;

// ─── Duration constant ───────────────────────────────────────────────────────

/// Duration a toast notification remains visible before it auto-dismisses.
const TOAST_DURATION: Duration = Duration::from_millis(1500);

/// Sentinel `entry_index` used in `thinking_title_rows` for the in-flight
/// (not yet flushed) thinking bubble rendered from `partial_thinking_text`.
/// Real entries are keyed by their `chat_entries` index (always `< usize::MAX`
/// in practice), so this value can never collide with a genuine index; it
/// lets click handling route to `AppState::toggle_partial_thinking_collapsed`
/// instead of `AppState::toggle_thinking_collapsed`.
pub const STREAMING_THINKING_ENTRY_INDEX: usize = usize::MAX;

// ─── ChatSelection ───────────────────────────────────────────────────────────

/// Screen-space selection state for the chat area. Line and column are
/// Normalised (start ≤ end) drag selection inside the clarify banner, in
/// absolute screen coordinates. Used only for rendering the highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerDragSelection {
    pub start_row: u16,
    pub start_col: u16,
    pub end_row: u16,
    pub end_col: u16,
}

/// absolute line indices into `TuiState::chat_rendered_lines`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatSelection {
    pub anchor_line: usize,
    pub anchor_col: usize,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

impl ChatSelection {
    /// Returns `(start_line, start_col, end_line, end_col)` in reading order.
    pub fn normalized(&self) -> (usize, usize, usize, usize) {
        if (self.anchor_line, self.anchor_col) <= (self.cursor_line, self.cursor_col) {
            (
                self.anchor_line,
                self.anchor_col,
                self.cursor_line,
                self.cursor_col,
            )
        } else {
            (
                self.cursor_line,
                self.cursor_col,
                self.anchor_line,
                self.anchor_col,
            )
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor_line == self.cursor_line && self.anchor_col == self.cursor_col
    }
}

/// Extract `chars[start..end]` from a UTF-8 string, clamped to bounds.
fn char_range(s: &str, start: usize, end: usize) -> String {
    s.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

/// The on-screen rect of the input box, written by `ui::draw` every frame and
/// read by the event loop for mouse click/drag cursor placement. Ephemeral
/// view state, reset to an empty rect between frames.
pub type InputArea = ratatui::layout::Rect;

/// State of a user message entry in the TUI chat history.
/// `ChatEntry` is ephemeral view state only — never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMessageState {
    /// Committed: injected into the model context.
    Sent,
    /// Pending steer: queued, not yet injected.
    Queued,
    /// Steer successfully applied: injected into the model context as a
    /// mid-stream correction — once injected, the LLM has seen it just like
    /// any other user message, and that's the only distinction that matters.
    Steered,
    /// Cancelled: stream was interrupted before injection; text retained in
    /// input_history so the user can recover via ↑.
    Cancelled,
}

/// Well-known result-section IDs used to identify [`ResultSection`] content in tool output.
///
/// The UI derives display titles from the combination of a tool name and one of these IDs.
pub mod result_section_ids {
    /// Standard tool output (used by all tools that return plain text results).
    pub const RESULT: &str = "result";
    /// The user's answer to a clarify question (used by the `clarify` tool).
    pub const USER_ANSWER: &str = "user_answer";
}

/// How the tool's result string is displayed in its completed bubble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultSection {
    /// Stable identifier for the section. See [`result_section_ids`] for known values.
    pub id: String,
    /// The result text.
    pub content: String,
}

impl ResultSection {
    /// Convenience constructor for a plain result with the standard [`result_section_ids::RESULT`] ID.
    pub fn plain(content: impl Into<String>) -> Self {
        Self {
            id: result_section_ids::RESULT.to_string(),
            content: content.into(),
        }
    }
}

/// State of a tool call bubble in the TUI chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallState {
    /// Awaiting user approval; arguments are shown so the user knows what the tool will do.
    Pending {
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
    /// Approved and running.
    Running {
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
    /// Completed successfully.
    Done {
        arguments: serde_json::Value,
        result_section: ResultSection,
        content_sections: Vec<crate::agent::tools::Section>,
        summary: Vec<(String, String)>,
        duration: std::time::Duration,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
    /// Tool returned an error.
    Error {
        error: String,
        title_arguments: Vec<(String, serde_json::Value)>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
    },
    /// Tool was killed (stream interrupted while tool was running).
    Killed,
    /// Tool was denied by the user; error message is the denial reason.
    Denied {
        arguments: serde_json::Value,
        error: String,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
}

/// One entry in the scrollable chat history area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEntry {
    UserMessage {
        text: String,
        state: UserMessageState,
    },
    AssistantMessage {
        text: String,
        interrupted: bool,
    },
    /// Extended-thinking / reasoning output emitted before the assistant's
    /// visible reply. It is collapsible; it starts expanded
    /// (`collapsed: false`) as soon as thinking starts — how this default is
    /// configured is not yet defined — and flushing from the completed
    /// stream carries that collapse state through unchanged rather than
    /// resetting it, so it never auto-collapses when thinking ends; only an
    /// explicit user toggle changes it.
    ThinkingMessage {
        text: String,
        collapsed: bool,
        /// Wall-clock time between the first and last thinking fragment of
        /// this stream, shown in the collapsed summary line.
        duration: std::time::Duration,
    },
    /// A single bubble representing all lifecycle stages of one tool invocation.
    ToolBubble {
        id: uuid::Uuid,
        name: String,
        state: Box<ToolCallState>,
        /// Whether the bubble body (arguments, result) is hidden; only the title
        /// line is rendered when `true`. Toggled by clicking the title line.
        collapsed: bool,
    },
    ErrorLine {
        message: String,
    },
}

/// Direction for [`AppState::navigate_input_message_history`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDirection {
    Up,
    Down,
}

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
    },
    /// The model is asking the user to clarify something.
    ClarifyPending {
        id: Uuid,
        question: String,
        context: Option<String>,
        choices: Option<Vec<String>>,
    },
    /// A tool call started executing (approved).
    ToolStart {
        id: Uuid,
        name: String,
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
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
    },
    /// A tool call was killed (e.g. via Stop).
    ToolKilled { id: Uuid },
    /// A tool call was denied by the user.
    ToolDenied { id: Uuid },
    /// An error occurred in the agent.
    Error(String),
    /// The double-press confirm window expired; revert to `return_to` mode.
    ConfirmExpired,
    /// A steer was queued while the agent was busy.
    SteerQueued,
    /// All queued steers have been injected into the LLM context.
    SteerApplied,
    /// A fragment of extended-thinking / reasoning-token output arrived.
    StreamThinkingChunk(String),
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
    /// Snapshot the current textarea lines into
    /// `AppState::pending_approval_input_snapshot` then clear the textarea.
    SaveTextareaAndClear,
    /// Restore the textarea from the snapshot saved by `SaveTextareaAndClear`.
    RestoreTextarea,
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
}

// ─── AppLayout ────────────────────────────────────────────────────────────────

/// Screen-space geometry for the chat scroll area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChatArea {
    pub rect: Rect,
}

impl ChatArea {
    /// Returns `true` if the terminal cell `(col, row)` falls inside this area.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }

    /// Number of rows a page-up / page-down should scroll.
    pub fn page_scroll_rows(&self) -> u16 {
        self.rect.height.saturating_sub(1)
    }
}

/// Screen-space geometry for the text-input box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputAreaLayout {
    pub rect: Rect,
}

impl InputAreaLayout {
    /// Returns `true` if the terminal cell `(col, row)` falls inside this area.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }

    /// Number of rows a page-up / page-down should scroll.
    pub fn page_scroll_rows(&self) -> u16 {
        self.rect.height.saturating_sub(1)
    }
}

/// Screen-space geometry for the help popup overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HelpPopupLayout {
    pub rect: Rect,
}

impl HelpPopupLayout {
    /// Returns `true` if the terminal cell `(col, row)` falls inside the
    /// popup's rendered area (equivalent to `contains` but named `hit` to
    /// signal it is used for modal dismiss-on-outside-click logic).
    pub fn hit(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }

    /// Number of rows a page-up / page-down should scroll.
    pub fn page_scroll_rows(&self) -> u16 {
        self.rect.height.saturating_sub(1)
    }
}

/// Aggregated screen-space geometry written by the View (`render.rs`) each
/// frame and read by the Presenter for mouse hit-testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppLayout {
    pub chat_area: ChatArea,
    pub input_area: InputAreaLayout,
    pub help_popup: HelpPopupLayout,
    /// Rect of the clarify banner. Zero-sized when no clarify is pending.
    pub clarify_banner: ratatui::layout::Rect,
    /// Rect of the Approve button rendered in the tool-approval footer.
    /// Written by the Driver after each `ui::draw` call; zero-sized when
    /// no approval footer is visible.
    pub approve_button: Rect,
    /// Rect of the Deny button rendered in the tool-approval footer.
    /// Written by the Driver after each `ui::draw` call; zero-sized when
    /// no approval footer is visible.
    pub deny_button: Rect,
}

// ─── AppState ────────────────────────────────────────────────────────────────

/// The authoritative TUI view-model owned by the Driver.
///
/// This is the single source of truth: `presenter::update` mutates it in
/// response to [`DomainEvent`]s, and every frame `render::project_for_draw`
/// derives a fresh [`crate::tui_render_state::TuiState`] snapshot from it for
/// `ui::draw` to render. `TuiState` is never the source of truth and is not fed back into
/// `AppState` except for the handful of view-geometry fields `ui::draw`
/// itself writes (see `render::sync_from_draw`).
///
/// Fields that require `TextArea` to interpret (e.g. `mouse_drag`, used by
/// `cursor_for_click`) or that exist purely to pass rendering artifacts between
/// draw calls (e.g. `button_areas`) are kept in the Driver and excluded here.
pub struct AppState {
    pub mode: TuiMode,
    pub focus: Focus,
    pub layout: AppLayout,

    // ── chat view-model ─────────────────────────────────────────────────────
    pub chat_entries: Vec<ChatEntry>,
    pub partial_assistant_text: String,
    /// In-flight extended-thinking/reasoning fragments, accumulated separately
    /// from `partial_assistant_text` so they never contaminate the assistant
    /// reply. Flushed into a `ChatEntry::ThinkingMessage` on `StreamDone`.
    pub partial_thinking_text: String,
    /// Set when the first fragment of `partial_thinking_text` arrives;
    /// cleared on flush and on `StreamStarted`. Used to compute the
    /// `ChatEntry::ThinkingMessage::duration` shown in the collapsed summary.
    /// Not part of `TuiState`: rendering only needs the final `duration` once
    /// baked into a flushed `ChatEntry`, not the live start instant.
    pub thinking_started_at: Option<Instant>,
    /// Collapse state for the thinking bubble that will be produced when
    /// `partial_thinking_text` is flushed. Set to `false` (expanded) alongside
    /// `thinking_started_at` when a new thinking stream starts, then carried
    /// unchanged into the flushed `ChatEntry::ThinkingMessage` — flushing never
    /// overrides it, so a manual collapse/expand made while the bubble was
    /// still in-flight (if ever supported) would survive the flush.
    pub partial_thinking_collapsed: bool,
    /// Steers (user messages sent while the agent is busy) staged for live
    /// rendering as `Queued` bubbles, not yet committed to `chat_entries`.
    /// See `queue_steer_message` / `commit_queued_steer_messages`.
    pub queued_steer_texts: Vec<String>,

    // ── scroll state ────────────────────────────────────────────────────────
    pub scroll_offset: usize,
    /// `true` = auto-scroll to bottom on new content.
    pub scroll_locked: bool,
    /// Updated on every resize event.
    pub chat_area_height: u16,

    // ── tool-approval ───────────────────────────────────────────────────────
    /// Textarea lines saved when a `ToolPending` approval sub-mode starts so
    /// the unsubmitted draft can be restored once the approval is resolved.
    /// `None` when no approval is in progress or the input was empty.
    pub pending_approval_input_snapshot: Option<Vec<String>>,

    // ── input history ───────────────────────────────────────────────────────
    /// All `UserMessage` texts, oldest → newest.
    pub input_history: Vec<String>,
    /// `None` = live draft; `Some(i)` = browsing history at index `i`.
    pub history_index: Option<usize>,
    /// Saved live draft while browsing history.
    pub draft: String,

    // ── steer indicator ─────────────────────────────────────────────────────
    /// `true` while ≥1 steer is queued.
    pub steer_indicator: bool,

    // ── chat selection / rendered lines ─────────────────────────────────────
    /// Plain text of every rendered line from the most recent draw call.
    pub chat_rendered_lines: Vec<String>,
    /// Number of leading chrome characters stripped per line (see
    /// `TuiState::chat_rendered_line_chrome_widths`).
    pub chat_rendered_line_chrome_widths: Vec<usize>,
    /// Active or committed chat selection.
    pub chat_selection: Option<ChatSelection>,

    // ── transient UI ────────────────────────────────────────────────────────
    /// Floating notification, auto-dismissed after [`TOAST_DURATION`].
    pub toast: Option<(String, Instant)>,
    /// Incremented each tick while agent is busy; drives the footer spinner.
    pub spinner_frame: u8,

    // ── help popup ──────────────────────────────────────────────────────────
    pub help_scroll_offset: usize,
    pub help_scroll_x: usize,

    // ── clarify banner ───────────────────────────────────────────────────────
    /// Current vertical scroll offset inside the clarify banner (lines from
    /// the top of the content). Clamped to the valid range by
    /// `draw_clarify_banner` each frame.
    pub clarify_scroll_offset: usize,

    // ── input field ─────────────────────────────────────────────────────────
    /// Mirrors ratatui-textarea's internal scroll-top so scroll indicators
    /// reflect the actual rendered viewport position.
    pub input_scroll_top: usize,

    // ── bubble hit-testing ──────────────────────────────────────────────────
    /// Maps each rendered tool bubble to its absolute line-index range
    /// `(start_row, end_row, id)` (inclusive, before scroll offset).
    pub bubble_title_rows: Vec<(usize, usize, Uuid)>,
    /// Maps each rendered thinking bubble to its absolute line-index range
    /// `(start_row, end_row, chat_entries index)` (inclusive, before scroll
    /// offset). Mirrors `bubble_title_rows` but is index-keyed since
    /// `ChatEntry::ThinkingMessage` has no `Uuid`.
    pub thinking_title_rows: Vec<(usize, usize, usize)>,

    // ── history navigation helper ────────────────────────────────────────────
    /// Exposes history navigation for use by the presenter.
    pub _history_direction: Option<HistoryDirection>,

    // ── chat drag selection ──────────────────────────────────────────────────
    /// `true` while the left mouse button is held down in the chat area.
    pub chat_drag_active: bool,

    // ── clarify choice click tracking ────────────────────────────────────────
    /// Terminal position where the left button was pressed inside a clarify
    /// choice row. Cleared on mouse-up. A mouse-up at the same position is a
    /// click (→ fill textarea); a mouse-up at a different position is a drag
    /// (→ no action, allows text selection).
    pub clarify_choice_mousedown: Option<(u16, u16)>,
    /// Updated on every `MouseDrag` event while `clarify_choice_mousedown` is
    /// set. Together with the down position this defines the highlighted row
    /// range in the banner. Cleared on mouse-up.
    pub clarify_banner_drag_end: Option<(u16, u16)>,

    // ── abort-exit flag ──────────────────────────────────────────────────────
    /// `true` when `TuiMode::Aborting` was entered via Ctrl+C double-press,
    /// meaning the Driver should break the event loop once mode returns to
    /// Idle. `false` when Aborting was entered via Esc (soft cancel) — the
    /// TUI stays open so the user can continue chatting.
    pub aborting_exit: bool,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            mode: TuiMode::Idle,
            focus: Focus::MainInput,
            layout: AppLayout::default(),

            chat_entries: Vec::new(),
            partial_assistant_text: String::new(),
            partial_thinking_text: String::new(),
            thinking_started_at: None,
            partial_thinking_collapsed: false,
            queued_steer_texts: Vec::new(),

            scroll_offset: 0,
            scroll_locked: true,
            chat_area_height: 0,

            pending_approval_input_snapshot: None,

            input_history: Vec::new(),
            history_index: None,
            draft: String::new(),

            steer_indicator: false,

            chat_rendered_lines: Vec::new(),
            chat_rendered_line_chrome_widths: Vec::new(),
            chat_selection: None,

            toast: None,
            spinner_frame: 0,

            help_scroll_offset: 0,
            help_scroll_x: 0,

            clarify_scroll_offset: 0,

            input_scroll_top: 0,

            bubble_title_rows: Vec::new(),
            thinking_title_rows: Vec::new(),

            _history_direction: None,

            chat_drag_active: false,
            clarify_choice_mousedown: None,
            clarify_banner_drag_end: None,
            aborting_exit: false,
        }
    }

    /// Returns `true` if a toast is active and has not yet expired.
    pub fn has_active_toast(&self) -> bool {
        self.toast
            .as_ref()
            .map(|(_, start)| start.elapsed() < TOAST_DURATION)
            .unwrap_or(false)
    }

    /// Flush only the in-flight thinking buffer into a `ChatEntry::ThinkingMessage`,
    /// leaving `partial_assistant_text` untouched. Used when a stream switches
    /// from thinking to response text (or hits a tool call) mid-turn, so each
    /// segment renders as its own bubble in chronological order rather than
    /// merging every thinking fragment of the turn into a single bubble.
    /// No-op when there is no pending thinking text.
    pub fn flush_partial_thinking_message(&mut self) {
        if self.partial_thinking_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.partial_thinking_text);
        let collapsed = self.partial_thinking_collapsed;
        let duration = self
            .thinking_started_at
            .take()
            .map(|started_at| started_at.elapsed())
            .unwrap_or_default();
        self.chat_entries.push(ChatEntry::ThinkingMessage {
            text,
            collapsed,
            duration,
        });
    }

    /// Flush only the in-flight assistant reply buffer into a
    /// `ChatEntry::AssistantMessage`, leaving `partial_thinking_text` untouched.
    /// Used when a stream switches from response text back to thinking mid-turn
    /// (mirrors `flush_partial_thinking_message`). Unlike
    /// `flush_partial_assistant_message`, this never relocates queued steers —
    /// those only need to move once, at the true end of the turn — and never
    /// marks the bubble interrupted, since a mid-turn segment switch is not
    /// an interruption. No-op when there is no pending assistant text.
    pub fn flush_partial_response_message(&mut self) {
        if self.partial_assistant_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.partial_assistant_text);
        self.chat_entries.push(ChatEntry::AssistantMessage {
            text,
            interrupted: false,
        });
    }

    /// Commit the in-flight partial assistant text into a full `AssistantMessage`
    /// entry and clear the partial buffer.
    ///
    /// If `partial_thinking_text` is non-empty, a `ChatEntry::ThinkingMessage` is
    /// pushed immediately before the assistant bubble — even when
    /// `partial_assistant_text` itself is empty (a thinking-only turn, e.g. the
    /// model reasons and then immediately calls a tool with no visible prose).
    pub fn flush_partial_assistant_message(&mut self, interrupted: bool) {
        let thinking_text = (!self.partial_thinking_text.is_empty())
            .then(|| std::mem::take(&mut self.partial_thinking_text));
        let thinking_collapsed = self.partial_thinking_collapsed;
        let thinking_duration = self
            .thinking_started_at
            .take()
            .map(|started_at| started_at.elapsed())
            .unwrap_or_default();
        if let Some(text) = thinking_text {
            self.chat_entries.push(ChatEntry::ThinkingMessage {
                text,
                collapsed: thinking_collapsed,
                duration: thinking_duration,
            });
        }
        if !self.partial_assistant_text.is_empty() {
            self.chat_entries.push(ChatEntry::AssistantMessage {
                text: std::mem::take(&mut self.partial_assistant_text),
                interrupted,
            });
        } else {
            self.partial_assistant_text.clear();
        }
    }

    /// Commit every staged queued-steer text (see `queue_steer_message`) into
    /// `chat_entries` as a `UserMessage` with the given final `state`
    /// (`Steered` once applied to the model, `Cancelled` once the round is
    /// interrupted), in the order they were typed, then clear the staging
    /// list. Must be called after any other flush for the same event (e.g.
    /// `flush_partial_assistant_message`) so the committed bubbles land after
    /// everything that happened while they were staged, rather than at the
    /// stale position where the user happened to press `Enter`.
    pub fn commit_queued_steer_messages(&mut self, state: UserMessageState) {
        for text in std::mem::take(&mut self.queued_steer_texts) {
            self.chat_entries
                .push(ChatEntry::UserMessage { text, state });
        }
    }

    /// Find the most-recent `ToolBubble` with `id == target_id`.
    pub fn find_tool_bubble_mut(&mut self, target_id: Uuid) -> Option<&mut ChatEntry> {
        self.chat_entries
            .iter_mut()
            .rev()
            .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == target_id))
    }

    /// Append a user message bubble and record the text in input history.
    /// Only for messages sent immediately (`UserMessageState::Sent`); a steer
    /// sent while the agent is busy must go through `queue_steer_message`
    /// instead, since it isn't safe to commit to `chat_entries` yet.
    pub fn push_user_bubble(&mut self, text: String, state: UserMessageState) {
        self.chat_entries.push(ChatEntry::UserMessage {
            text: text.clone(),
            state,
        });
        self.input_history.push(text);
        self.history_index = None;
    }

    /// Stage a steer (a user message sent while the agent is busy) without
    /// committing it to `chat_entries` yet. Committing immediately would fix
    /// its position in the list before thinking/tool-call content for the
    /// in-flight round has been flushed, wedging the steer bubble ahead of
    /// content that chronologically precedes it. The staged text renders as
    /// a live `Queued` bubble near the bottom of the chat (via
    /// `TuiState::queued_steer_texts`) until `commit_queued_steer_messages`
    /// — called on `SteerApplied` or cancellation — commits it to
    /// `chat_entries` at the then-current tail, guaranteed to be after
    /// everything already flushed.
    pub fn queue_steer_message(&mut self, text: String) {
        self.queued_steer_texts.push(text.clone());
        self.input_history.push(text);
        self.history_index = None;
    }

    /// Scroll chat up (toward earlier content); disables auto-scroll.
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_locked = false;
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    /// Scroll chat down (toward later content); disables auto-scroll.
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_locked = false;
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }

    /// Re-enable auto-scroll (jump to bottom).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_locked = true;
        self.scroll_offset = 0;
    }

    /// Scroll the help popup up.
    pub fn help_scroll_up(&mut self, lines: usize) {
        self.help_scroll_offset = self.help_scroll_offset.saturating_sub(lines);
    }

    /// Scroll the help popup down.
    pub fn help_scroll_down(&mut self, lines: usize) {
        self.help_scroll_offset = self.help_scroll_offset.saturating_add(lines);
    }

    /// Scroll the help popup left.
    pub fn help_scroll_left(&mut self, columns: usize) {
        self.help_scroll_x = self.help_scroll_x.saturating_sub(columns);
    }

    /// Scroll the help popup right.
    pub fn help_scroll_right(&mut self, columns: usize) {
        self.help_scroll_x = self.help_scroll_x.saturating_add(columns);
    }

    /// Scroll the clarify banner up (toward earlier content).
    pub fn clarify_scroll_up(&mut self, lines: usize) {
        self.clarify_scroll_offset = self.clarify_scroll_offset.saturating_sub(lines);
    }

    /// Scroll the clarify banner down (toward later content). The upper bound
    /// is applied in `draw_clarify_banner`, which knows the total content height.
    pub fn clarify_scroll_down(&mut self, lines: usize) {
        self.clarify_scroll_offset = self.clarify_scroll_offset.saturating_add(lines);
    }

    /// Toggle the collapsed/expanded state of the tool bubble with the given id.
    ///
    /// Collapsing a bubble whose body extends below the viewport shrinks the
    /// total content height, which can push `scroll_offset` past the new
    /// `max_offset` and re-lock the view to the bottom, scrolling the bubble's
    /// title out of sight. Clamping `scroll_offset` to the bubble's own start
    /// row keeps that title row visible in the current viewport.
    pub fn toggle_tool_bubble_collapsed(&mut self, target_id: Uuid) {
        for entry in &mut self.chat_entries {
            if let ChatEntry::ToolBubble { id, collapsed, .. } = entry {
                if *id == target_id {
                    *collapsed = !*collapsed;
                    self.scroll_locked = false;
                    if let Some((start_row, _, _)) = self
                        .bubble_title_rows
                        .iter()
                        .find(|(_, _, bubble_id)| *bubble_id == target_id)
                    {
                        self.scroll_offset = self.scroll_offset.min(*start_row);
                    }
                    return;
                }
            }
        }
    }

    /// Toggle the collapsed state of the `ChatEntry::ThinkingMessage` at
    /// `target_index` in `chat_entries`. Mirrors `toggle_tool_bubble_collapsed`
    /// but is index-keyed since thinking bubbles carry no `Uuid`.
    pub fn toggle_thinking_collapsed(&mut self, target_index: usize) {
        if let Some(ChatEntry::ThinkingMessage { collapsed, .. }) =
            self.chat_entries.get_mut(target_index)
        {
            *collapsed = !*collapsed;
            self.scroll_locked = false;
            if let Some((start_row, _, _)) = self
                .thinking_title_rows
                .iter()
                .find(|(_, _, entry_index)| *entry_index == target_index)
            {
                self.scroll_offset = self.scroll_offset.min(*start_row);
            }
        }
    }

    /// Toggle the collapsed state of the in-flight (not yet flushed) thinking
    /// bubble rendered from `partial_thinking_text`. Mirrors
    /// `toggle_thinking_collapsed` but flips `partial_thinking_collapsed`
    /// directly since there is no `ChatEntry` yet — the value carries through
    /// unchanged into the flushed `ChatEntry::ThinkingMessage` once the
    /// stream ends (see `flush_partial_thinking_message`).
    pub fn toggle_partial_thinking_collapsed(&mut self) {
        self.partial_thinking_collapsed = !self.partial_thinking_collapsed;
        self.scroll_locked = false;
        if let Some((start_row, _, _)) = self
            .thinking_title_rows
            .iter()
            .find(|(_, _, entry_index)| *entry_index == STREAMING_THINKING_ENTRY_INDEX)
        {
            self.scroll_offset = self.scroll_offset.min(*start_row);
        }
    }

    /// Extract the text covered by the current `chat_selection`.
    pub fn extract_chat_selection_text(&self) -> Option<String> {
        let sel = self.chat_selection.as_ref()?;
        if sel.is_empty() {
            return None;
        }
        let (start_line, start_col, end_line, end_col) = sel.normalized();
        let lines = &self.chat_rendered_lines;
        let start_line = start_line.min(lines.len().saturating_sub(1));
        let end_line = end_line.min(lines.len().saturating_sub(1));
        if start_line > end_line || lines.is_empty() {
            return None;
        }
        let chrome = |index: usize| -> usize {
            self.chat_rendered_line_chrome_widths
                .get(index)
                .copied()
                .unwrap_or(0)
        };
        if start_line == end_line {
            let line = &lines[start_line];
            let c = chrome(start_line);
            Some(char_range(
                line,
                start_col.saturating_sub(c),
                end_col.saturating_sub(c),
            ))
        } else {
            let mut result = String::new();
            for (absolute_index, line) in lines
                .iter()
                .enumerate()
                .skip(start_line)
                .take(end_line - start_line + 1)
            {
                let c = chrome(absolute_index);
                let fragment = if absolute_index == start_line {
                    char_range(line, start_col.saturating_sub(c), line.chars().count())
                } else if absolute_index == end_line {
                    char_range(line, 0, end_col.saturating_sub(c))
                } else {
                    line.clone()
                };
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&fragment);
            }
            Some(result)
        }
    }

    /// Show a transient toast notification.
    pub fn set_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now() + TOAST_DURATION));
    }

    /// Navigate input history. Returns the text to load, or `None` if boundary reached.
    pub fn navigate_input_message_history(
        &mut self,
        direction: HistoryDirection,
        current_text: &str,
    ) -> Option<String> {
        match direction {
            HistoryDirection::Up => {
                if self.input_history.is_empty() {
                    return None;
                }
                match self.history_index {
                    None => {
                        self.draft = current_text.to_string();
                        self.history_index = Some(self.input_history.len() - 1);
                    }
                    Some(0) => return None,
                    Some(index) => {
                        self.history_index = Some(index - 1);
                    }
                }
                Some(self.input_history[self.history_index.expect("just set")].clone())
            }
            HistoryDirection::Down => match self.history_index {
                None => None,
                Some(index) if index < self.input_history.len() - 1 => {
                    self.history_index = Some(index + 1);
                    Some(self.input_history[index + 1].clone())
                }
                Some(_) => {
                    self.history_index = None;
                    Some(std::mem::take(&mut self.draft))
                }
            },
        }
    }

    /// Exit history browsing (if active) before an edit keypress.
    pub fn exit_input_history_browsing(&mut self, current_text: &str) {
        if self.history_index.is_some() {
            self.draft = current_text.to_string();
            self.history_index = None;
        }
    }

    // ── View-layer helpers ────────────────────────────────────────────────────

    /// Convert the current [`TuiMode`] (and [`Focus`]) to the legacy
    /// [`AgentMode`] used by `ui.rs` for footer and spinner rendering.
    pub fn agent_mode(&self) -> AgentMode {
        match &self.mode {
            TuiMode::Idle => AgentMode::Idle,
            TuiMode::Pending => AgentMode::Pending,
            TuiMode::Streaming => {
                if matches!(self.focus, Focus::ToolApproval { .. }) {
                    AgentMode::ToolPending
                } else if self.has_running_tool_bubble() {
                    AgentMode::ToolRunning
                } else {
                    AgentMode::Streaming
                }
            }
            TuiMode::ClarifyPending { .. } => AgentMode::ClarifyPending,
            TuiMode::Aborting => AgentMode::Aborting,
            TuiMode::DoublePressAbort { return_to, .. } => match return_to.as_ref() {
                TuiMode::Idle | TuiMode::DoublePressAbort { .. } => AgentMode::Idle,
                TuiMode::Pending => AgentMode::Pending,
                TuiMode::Streaming => {
                    if self.has_running_tool_bubble() {
                        AgentMode::ToolRunning
                    } else {
                        AgentMode::Streaming
                    }
                }
                TuiMode::ClarifyPending { .. } => AgentMode::ClarifyPending,
                TuiMode::Aborting => AgentMode::Aborting,
            },
        }
    }

    /// Returns `true` if any `ChatEntry::ToolBubble` is currently in the
    /// `Running` state, i.e. a tool call has started but not yet finished
    /// (`Done`/`Error`/`Denied`/`Killed`). Used by `agent_mode` to
    /// distinguish "a tool is executing" from plain LLM streaming (thinking
    /// or assistant text) in the footer.
    fn has_running_tool_bubble(&self) -> bool {
        self.chat_entries.iter().any(|entry| {
            matches!(
                entry,
                ChatEntry::ToolBubble { state, .. } if matches!(state.as_ref(), ToolCallState::Running { .. })
            )
        })
    }

    /// Returns `true` when the help popup is currently displayed.
    pub fn show_help(&self) -> bool {
        matches!(self.focus, Focus::HelpPopup)
    }

    /// Returns `true` when a tool is awaiting user approval.
    pub fn is_approval_pending(&self) -> bool {
        matches!(self.focus, Focus::ToolApproval { .. })
    }

    /// Returns the name of the tool awaiting approval, or `None` when not in
    /// approval sub-mode.
    pub fn approval_tool_name(&self) -> Option<&str> {
        if let Focus::ToolApproval { tool_name, .. } = &self.focus {
            Some(tool_name.as_str())
        } else {
            None
        }
    }

    /// Update the tool bubble for `id` to the Error state. If `id` was the
    /// tool awaiting approval, clear the approval focus and reset mode to Idle.
    pub fn apply_tool_error(
        &mut self,
        id: Uuid,
        error: String,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
    ) {
        if let Some(entry) = self.find_tool_bubble_mut(id) {
            if let ChatEntry::ToolBubble {
                state: bubble_state,
                ..
            } = entry
            {
                // Skip bubbles already transitioned to Denied by the Driver
                // (via apply_tool_denied). The ToolError that the agent sends
                // after processing a DenyTool command is redundant for those.
                if matches!(bubble_state.as_ref(), ToolCallState::Denied { .. }) {
                    return;
                }
                let preserved_title_arguments = match bubble_state.as_ref() {
                    ToolCallState::Pending {
                        title_arguments, ..
                    }
                    | ToolCallState::Running {
                        title_arguments, ..
                    }
                    | ToolCallState::Done {
                        title_arguments, ..
                    }
                    | ToolCallState::Error {
                        title_arguments, ..
                    } => title_arguments.clone(),
                    _ => vec![],
                };
                **bubble_state = ToolCallState::Error {
                    error: error.clone(),
                    title_arguments: preserved_title_arguments,
                    duration,
                    content_sections,
                };
            }
        } else {
            self.chat_entries.push(ChatEntry::ErrorLine {
                message: error.clone(),
            });
        }
        if matches!(self.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
            self.focus = Focus::MainInput;
            self.mode = TuiMode::Idle;
        }
    }

    /// Create a `Denied` bubble for a tool that was auto-denied by the sandbox
    /// before it ever started. No prior `ToolStart` was sent for this id.
    pub fn apply_tool_auto_denied(
        &mut self,
        id: Uuid,
        name: String,
        title_arguments: Vec<(String, serde_json::Value)>,
        content_sections: Vec<crate::agent::tools::Section>,
        reason: String,
    ) {
        self.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name,
            state: Box::new(ToolCallState::Denied {
                arguments: serde_json::Value::Null,
                error: reason,
                content_sections,
                title_arguments,
            }),
            collapsed: false,
        });
    }

    /// Mark a `Pending` tool bubble as `Denied`, preserving its arguments.
    ///
    /// Called by the Driver immediately when `Effect::DenyTool` is executed,
    /// before the agent sends the corresponding `ToolError` response.
    /// `reason` is the user-supplied denial text; empty means no reason given.
    pub fn apply_tool_denied(&mut self, id: Uuid, reason: &str) {
        if let Some(ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        }) = self.find_tool_bubble_mut(id)
        {
            let (preserved_arguments, preserved_content_sections, preserved_title_arguments) =
                match bubble_state.as_mut() {
                    ToolCallState::Pending {
                        arguments,
                        content_sections,
                        title_arguments,
                    }
                    | ToolCallState::Running {
                        arguments,
                        content_sections,
                        title_arguments,
                    } => (
                        std::mem::replace(arguments, serde_json::Value::Null),
                        std::mem::take(content_sections),
                        std::mem::take(title_arguments),
                    ),
                    _ => (serde_json::Value::Null, vec![], vec![]),
                };
            **bubble_state = ToolCallState::Denied {
                arguments: preserved_arguments,
                error: if reason.is_empty() {
                    "Denied by user".to_string()
                } else {
                    format!("Denied by user: {reason}")
                },
                content_sections: preserved_content_sections,
                title_arguments: preserved_title_arguments,
            };
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Focus::is_modal ───────────────────────────────────────────────────────

    #[test]
    fn focus_is_modal_returns_true_only_for_help_popup() {
        assert!(Focus::HelpPopup.is_modal());
        assert!(!Focus::MainInput.is_modal());
        assert!(!Focus::ToolApproval {
            tool_id: Uuid::new_v4(),
            tool_name: "test_tool".to_string(),
        }
        .is_modal());
    }

    // ── ChatArea::contains geometry ───────────────────────────────────────────

    #[test]
    fn chat_area_contains_detects_interior_cells() {
        let area = ChatArea {
            rect: Rect::new(2, 3, 10, 5), // x=2, y=3, w=10, h=5
        };
        // Interior
        assert!(area.contains(2, 3)); // top-left corner
        assert!(area.contains(11, 7)); // bottom-right corner
        assert!(area.contains(5, 5)); // somewhere inside

        // Exterior
        assert!(!area.contains(1, 3)); // left of area
        assert!(!area.contains(12, 3)); // right of area (x + w = 12)
        assert!(!area.contains(2, 2)); // above area
        assert!(!area.contains(2, 8)); // below area (y + h = 8)
    }

    #[test]
    fn chat_area_page_scroll_rows_returns_height_minus_one() {
        let area = ChatArea {
            rect: Rect::new(0, 0, 80, 20),
        };
        assert_eq!(area.page_scroll_rows(), 19);
    }

    #[test]
    fn chat_area_page_scroll_rows_saturates_at_zero_for_tiny_area() {
        let area = ChatArea {
            rect: Rect::new(0, 0, 80, 0),
        };
        assert_eq!(area.page_scroll_rows(), 0);
    }

    // ── InputAreaLayout::contains geometry ───────────────────────────────────

    #[test]
    fn input_area_contains_detects_interior_cells() {
        let area = InputAreaLayout {
            rect: Rect::new(0, 20, 80, 4),
        };
        assert!(area.contains(0, 20));
        assert!(area.contains(79, 23));
        assert!(!area.contains(80, 20));
        assert!(!area.contains(0, 24));
    }

    // ── HelpPopupLayout::hit ──────────────────────────────────────────────────

    #[test]
    fn help_popup_hit_detects_interior_cells() {
        let popup = HelpPopupLayout {
            rect: Rect::new(10, 5, 60, 30),
        };
        assert!(popup.hit(10, 5));
        assert!(popup.hit(69, 34));
        assert!(!popup.hit(9, 5));
        assert!(!popup.hit(70, 5));
    }

    // ── TuiMode construction and equality ─────────────────────────────────────

    #[test]
    fn tui_mode_idle_equals_idle() {
        assert_eq!(TuiMode::Idle, TuiMode::Idle);
    }

    #[test]
    fn tui_mode_idle_does_not_equal_pending() {
        assert_ne!(TuiMode::Idle, TuiMode::Pending);
    }

    #[test]
    fn tui_mode_double_press_abort_holds_pressed_at_and_return_to() {
        let now = Instant::now();
        let mode = TuiMode::DoublePressAbort {
            pressed_at: now,
            return_to: Box::new(TuiMode::Idle),
            armed_by: ConfirmKey::CtrlC,
        };
        if let TuiMode::DoublePressAbort { return_to, .. } = &mode {
            assert_eq!(**return_to, TuiMode::Idle);
        } else {
            panic!("expected DoublePressAbort");
        }
    }

    #[test]
    fn agent_mode_maps_clarify_pending_to_clarify_pending() {
        let mut state = AppState::new();
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Test?".to_string(),
            context: None,
            choices: None,
        };
        assert_eq!(state.agent_mode(), AgentMode::ClarifyPending);
    }

    #[test]
    fn agent_mode_reports_tool_running_when_a_tool_bubble_is_running() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: "read".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
            }),
            collapsed: true,
        });
        assert_eq!(state.agent_mode(), AgentMode::ToolRunning);
    }

    #[test]
    fn agent_mode_reports_streaming_when_no_tool_bubble_is_running() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: "read".to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({}),
                result_section: ResultSection {
                    id: "output".to_string(),
                    content: String::new(),
                },
                content_sections: vec![],
                summary: vec![],
                duration: std::time::Duration::ZERO,
                title_arguments: vec![],
            }),
            collapsed: true,
        });
        assert_eq!(state.agent_mode(), AgentMode::Streaming);
    }

    #[test]
    fn apply_tool_denied_preserves_arguments() {
        let mut state = AppState::new();
        let id = Uuid::new_v4();
        let args = serde_json::json!({"cmd": "rm -rf /"});
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name: "shell".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: args.clone(),
                content_sections: vec![],

                title_arguments: vec![],
            }),
            collapsed: false,
        });
        state.apply_tool_denied(id, "");
        assert!(
            matches!(
                state.chat_entries.last(),
                Some(ChatEntry::ToolBubble {
                    state,
                    ..
                }) if matches!(state.as_ref(), ToolCallState::Denied { arguments, error, .. } if *arguments == args && error == "Denied by user")
            ),
            "apply_tool_denied must produce Denied with the original Pending arguments"
        );
    }

    #[test]
    fn apply_tool_error_skips_already_denied_bubble() {
        let mut state = AppState::new();
        let id = Uuid::new_v4();
        let args = serde_json::json!({"cmd": "ls"});
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name: "shell".to_string(),
            state: Box::new(ToolCallState::Denied {
                arguments: args.clone(),
                error: "Denied by user".to_string(),
                content_sections: vec![],

                title_arguments: vec![],
            }),
            collapsed: false,
        });
        state.apply_tool_error(
            id,
            "Tool 'shell' was denied: Denied by user".to_string(),
            std::time::Duration::ZERO,
            vec![],
        );
        assert!(
            matches!(
                state.chat_entries.last(),
                Some(ChatEntry::ToolBubble {
                    state,
                    ..
                }) if matches!(state.as_ref(), ToolCallState::Denied { arguments, .. } if *arguments == args)
            ),
            "apply_tool_error must not overwrite a Denied bubble"
        );
    }

    #[test]
    fn apply_tool_error_on_running_bubble_produces_error() {
        let mut state = AppState::new();
        let id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name: "shell".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({"cmd": "ls"}),
                content_sections: vec![],

                title_arguments: vec![],
            }),
            collapsed: false,
        });
        state.apply_tool_error(
            id,
            "execution failed".to_string(),
            std::time::Duration::ZERO,
            vec![],
        );
        assert!(
            matches!(
                state.chat_entries.last(),
                Some(ChatEntry::ToolBubble {
                    state,
                    ..
                }) if matches!(state.as_ref(), ToolCallState::Error { error, .. } if error == "execution failed")
            ),
            "Running + ToolError must produce Error"
        );
    }

    // ── Thinking bubble flush / toggle ────────────────────────────────────────

    #[test]
    fn flush_partial_assistant_message_pushes_thinking_before_assistant_bubble() {
        let mut state = AppState::new();
        state.partial_thinking_text = "pondering...".to_string();
        state.partial_assistant_text = "the answer".to_string();
        state.flush_partial_assistant_message(false);

        assert!(state.partial_thinking_text.is_empty());
        assert!(state.partial_assistant_text.is_empty());
        assert_eq!(state.chat_entries.len(), 2);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { text, collapsed, .. } if text == "pondering..." && !*collapsed
        ));
        assert!(matches!(
            &state.chat_entries[1],
            ChatEntry::AssistantMessage { text, interrupted } if text == "the answer" && !*interrupted
        ));
    }

    #[test]
    fn flush_partial_assistant_message_computes_thinking_duration_from_started_at() {
        let mut state = AppState::new();
        state.partial_thinking_text = "pondering...".to_string();
        state.thinking_started_at =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(50));
        state.flush_partial_assistant_message(false);

        assert!(state.thinking_started_at.is_none());
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { duration, .. } if *duration >= std::time::Duration::from_millis(50)
        ));
    }

    #[test]
    fn flush_partial_assistant_message_defaults_thinking_duration_when_not_started() {
        let mut state = AppState::new();
        state.partial_thinking_text = "pondering...".to_string();
        state.flush_partial_assistant_message(false);

        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { duration, .. } if *duration == std::time::Duration::ZERO
        ));
    }

    #[test]
    fn flush_partial_assistant_message_pushes_thinking_only_when_assistant_text_empty() {
        // A thinking-only turn: the model reasons, then calls a tool with no
        // visible prose. Must still flush the thinking bubble, without an
        // empty AssistantMessage.
        let mut state = AppState::new();
        state.partial_thinking_text = "considering options...".to_string();
        state.flush_partial_assistant_message(false);

        assert!(state.partial_thinking_text.is_empty());
        assert_eq!(state.chat_entries.len(), 1);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { text, collapsed, .. } if text == "considering options..." && !*collapsed
        ));
    }

    #[test]
    fn flush_partial_assistant_message_pushes_nothing_when_both_empty() {
        let mut state = AppState::new();
        state.flush_partial_assistant_message(false);
        assert!(state.chat_entries.is_empty());
    }

    #[test]
    fn flush_partial_thinking_message_pushes_thinking_bubble_and_leaves_assistant_text_untouched() {
        let mut state = AppState::new();
        state.partial_thinking_text = "pondering...".to_string();
        state.partial_assistant_text = "so far so good".to_string();
        state.flush_partial_thinking_message();

        assert!(state.partial_thinking_text.is_empty());
        assert_eq!(
            state.partial_assistant_text, "so far so good",
            "must not touch the assistant buffer"
        );
        assert_eq!(state.chat_entries.len(), 1);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { text, .. } if text == "pondering..."
        ));
    }

    #[test]
    fn flush_partial_thinking_message_is_noop_when_no_pending_thinking_text() {
        let mut state = AppState::new();
        state.flush_partial_thinking_message();
        assert!(state.chat_entries.is_empty());
    }

    #[test]
    fn flush_partial_response_message_pushes_assistant_bubble_and_leaves_thinking_text_untouched() {
        let mut state = AppState::new();
        state.partial_assistant_text = "the answer".to_string();
        state.partial_thinking_text = "still pondering...".to_string();
        state.flush_partial_response_message();

        assert!(state.partial_assistant_text.is_empty());
        assert_eq!(
            state.partial_thinking_text, "still pondering...",
            "must not touch the thinking buffer"
        );
        assert_eq!(state.chat_entries.len(), 1);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::AssistantMessage { text, interrupted } if text == "the answer" && !*interrupted
        ));
    }

    #[test]
    fn flush_partial_response_message_is_noop_when_no_pending_assistant_text() {
        let mut state = AppState::new();
        state.flush_partial_response_message();
        assert!(state.chat_entries.is_empty());
    }

    #[test]
    fn toggle_thinking_collapsed_flips_state_at_index() {
        let mut state = AppState::new();
        state.chat_entries.push(ChatEntry::ThinkingMessage {
            text: "reasoning".to_string(),
            collapsed: true,
            duration: std::time::Duration::from_secs(3),
        });
        state.scroll_locked = true;

        state.toggle_thinking_collapsed(0);

        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { collapsed, .. } if !*collapsed
        ));
        assert!(!state.scroll_locked);
    }

    #[test]
    fn toggle_partial_thinking_collapsed_flips_state_while_streaming() {
        let mut state = AppState::new();
        state.partial_thinking_text = "pondering...".to_string();
        state.partial_thinking_collapsed = false;
        state.scroll_locked = true;

        state.toggle_partial_thinking_collapsed();

        assert!(state.partial_thinking_collapsed);
        assert!(!state.scroll_locked);

        state.toggle_partial_thinking_collapsed();

        assert!(!state.partial_thinking_collapsed);
    }

    #[test]
    fn toggle_partial_thinking_collapsed_clamps_scroll_offset_to_bubble_start_row() {
        let mut state = AppState::new();
        state.partial_thinking_text = "pondering...".to_string();
        // Simulate a previous render pass recording the in-flight bubble's
        // title row under the streaming sentinel index.
        state.thinking_title_rows = vec![(5, 40, STREAMING_THINKING_ENTRY_INDEX)];
        state.scroll_offset = 30;
        state.scroll_locked = false;

        state.toggle_partial_thinking_collapsed();

        assert_eq!(state.scroll_offset, 5);
    }

    #[test]
    fn toggle_thinking_collapsed_clamps_scroll_offset_to_bubble_start_row() {
        let mut state = AppState::new();
        state.chat_entries.push(ChatEntry::ThinkingMessage {
            text: "reasoning".to_string(),
            collapsed: false,
            duration: std::time::Duration::from_secs(3),
        });
        // Simulate a previous render pass recording this bubble's title row
        // and a viewport scrolled well below it (e.g. the long expanded body
        // pushed the view down).
        state.thinking_title_rows = vec![(5, 40, 0)];
        state.scroll_offset = 30;
        state.scroll_locked = false;

        state.toggle_thinking_collapsed(0);

        // The offset must not exceed the bubble's own start row, otherwise
        // the title scrolls out of the viewport once the body collapses.
        assert_eq!(state.scroll_offset, 5);
    }

    #[test]
    fn toggle_tool_bubble_collapsed_clamps_scroll_offset_to_bubble_start_row() {
        let mut state = AppState::new();
        let id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name: "read".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
            }),
            collapsed: false,
        });
        // Simulate a previous render pass recording this bubble's title row
        // and a viewport scrolled well below it (e.g. the long expanded body
        // pushed the view down).
        state.bubble_title_rows = vec![(5, 40, id)];
        state.scroll_offset = 30;
        state.scroll_locked = false;

        state.toggle_tool_bubble_collapsed(id);

        // The offset must not exceed the bubble's own start row, otherwise
        // the title scrolls out of the viewport once the body collapses.
        assert_eq!(state.scroll_offset, 5);
    }

    #[test]
    fn toggle_thinking_collapsed_ignores_out_of_range_or_wrong_variant_index() {
        let mut state = AppState::new();
        state.chat_entries.push(ChatEntry::AssistantMessage {
            text: "hi".to_string(),
            interrupted: false,
        });
        // Neither index should panic or mutate anything.
        state.toggle_thinking_collapsed(0);
        state.toggle_thinking_collapsed(5);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::AssistantMessage { .. }
        ));
    }
}
