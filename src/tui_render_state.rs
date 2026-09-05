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

//! Render-projection snapshot consumed by the View/Driver layers.
//!
//! This module defines [`TuiState`] — rebuilt every frame from the canonical
//! [`crate::tui_reducer_state::AppState`] by `render::project_for_draw` (see
//! `src/render.rs`) and by `tui_driver::app_state_to_tui_state`. It is *not*
//! itself authoritative and must never be mutated directly by the Driver or
//! Presenter in production. It carries no reducer of its own — all state
//! transitions live in [`crate::tui_reducer_state::AppState`] / `presenter::update`.

use std::time::Instant;

use crate::agent::AgentMode;
use crate::tui_reducer_state::{
    BannerDragSelection, ChatEntry, ChatSelection, InputArea, ToolCallState,
};

/// Render-projection snapshot consumed by [`crate::ui::draw`].
///
/// Rebuilt every frame from the canonical [`crate::tui_reducer_state::AppState`] by
/// `render::project_for_draw` (see `src/render.rs`) and by
/// `tui_driver::app_state_to_tui_state`; it is *not* itself authoritative and
/// must never be mutated directly by the Driver or Presenter in production.
/// It carries no reducer of its own — all state transitions live in
/// [`crate::tui_reducer_state::AppState`] / `presenter::update`.
pub struct TuiState {
    pub agent_mode: AgentMode,
    pub chat_entries: Vec<ChatEntry>,
    pub partial_assistant_text: String,
    /// In-flight extended-thinking/reasoning fragments, accumulated separately
    /// from `partial_assistant_text` so they never contaminate the assistant
    /// reply. Flushed into a `ChatEntry::ThinkingMessage` on `StreamDone`.
    pub partial_thinking_text: String,
    /// Mirrors `AppState::thinking_started_at`; used to render the live
    /// "Thinking... (Ns)" elapsed-time title while a thinking stream is
    /// in progress.
    pub thinking_started_at: Option<Instant>,
    /// Mirrors `AppState::partial_thinking_collapsed` so the in-flight
    /// thinking bubble can be collapsed/expanded by clicking its title
    /// before it is flushed into a `ChatEntry::ThinkingMessage`.
    pub partial_thinking_collapsed: bool,
    /// Mirrors `AppState::queued_steer_texts` — steers (user messages sent
    /// while the agent is busy) staged for live rendering as `Queued`
    /// bubbles near the bottom of the chat, not yet committed to
    /// `chat_entries`. See `AppState::queue_steer_message`.
    pub queued_steer_texts: Vec<String>,
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
    pub pending_approval: Option<(uuid::Uuid, String, serde_json::Value)>, // (tool_id, tool_name, arguments)
    /// `true` when `pending_approval`'s tool call belongs to a subagent
    /// (i.e. it lives in `AppState::child_messages`) rather than the root
    /// agent — even while `active_view` is `Root` (the user manually
    /// navigated back while the subagent's tool call is still awaiting
    /// approval). Used by `ui::draw_approval_footer` to prefix the approval
    /// title so it's unambiguous which agent's tool call is being approved.
    /// Meaningless when `pending_approval` is `None`.
    pub pending_approval_is_subagent: bool,
    /// Set by `UiUpdate::ClarifyPending`; cleared once the user answers or the
    /// stream returns to idle.
    pub pending_clarify: Option<(String, Option<Vec<String>>, Option<String>)>,
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
    /// On-screen rect of the clarify banner, written each frame by `ui::draw`
    /// for mouse click-to-select-choice hit-testing. Zero-sized when no
    /// clarify is pending.
    pub clarify_banner: ratatui::layout::Rect,
    /// On-screen rect of the Help popup, written each frame by
    /// `ui::draw_help_popup` for mouse click-outside-to-close hit-testing.
    /// Zero-sized when the popup isn't shown.
    pub help_popup_rect: ratatui::layout::Rect,
    /// Plain text of every rendered line built by the most recent `draw_chat`
    /// call. Indexed by absolute line number (before scroll offset). Used for
    /// extracting selected text on mouse-up.
    pub chat_rendered_lines: Vec<String>,
    /// Number of leading chrome characters (`▌  `) stripped from the
    /// corresponding `chat_rendered_lines` entry. Zero for non-bubble lines.
    /// Used by `extract_chat_selection_text` to map terminal columns (which
    /// include the chrome) back to content columns.
    pub chat_rendered_line_chrome_widths: Vec<usize>,
    /// Active or committed chat selection. `None` = no selection.
    pub chat_selection: Option<ChatSelection>,
    /// True while the left mouse button is held down in the chat area (drag
    /// in progress). Cleared on mouse-up.
    pub chat_drag_active: bool,
    /// While the user drags in the clarify banner, holds the normalised
    /// selection (start_row, start_col, end_row, end_col) in absolute screen
    /// coordinates, for highlighting. `None` when no drag is in progress.
    pub clarify_banner_drag: Option<BannerDragSelection>,
    /// Transient notification displayed as a floating overlay, auto-dismissed
    /// after `TOAST_DURATION`. Set by e.g. clipboard copy; `None` when no
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
    /// Current vertical scroll offset inside the clarify banner (lines from
    /// the top of the content). Clamped to the valid range by
    /// `draw_clarify_banner` each frame.
    pub clarify_scroll_offset: usize,
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
    /// Maps each rendered thinking bubble to its absolute line-index range
    /// `(start_row, end_row, chat_entries index)` (inclusive, before scroll
    /// offset). Populated by `ui::draw_chat` every frame; mirrors
    /// `bubble_title_rows` but is index-keyed since `ChatEntry::ThinkingMessage`
    /// has no `Uuid`.
    pub thinking_title_rows: Vec<(usize, usize, usize)>,
    /// Set when the drill-in view (`AppState::active_view ==
    /// ConversationView::Child`) is showing a child conversation; carries the
    /// real root `spawn_subagent` `ToolBubble`'s `(id, name, state,
    /// collapsed)` so `ui::draw` can re-render it verbatim as the header —
    /// same chrome, same collapsed/expanded state, same task/label/
    /// agent_type content the user saw before drilling in. `None` while
    /// viewing `Root`.
    pub child_view_bubble: Option<(uuid::Uuid, String, ToolCallState, bool)>,
    /// Current vertical scroll offset inside the child-view header bubble
    /// (lines from the top of its content), used only when the bubble's full
    /// content exceeds the header's clamped max height. Clamped to the valid
    /// range by `ui::draw` each frame. Meaningless when `child_view_bubble`
    /// is `None`.
    pub header_bubble_scroll_offset: usize,
    /// On-screen rect of the header bubble's content column (excluding its
    /// scrollbar column), written each frame by `ui::draw` for mouse
    /// hit-testing (routing scroll-wheel events to the header instead of the
    /// chat area below it). Zero-sized while `child_view_bubble` is `None`.
    pub header_bubble_area: ratatui::layout::Rect,
}

impl TuiState {
    pub fn new() -> Self {
        TuiState {
            agent_mode: AgentMode::Idle,
            chat_entries: Vec::new(),
            partial_assistant_text: String::new(),
            partial_thinking_text: String::new(),
            thinking_started_at: None,
            partial_thinking_collapsed: false,
            queued_steer_texts: Vec::new(),
            show_help: false,
            steer_indicator: false,
            scroll_offset: 0,
            scroll_locked: true,
            chat_area_height: 0,
            pending_approval: None,
            pending_approval_is_subagent: false,
            pending_clarify: None,
            pending_approval_input_snapshot: None,
            input_history: Vec::new(),
            history_index: None,
            draft: String::new(),
            input_area: ratatui::layout::Rect::default(),
            chat_area: ratatui::layout::Rect::default(),
            clarify_banner: ratatui::layout::Rect::default(),
            help_popup_rect: ratatui::layout::Rect::default(),
            chat_rendered_lines: Vec::new(),
            chat_rendered_line_chrome_widths: Vec::new(),
            chat_selection: None,
            chat_drag_active: false,
            clarify_banner_drag: None,
            toast: None,
            spinner_frame: 0,
            help_scroll_offset: 0,
            help_scroll_x: 0,
            clarify_scroll_offset: 0,
            input_scroll_top: 0,
            bubble_title_rows: Vec::new(),
            thinking_title_rows: Vec::new(),
            child_view_bubble: None,
            header_bubble_scroll_offset: 0,
            header_bubble_area: ratatui::layout::Rect::default(),
        }
    }

    /// Remove an expired toast. Called once per frame by `ui::draw` before
    /// rendering so the overlay disappears on time without requiring a
    /// separate timer. `render::sync_from_draw` copies the (possibly now
    /// `None`) `toast` field back into `AppState` afterwards, so this is the
    /// sole place toast expiry is actually evaluated.
    pub fn clear_expired_toast(&mut self) {
        if let Some((_, expires_at)) = &self.toast {
            if Instant::now() >= *expires_at {
                self.toast = None;
            }
        }
    }
}

impl Default for TuiState {
    fn default() -> Self {
        Self::new()
    }
}
