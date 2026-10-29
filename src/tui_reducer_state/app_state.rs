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

use std::collections::HashMap;
use std::time::Instant;

use uuid::Uuid;

use super::*;
use crate::llm::engine::EngineDisplayInfo;

mod interaction;

// ─── AppState ────────────────────────────────────────────────────────────────

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
    /// Profile `[tui]` presentation settings (tool-bubble auto-expansion).
    pub tui_config: crate::config::TuiConfig,
    /// Tool bubbles that `[tui] auto_expand_approval` expanded while awaiting
    /// approval and that must collapse once the approval resolves. A bubble
    /// the user toggled by hand is removed, so the feature never overrides a
    /// manual choice.
    pub(crate) approval_auto_expanded: std::collections::HashSet<Uuid>,
    /// Provider/model identity for the root conversation. The Driver refreshes
    /// this from the live root engine before each frame so Engine runtime model
    /// switches are reflected immediately.
    pub engine_display: EngineDisplayInfo,
    /// Generated title of the active session; empty or `None` until generated.
    /// The Driver refreshes it on activation and title updates.
    pub session_title: Option<String>,

    // ── chat view-model ─────────────────────────────────────────────────────
    pub chat_entries: Vec<ChatEntry>,
    /// Root conversation's in-flight stream-text accumulator. See
    /// `TurnBuffer`; every subagent conversation has its own instance
    /// (`SubagentConversation::turn_buffer`).
    pub turn_buffer: TurnBuffer,
    /// Steers (user messages sent while the agent is busy) staged for live
    /// rendering as `Queued` bubbles, not yet committed to `chat_entries`.
    /// See `queue_steer_message` / `commit_queued_steer_messages`.
    pub queued_steer_texts: Vec<String>,

    // ── scroll state / chat selection / bubble hit-testing ─────────────────
    pub conversation: ConversationListState,

    // ── tool-approval ───────────────────────────────────────────────────────
    /// The slash overlay hidden by `Effect::ParkSlashOverlay` while a tool
    /// approval or clarify question owns the rows above the input; given back
    /// by `Effect::RestoreSlashOverlay`. The draft itself is never parked.
    pub parked_slash_overlay: Option<SlashOverlay>,
    /// The button of the visible approval/clarify prompt that has keyboard
    /// focus; `None` until the user presses `Tab`.
    pub prompt_button_focus: Option<PromptButton>,
    /// Keystroke timing that defers prompts while the user types.
    pub input_activity: InputActivity,

    // ── pasted blobs ────────────────────────────────────────────────────────
    /// Large pastes collapsed to markers in the current draft; expanded on
    /// submit. History recall never creates entries here.
    pub pasted_blobs: PastedBlobs,

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
    /// rendered in the footer only while `active_view` is `Root`. Subagent values
    /// are stored on their respective `SubagentConversation`s.
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

    // ── agents popup ────────────────────────────────────────────────────────
    pub agents_popup: AgentsPopupState,

    // ── clarify banner ───────────────────────────────────────────────────────
    pub clarify_banner_state: ClarifyBannerState,

    // ── subagent-view header bubble ────────────────────────────────────────────
    pub subagent_view: SubagentViewState,

    // ── input field ─────────────────────────────────────────────────────────
    /// Mirrors ratatui-textarea's internal scroll-top so scroll indicators
    /// reflect the actual rendered viewport position.
    pub input_scroll_top: usize,

    // ── history navigation helper ────────────────────────────────────────────
    /// Exposes history navigation for use by the presenter.
    pub _history_direction: Option<HistoryDirection>,

    // ── abort-exit flag ──────────────────────────────────────────────────────
    /// `true` when `TuiMode::Aborting` was entered via ctrl-c double-press,
    /// meaning the Driver should break the event loop once mode returns to
    /// Idle. `false` when Aborting was entered via Esc (soft cancel) — the
    /// TUI stays open so the user can continue chatting.
    pub aborting_exit: bool,

    // ── drill-in conversation model (Phase 6.10) ─────────────────────────────
    /// Which transcript (root or subagent) is currently rendered in the main
    /// chat area. Independent of `running_subagent`: the parent stays in
    /// `Root` view while a subagent streams in the background.
    pub active_view: ConversationView,
    /// Every subagent run's own conversation transcript/partial-text
    /// accumulator, keyed by `run_id` — one entry per `spawn_subagent`
    /// subagent that has ever started, added on `SubagentStarted` and never
    /// removed (mirrors `chat_entries`, which is also session-lifetime
    /// unbounded), so any of them can still be drilled into later even
    /// while another subagent is running/queued or has since finished. Fixes
    /// the bug where a single shared transcript was silently overwritten
    /// whenever the next queued subagent started, corrupting whichever
    /// subagent's conversation the user was currently viewing.
    pub subagent_conversations: HashMap<Uuid, SubagentConversation>,
    /// Instant each `bash` call started running, keyed by tool bubble id;
    /// drives the live duration counter while the bubble is `Running`.
    pub bash_started_at: HashMap<Uuid, Instant>,
    /// Provider/model identity for each constructed subagent engine, keyed by
    /// subagent run id. Subagent conversations can remain drillable after a run
    /// finishes, so these entries have the same session lifetime as their
    /// transcripts.
    pub subagent_engine_displays: HashMap<Uuid, EngineDisplayInfo>,
    /// Identifies every currently *running* subagent (several may run at once
    /// — see `SubagentManager`), independent of whether `active_view` has
    /// switched to one yet, and independent of which subagent the user is
    /// currently viewing. Keyed by `run_id`; empty when no subagent is running
    /// (earlier subagents may still have entries in `subagent_conversations`).
    /// Populated on `SubagentStarted`, removed on `SubagentDone`.
    pub running_subagents: HashMap<Uuid, SubagentRunInfo>,
    /// Ids of tool approvals and clarify questions awaiting the user, in
    /// arrival order (across the root and every subagent). Strict FIFO: the
    /// head is the prompt shown (`Focus::ToolApproval` or the clarify
    /// banner); the rest wait their turn. Entries whose approval bubble has
    /// left `Pending`, or whose queued clarify was resolved, are pruned
    /// lazily.
    pub pending_approval_order: Vec<Uuid>,
    /// Clarify questions that arrived while another approval or clarify was
    /// already on screen, keyed by clarify id. Each id is also in
    /// `pending_approval_order`, which decides when it is shown.
    pub queued_clarifies: HashMap<Uuid, QueuedClarify>,
    /// Set when a queued clarify was just shown; `presenter::update` turns it
    /// into `Effect::ParkSlashOverlay`.
    pub(crate) queued_clarify_activated: bool,
    /// `true` when `active_view` was switched automatically (an interactive
    /// pause — `note_interactive_pause`), to `Subagent` *or* `Root`, rather than
    /// by explicit user navigation (click/Esc). Gates whether the view
    /// auto-returns to `pre_pause_view` once that pause resolves or the
    /// owning subagent finishes.
    pub auto_switched_to_subagent: bool,
    /// Snapshot of `active_view` from immediately before an auto-switch
    /// (`note_interactive_pause`), so `resolve_possible_interactive_pause`
    /// can restore *that* view — which may be `Root` or a different,
    /// already-finished subagent's view the user had manually drilled into —
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
    /// `switch_to_subagent_view` right before it re-pins the shared scroll
    /// state to the bottom of the subagent's transcript. Restored by
    /// `switch_to_root_view` (Esc with an empty draft) as an immediate best-effort
    /// placeholder — the precise fix is `saved_root_scroll_bubble` below,
    /// resolved once root is rendered again. `None` before the first
    /// drill-in, or once restored.
    pub saved_root_scroll: Option<(usize, bool)>,
    /// `Uuid` of the `spawn_subagent` `ToolBubble` the user drilled in from,
    /// captured by `switch_to_subagent_view` (from the drilled-into run's
    /// `SubagentRunInfo::bubble_id`). Consumed by
    /// `switch_to_root_view`, which moves it into `pending_scroll_to_bubble`
    /// so `render::sync_from_draw` can scroll to that bubble's *current* row
    /// — robust against the root conversation having grown while the subagent
    /// view was open, unlike a raw saved offset which would drift. `None`
    /// before the first drill-in, or once consumed.
    pub saved_root_scroll_bubble: Option<uuid::Uuid>,
    /// Armed by `switch_to_root_view` with the `Uuid` from
    /// `saved_root_scroll_bubble`; consumed by `render::sync_from_draw`
    /// immediately after the first root frame following the switch is drawn
    /// (so `AppState::bubble_title_rows` reflects root's, not the subagent's,
    /// bubbles), which scrolls to that bubble's row and clears this field.
    /// `None` when there is nothing pending.
    pub pending_scroll_to_bubble: Option<uuid::Uuid>,

    // ── slash commands (Phase 7) ─────────────────────────────────────────────
    /// Suggestion list / model picker driven by the slash-command line.
    pub slash_overlay: SlashOverlay,
    /// Screen position where a slash-overlay text drag started.
    pub slash_overlay_drag_start: Option<(u16, u16)>,
    /// Detects double-clicks on rows of the suggestion, model and session lists.
    pub slash_overlay_click: super::DoubleClickTracker<usize>,
    /// Current screen position of a slash-overlay text drag.
    pub slash_overlay_drag_end: Option<(u16, u16)>,
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
            .and_then(|run_id| self.subagent_conversations.get(&run_id))
            .map_or(self.chat_entries.len(), |conversation| {
                conversation.messages.len()
            });
        for index in 0..length {
            *self.render_revisions.entry((run_id, index)).or_default() += 1;
        }
    }

    pub(super) fn release_active_render_layout(&mut self) {
        self.pending_bubble_expansion = None;
        self.render_cache.clear();
        self.conversation.chat_rendered_lines = Vec::new();
        self.conversation.chat_rendered_line_prefix_widths = Vec::new();
        self.conversation.bubble_title_rows = Vec::new();
        self.conversation.thinking_title_rows = Vec::new();
        self.conversation.chat_selection = None;
        self.subagent_view.header_rendered_lines = Vec::new();
        self.subagent_view.header_rendered_line_prefix_widths = Vec::new();
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
            session_title: None,
            tui_config: crate::config::TuiConfig::default(),
            approval_auto_expanded: std::collections::HashSet::new(),
            engine_display: EngineDisplayInfo::default(),
            context_usage: None,
            replay_compaction_boundary: None,
            compaction_requests: std::collections::HashMap::new(),

            chat_entries: Vec::new(),
            turn_buffer: TurnBuffer::default(),
            queued_steer_texts: Vec::new(),

            conversation: ConversationListState::default(),

            parked_slash_overlay: None,
            prompt_button_focus: None,
            input_activity: InputActivity::default(),

            pasted_blobs: PastedBlobs::default(),

            input_history: Vec::new(),
            history_index: None,
            draft: String::new(),

            steer_indicator: false,

            toast: None,
            spinner_frame: 0,
            pending_bubble_expansion: None,

            help_popup: HelpPopupState::default(),

            agents_popup: AgentsPopupState::default(),

            clarify_banner_state: ClarifyBannerState::default(),

            subagent_view: SubagentViewState::default(),

            input_scroll_top: 0,

            _history_direction: None,

            slash_overlay: SlashOverlay::default(),
            slash_overlay_drag_start: None,
            slash_overlay_click: super::DoubleClickTracker::default(),
            slash_overlay_drag_end: None,

            aborting_exit: false,

            active_view: ConversationView::Root,
            subagent_conversations: HashMap::new(),
            bash_started_at: HashMap::new(),
            subagent_engine_displays: HashMap::new(),
            running_subagents: HashMap::new(),
            pending_approval_order: Vec::new(),
            queued_clarifies: HashMap::new(),
            queued_clarify_activated: false,
            auto_switched_to_subagent: false,
            pre_pause_view: None,
            last_bubble_click: None,
            saved_root_scroll: None,
            saved_root_scroll_bubble: None,
            pending_scroll_to_bubble: None,
        }
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
        self.subagent_conversations.clear();
        self.running_subagents.clear();
        self.pending_approval_order.clear();
        self.queued_clarifies.clear();
        self.active_view = ConversationView::Root;
        self.subagent_view = SubagentViewState::default();
        self.conversation.chat_selection = None;
        self.conversation.scroll_offset = 0;
        self.conversation.scroll_locked = true;
        self.history_index = None;
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
