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

//! Conversation / navigation state and operations.

use super::{
    AppState, BubbleExpansionTarget, ChatEntry, ConversationView, Focus, ToolCallState, TuiMode,
};
use uuid::Uuid;

impl AppState {
    /// Called whenever a `ToolPending`/`ClarifyPending` bubble tagged with
    /// `run_id` is created. Auto-switches `active_view` to wherever this
    /// bubble actually lives — the root conversation when `run_id` is `None`
    /// or unrecognised, or that specific child's transcript when `run_id`
    /// identifies a tracked child — regardless of which conversation (root
    /// or any other subagent) is currently on screen, so an approval is
    /// never stranded behind an unrelated transcript. Armed for auto-return
    /// via `auto_switched_to_child`, snapshotting whatever view was active
    /// beforehand into `pre_pause_view` so `resolve_possible_interactive_pause`
    /// can restore it exactly. If the user is already looking at the view
    /// this bubble belongs to (manually or via an earlier auto-switch), no
    /// further switch/re-arm happens — `auto_switched_to_child` is reset to
    /// `false` (the user is there on their own terms; no auto-return).
    pub(crate) fn note_interactive_pause(&mut self, run_id: Option<Uuid>) {
        let target_view = match run_id.and_then(|id| self.child_conversations.get(&id)) {
            Some(conversation) => ConversationView::Child {
                run_id: conversation.info.run_id,
                agent_type: conversation.info.agent_type.clone(),
                task: conversation.info.task.clone(),
                label: conversation.info.label.clone(),
            },
            None => ConversationView::Root,
        };
        let already_viewing_target = match (&self.active_view, &target_view) {
            (ConversationView::Root, ConversationView::Root) => true,
            (
                ConversationView::Child { run_id: a, .. },
                ConversationView::Child { run_id: b, .. },
            ) => a == b,
            _ => false,
        };
        if already_viewing_target {
            self.auto_switched_to_child = false;
            return;
        }
        self.pre_pause_view = Some(self.active_view.clone());
        self.release_active_render_layout();
        self.active_view = target_view;
        self.auto_switched_to_child = true;
    }

    /// Called whenever a tool/clarify bubble identified by `id` reaches a
    /// terminal state (or is denied). If `id` lives in whichever transcript
    /// is currently being viewed (root or a child) and the drill-in view
    /// was auto-switched there for it, auto-returns to whatever view was
    /// active before the auto-switch (`pre_pause_view` — `Root` or a
    /// different child's view), falling back to `Root` if none was
    /// recorded — matches Phase 6.10 step 74: "When child interactive pause
    /// resolves: auto-return to `Root` only if `auto_switched_to_child` is
    /// `true`," generalized to cover an auto-switch into `Root` for a
    /// root-owned approval as well.
    pub(crate) fn resolve_possible_interactive_pause(&mut self, id: Uuid) {
        if !self.auto_switched_to_child {
            self.resume_pending_tool_approval();
            return;
        }
        let bubble_is_in_current_view = match &self.active_view {
            ConversationView::Root => self.chat_entries.iter().any(
                |entry| matches!(entry, ChatEntry::ToolBubble { id: entry_id, .. } if *entry_id == id),
            ),
            ConversationView::Child { run_id, .. } => {
                self.child_conversations.get(run_id).is_some_and(|conversation| {
                    conversation.messages.iter().any(
                        |entry| matches!(entry, ChatEntry::ToolBubble { id: entry_id, .. } if *entry_id == id),
                    )
                })
            }
        };
        if bubble_is_in_current_view {
            self.release_active_render_layout();
            self.active_view = self.pre_pause_view.take().unwrap_or(ConversationView::Root);
            self.auto_switched_to_child = false;
        }
        self.resume_pending_tool_approval();
    }

    pub(in crate::tui_reducer_state) fn resume_pending_tool_approval(&mut self) {
        if matches!(self.focus, Focus::HelpPopup)
            || matches!(
                self.mode,
                TuiMode::ClarifyPending { .. } | TuiMode::Aborting
            )
        {
            return;
        }
        if let Focus::ToolApproval { tool_id, .. } = self.focus {
            if matches!(
                self.find_tool_bubble_mut(tool_id),
                Some(ChatEntry::ToolBubble { state, .. })
                    if matches!(state.as_ref(), ToolCallState::Pending { .. })
            ) {
                return;
            }
            self.focus = Focus::MainInput;
        }
        if let Some((tool_id, tool_name)) = self.pending_tool_approval() {
            let run_id = self
                .child_conversations
                .iter()
                .find_map(|(run_id, conversation)| {
                    conversation.messages.iter().any(
                    |entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == tool_id),
                ).then_some(*run_id)
                });
            self.focus = Focus::ToolApproval { tool_id, tool_name };
            self.note_interactive_pause(run_id);
        }
    }

    pub fn switch_to_child_view(&mut self, run_id: Uuid) {
        let Some(info) = self
            .child_conversations
            .get(&run_id)
            .map(|conversation| conversation.info.clone())
        else {
            return;
        };
        self.release_active_render_layout();
        self.active_view = ConversationView::Child {
            run_id: info.run_id,
            agent_type: info.agent_type,
            task: info.task,
            label: info.label,
        };
        self.auto_switched_to_child = false;
        // Capture the root's current scroll position before it gets
        // overwritten below, so `switch_to_root_view` can restore it as a
        // best-effort fallback (see `saved_root_scroll_bubble` below for the
        // primary, content-shift-proof resolution).
        self.saved_root_scroll = Some((
            self.conversation.scroll_offset,
            self.conversation.scroll_locked,
        ));
        // Record the actual `spawn_subagent` bubble being drilled into, so
        // `switch_to_root_view` can scroll back to *its* current row rather
        // than a raw offset that would drift if the root conversation grows
        // while the user is away (new messages/tool bubbles shift row
        // indices below — sometimes even at — the saved offset).
        self.saved_root_scroll_bubble = Some(info.bubble_id);
        // Always start scrolled to the top of the header bubble on a fresh
        // navigation into the child view, regardless of where it was left
        // scrolled to on a previous visit.
        self.child_view.header_bubble_scroll_offset = 0;
        self.child_view.header_selection = None;
        self.child_view.header_drag_active = false;
        // The main chat area's `scroll_offset`/`scroll_locked` are shared
        // between the root and child views (single fields, not per-view) —
        // if the user had scrolled away from the bottom while viewing
        // `Root`, that stale, non-bottom position would otherwise carry
        // over into the child's transcript. Drilling in while the subagent
        // is running should always land on its latest activity, so
        // re-pin to the bottom on every navigation into the child view.
        self.scroll_to_bottom();
    }

    /// Finds the `id` of the real root `spawn_subagent` `ToolBubble`
    /// currently re-rendered as the child-view header (see
    /// `render::project_for_draw`'s `child_view_bubble`) — i.e. the bubble
    /// that belongs to whichever child `active_view` currently identifies.
    /// `None` while viewing `Root`, or if that child is somehow untracked.
    pub(crate) fn header_bubble_id(&self) -> Option<Uuid> {
        let ConversationView::Child { run_id, .. } = &self.active_view else {
            return None;
        };
        self.child_conversations
            .get(run_id)
            .map(|conversation| conversation.info.bubble_id)
    }

    /// Finds the `run_id` of the tracked child (running, queued, or already
    /// finished) whose root `spawn_subagent` `ToolBubble` is `bubble_id` —
    /// the inverse of `header_bubble_id`/`ChildRunInfo::bubble_id`. Used by
    /// the double-click handler to resolve *which* child a specific clicked
    /// bubble belongs to, rather than assuming "the" (singular) child, since
    /// multiple `spawn_subagent` bubbles (active + queued, or already
    /// finished) can coexist in `chat_entries`. `None` if `bubble_id` isn't
    /// (or is no longer) a tracked child's bubble.
    pub(crate) fn run_id_for_bubble(&self, bubble_id: Uuid) -> Option<Uuid> {
        self.child_conversations
            .iter()
            .find(|(_, conversation)| conversation.info.bubble_id == bubble_id)
            .map(|(run_id, _)| *run_id)
    }

    /// Toggle the collapsed state of the child-view header bubble (see
    /// `header_bubble_id`). Unlike `toggle_tool_bubble_collapsed`, this does
    /// not touch `scroll_offset`/`scroll_locked`/`bubble_title_rows` — those
    /// describe the currently-visible chat area (the child's own
    /// transcript), which the header bubble is not part of; any resulting
    /// change in the header's own content height is instead clamped by
    /// `tui_widgets::draw` against `header_bubble_scroll_offset` each frame. No-op
    /// while viewing `Root` (no header bubble is shown).
    pub fn toggle_header_bubble_collapsed(&mut self) {
        let Some(target_id) = self.header_bubble_id() else {
            return;
        };
        for entry in &mut self.chat_entries {
            if let ChatEntry::ToolBubble { id, collapsed, .. } = entry {
                if *id == target_id {
                    *collapsed = !*collapsed;
                    self.pending_bubble_expansion =
                        (!*collapsed).then_some(BubbleExpansionTarget::Tool(target_id));
                    return;
                }
            }
        }
    }

    /// Manual "back to parent" navigation (Esc with an empty draft). Restores the root
    /// scroll position saved by `switch_to_child_view` (when present) as an
    /// immediate best-effort placeholder — avoids a blank/wrong-looking frame
    /// before the precise fix below lands — and arms `pending_scroll_to_bubble`
    /// with the drilled-into `spawn_subagent` bubble's id so `render.rs`'s
    /// `sync_from_draw` can scroll to that bubble's *current* row once root
    /// is rendered again this same frame. Resolving by bubble id (rather than
    /// trusting the raw saved offset) is what makes this correct even though
    /// the root conversation may have grown while the user was viewing the
    /// child — new messages/tool bubbles can shift row indices at or below
    /// the saved offset, landing it past the target bubble otherwise.
    /// `saved_root_scroll`/`saved_root_scroll_bubble` are `None` when the
    /// child view was entered via auto-switch (`note_interactive_pause`)
    /// rather than manual navigation; in that case the shared scroll fields
    /// were never touched, so there is nothing to restore.
    pub fn switch_to_root_view(&mut self) {
        self.release_active_render_layout();
        self.active_view = ConversationView::Root;
        self.auto_switched_to_child = false;
        if let Some((scroll_offset, scroll_locked)) = self.saved_root_scroll.take() {
            self.conversation.scroll_offset = scroll_offset;
            self.conversation.scroll_locked = scroll_locked;
        }
        self.pending_scroll_to_bubble = self.saved_root_scroll_bubble.take();
    }

    /// Scroll chat up (toward earlier content); disables auto-scroll.
    pub fn scroll_up(&mut self, lines: usize) {
        self.conversation.scroll_locked = false;
        self.conversation.scroll_offset = self.conversation.scroll_offset.saturating_sub(lines);
    }

    /// Scroll chat down (toward later content); disables auto-scroll.
    pub fn scroll_down(&mut self, lines: usize) {
        self.conversation.scroll_locked = false;
        self.conversation.scroll_offset = self.conversation.scroll_offset.saturating_add(lines);
    }

    /// Re-enable auto-scroll (jump to bottom).
    pub fn scroll_to_bottom(&mut self) {
        self.conversation.scroll_locked = true;
        self.conversation.scroll_offset = 0;
    }

    /// Scroll the child-view header bubble up (toward earlier content).
    pub fn header_bubble_scroll_up(&mut self, lines: usize) {
        self.child_view.header_bubble_scroll_offset = self
            .child_view
            .header_bubble_scroll_offset
            .saturating_sub(lines);
    }

    /// Scroll the child-view header bubble down (toward later content). The
    /// upper bound is applied in `tui_widgets::draw`, which knows the total content
    /// height.
    pub fn header_bubble_scroll_down(&mut self, lines: usize) {
        self.child_view.header_bubble_scroll_offset = self
            .child_view
            .header_bubble_scroll_offset
            .saturating_add(lines);
    }
}
