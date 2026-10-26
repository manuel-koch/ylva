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

//! Conversation / tools state and operations.

use super::super::{ConversationView, STREAMING_THINKING_ENTRY_INDEX};
use super::{AppState, BubbleExpansionTarget, ChatEntry, Focus, ToolCallState, TuiMode};
use crate::tui_reducer_state::QueuedClarify;
use uuid::Uuid;

/// The next prompt waiting for the user, as picked by
/// [`AppState::next_pending_interaction`].
pub(crate) enum PendingInteraction {
    Approval { tool_id: Uuid, tool_name: String },
    Clarify(QueuedClarify),
}

impl AppState {
    /// Find the most-recent `ToolBubble` with `id == target_id`, searching
    /// every subagent's own transcript (`subagent_conversations`) and the root
    /// (`chat_entries`) — a bubble's `id` (a `Uuid`) is unique regardless of
    /// which transcript it was created in (see `target_messages_mut`), so
    /// updaters (`apply_tool_*`) can stay agnostic of which one it lives in.
    pub fn find_tool_bubble_mut(&mut self, target_id: Uuid) -> Option<&mut ChatEntry> {
        self.find_tool_bubble_for_render_mutation(target_id, true)
    }

    pub(crate) fn find_tool_bubble_title_mut(&mut self, target_id: Uuid) -> Option<&mut ChatEntry> {
        *self.render_title_revisions.entry(target_id).or_default() += 1;
        self.find_tool_bubble_for_render_mutation(target_id, false)
    }

    fn find_tool_bubble_for_render_mutation(
        &mut self,
        target_id: Uuid,
        content_changed: bool,
    ) -> Option<&mut ChatEntry> {
        for (run_id, conversation) in &mut self.subagent_conversations {
            if let Some(pos) = conversation.messages.iter().rposition(
                |entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == target_id),
            ) {
                if content_changed {
                    *self
                        .render_revisions
                        .entry((Some(*run_id), pos))
                        .or_default() += 1;
                }
                return conversation.messages.get_mut(pos);
            }
        }
        let position = self.chat_entries.iter().rposition(
            |entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == target_id),
        )?;
        if content_changed {
            *self.render_revisions.entry((None, position)).or_default() += 1;
        }
        self.chat_entries.get_mut(position)
    }

    /// Target transcript for a *new* tool/clarify bubble tagged with
    /// `run_id`: that subagent's own transcript when `run_id` identifies any
    /// tracked subagent run (running, queued, or already finished — see
    /// `subagent_conversations`), the root transcript otherwise (`run_id` is
    /// `None`, or an unrecognised id).
    pub(crate) fn target_messages_mut(&mut self, run_id: Option<Uuid>) -> &mut Vec<ChatEntry> {
        if let Some(run_id) = run_id {
            if let Some(conversation) = self.subagent_conversations.get_mut(&run_id) {
                return &mut conversation.messages;
            }
        }
        &mut self.chat_entries
    }

    /// The head of the queued tool approvals, as `(tool_id, tool_name)`, when
    /// its bubble is still in [`ToolCallState::Pending`] in the root or a subagent
    /// transcript.
    ///
    /// This is the authoritative "an approval is outstanding" marker —
    /// arming `Focus::ToolApproval` pushes such a bubble in the same event
    /// and a verdict resolves both together — so it stays usable while the
    /// help popup has temporarily replaced the focus.
    pub fn pending_tool_approval(&self) -> Option<(Uuid, String)> {
        self.pending_approval_order.iter().find_map(|tool_id| {
            self.pending_approval_name(*tool_id)
                .map(|name| (*tool_id, name))
        })
    }

    /// The earliest-arrived prompt still waiting (approval or queued
    /// clarify), so approvals and clarify questions are shown in strict
    /// arrival order.
    pub(crate) fn next_pending_interaction(&self) -> Option<PendingInteraction> {
        self.pending_approval_order.iter().find_map(|id| {
            if let Some(request) = self.queued_clarifies.get(id) {
                return Some(PendingInteraction::Clarify(request.clone()));
            }
            self.pending_approval_name(*id)
                .map(|tool_name| PendingInteraction::Approval {
                    tool_id: *id,
                    tool_name,
                })
        })
    }

    /// Name of the tool bubble `tool_id` when it is still awaiting approval
    /// in the root or any subagent transcript.
    fn pending_approval_name(&self, tool_id: Uuid) -> Option<String> {
        fn find_in(messages: &[ChatEntry], tool_id: Uuid) -> Option<String> {
            messages.iter().find_map(|entry| match entry {
                ChatEntry::ToolBubble {
                    id, name, state, ..
                } if *id == tool_id && matches!(**state, ToolCallState::Pending { .. }) => {
                    Some(name.clone())
                }
                _ => None,
            })
        }
        find_in(&self.chat_entries, tool_id).or_else(|| {
            self.subagent_conversations
                .values()
                .find_map(|conversation| find_in(&conversation.messages, tool_id))
        })
    }

    /// Whether the clarify bubble `id` is still running, i.e. unanswered.
    fn clarify_bubble_is_running(&self, id: Uuid) -> bool {
        fn find_in(messages: &[ChatEntry], id: Uuid) -> bool {
            messages.iter().any(|entry| {
                matches!(
                    entry,
                    ChatEntry::ToolBubble { id: entry_id, state, .. }
                        if *entry_id == id && matches!(**state, ToolCallState::Running { .. })
                )
            })
        }
        find_in(&self.chat_entries, id)
            || self
                .subagent_conversations
                .values()
                .any(|conversation| find_in(&conversation.messages, id))
    }

    /// Drops queued prompts that were resolved — an approval whose bubble
    /// left `Pending` (answered, denied, stopped, or cleared), or a queued
    /// clarify that is no longer running — keeping the arrival order of the
    /// rest.
    pub(crate) fn prune_pending_approval_order(&mut self) {
        let still_pending: Vec<Uuid> = self
            .pending_approval_order
            .iter()
            .copied()
            .filter(|id| {
                self.pending_approval_name(*id).is_some()
                    || (self.queued_clarifies.contains_key(id)
                        && self.clarify_bubble_is_running(*id))
            })
            .collect();
        self.queued_clarifies
            .retain(|id, _| still_pending.contains(id));
        self.pending_approval_order = still_pending;
    }

    /// Mark unfinished tool bubbles in a cancelled subagent transcript as
    /// killed, returning their ids so the presenter can release any approval
    /// focus and resume the next queued prompt.
    pub(crate) fn kill_unfinished_subagent_tools(&mut self, run_id: Uuid) -> Vec<Uuid> {
        let Some(conversation) = self.subagent_conversations.get_mut(&run_id) else {
            return Vec::new();
        };
        let mut killed_tool_ids = Vec::new();
        for entry in &mut conversation.messages {
            let ChatEntry::ToolBubble { id, state, .. } = entry else {
                continue;
            };
            if matches!(
                state.as_ref(),
                ToolCallState::Pending { .. } | ToolCallState::Running { .. }
            ) {
                **state = ToolCallState::Killed;
                killed_tool_ids.push(*id);
            }
        }
        self.prune_pending_approval_order();
        killed_tool_ids
    }

    /// Toggle the collapsed/expanded state of the tool bubble with the given id.
    ///
    /// Collapsing a bubble whose body extends below the viewport shrinks the
    /// total content height, which can push `scroll_offset` past the new
    /// `max_offset` and re-lock the view to the bottom, scrolling the bubble's
    /// title out of sight. Clamping `scroll_offset` to the bubble's own start
    /// row keeps that title row visible in the current viewport.
    ///
    /// Delegates the actual lookup to `find_tool_bubble_mut`, which searches
    /// both the root (`chat_entries`) and every tracked subagent's own
    /// transcript (`subagent_conversations`) — the click that triggers this can
    /// target a bubble in any of them, depending on `active_view`.
    pub fn toggle_tool_bubble_collapsed(&mut self, target_id: Uuid) {
        let expanded = if let Some(ChatEntry::ToolBubble { collapsed, .. }) =
            self.find_tool_bubble_for_render_mutation(target_id, false)
        {
            *collapsed = !*collapsed;
            Some(!*collapsed)
        } else {
            None
        };
        if let Some(expanded) = expanded {
            self.pending_bubble_expansion =
                expanded.then_some(BubbleExpansionTarget::Tool(target_id));
            self.conversation.scroll_locked = false;
            if let Some((start_row, _, _)) = self
                .conversation
                .bubble_title_rows
                .iter()
                .find(|(_, _, bubble_id)| *bubble_id == target_id)
            {
                self.conversation.scroll_offset = self.conversation.scroll_offset.min(*start_row);
            }
        }
    }

    /// Toggle the collapsed state of the `ChatEntry::ThinkingMessage` at
    /// `target_index` in `chat_entries`. Mirrors `toggle_tool_bubble_collapsed`
    /// but is index-keyed since thinking bubbles carry no `Uuid`.
    pub fn toggle_thinking_collapsed(&mut self, target_index: usize) {
        let entries = match self.active_view {
            ConversationView::Root => &mut self.chat_entries,
            ConversationView::Subagent { run_id, .. } => {
                let Some(conversation) = self.subagent_conversations.get_mut(&run_id) else {
                    return;
                };
                &mut conversation.messages
            }
        };
        if let Some(ChatEntry::ThinkingMessage { collapsed, .. }) = entries.get_mut(target_index) {
            *collapsed = !*collapsed;
            self.pending_bubble_expansion =
                (!*collapsed).then_some(BubbleExpansionTarget::Thinking(target_index));
            self.conversation.scroll_locked = false;
            if let Some((start_row, _, _)) = self
                .conversation
                .thinking_title_rows
                .iter()
                .find(|(_, _, entry_index)| *entry_index == target_index)
            {
                self.conversation.scroll_offset = self.conversation.scroll_offset.min(*start_row);
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
        let turn = match self.active_view {
            ConversationView::Root => &mut self.turn_buffer,
            ConversationView::Subagent { run_id, .. } => {
                let Some(conversation) = self.subagent_conversations.get_mut(&run_id) else {
                    return;
                };
                &mut conversation.turn_buffer
            }
        };
        turn.partial_thinking_collapsed = !turn.partial_thinking_collapsed;
        self.pending_bubble_expansion = (!turn.partial_thinking_collapsed).then_some(
            BubbleExpansionTarget::Thinking(STREAMING_THINKING_ENTRY_INDEX),
        );
        self.conversation.scroll_locked = false;
        if let Some((start_row, _, _)) = self
            .conversation
            .thinking_title_rows
            .iter()
            .find(|(_, _, entry_index)| *entry_index == STREAMING_THINKING_ENTRY_INDEX)
        {
            self.conversation.scroll_offset = self.conversation.scroll_offset.min(*start_row);
        }
    }

    /// Update the tool bubble for `id` to the Error state. If `id` was the
    /// tool awaiting approval, clear the approval focus and reset mode to Idle
    /// unless an abort is waiting for the agent's explicit idle boundary.
    pub fn apply_tool_error(
        &mut self,
        id: Uuid,
        error: String,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
        run_id: Option<Uuid>,
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
            self.target_messages_mut(run_id).push(ChatEntry::ErrorLine {
                message: error.clone(),
            });
        }
        if matches!(self.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id)
            && !matches!(self.mode, TuiMode::Aborting)
        {
            self.focus = Focus::MainInput;
            self.mode = TuiMode::Idle;
        }
        self.resolve_possible_interactive_pause(id);
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
        run_id: Option<Uuid>,
    ) {
        self.target_messages_mut(run_id)
            .push(ChatEntry::ToolBubble {
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
                        ..
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
        self.resolve_possible_interactive_pause(id);
    }

    /// Commit an approval locally before the corresponding `ToolStart` arrives.
    pub(crate) fn apply_tool_approved(&mut self, id: Uuid) {
        if let Some(ChatEntry::ToolBubble { state, .. }) = self.find_tool_bubble_mut(id) {
            if let ToolCallState::Pending {
                arguments,
                content_sections,
                title_arguments,
            } = state.as_mut()
            {
                **state = ToolCallState::Running {
                    arguments: std::mem::take(arguments),
                    content_sections: std::mem::take(content_sections),
                    title_arguments: std::mem::take(title_arguments),
                    progress_arguments: vec![],
                };
            }
        }
        self.resolve_possible_interactive_pause(id);
    }
}
