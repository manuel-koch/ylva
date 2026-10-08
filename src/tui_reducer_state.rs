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

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::agent::AgentMode;
pub use crate::agent::ContextUsageSource;
use crate::llm::engine::EngineDisplayInfo;

mod conversation;
mod events;
mod input_history;
mod layout;
mod model_picker;
mod selection;

pub use conversation::{
    project_messages, result_section_ids, ActiveSubagentCounts, BubbleExpansionTarget, ChatEntry,
    ChildConversation, ChildRunInfo, ChildViewState, ConversationListState, ConversationView,
    ResultSection, ToolCallState, TurnBuffer, UserMessageState,
};
pub use events::{ConfirmKey, DomainEvent, Effect, Focus, InputEvent, TuiMode};
pub use input_history::HistoryDirection;
pub use layout::{
    AppLayout, ChatArea, ChatScrollbarLayout, HelpPopupLayout, InputArea, InputAreaLayout,
    SlashOverlayLayout,
};
pub(crate) use model_picker::{
    engine_choice_label, extract_models_filter, format_context_length, matching_model_indices,
    model_choice_label, model_picker_row_entries, slash_overlay_text_rows,
};
pub use model_picker::{InputSnapshot, ModelChoice, SlashOverlay};
pub use selection::{BannerDragSelection, ChatSelection};

// ─── Shared TUI constants ────────────────────────────────────────────────────

/// Duration a toast notification remains visible before it auto-dismisses.
const TOAST_DURATION: Duration = Duration::from_millis(1500);

/// Maximum gap between two clicks on the same chat bubble's title row for
/// the second click to count as a double-click (the drill-into-subagent
/// gesture) rather than a second, independent single click (collapse toggle).
pub(crate) const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(250);

/// Window for confirming the second press of a `Ctrl+C`/`Esc` action.
pub(crate) const CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);

/// Number of columns moved by one horizontal help-popup scroll action.
pub(crate) const HELP_HORIZONTAL_SCROLL_COLUMNS: usize = 4;

/// Sentinel `entry_index` used in `thinking_title_rows` for the in-flight
/// (not yet flushed) thinking bubble rendered from `partial_thinking_text`.
/// Real entries are keyed by their `chat_entries` index (always `< usize::MAX`
/// in practice), so this value can never collide with a genuine index; it
/// lets click handling route to `AppState::toggle_partial_thinking_collapsed`
/// instead of `AppState::toggle_thinking_collapsed`.
pub const STREAMING_THINKING_ENTRY_INDEX: usize = usize::MAX;

/// Estimated context size against the active context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsage {
    /// Estimated token count.
    pub estimated_tokens: usize,
    /// The active window the estimate is measured against.
    pub context_length: usize,
    /// The representation this estimate covers.
    pub source: ContextUsageSource,
}

/// Render the token count and fill percentage for a context-usage estimate.
///
/// The percentage is the estimate's fill of the active window, rounded to a
/// whole percent and clamped to `100%` (an estimate can overshoot briefly
/// before compaction runs).
pub fn format_context_usage(estimated_tokens: usize, context_length: usize) -> String {
    let percentage = if context_length == 0 {
        0.0
    } else {
        (estimated_tokens as f64 / context_length as f64) * 100.0
    }
    .round()
    .clamp(0.0, 100.0) as u64;
    format!(
        "{} ({percentage}%)",
        format_token_estimate(estimated_tokens)
    )
}

/// Format the footer value independently of the estimate source.
pub fn format_context_usage_readout(context_usage: ContextUsage) -> String {
    format_context_usage(context_usage.estimated_tokens, context_usage.context_length)
}

/// Abbreviate a token estimate: `950`, `64.2K`, `1.2M`.
fn format_token_estimate(estimated_tokens: usize) -> String {
    const THOUSAND: f64 = 1_000.0;
    const MILLION: f64 = 1_000_000.0;
    let tokens = estimated_tokens as f64;
    if tokens >= MILLION {
        format!("{:.1}M", tokens / MILLION)
    } else if tokens >= THOUSAND {
        format!("{:.1}K", tokens / THOUSAND)
    } else {
        estimated_tokens.to_string()
    }
}

// ─── Per-area sub-structs ──────────────────────────────────────────────────────
//
// Groups related `AppState` fields by the user-facing area they belong to
// (help popup, drill-in child view, clarify banner, conversation list),
// mirroring the `AppLayout` precedent above. On-screen rects for these same
// areas stay in `AppLayout` (written once per frame by the Driver) rather
// than living here — these sub-structs are data-only. `TuiState`
// (`src/tui_render_state.rs`) defines its own, differently-shaped sub-structs
// with the same names: it has no separate layout type, so its versions carry
// their area's rect field(s) alongside the data. The two are intentionally
// distinct types, analogous per area rather than identical.

/// Help popup (`?` overlay) scroll state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HelpPopupState {
    pub help_scroll_offset: usize,
    pub help_scroll_x: usize,
    pub help_rendered_lines: Vec<String>,
    pub help_selection: Option<ChatSelection>,
    pub help_drag_active: bool,
}

/// Clarify-tool banner scroll/drag state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClarifyBannerState {
    /// Current vertical scroll offset inside the clarify banner (lines from
    /// the top of the content). Clamped to the valid range by
    /// `draw_clarify_banner` each frame.
    pub clarify_scroll_offset: usize,
    /// Terminal position where the left button was pressed inside a clarify
    /// choice row. Cleared on mouse-up. A mouse-up at the same position is a
    /// click (→ fill textarea); a mouse-up at a different position is a drag
    /// (→ no action, allows text selection).
    pub clarify_choice_mousedown: Option<(u16, u16)>,
    /// Updated on every `MouseDrag` event while `clarify_choice_mousedown` is
    /// set. Together with the down position this defines the highlighted row
    /// range in the banner. Cleared on mouse-up. Combined with
    /// `clarify_choice_mousedown` into a single `BannerDragSelection` by
    /// `render::project_for_draw` for `TuiState::clarify_banner_state`.
    pub clarify_banner_drag_end: Option<(u16, u16)>,
}

// ─── AppState ────────────────────────────────────────────────────────────────

/// Active profile and workspace paths displayed in the TUI's startup logo
/// and help popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TuiDisplayMetadata {
    pub profile_name: String,
    pub profile_path: Option<PathBuf>,
    pub ylva_home: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
}

impl Default for TuiDisplayMetadata {
    fn default() -> Self {
        Self {
            profile_name: crate::config::DEFAULT_PROFILE_NAME.to_string(),
            profile_path: None,
            ylva_home: None,
            workspace_root: None,
        }
    }
}

/// The authoritative TUI view-model owned by the Driver.
///
/// This is the single source of truth: `presenter::update` mutates it in
/// response to [`DomainEvent`]s. Production frames borrow transcript content
/// alongside a [`crate::tui_render_state::TuiState`] metadata/geometry shell.
/// `TuiState` is never the source of truth and is not fed back into
/// `AppState` except for the handful of view-geometry fields `tui_widgets::draw`
/// itself writes (see `render::sync_from_draw`).
///
/// Fields that require `TextArea` to interpret (e.g. `mouse_drag`, used by
/// `cursor_for_click`) or that exist purely to pass rendering artifacts between
/// draw calls (e.g. `button_areas`) are kept in the Driver and excluded here.
pub struct AppState {
    pub render_cache: crate::conversation_render_cache::ConversationRenderCache,
    pub(crate) render_revisions: HashMap<(Option<Uuid>, usize), u64>,
    pub(crate) render_title_revisions: HashMap<Uuid, u64>,
    pub(crate) render_stream_revisions: HashMap<Option<Uuid>, u64>,
    pub(crate) render_cache_tracking: bool,
    pub(crate) queued_render_revision: u64,
    pub mode: TuiMode,
    pub focus: Focus,
    pub layout: AppLayout,
    /// Active profile and workspace paths shown by the TUI's startup logo
    /// and help popup.
    pub display_metadata: TuiDisplayMetadata,
    /// Provider/model identity for the root conversation. The Driver refreshes
    /// this from the live root engine before each frame so Engine runtime model
    /// switches are reflected immediately.
    pub engine_display: EngineDisplayInfo,

    // ── chat view-model ─────────────────────────────────────────────────────
    pub chat_entries: Vec<ChatEntry>,
    /// Root conversation's in-flight stream-text accumulator. See
    /// `TurnBuffer`; every child conversation has its own instance
    /// (`ChildConversation::turn_buffer`).
    pub turn_buffer: TurnBuffer,
    /// Steers (user messages sent while the agent is busy) staged for live
    /// rendering as `Queued` bubbles, not yet committed to `chat_entries`.
    /// See `queue_steer_message` / `commit_queued_steer_messages`.
    pub queued_steer_texts: Vec<String>,

    // ── scroll state / chat selection / bubble hit-testing ─────────────────
    pub conversation: ConversationListState,

    // ── tool-approval ───────────────────────────────────────────────────────
    /// The message-draft input state parked by `Effect::SaveInputStateAndClear`
    /// when a tool approval or clarify question pauses the input, restored as
    /// one unit by `Effect::RestoreInputState` (see [`InputSnapshot`]).
    /// `None` when no pause is in progress or the input was empty.
    pub pending_input_snapshot: Option<InputSnapshot>,

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

    // ── context readout ─────────────────────────────────────────────────────
    /// Latest root model-view estimate reported by `UiUpdate::ContextUsage`;
    /// rendered in the footer only while `active_view` is `Root`. Child values
    /// are stored on their respective `ChildConversation`s.
    pub context_usage: Option<ContextUsage>,
    /// The persisted compaction snapshot's transcript-relative boundary,
    /// read once at startup and handed to [`Self::replay_messages`] so the
    /// marker lands where the model view was cut. `None` when the session
    /// never compacted or its snapshot predates the field.
    pub replay_compaction_boundary: Option<usize>,
    /// In-flight summary request identity per conversation, not agent activity.
    pub compaction_requests: std::collections::HashMap<Option<Uuid>, Uuid>,

    // ── transient UI ────────────────────────────────────────────────────────
    /// Floating notification, auto-dismissed after [`TOAST_DURATION`].
    pub toast: Option<(String, Instant)>,
    /// Incremented each tick while agent is busy; drives the footer spinner.
    pub spinner_frame: u8,
    /// Expansion whose collapsed title must be painted before building its body.
    pub pending_bubble_expansion: Option<BubbleExpansionTarget>,

    // ── help popup ──────────────────────────────────────────────────────────
    pub help_popup: HelpPopupState,

    // ── clarify banner ───────────────────────────────────────────────────────
    pub clarify_banner_state: ClarifyBannerState,

    // ── child-view header bubble ────────────────────────────────────────────
    pub child_view: ChildViewState,

    // ── input field ─────────────────────────────────────────────────────────
    /// Mirrors ratatui-textarea's internal scroll-top so scroll indicators
    /// reflect the actual rendered viewport position.
    pub input_scroll_top: usize,

    // ── history navigation helper ────────────────────────────────────────────
    /// Exposes history navigation for use by the presenter.
    pub _history_direction: Option<HistoryDirection>,

    // ── abort-exit flag ──────────────────────────────────────────────────────
    /// `true` when `TuiMode::Aborting` was entered via Ctrl+C double-press,
    /// meaning the Driver should break the event loop once mode returns to
    /// Idle. `false` when Aborting was entered via Esc (soft cancel) — the
    /// TUI stays open so the user can continue chatting.
    pub aborting_exit: bool,

    // ── drill-in conversation model (Phase 6.10) ─────────────────────────────
    /// Which transcript (root or child) is currently rendered in the main
    /// chat area. Independent of `running_child`: the parent stays in
    /// `Root` view while a child streams in the background.
    pub active_view: ConversationView,
    /// Every child run's own conversation transcript/partial-text
    /// accumulator, keyed by `run_id` — one entry per `spawn_subagent`
    /// child that has ever started, added on `ChildAgentStarted` and never
    /// removed (mirrors `chat_entries`, which is also session-lifetime
    /// unbounded), so any of them can still be drilled into later even
    /// while another child is running/queued or has since finished. Fixes
    /// the bug where a single shared transcript was silently overwritten
    /// whenever the next queued child started, corrupting whichever
    /// child's conversation the user was currently viewing.
    pub child_conversations: HashMap<Uuid, ChildConversation>,
    /// Provider/model identity for each constructed child engine, keyed by
    /// child run id. Child conversations can remain drillable after a run
    /// finishes, so these entries have the same session lifetime as their
    /// transcripts.
    pub child_engine_displays: HashMap<Uuid, EngineDisplayInfo>,
    /// Identifies the currently *running* child (at most one at a time —
    /// see `ChildAgentManager`), independent of whether `active_view` has
    /// switched to it yet, and independent of which (if any) child the user
    /// is currently viewing. `None` when no child is currently running
    /// (though earlier children may still have entries in
    /// `child_conversations`). Populated on `ChildAgentStarted`, cleared on
    /// `ChildAgentDone`.
    pub running_child: Option<ChildRunInfo>,
    /// `true` when `active_view` was switched automatically (an interactive
    /// pause — `note_interactive_pause`), to `Child` *or* `Root`, rather than
    /// by explicit user navigation (click/Esc). Gates whether the view
    /// auto-returns to `pre_pause_view` once that pause resolves or the
    /// owning child finishes.
    pub auto_switched_to_child: bool,
    /// Snapshot of `active_view` from immediately before an auto-switch
    /// (`note_interactive_pause`), so `resolve_possible_interactive_pause`
    /// can restore *that* view — which may be `Root` or a different,
    /// already-finished child's view the user had manually drilled into —
    /// instead of unconditionally landing back on `Root`. Populated only by
    /// `note_interactive_pause`, consumed (taken) by
    /// `resolve_possible_interactive_pause`.
    pub pre_pause_view: Option<ConversationView>,
    /// `(bubble_id, clicked_at)` of the most recent plain click on a chat
    /// bubble's title row, used to detect a second click on the same
    /// `spawn_subagent` bubble within `DOUBLE_CLICK_WINDOW` as the
    /// drill-in-conversation gesture — distinct from the single-click
    /// collapse/expand toggle every bubble (including spawn bubbles) always
    /// supports.
    pub last_bubble_click: Option<(uuid::Uuid, std::time::Instant)>,
    /// `(scroll_offset, scroll_locked)` of the root transcript, captured by
    /// `switch_to_child_view` right before it re-pins the shared scroll
    /// state to the bottom of the child's transcript. Restored by
    /// `switch_to_root_view` (Esc with an empty draft) as an immediate best-effort
    /// placeholder — the precise fix is `saved_root_scroll_bubble` below,
    /// resolved once root is rendered again. `None` before the first
    /// drill-in, or once restored.
    pub saved_root_scroll: Option<(usize, bool)>,
    /// `Uuid` of the `spawn_subagent` `ToolBubble` the user drilled in from,
    /// captured by `switch_to_child_view` (from the drilled-into run's
    /// `ChildRunInfo::bubble_id`). Consumed by
    /// `switch_to_root_view`, which moves it into `pending_scroll_to_bubble`
    /// so `render::sync_from_draw` can scroll to that bubble's *current* row
    /// — robust against the root conversation having grown while the child
    /// view was open, unlike a raw saved offset which would drift. `None`
    /// before the first drill-in, or once consumed.
    pub saved_root_scroll_bubble: Option<uuid::Uuid>,
    /// Armed by `switch_to_root_view` with the `Uuid` from
    /// `saved_root_scroll_bubble`; consumed by `render::sync_from_draw`
    /// immediately after the first root frame following the switch is drawn
    /// (so `AppState::bubble_title_rows` reflects root's, not the child's,
    /// bubbles), which scrolls to that bubble's row and clears this field.
    /// `None` when there is nothing pending.
    pub pending_scroll_to_bubble: Option<uuid::Uuid>,

    // ── slash commands (Phase 7) ─────────────────────────────────────────────
    /// Suggestion list / model picker driven by the slash-command line.
    pub slash_overlay: SlashOverlay,
    /// Screen position where a slash-overlay text drag started.
    pub slash_overlay_drag_start: Option<(u16, u16)>,
    /// Current screen position of a slash-overlay text drag.
    pub slash_overlay_drag_end: Option<(u16, u16)>,
    /// Names of the tools the running agent may call, captured once at
    /// startup from the resolved `ToolRegistry` so `/tools` needs no I/O.
    pub available_tool_names: Vec<String>,
}

impl AppState {
    /// Opt into mutation-tracked rendering. Direct field edits thereafter must
    /// call `invalidate_render_cache`; ordinary public states stay conservative.
    pub fn enable_render_cache(&mut self) {
        self.invalidate_render_cache();
        self.render_cache_tracking = true;
    }

    pub fn invalidate_render_cache(&mut self) {
        self.render_cache.clear();
        self.render_revisions.clear();
        self.render_title_revisions.clear();
        self.render_stream_revisions.clear();
    }

    pub(crate) fn invalidate_transcript_layout(&mut self, run_id: Option<Uuid>) {
        let length = run_id
            .and_then(|run_id| self.child_conversations.get(&run_id))
            .map_or(self.chat_entries.len(), |conversation| {
                conversation.messages.len()
            });
        for index in 0..length {
            *self.render_revisions.entry((run_id, index)).or_default() += 1;
        }
    }

    fn release_active_render_layout(&mut self) {
        self.pending_bubble_expansion = None;
        self.render_cache.clear();
        self.conversation.chat_rendered_lines = Vec::new();
        self.conversation.chat_rendered_line_prefix_widths = Vec::new();
        self.conversation.bubble_title_rows = Vec::new();
        self.conversation.thinking_title_rows = Vec::new();
        self.conversation.chat_selection = None;
        self.child_view.header_rendered_lines = Vec::new();
        self.child_view.header_rendered_line_prefix_widths = Vec::new();
    }

    pub(crate) fn revise_stream(&mut self, run_id: Option<Uuid>) {
        *self.render_stream_revisions.entry(run_id).or_default() += 1;
    }

    pub fn new() -> Self {
        AppState {
            render_cache: Default::default(),
            render_revisions: HashMap::new(),
            render_title_revisions: HashMap::new(),
            render_stream_revisions: HashMap::new(),
            render_cache_tracking: false,
            queued_render_revision: 0,
            mode: TuiMode::Idle,
            focus: Focus::MainInput,
            layout: AppLayout::default(),
            display_metadata: TuiDisplayMetadata::default(),
            engine_display: EngineDisplayInfo::default(),
            context_usage: None,
            replay_compaction_boundary: None,
            compaction_requests: std::collections::HashMap::new(),

            chat_entries: Vec::new(),
            turn_buffer: TurnBuffer::default(),
            queued_steer_texts: Vec::new(),

            conversation: ConversationListState::default(),

            pending_input_snapshot: None,

            input_history: Vec::new(),
            history_index: None,
            draft: String::new(),

            steer_indicator: false,

            toast: None,
            spinner_frame: 0,
            pending_bubble_expansion: None,

            help_popup: HelpPopupState::default(),

            clarify_banner_state: ClarifyBannerState::default(),

            child_view: ChildViewState::default(),

            input_scroll_top: 0,

            _history_direction: None,

            slash_overlay: SlashOverlay::default(),
            slash_overlay_drag_start: None,
            slash_overlay_drag_end: None,
            available_tool_names: Vec::new(),

            aborting_exit: false,

            active_view: ConversationView::Root,
            child_conversations: HashMap::new(),
            child_engine_displays: HashMap::new(),
            running_child: None,
            auto_switched_to_child: false,
            pre_pause_view: None,
            last_bubble_click: None,
            saved_root_scroll: None,
            saved_root_scroll_bubble: None,
            pending_scroll_to_bubble: None,
        }
    }

    /// Returns `true` if a toast is active and has not yet expired.
    pub fn has_active_toast(&self) -> bool {
        self.toast
            .as_ref()
            .map(|(_, start)| start.elapsed() < TOAST_DURATION)
            .unwrap_or(false)
    }

    /// Append a user message bubble and record the text in input history.
    /// Only for messages sent immediately (`UserMessageState::Sent`); a steer
    /// sent while the agent is busy must go through `queue_steer_message`
    /// instead, since it isn't safe to commit to `chat_entries` yet.
    /// Drop everything the user can see of the current conversation, keeping
    /// only what is not part of it: the input history (so ↑ still recalls
    /// earlier prompts) and the captured tool names.
    ///
    /// The in-memory `Context` and the session file are *not* touched here —
    /// the agent loop owns those and resets them on `AgentCommand::ClearContext`.
    pub fn clear_conversation(&mut self) {
        self.invalidate_render_cache();
        self.release_active_render_layout();
        self.context_usage = None;
        self.compaction_requests.clear();
        self.chat_entries.clear();
        self.turn_buffer = TurnBuffer::default();
        self.queued_steer_texts.clear();
        self.steer_indicator = false;
        self.child_conversations.clear();
        self.running_child = None;
        self.active_view = ConversationView::Root;
        self.child_view = ChildViewState::default();
        self.conversation.chat_selection = None;
        self.conversation.scroll_offset = 0;
        self.conversation.scroll_locked = true;
        self.history_index = None;
    }

    /// The focus to take when the help popup closes. An outstanding tool
    /// approval outranks the normal input: returning to `Focus::MainInput`
    /// there would hand the slash-command-capable main input to the user
    /// while a tool still awaits its verdict (and hide the approval
    /// controls), so the approval focus is resumed instead.
    pub fn restore_focus_after_help(&mut self) {
        self.focus = Focus::MainInput;
        self.resume_pending_tool_approval();
    }

    /// Scroll the help popup up.
    pub fn help_scroll_up(&mut self, lines: usize) {
        self.help_popup.help_scroll_offset =
            self.help_popup.help_scroll_offset.saturating_sub(lines);
        self.clear_help_selection();
    }

    /// Scroll the help popup down.
    pub fn help_scroll_down(&mut self, lines: usize) {
        self.help_popup.help_scroll_offset =
            self.help_popup.help_scroll_offset.saturating_add(lines);
        self.clear_help_selection();
    }

    /// Scroll the help popup left.
    pub fn help_scroll_left(&mut self, columns: usize) {
        self.help_popup.help_scroll_x = self.help_popup.help_scroll_x.saturating_sub(columns);
        self.clear_help_selection();
    }

    /// Scroll the help popup right.
    pub fn help_scroll_right(&mut self, columns: usize) {
        self.help_popup.help_scroll_x = self.help_popup.help_scroll_x.saturating_add(columns);
        self.clear_help_selection();
    }

    pub fn clear_help_selection(&mut self) {
        self.help_popup.help_selection = None;
        self.help_popup.help_drag_active = false;
    }

    /// Scroll the clarify banner up (toward earlier content).
    pub fn clarify_scroll_up(&mut self, lines: usize) {
        self.clarify_banner_state.clarify_scroll_offset = self
            .clarify_banner_state
            .clarify_scroll_offset
            .saturating_sub(lines);
    }

    /// Scroll the clarify banner down (toward later content). The upper bound
    /// is applied in `draw_clarify_banner`, which knows the total content height.
    pub fn clarify_scroll_down(&mut self, lines: usize) {
        self.clarify_banner_state.clarify_scroll_offset = self
            .clarify_banner_state
            .clarify_scroll_offset
            .saturating_add(lines);
    }

    /// Show a transient toast notification.
    pub fn set_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now() + TOAST_DURATION));
    }

    // ── View-layer helpers ────────────────────────────────────────────────────

    /// Convert the current [`TuiMode`] (and [`Focus`]) to the legacy
    /// [`AgentMode`] used by `tui_widgets.rs` for footer and spinner rendering.
    pub fn agent_mode(&self) -> AgentMode {
        match &self.mode {
            // A spawned subagent keeps its `spawn_subagent` tool bubble in
            // `Running` until the child actually finishes, independently of
            // the root loop's own `TuiMode` — the root can go `Idle` (its
            // own turn ended) while the child is still working. Report
            // `ToolRunning` in that case so the footer/spinner doesn't look
            // finished while a child is still active.
            TuiMode::Idle => {
                if self.has_running_tool_bubble() {
                    AgentMode::ToolRunning
                } else {
                    AgentMode::Idle
                }
            }
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
                TuiMode::Idle | TuiMode::DoublePressAbort { .. } => {
                    if self.has_running_tool_bubble() {
                        AgentMode::ToolRunning
                    } else {
                        AgentMode::Idle
                    }
                }
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
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
