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

use crate::app::{ChatEntry, ChatSelection, HistoryDirection};

// ─── Duration constant ───────────────────────────────────────────────────────

/// Duration a toast notification remains visible before it auto-dismisses.
const TOAST_DURATION: Duration = Duration::from_millis(1500);

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
    /// `AgentCommand::Stop` has been sent; waiting for `StreamCancelled` or
    /// the agent to become idle.
    Aborting,
    /// First Ctrl+C has been received; waiting for a second press within the
    /// confirm window to actually quit/stop, or for the window to expire.
    DoublePressAbort {
        pressed_at: Instant,
        /// The mode to restore when the confirm window expires.
        return_to: Box<TuiMode>,
    },
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
    },
    /// A tool call started executing (approved).
    ToolStart {
        id: Uuid,
        name: String,
        arguments: serde_json::Value,
    },
    /// A tool call completed successfully.
    ToolDone {
        id: Uuid,
        name: String,
        result: String,
        summary: Vec<(String, String)>,
        duration: std::time::Duration,
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
    /// Feed a key event into the textarea.
    TextareaKey(KeyEvent),
    /// Feed a pasted string (dedented) into the textarea.
    Paste(String),
    /// Send `AgentCommand::ApproveTool { id }` to the agent.
    ApproveTool { id: Uuid },
    /// Send `AgentCommand::DenyTool { id, reason }` to the agent.
    DenyTool { id: Uuid },
    /// Send `AgentCommand::Stop` to the agent.
    StopAgent,
    /// Quit the TUI event loop.
    Quit,
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
}

// ─── AppState ────────────────────────────────────────────────────────────────

/// The authoritative TUI view-model owned by the Driver.
///
/// Fields that belong exclusively to the Driver (`mouse_drag`, `button_areas`)
/// are **not** included here.
pub struct AppState {
    pub mode: TuiMode,
    pub focus: Focus,
    pub layout: AppLayout,

    // ── chat view-model ─────────────────────────────────────────────────────
    pub chat_entries: Vec<ChatEntry>,
    pub partial_assistant_text: String,

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

    // ── input field ─────────────────────────────────────────────────────────
    /// Mirrors ratatui-textarea's internal scroll-top so scroll indicators
    /// reflect the actual rendered viewport position.
    pub input_scroll_top: usize,

    // ── bubble hit-testing ──────────────────────────────────────────────────
    /// Maps each rendered tool bubble to its absolute line-index range
    /// `(start_row, end_row, id)` (inclusive, before scroll offset).
    pub bubble_title_rows: Vec<(usize, usize, Uuid)>,

    // ── history navigation helper ────────────────────────────────────────────
    /// Exposes history navigation for use by the presenter.
    pub _history_direction: Option<HistoryDirection>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            mode: TuiMode::Idle,
            focus: Focus::MainInput,
            layout: AppLayout::default(),

            chat_entries: Vec::new(),
            partial_assistant_text: String::new(),

            scroll_offset: 0,
            scroll_locked: true,
            chat_area_height: 0,

            pending_approval_input_snapshot: None,

            input_history: Vec::new(),
            history_index: None,
            draft: String::new(),

            steer_indicator: false,

            chat_rendered_lines: Vec::new(),
            chat_selection: None,

            toast: None,
            spinner_frame: 0,

            help_scroll_offset: 0,
            help_scroll_x: 0,

            input_scroll_top: 0,

            bubble_title_rows: Vec::new(),

            _history_direction: None,
        }
    }

    /// Returns `true` if a toast is active and has not yet expired.
    pub fn has_active_toast(&self) -> bool {
        self.toast
            .as_ref()
            .map(|(_, start)| start.elapsed() < TOAST_DURATION)
            .unwrap_or(false)
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
        };
        if let TuiMode::DoublePressAbort { return_to, .. } = &mode {
            assert_eq!(**return_to, TuiMode::Idle);
        } else {
            panic!("expected DoublePressAbort");
        }
    }
}
