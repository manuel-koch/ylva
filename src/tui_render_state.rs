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
//! This module defines the owned compatibility [`TuiState`]. Production frames
//! borrow conversation content alongside a metadata/geometry shell; the driver
//! returns an owned snapshot through `tui_driver/updates.rs`. It is *not*
//! itself authoritative and must never be mutated directly by the Driver or
//! Presenter in production. It carries no reducer of its own — all state
//! transitions live in [`crate::tui_reducer_state::AppState`] / `presenter::update`.

use std::collections::HashMap;
use std::time::Instant;

use crate::agent::AgentMode;
use crate::llm::engine::EngineDisplayInfo;
use crate::tui_reducer_state::{
    ActiveSubagentCounts, BannerDragSelection, ChatEntry, ChatSelection, InputArea, SlashOverlay,
    SlashOverlayLayout, ToolCallState, TuiDisplayMetadata,
};

// ─── Per-area sub-structs ──────────────────────────────────────────────────────
//
// Mirrors the four sub-structs of the same name on
// [`crate::tui_reducer_state::AppState`], grouping the same user-facing
// areas (help popup, drill-in child view, clarify banner, conversation
// list). Unlike `AppState` — which keeps on-screen rects together in
// `AppLayout`, written once per frame by the Driver — `TuiState` has no
// separate layout type: it is a disposable, rebuilt-every-frame snapshot, so
// each sub-struct here carries its own rect field(s) alongside its data.
// The two are intentionally distinct types, analogous per area rather than
// identical.

/// Help popup (`?` overlay) render state.
pub struct HelpPopupState {
    pub show_help: bool,
    /// Current scroll offset inside the help popup (lines from the top of the
    /// content). Clamped to the valid range by `draw_help_popup` each frame.
    pub help_scroll_offset: usize,
    /// Current horizontal scroll offset inside the help popup (columns from
    /// the left). Clamped to the valid range by `draw_help_popup` each frame.
    pub help_scroll_x: usize,
    /// On-screen rect of the Help popup, written each frame by
    /// `tui_widgets::draw_help_popup` for mouse click-outside-to-close hit-testing.
    /// Zero-sized when the popup isn't shown.
    pub help_popup_rect: ratatui::layout::Rect,
    /// On-screen content rect of the Help popup, excluding its border and
    /// padding. Used to map mouse selections to popup content coordinates.
    pub help_content_area: ratatui::layout::Rect,
    /// Plain text of every rendered help line, indexed before scroll offsets.
    pub help_rendered_lines: Vec<String>,
    /// Active or committed help-popup selection.
    pub help_selection: Option<ChatSelection>,
    /// True while the left mouse button is held down in the Help popup.
    pub help_drag_active: bool,
}

/// Drill-in child-view header bubble render state.
pub struct ChildViewState {
    /// Set when the drill-in view (`AppState::active_view ==
    /// ConversationView::Child`) is showing a child conversation; carries the
    /// real root `spawn_subagent` `ToolBubble`'s `(id, name, state,
    /// collapsed)` so `tui_widgets::draw` can re-render it verbatim as the header —
    /// same chrome, same collapsed/expanded state, same task/label/
    /// agent_type content the user saw before drilling in. The one
    /// deviation is the title: the header shortens it to `"Subagent"` (the
    /// drill-in view already sits above that subagent's own transcript, so
    /// the root-relative "Spawn" is noise) — see
    /// `ToolBubbleArgs::display_title_override`. `None` while
    /// viewing `Root`.
    pub child_view_bubble: Option<(uuid::Uuid, String, ToolCallState, bool)>,
    /// Current vertical scroll offset inside the child-view header bubble
    /// (lines from the top of its content), used only when the bubble's full
    /// content exceeds the header's clamped max height. Clamped to the valid
    /// range by `tui_widgets::draw` each frame. Meaningless when `child_view_bubble`
    /// is `None`.
    pub header_bubble_scroll_offset: usize,
    /// On-screen rect of the header bubble's content column (excluding its
    /// scrollbar column), written each frame by `tui_widgets::draw` for mouse
    /// hit-testing (routing scroll-wheel events to the header instead of the
    /// chat area below it). Zero-sized while `child_view_bubble` is `None`.
    pub header_bubble_area: ratatui::layout::Rect,
    /// Plain text of every rendered header line, indexed before the vertical
    /// scroll offset. Used for extracting selected text on mouse-up.
    pub header_rendered_lines: Vec<String>,
    /// Number of leading header chrome characters stripped from each rendered
    /// line. Used to map terminal columns back to content columns.
    pub header_rendered_line_prefix_widths: Vec<usize>,
    /// Active or committed header selection.
    pub header_selection: Option<ChatSelection>,
    /// True while the left mouse button is held down in the header bubble.
    pub header_drag_active: bool,
}

/// Clarify-tool banner render state.
pub struct ClarifyBannerState {
    /// Set by `UiUpdate::ClarifyPending`; cleared once the user answers or the
    /// stream returns to idle.
    pub pending_clarify: Option<(String, Option<Vec<String>>, Option<String>)>,
    /// Current vertical scroll offset inside the clarify banner (lines from
    /// the top of the content). Clamped to the valid range by
    /// `draw_clarify_banner` each frame.
    pub clarify_scroll_offset: usize,
    /// While the user drags in the clarify banner, holds the normalised
    /// selection (start_row, start_col, end_row, end_col) in absolute screen
    /// coordinates, for highlighting. `None` when no drag is in progress.
    pub clarify_banner_drag: Option<BannerDragSelection>,
    /// On-screen rect of the clarify banner, written each frame by `tui_widgets::draw`
    /// for mouse click-to-select-choice hit-testing. Zero-sized when no
    /// clarify is pending.
    pub clarify_banner: ratatui::layout::Rect,
}

/// Root-or-child scrollable conversation-list render state (Phase 6.10
/// drill-in model: shared by both `active_view` cases, not duplicated per
/// view).
pub struct ConversationListState {
    pub scroll_offset: usize,
    /// true = auto-scroll to bottom.
    pub scroll_locked: bool,
    /// updated on every resize.
    pub chat_area_height: u16,
    /// On-screen rect of the chat content area (not the scrollbar column),
    /// written each frame by `tui_widgets::draw_chat` for mouse hit-testing.
    pub chat_area: ratatui::layout::Rect,
    pub chat_scrollbar: crate::tui_reducer_state::ChatScrollbarLayout,
    /// Plain text of every rendered line built by the most recent `draw_chat`
    /// call. Indexed by absolute line number (before scroll offset). Used for
    /// extracting selected text on mouse-up.
    pub chat_rendered_lines: Vec<String>,
    /// Number of leading chrome characters (`▌  `) stripped from the
    /// corresponding `chat_rendered_lines` entry. Zero for non-bubble lines.
    /// Used by `extract_chat_selection_text` to map terminal columns (which
    /// include the chrome) back to content columns.
    pub chat_rendered_line_prefix_widths: Vec<usize>,
    /// Active or committed chat selection. `None` = no selection.
    pub chat_selection: Option<ChatSelection>,
    /// True while the left mouse button is held down in the chat area (drag
    /// in progress). Cleared on mouse-up.
    pub chat_drag_active: bool,
    /// Maps each rendered tool bubble to its absolute line-index range
    /// `(start_row, end_row, id)` (inclusive, before scroll offset). Populated
    /// by `tui_widgets::draw_chat` every frame. Used to route click events to the
    /// correct bubble's expand/collapse toggle: a click on any row in
    /// `[start_row, end_row]` at column 0 (the border bar) toggles the bubble,
    /// as does a click anywhere on the title row regardless of column.
    pub bubble_title_rows: Vec<(usize, usize, uuid::Uuid)>,
    /// Maps each rendered thinking bubble to its absolute line-index range
    /// `(start_row, end_row, chat_entries index)` (inclusive, before scroll
    /// offset). Populated by `tui_widgets::draw_chat` every frame; mirrors
    /// `bubble_title_rows` but is index-keyed since `ChatEntry::ThinkingMessage`
    /// has no `Uuid`.
    pub thinking_title_rows: Vec<(usize, usize, usize)>,
}

impl Default for ConversationListState {
    /// `scroll_locked` defaults to `true` (auto-scroll to bottom) — every
    /// other field defaults to empty/zero.
    fn default() -> Self {
        ConversationListState {
            scroll_offset: 0,
            scroll_locked: true,
            chat_area_height: 0,
            chat_area: ratatui::layout::Rect::default(),
            chat_scrollbar: crate::tui_reducer_state::ChatScrollbarLayout::default(),
            chat_rendered_lines: Vec::new(),
            chat_rendered_line_prefix_widths: Vec::new(),
            chat_selection: None,
            chat_drag_active: false,
            bubble_title_rows: Vec::new(),
            thinking_title_rows: Vec::new(),
        }
    }
}

pub struct TuiState {
    /// Active profile and workspace paths displayed in the startup logo and
    /// help popup.
    pub display_metadata: TuiDisplayMetadata,
    /// Status of the root/parent agent loop. While a child transcript is
    /// displayed, [`Self::agent_mode`] describes that visible child instead,
    /// so the footer can still report parent activity separately.
    pub parent_agent_mode: AgentMode,
    pub agent_mode: AgentMode,
    /// Active child-agent counts from the root conversation. This is empty
    /// while a drilled-in child transcript is displayed because the footer
    /// indicator belongs to the main conversation.
    pub active_subagent_counts: ActiveSubagentCounts,
    /// Engine label for the transcript currently being rendered.
    pub engine_display: EngineDisplayInfo,
    pub chat_entries: Vec<ChatEntry>,
    pub subagent_started_at: HashMap<uuid::Uuid, Instant>,
    pub partial_assistant_text: String,
    /// In-flight extended-thinking/reasoning fragments, accumulated separately
    /// from `partial_assistant_text` so they never contaminate the assistant
    /// reply. Flushed into a `ChatEntry::ThinkingMessage` on `StreamDone`.
    pub partial_thinking_text: String,
    /// Mirrors `AppState::thinking_started_at`; used to render the live
    /// "Thinking… (Ns)" elapsed-time title while a thinking stream is
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
    /// true while ≥1 steer is queued.
    pub steer_indicator: bool,
    /// Pre-formatted footer readout of the active estimate and context window
    /// (`64.2K (45%)`).
    /// `None` until the first report arrives.
    pub context_usage_text: Option<String>,
    pub compacting: bool,
    /// Set by `UiUpdate::ToolPending`; cleared by `ToolStart`/`ToolKilled`/
    /// `StreamCancelled`. While `Some`, the TUI is in approval sub-mode.
    pub pending_approval: Option<(uuid::Uuid, String, serde_json::Value)>, // (tool_id, tool_name, arguments)
    /// `true` when `pending_approval`'s tool call belongs to a subagent
    /// (i.e. it lives in `AppState::child_messages`) rather than the root
    /// agent — even while `active_view` is `Root` (the user manually
    /// navigated back while the subagent's tool call is still awaiting
    /// approval). Used by `tui_widgets::draw_approval_footer` to prefix the approval
    /// title so it's unambiguous which agent's tool call is being approved.
    /// Meaningless when `pending_approval` is `None`.
    pub pending_approval_is_subagent: bool,
    /// All `UserMessage` texts, oldest → newest (including pending and
    /// cancelled steers, so the user can recover them via ↑).
    pub input_history: Vec<String>,
    /// `None` = live draft; `Some(i)` = browsing history at index `i`.
    pub history_index: Option<usize>,
    /// Saved live draft while browsing.
    pub draft: String,
    /// On-screen rect of the input box, written each frame by `tui_widgets::draw` for
    /// mouse hit-testing (click-to-cursor / drag-select).
    pub input_area: InputArea,
    /// Transient notification displayed as a floating overlay, auto-dismissed
    /// after `TOAST_DURATION`. Set by e.g. clipboard copy; `None` when no
    /// notification is active.
    pub toast: Option<(String, Instant)>,
    /// Incremented on every tick while the agent is busy; used to animate the
    /// footer spinner.
    pub spinner_frame: u8,
    /// Mirrors ratatui-textarea's internal scroll-top (in wrapped screen rows)
    /// so that the input-field scroll indicators reflect the actual rendered
    /// viewport position rather than the cursor row.
    pub input_scroll_top: usize,

    // ── help popup ──────────────────────────────────────────────────────────
    pub help_popup: HelpPopupState,

    // ── clarify banner ───────────────────────────────────────────────────────
    pub clarify_banner_state: ClarifyBannerState,

    // ── drill-in child view ─────────────────────────────────────────────────
    pub child_view: ChildViewState,

    // ── scroll state / chat selection / bubble hit-testing ─────────────────
    pub conversation: ConversationListState,

    /// Slash-command suggestion list / model picker, rendered as a floating
    /// overlay directly above the input box.
    pub slash_overlay: SlashOverlay,
    /// While the left mouse button is held in the slash overlay, the
    /// normalised screen-space text selection to highlight.
    pub slash_overlay_drag: Option<BannerDragSelection>,
    /// On-screen geometry of the slash overlay's selectable content, written
    /// by `tui_widgets::draw_slash_overlay` for mouse hit-testing.
    pub slash_overlay_layout: SlashOverlayLayout,
}

impl TuiState {
    pub fn new() -> Self {
        TuiState {
            subagent_started_at: HashMap::new(),
            display_metadata: TuiDisplayMetadata::default(),
            parent_agent_mode: AgentMode::Idle,
            agent_mode: AgentMode::Idle,
            active_subagent_counts: ActiveSubagentCounts::default(),
            engine_display: EngineDisplayInfo::default(),
            chat_entries: Vec::new(),
            partial_assistant_text: String::new(),
            partial_thinking_text: String::new(),
            thinking_started_at: None,
            partial_thinking_collapsed: false,
            queued_steer_texts: Vec::new(),
            steer_indicator: false,
            context_usage_text: None,
            compacting: false,
            pending_approval: None,
            pending_approval_is_subagent: false,
            input_history: Vec::new(),
            history_index: None,
            draft: String::new(),
            input_area: ratatui::layout::Rect::default(),
            toast: None,
            spinner_frame: 0,
            input_scroll_top: 0,
            help_popup: HelpPopupState {
                show_help: false,
                help_scroll_offset: 0,
                help_scroll_x: 0,
                help_popup_rect: ratatui::layout::Rect::default(),
                help_content_area: ratatui::layout::Rect::default(),
                help_rendered_lines: Vec::new(),
                help_selection: None,
                help_drag_active: false,
            },
            clarify_banner_state: ClarifyBannerState {
                pending_clarify: None,
                clarify_scroll_offset: 0,
                clarify_banner_drag: None,
                clarify_banner: ratatui::layout::Rect::default(),
            },
            child_view: ChildViewState {
                child_view_bubble: None,
                header_bubble_scroll_offset: 0,
                header_bubble_area: ratatui::layout::Rect::default(),
                header_rendered_lines: Vec::new(),
                header_rendered_line_prefix_widths: Vec::new(),
                header_selection: None,
                header_drag_active: false,
            },
            conversation: ConversationListState::default(),
            slash_overlay: SlashOverlay::default(),
            slash_overlay_drag: None,
            slash_overlay_layout: SlashOverlayLayout::default(),
        }
    }

    /// Remove an expired toast. Called once per frame by `tui_widgets::draw` before
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
