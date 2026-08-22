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

use crate::agent::{AgentMode, UiUpdate};

// ─── Duration constant ───────────────────────────────────────────────────────

/// Duration a toast notification remains visible before it auto-dismisses.
const TOAST_DURATION: Duration = Duration::from_millis(1500);

// ─── ChatSelection ───────────────────────────────────────────────────────────

/// Screen-space selection state for the chat area. Line and column are
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
    /// mid-stream correction. Visually distinct from a regular `Sent` bubble
    /// so the user can always tell which messages were steers.
    Steered,
    /// Cancelled: stream was interrupted before injection; text retained in
    /// input_history so the user can recover via ↑.
    Cancelled,
}

/// State of a tool call bubble in the TUI chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolBubbleState {
    /// Awaiting user approval; arguments are shown so the user knows what the tool will do.
    Pending { arguments: serde_json::Value },
    /// Approved and running.
    Running { arguments: serde_json::Value },
    /// Completed successfully.
    Done {
        arguments: serde_json::Value,
        result: String,
        summary: Vec<(String, String)>,
        duration: std::time::Duration,
    },
    /// Tool returned an error.
    Error { error: String },
    /// Tool was killed (stream interrupted while tool was running).
    Killed,
    /// Tool was denied by the user; error message is the denial reason.
    Denied { error: String },
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
    /// A single bubble representing all lifecycle stages of one tool invocation.
    ToolBubble {
        id: uuid::Uuid,
        name: String,
        state: ToolBubbleState,
        /// Whether the bubble body (arguments, result) is hidden; only the title
        /// line is rendered when `true`. Toggled by clicking the title line.
        collapsed: bool,
    },
    ErrorLine {
        message: String,
    },
}

/// Direction for [`TuiState::navigate_history`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDirection {
    Up,
    Down,
}

// ─── TuiState ────────────────────────────────────────────────────────────────

/// The TUI's live rendering state. Populated exclusively by
/// [`TuiState::apply_update`] (from `UiUpdate`) and the driver helpers below.
pub struct TuiState {
    pub agent_mode: AgentMode,
    pub chat_entries: Vec<ChatEntry>,
    pub partial_assistant_text: String,
    pub show_help: bool,
    /// true while ≥1 steer is queued.
    pub steer_indicator: bool,
    pub scroll_offset: usize,
    /// true = auto-scroll to bottom.
    pub scroll_locked: bool,
    /// updated on every resize.
    pub chat_area_height: u16,
    /// Set by `UiUpdate::ToolPending`; cleared by `ToolStart`/`ToolKilled`/
    /// `StreamCancelled`. While `Some`, the TUI is in approval sub-mode.
    pub pending_approval: Option<(Uuid, String, serde_json::Value)>, // (tool_id, tool_name, arguments)
    /// Textarea lines saved when a `ToolPending` approval sub-mode starts so
    /// the unsubmitted draft can be restored once the approval is resolved.
    /// `None` when no approval is in progress or the input was empty.
    pub pending_approval_input_snapshot: Option<Vec<String>>,
    /// All `UserMessage` texts, oldest → newest (including pending and
    /// cancelled steers, so the user can recover them via ↑).
    pub input_history: Vec<String>,
    /// `None` = live draft; `Some(i)` = browsing history at index `i`.
    pub history_index: Option<usize>,
    /// Saved live draft while browsing.
    pub draft: String,
    /// On-screen rect of the input box, written each frame by `ui::draw` for
    /// mouse hit-testing (click-to-cursor / drag-select).
    pub input_area: InputArea,
    /// On-screen rect of the chat content area (not the scrollbar column),
    /// written each frame by `ui::draw_chat` for mouse hit-testing.
    pub chat_area: ratatui::layout::Rect,
    /// Plain text of every rendered line built by the most recent `draw_chat`
    /// call. Indexed by absolute line number (before scroll offset). Used for
    /// extracting selected text on mouse-up.
    pub chat_rendered_lines: Vec<String>,
    /// Active or committed chat selection. `None` = no selection.
    pub chat_selection: Option<ChatSelection>,
    /// True while the left mouse button is held down in the chat area (drag
    /// in progress). Cleared on mouse-up.
    pub chat_drag_active: bool,
    /// Transient notification displayed as a floating overlay, auto-dismissed
    /// after [`TOAST_DURATION`]. Set by e.g. clipboard copy; `None` when no
    /// notification is active.
    pub toast: Option<(String, Instant)>,
    /// Incremented on every tick while the agent is busy; used to animate the
    /// footer spinner.
    pub spinner_frame: u8,
    /// Current scroll offset inside the help popup (lines from the top of the
    /// content). Clamped to the valid range by `draw_help_popup` each frame.
    pub help_scroll_offset: usize,
    /// Current horizontal scroll offset inside the help popup (columns from
    /// the left). Clamped to the valid range by `draw_help_popup` each frame.
    pub help_scroll_x: usize,
    /// Mirrors ratatui-textarea's internal scroll-top (in wrapped screen rows)
    /// so that the input-field scroll indicators reflect the actual rendered
    /// viewport position rather than the cursor row.
    pub input_scroll_top: usize,
    /// Maps each rendered tool bubble to its absolute line-index range
    /// `(start_row, end_row, id)` (inclusive, before scroll offset). Populated
    /// by `ui::draw_chat` every frame. Used to route click events to the
    /// correct bubble's expand/collapse toggle: a click on any row in
    /// `[start_row, end_row]` at column 0 (the border bar) toggles the bubble,
    /// as does a click anywhere on the title row regardless of column.
    pub bubble_title_rows: Vec<(usize, usize, uuid::Uuid)>,
}

impl TuiState {
    pub fn new() -> Self {
        TuiState {
            agent_mode: AgentMode::Idle,
            chat_entries: Vec::new(),
            partial_assistant_text: String::new(),
            show_help: false,
            steer_indicator: false,
            scroll_offset: 0,
            scroll_locked: true,
            chat_area_height: 0,
            pending_approval: None,
            pending_approval_input_snapshot: None,
            input_history: Vec::new(),
            history_index: None,
            draft: String::new(),
            input_area: ratatui::layout::Rect::default(),
            chat_area: ratatui::layout::Rect::default(),
            chat_rendered_lines: Vec::new(),
            chat_selection: None,
            chat_drag_active: false,
            toast: None,
            spinner_frame: 0,
            help_scroll_offset: 0,
            help_scroll_x: 0,
            input_scroll_top: 0,
            bubble_title_rows: Vec::new(),
        }
    }

    /// Commit the in-flight partial assistant text into a full
    /// `AssistantMessage` entry and clear the partial buffer.
    ///
    /// Any steer (`UserMessage { state: Queued }`) entries that arrived while
    /// this stream was in progress are currently sitting *before* the not-yet-
    /// committed assistant response in `chat_entries`. They are lifted out,
    /// the `AssistantMessage` is appended first, and then the steers are
    /// re-appended so the logical order is always:
    /// `… → AssistantMessage → QueuedSteer → …`
    fn commit_partial(&mut self, interrupted: bool) {
        if !self.partial_assistant_text.is_empty() {
            let queued_steers: Vec<ChatEntry> = self
                .chat_entries
                .iter()
                .filter(|entry| {
                    matches!(
                        entry,
                        ChatEntry::UserMessage {
                            state: UserMessageState::Queued,
                            ..
                        }
                    )
                })
                .cloned()
                .collect();
            self.chat_entries.retain(|entry| {
                !matches!(
                    entry,
                    ChatEntry::UserMessage {
                        state: UserMessageState::Queued,
                        ..
                    }
                )
            });
            self.chat_entries.push(ChatEntry::AssistantMessage {
                text: std::mem::take(&mut self.partial_assistant_text),
                interrupted,
            });
            self.chat_entries.extend(queued_steers);
        } else {
            self.partial_assistant_text.clear();
        }
    }

    fn mark_queued_as(&mut self, state: UserMessageState) {
        for entry in self.chat_entries.iter_mut() {
            if let ChatEntry::UserMessage {
                state: entry_state, ..
            } = entry
            {
                if *entry_state == UserMessageState::Queued {
                    *entry_state = state;
                }
            }
        }
    }

    fn find_tool_bubble_mut(&mut self, target_id: Uuid) -> Option<&mut ChatEntry> {
        self.chat_entries
            .iter_mut()
            .rev()
            .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == target_id))
    }

    /// Toggle the collapsed/expanded state of the tool bubble with the given
    /// `id`. A no-op when no matching bubble is found.
    ///
    /// Disables auto-scroll so the bubble's top edge stays at the same screen
    /// position after the content height changes. `draw_chat` writes the
    /// effective `scroll_offset` back to `state` every frame, so the correct
    /// offset is already present when this is called.
    pub fn toggle_tool_bubble_collapsed(&mut self, target_id: Uuid) {
        for entry in &mut self.chat_entries {
            if let ChatEntry::ToolBubble { id, collapsed, .. } = entry {
                if *id == target_id {
                    *collapsed = !*collapsed;
                    self.scroll_locked = false;
                    return;
                }
            }
        }
    }

    /// Apply a single `UiUpdate` to the view state.
    pub fn apply_update(&mut self, update: &UiUpdate) {
        match update {
            UiUpdate::StreamStarted => {
                // Commit any text that accumulated during the previous stream
                // (e.g. prose that preceded tool calls) so it is preserved as
                // a permanent AssistantMessage bubble before the new stream
                // clears the partial buffer.
                self.commit_partial(false);
                // While aborting, keep the Aborting mode so the TUI can
                // detect the eventual Idle transition and quit.
                if self.agent_mode != AgentMode::Aborting {
                    self.agent_mode = AgentMode::Streaming;
                }
                self.partial_assistant_text.clear();
            }
            UiUpdate::AppendText(text) => {
                self.partial_assistant_text.push_str(text);
                if self.scroll_locked {
                    self.scroll_offset = 0;
                }
            }
            UiUpdate::StreamDone => {
                self.commit_partial(false);
                self.agent_mode = AgentMode::Idle;
                self.scroll_locked = true;
                self.scroll_offset = 0;
            }
            UiUpdate::StreamCancelled => {
                self.commit_partial(true);
                self.mark_queued_as(UserMessageState::Cancelled);
                self.steer_indicator = false;
                self.pending_approval = None;
                self.agent_mode = AgentMode::Idle;
                self.scroll_locked = true;
                self.scroll_offset = 0;
            }
            UiUpdate::SteerQueued => {
                self.steer_indicator = true;
            }
            UiUpdate::SteerApplied => {
                self.mark_queued_as(UserMessageState::Steered);
                self.steer_indicator = false;
            }
            UiUpdate::ToolPending {
                name,
                id,
                arguments,
            } => {
                // While aborting, skip approval sub-mode entirely: the agent
                // will process the pending Stop command and deny this tool on
                // its own; showing the approval UI here would be misleading.
                if self.agent_mode == AgentMode::Aborting {
                    return;
                }
                // Commit any text that arrived in the same stream as the tool
                // calls (prose before/alongside the call) so it is preserved
                // as a permanent AssistantMessage bubble.
                self.commit_partial(false);
                self.chat_entries.push(ChatEntry::ToolBubble {
                    id: *id,
                    name: name.clone(),
                    state: ToolBubbleState::Pending {
                        arguments: arguments.clone(),
                    },
                    collapsed: true,
                });
                self.pending_approval = Some((*id, name.clone(), arguments.clone()));
            }
            UiUpdate::ToolStart {
                name,
                id,
                arguments,
                ..
            } => {
                if let Some(entry) = self.find_tool_bubble_mut(*id) {
                    if let ChatEntry::ToolBubble { state, .. } = entry {
                        *state = ToolBubbleState::Running {
                            arguments: arguments.clone(),
                        };
                    }
                } else {
                    self.chat_entries.push(ChatEntry::ToolBubble {
                        id: *id,
                        name: name.clone(),
                        state: ToolBubbleState::Running {
                            arguments: arguments.clone(),
                        },
                        collapsed: true,
                    });
                }
                self.pending_approval = None;
            }
            UiUpdate::ToolDone {
                id,
                name,
                result,
                summary,
                duration,
            } => {
                let previous_arguments = self
                    .find_tool_bubble_mut(*id)
                    .and_then(|entry| {
                        if let ChatEntry::ToolBubble { state, .. } = entry {
                            match state {
                                ToolBubbleState::Running { arguments }
                                | ToolBubbleState::Pending { arguments } => Some(arguments.clone()),
                                _ => None,
                            }
                        } else {
                            None
                        }
                    })
                    .unwrap_or(serde_json::Value::Null);
                if let Some(entry) = self.find_tool_bubble_mut(*id) {
                    if let ChatEntry::ToolBubble { state, .. } = entry {
                        *state = ToolBubbleState::Done {
                            arguments: previous_arguments,
                            result: result.clone(),
                            summary: summary.clone(),
                            duration: *duration,
                        };
                    }
                } else {
                    self.chat_entries.push(ChatEntry::ToolBubble {
                        id: *id,
                        name: name.clone(),
                        state: ToolBubbleState::Done {
                            arguments: serde_json::Value::Null,
                            result: result.clone(),
                            summary: summary.clone(),
                            duration: *duration,
                        },
                        collapsed: true,
                    });
                }
            }
            UiUpdate::ToolError { id, error } => {
                if let Some(entry) = self.find_tool_bubble_mut(*id) {
                    if let ChatEntry::ToolBubble { state, .. } = entry {
                        *state = ToolBubbleState::Error {
                            error: error.clone(),
                        };
                    }
                } else {
                    self.chat_entries.push(ChatEntry::ErrorLine {
                        message: error.clone(),
                    });
                }
                // When the errored tool was the one awaiting approval, the
                // agent aborted it without reprompting (e.g. via Stop) — clear
                // the approval footer and return to idle.
                if self
                    .pending_approval
                    .as_ref()
                    .is_some_and(|(pending_id, _, _)| pending_id == id)
                {
                    self.pending_approval = None;
                    self.agent_mode = AgentMode::Idle;
                }
            }
            UiUpdate::ToolKilled { id } => {
                if let Some(entry) = self.find_tool_bubble_mut(*id) {
                    if let ChatEntry::ToolBubble { state, .. } = entry {
                        *state = ToolBubbleState::Killed;
                    }
                } else {
                    self.chat_entries.push(ChatEntry::ToolBubble {
                        id: *id,
                        name: String::new(),
                        state: ToolBubbleState::Killed,
                        collapsed: true,
                    });
                }
                self.pending_approval = None;
            }
            UiUpdate::Error(message) => {
                self.chat_entries.push(ChatEntry::ErrorLine {
                    message: message.clone(),
                });
                self.agent_mode = AgentMode::Idle;
            }
        }
    }

    /// Called by the driver before sending `AgentCommand::Prompt`.
    /// `state == Queued` when `agent_mode != Idle` (steer); `Sent` otherwise.
    /// Appends text to `input_history` and resets `history_index` to `None`.
    pub fn push_user_bubble(&mut self, text: String, state: UserMessageState) {
        self.chat_entries.push(ChatEntry::UserMessage {
            text: text.clone(),
            state,
        });
        self.input_history.push(text);
        self.history_index = None;
    }

    /// Called when the user scrolls up (toward earlier content) — disables
    /// auto-scroll and decreases the row offset. Clamped at 0 (the top).
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_locked = false;
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    /// Called when the user scrolls down (toward later content) — disables
    /// auto-scroll and increases the row offset. The upper bound is applied in
    /// `ui::draw`, which knows the rendered content height.
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_locked = false;
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }

    /// Scroll the help popup up (toward earlier content).
    pub fn help_scroll_up(&mut self, lines: usize) {
        self.help_scroll_offset = self.help_scroll_offset.saturating_sub(lines);
    }

    /// Scroll the help popup down (toward later content). The upper bound is
    /// applied in `draw_help_popup`, which knows the total content height.
    pub fn help_scroll_down(&mut self, lines: usize) {
        self.help_scroll_offset = self.help_scroll_offset.saturating_add(lines);
    }

    /// Scroll the help popup left (toward earlier columns).
    pub fn help_scroll_left(&mut self, columns: usize) {
        self.help_scroll_x = self.help_scroll_x.saturating_sub(columns);
    }

    /// Scroll the help popup right (toward later columns). The upper bound is
    /// applied in `draw_help_popup`, which knows the maximum line width.
    pub fn help_scroll_right(&mut self, columns: usize) {
        self.help_scroll_x = self.help_scroll_x.saturating_add(columns);
    }

    /// Called on End key or scroll-to-bottom event — re-enables auto-scroll.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_locked = true;
        self.scroll_offset = 0;
    }

    /// Extract the text covered by the current `chat_selection` from
    /// `chat_rendered_lines`. Returns `None` when there is no selection or the
    /// selection is empty (anchor == cursor).
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
        if start_line == end_line {
            let line = &lines[start_line];
            Some(char_range(line, start_col, end_col))
        } else {
            let mut result = String::new();
            for (absolute_index, line) in lines
                .iter()
                .enumerate()
                .skip(start_line)
                .take(end_line - start_line + 1)
            {
                let fragment = if absolute_index == start_line {
                    char_range(line, start_col, line.chars().count())
                } else if absolute_index == end_line {
                    char_range(line, 0, end_col)
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

    /// Show a transient toast notification for [`TOAST_DURATION`]. Replaces
    /// any currently visible toast.
    pub fn set_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now() + TOAST_DURATION));
    }

    /// Remove an expired toast. Called once per frame before rendering so
    /// the overlay disappears on time without requiring a separate timer.
    pub fn clear_expired_toast(&mut self) {
        if let Some((_, expires_at)) = &self.toast {
            if Instant::now() >= *expires_at {
                self.toast = None;
            }
        }
    }

    /// Navigate input history. Returns the text to load into the textarea,
    /// or `None` if the boundary was already reached (no change).
    ///
    /// - `Up` from live draft: save current textarea content as draft, move to
    ///   the newest history entry.
    /// - `Up` at oldest entry: no change.
    /// - `Down` at newest entry: restore the saved draft (exit browsing).
    /// - `Down` at live draft: no change.
    pub fn navigate_history(
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
            HistoryDirection::Down => {
                match self.history_index {
                    None => None,
                    Some(index) if index < self.input_history.len() - 1 => {
                        self.history_index = Some(index + 1);
                        Some(self.input_history[index + 1].clone())
                    }
                    Some(_) => {
                        // At newest entry: restore the saved draft and exit.
                        self.history_index = None;
                        Some(std::mem::take(&mut self.draft))
                    }
                }
            }
        }
    }

    /// Exit history browsing (if active), copying the browsed entry into the
    /// live draft, before an edit keypress is processed.
    pub fn exit_browsing(&mut self, current_text: &str) {
        if self.history_index.is_some() {
            self.draft = current_text.to_string();
            self.history_index = None;
        }
    }
}

impl Default for TuiState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── TuiState tests (moved from app.rs) ──────────────────────────────────────

#[cfg(test)]
mod tui_state_tests {
    use super::*;
    use uuid::Uuid;

    fn tool_pending() -> UiUpdate {
        UiUpdate::ToolPending {
            name: "read".to_string(),
            id: Uuid::new_v4(),
            arguments: serde_json::json!({}),
        }
    }

    #[test]
    fn pending_transitions_to_correct_mode_on_stream_events() {
        let mut state = TuiState::new();

        state.agent_mode = AgentMode::Pending;
        state.apply_update(&UiUpdate::StreamStarted);
        assert_eq!(
            state.agent_mode,
            AgentMode::Streaming,
            "StreamStarted → Streaming"
        );

        state.agent_mode = AgentMode::Pending;
        state.apply_update(&UiUpdate::StreamDone);
        assert_eq!(state.agent_mode, AgentMode::Idle, "StreamDone → Idle");

        state.agent_mode = AgentMode::Pending;
        state.apply_update(&UiUpdate::StreamCancelled);
        assert_eq!(state.agent_mode, AgentMode::Idle, "StreamCancelled → Idle");

        state.agent_mode = AgentMode::Pending;
        state.apply_update(&UiUpdate::Error("boom".to_string()));
        assert_eq!(state.agent_mode, AgentMode::Idle, "Error → Idle");
    }

    #[test]
    fn stream_started_sets_streaming_mode_and_clears_partial() {
        let mut state = TuiState::new();
        state.apply_update(&UiUpdate::AppendText("hello".to_string()));
        state.apply_update(&UiUpdate::StreamStarted);
        assert_eq!(state.agent_mode, AgentMode::Streaming);
        assert_eq!(state.partial_assistant_text, "");
    }

    #[test]
    fn append_text_accumulates_partial() {
        let mut state = TuiState::new();
        state.apply_update(&UiUpdate::StreamStarted);
        state.apply_update(&UiUpdate::AppendText("foo".to_string()));
        state.apply_update(&UiUpdate::AppendText("bar".to_string()));
        assert_eq!(state.partial_assistant_text, "foobar");
    }

    #[test]
    fn stream_done_commits_assistant_and_idles() {
        let mut state = TuiState::new();
        state.apply_update(&UiUpdate::StreamStarted);
        state.apply_update(&UiUpdate::AppendText("done".to_string()));
        state.apply_update(&UiUpdate::StreamDone);
        assert_eq!(state.agent_mode, AgentMode::Idle);
        assert_eq!(state.partial_assistant_text, "");
        assert_eq!(
            state.chat_entries.last(),
            Some(&ChatEntry::AssistantMessage {
                text: "done".to_string(),
                interrupted: false
            })
        );
        assert!(state.scroll_locked);
    }

    #[test]
    fn stream_cancelled_marks_queued_cancelled_and_clears_approval() {
        let mut state = TuiState::new();
        state.push_user_bubble("steer".to_string(), UserMessageState::Queued);
        state.apply_update(&tool_pending());
        assert!(state.pending_approval.is_some());
        state.apply_update(&UiUpdate::StreamStarted);
        state.apply_update(&UiUpdate::AppendText("partial".to_string()));
        state.apply_update(&UiUpdate::StreamCancelled);
        assert_eq!(state.agent_mode, AgentMode::Idle);
        assert_eq!(state.pending_approval, None);
        assert!(!state.steer_indicator);
        assert_eq!(
            state.chat_entries[1],
            ChatEntry::AssistantMessage {
                text: "partial".to_string(),
                interrupted: true
            }
        );
        assert_eq!(
            state.chat_entries.last(),
            Some(&ChatEntry::UserMessage {
                text: "steer".to_string(),
                state: UserMessageState::Cancelled
            })
        );
    }

    #[test]
    fn steer_queued_sets_indicator_and_steer_applied_marks_sent() {
        let mut state = TuiState::new();
        state.push_user_bubble("a".to_string(), UserMessageState::Queued);
        state.push_user_bubble("b".to_string(), UserMessageState::Queued);
        state.apply_update(&UiUpdate::SteerQueued);
        assert!(state.steer_indicator);
        state.apply_update(&UiUpdate::SteerApplied);
        assert!(!state.steer_indicator);
        assert_eq!(
            state.chat_entries[0],
            ChatEntry::UserMessage {
                text: "a".to_string(),
                state: UserMessageState::Steered
            }
        );
        assert_eq!(
            state.chat_entries[1],
            ChatEntry::UserMessage {
                text: "b".to_string(),
                state: UserMessageState::Steered
            }
        );
    }

    #[test]
    fn tool_pending_sets_approval_and_tool_start_clears_it() {
        let mut state = TuiState::new();
        let id = Uuid::new_v4();
        let args = serde_json::json!({"path": "foo.txt"});
        state.apply_update(&UiUpdate::ToolPending {
            name: "write".to_string(),
            id,
            arguments: args.clone(),
        });
        assert_eq!(
            state.pending_approval,
            Some((id, "write".to_string(), args.clone()))
        );
        assert!(matches!(
            state.chat_entries.last(),
            Some(ChatEntry::ToolBubble {
                name,
                state: ToolBubbleState::Pending { .. },
                ..
            }) if name == "write"
        ));
        state.apply_update(&UiUpdate::ToolStart {
            name: "write".to_string(),
            id,
            arguments: serde_json::json!({}),
        });
        assert_eq!(state.pending_approval, None);
        assert!(matches!(
            state.chat_entries.last(),
            Some(ChatEntry::ToolBubble {
                state: ToolBubbleState::Running { .. },
                ..
            })
        ));
    }

    #[test]
    fn tool_error_becomes_error_line() {
        let mut state = TuiState::new();
        let id = Uuid::new_v4();
        state.apply_update(&UiUpdate::ToolPending {
            name: "read".to_string(),
            id,
            arguments: serde_json::json!({}),
        });
        state.apply_update(&UiUpdate::ToolError {
            id,
            error: "boom".to_string(),
        });
        assert!(matches!(
            state.chat_entries.last(),
            Some(ChatEntry::ToolBubble {
                state: ToolBubbleState::Error { error },
                ..
            }) if error == "boom"
        ));
    }

    #[test]
    fn error_update_idles() {
        let mut state = TuiState::new();
        state.apply_update(&UiUpdate::StreamStarted);
        state.apply_update(&UiUpdate::Error("fatal".to_string()));
        assert_eq!(state.agent_mode, AgentMode::Idle);
        assert_eq!(
            state.chat_entries.last(),
            Some(&ChatEntry::ErrorLine {
                message: "fatal".to_string()
            })
        );
    }

    #[test]
    fn push_user_bubble_appends_history_and_resets_browsing() {
        let mut state = TuiState::new();
        state.push_user_bubble("first".to_string(), UserMessageState::Sent);
        state.push_user_bubble("second".to_string(), UserMessageState::Queued);
        assert_eq!(state.input_history, vec!["first", "second"]);
        assert_eq!(state.history_index, None);
        assert_eq!(
            state.chat_entries[0],
            ChatEntry::UserMessage {
                text: "first".to_string(),
                state: UserMessageState::Sent
            }
        );
    }

    #[test]
    fn scroll_up_moves_toward_top_and_disables_auto_scroll() {
        let mut state = TuiState::new();
        assert!(state.scroll_locked);
        state.scroll_down(5);
        assert!(!state.scroll_locked);
        assert_eq!(state.scroll_offset, 5);
        state.scroll_up(2);
        assert!(!state.scroll_locked);
        assert_eq!(state.scroll_offset, 3);
        state.scroll_up(10);
        assert_eq!(state.scroll_offset, 0, "clamped at the top");
        state.scroll_to_bottom();
        assert!(state.scroll_locked);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_increases_offset_toward_bottom() {
        let mut state = TuiState::new();
        state.scroll_down(4);
        assert!(!state.scroll_locked);
        assert_eq!(state.scroll_offset, 4);
        state.scroll_down(3);
        assert_eq!(state.scroll_offset, 7);
        assert!(!state.scroll_locked);
    }

    #[test]
    fn navigate_history_up_down_and_draft_restore() {
        let mut state = TuiState::new();
        state.push_user_bubble("old".to_string(), UserMessageState::Sent);
        state.push_user_bubble("new".to_string(), UserMessageState::Sent);

        let text = state
            .navigate_history(HistoryDirection::Up, "live draft")
            .expect("moves to newest");
        assert_eq!(text, "new");
        assert_eq!(state.history_index, Some(1));

        let text = state
            .navigate_history(HistoryDirection::Up, "new")
            .expect("moves to oldest");
        assert_eq!(text, "old");
        assert_eq!(state.history_index, Some(0));

        assert_eq!(state.navigate_history(HistoryDirection::Up, "old"), None);
        assert_eq!(state.history_index, Some(0));

        let text = state
            .navigate_history(HistoryDirection::Down, "old")
            .expect("moves forward");
        assert_eq!(text, "new");
        assert_eq!(state.history_index, Some(1));

        let text = state
            .navigate_history(HistoryDirection::Down, "new")
            .expect("restores draft");
        assert_eq!(text, "live draft");
        assert_eq!(state.history_index, None);
    }

    #[test]
    fn navigate_history_down_from_live_draft_is_noop() {
        let mut state = TuiState::new();
        state.push_user_bubble("only".to_string(), UserMessageState::Sent);
        assert_eq!(state.navigate_history(HistoryDirection::Down, ""), None);
    }

    #[test]
    fn navigate_history_up_on_empty_history_is_noop() {
        let mut state = TuiState::new();
        assert_eq!(state.navigate_history(HistoryDirection::Up, ""), None);
    }

    #[test]
    fn exit_browsing_saves_current_text_as_draft() {
        let mut state = TuiState::new();
        state.push_user_bubble("history".to_string(), UserMessageState::Sent);
        state
            .navigate_history(HistoryDirection::Up, "")
            .expect("enters browsing");
        assert!(state.history_index.is_some());
        state.exit_browsing("history");
        assert_eq!(state.history_index, None);
        assert_eq!(state.draft, "history");
    }

    #[test]
    fn set_toast_stores_message_and_expiry() {
        let before = Instant::now();
        let mut state = TuiState::new();
        state.set_toast("hello");
        let (msg, expires_at) = state.toast.as_ref().expect("toast set");
        assert_eq!(msg, "hello");
        assert!(*expires_at >= before + Duration::from_secs(1));
        assert!(*expires_at <= before + Duration::from_secs(4));
    }

    #[test]
    fn clear_expired_toast_removes_past_toast_and_keeps_future_toast() {
        let mut state = TuiState::new();
        state.toast = Some(("stale".to_string(), Instant::now() - Duration::from_secs(1)));
        state.clear_expired_toast();
        assert!(state.toast.is_none(), "expired toast must be cleared");

        state.set_toast("fresh");
        state.clear_expired_toast();
        assert!(
            state.toast.is_some(),
            "non-expired toast must not be cleared"
        );
    }

    #[test]
    fn chat_selection_extract_empty_returns_none() {
        let mut state = TuiState::new();
        state.chat_rendered_lines = vec!["hello world".to_string()];
        state.chat_selection = Some(ChatSelection {
            anchor_line: 0,
            anchor_col: 3,
            cursor_line: 0,
            cursor_col: 3,
        });
        assert_eq!(state.extract_chat_selection_text(), None);
    }

    #[test]
    fn chat_selection_extract_single_line() {
        let mut state = TuiState::new();
        state.chat_rendered_lines = vec!["hello world".to_string()];
        state.chat_selection = Some(ChatSelection {
            anchor_line: 0,
            anchor_col: 6,
            cursor_line: 0,
            cursor_col: 11,
        });
        assert_eq!(
            state.extract_chat_selection_text(),
            Some("world".to_string())
        );
    }

    #[test]
    fn chat_selection_extract_multi_line() {
        let mut state = TuiState::new();
        state.chat_rendered_lines = vec![
            "line one".to_string(),
            "line two".to_string(),
            "line three".to_string(),
        ];
        state.chat_selection = Some(ChatSelection {
            anchor_line: 0,
            anchor_col: 5,
            cursor_line: 2,
            cursor_col: 4,
        });
        let result = state
            .extract_chat_selection_text()
            .expect("should have selection");
        assert_eq!(result, "one\nline two\nline");
    }

    #[test]
    fn chat_selection_normalized_reverses_if_anchor_after_cursor() {
        let selection = ChatSelection {
            anchor_line: 5,
            anchor_col: 10,
            cursor_line: 2,
            cursor_col: 3,
        };
        let (start_line, start_col, end_line, end_col) = selection.normalized();
        assert_eq!(start_line, 2);
        assert_eq!(start_col, 3);
        assert_eq!(end_line, 5);
        assert_eq!(end_col, 10);
    }

    #[test]
    fn tool_start_updates_existing_bubble_to_running() {
        let mut state = TuiState::new();
        let id = Uuid::new_v4();
        let pending_args = serde_json::json!({"path": "foo.txt"});
        let start_args = serde_json::json!({"path": "foo.txt"});
        state.apply_update(&UiUpdate::ToolPending {
            name: "read".to_string(),
            id,
            arguments: pending_args,
        });
        state.apply_update(&UiUpdate::ToolStart {
            name: "read".to_string(),
            id,
            arguments: start_args.clone(),
        });
        assert!(matches!(
            state.chat_entries.last(),
            Some(ChatEntry::ToolBubble {
                state: ToolBubbleState::Running { arguments },
                ..
            }) if *arguments == start_args
        ));
    }

    #[test]
    fn tool_done_updates_existing_bubble_to_done() {
        let mut state = TuiState::new();
        let id = Uuid::new_v4();
        let arguments = serde_json::json!({"path": "foo.txt"});
        state.apply_update(&UiUpdate::ToolPending {
            name: "read".to_string(),
            id,
            arguments: arguments.clone(),
        });
        state.apply_update(&UiUpdate::ToolStart {
            name: "read".to_string(),
            id,
            arguments: arguments.clone(),
        });
        state.apply_update(&UiUpdate::ToolDone {
            id,
            name: "read".to_string(),
            result: "file contents".to_string(),
            summary: Vec::new(),
            duration: std::time::Duration::from_millis(0),
        });
        assert!(matches!(
            state.chat_entries.last(),
            Some(ChatEntry::ToolBubble {
                state: ToolBubbleState::Done { result, .. },
                ..
            }) if result == "file contents"
        ));
    }

    #[test]
    fn tool_killed_updates_existing_bubble_to_killed() {
        let mut state = TuiState::new();
        let id = Uuid::new_v4();
        state.apply_update(&UiUpdate::ToolPending {
            name: "exec".to_string(),
            id,
            arguments: serde_json::json!({}),
        });
        state.apply_update(&UiUpdate::ToolStart {
            name: "exec".to_string(),
            id,
            arguments: serde_json::json!({}),
        });
        state.apply_update(&UiUpdate::ToolKilled { id });
        assert!(matches!(
            state.chat_entries.last(),
            Some(ChatEntry::ToolBubble {
                state: ToolBubbleState::Killed,
                ..
            })
        ));
    }
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
