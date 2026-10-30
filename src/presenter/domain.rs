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

//! Pure domain event handling.

use super::tool_bubbles::{
    push_clarify_pending_bubble, push_tool_pending_bubble, update_tool_bubble_progress,
    update_tool_bubble_to_denied, update_tool_bubble_to_done, update_tool_bubble_to_killed,
    update_tool_bubble_to_running, ToolDonePayload,
};
use crate::agent::subagent_manager::STATUS_CANCELLED;
use crate::tui_reducer_state::{
    AppState, ChatEntry, ConfirmKey, ContextUsage, ConversationView, DomainEvent, Effect, Focus,
    SubagentOutcome, SubagentRunInfo, TuiMode, UserMessageState,
};
use std::time::Instant;

// ─── Domain routing ───────────────────────────────────────────────────────────

pub(super) fn route_domain(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match &event {
        DomainEvent::SubagentToken { run_id, .. } | DomainEvent::SubagentDone { run_id, .. } => {
            state.revise_stream(Some(*run_id))
        }
        DomainEvent::StreamStarted
        | DomainEvent::StreamChunk(_)
        | DomainEvent::StreamThinkingChunk(_)
        | DomainEvent::StreamDone
        | DomainEvent::StreamCancelled => state.revise_stream(None),
        DomainEvent::CompactionApplied { run_id, .. } => {
            state.invalidate_transcript_layout(*run_id)
        }
        _ => {}
    }
    if matches!(event, DomainEvent::StreamCancelled) {
        state.compaction_requests.remove(&None);
    }
    let event_kind = event.kind();
    let correlation_id = event.correlation_id();
    let mode_before = state.mode.kind();
    // Subagent-lifecycle events never depend on / alter the parent's own
    // `TuiMode` (that's driven exclusively by the root's own stream/tool
    // state) — handle them uniformly ahead of the per-mode dispatch below.
    let effects = domain_subagent_lifecycle(state, event);
    // Deliberately logs only the event's variant tag, its correlation `id`
    // (when it has one), and the mode transition — never chunk text, tool
    // arguments, or tool output — just enough to follow the state-transition
    // flow and match up a call's `ToolStart`/`ToolProgress`/`ToolDone` (or
    // `ClarifyPending`) sequence across log lines.
    tracing::debug!(
        event = event_kind,
        correlation_id = ?correlation_id,
        mode_before,
        mode_after = state.mode.kind(),
        "domain event handled"
    );
    effects
}

/// Handles subagent lifecycle/configuration events ahead of the mode-specific
/// dispatch; every other event falls through unchanged to the per-`TuiMode`
/// handlers.
fn domain_subagent_lifecycle(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::CompactionActivity {
            run_id,
            request_id,
            active,
        } => {
            if active {
                state.compaction_requests.insert(run_id, request_id);
            } else if state.compaction_requests.get(&run_id) == Some(&request_id) {
                state.compaction_requests.remove(&run_id);
            }
            vec![]
        }
        DomainEvent::SubagentStarted {
            run_id,
            call_id,
            agent_type,
            task,
            label,
        } => {
            state.start_subagent_run(SubagentRunInfo {
                run_id,
                bubble_id: call_id,
                agent_type,
                task,
                label,
            });
            state.subagent_view.header_bubble_scroll_offset = 0;
            // No synthetic "task" message is seeded here: the subagent-view
            // header re-renders the real root `spawn_subagent` bubble (see
            // `render::project_for_draw`'s `subagent_view_bubble`), which
            // already shows the task/label/agent_type — duplicating it as
            // the transcript's first turn would just repeat the same text.
            vec![]
        }
        DomainEvent::SubagentEngineConfigured { run_id, engine } => {
            state.subagent_engine_displays.insert(run_id, engine);
            vec![]
        }
        DomainEvent::SubagentToken {
            run_id,
            text,
            is_thinking,
        } => {
            if let Some(conversation) = state.subagent_conversations.get_mut(&run_id) {
                if is_thinking {
                    conversation
                        .turn_buffer
                        .push_thinking_chunk(&text, &mut conversation.messages);
                } else {
                    conversation
                        .turn_buffer
                        .push_response_chunk(&text, &mut conversation.messages);
                }
            }
            vec![]
        }
        DomainEvent::SubagentContextUsage {
            run_id,
            estimated_tokens,
            context_length,
            source,
        } => {
            if let Some(conversation) = state.subagent_conversations.get_mut(&run_id) {
                conversation.context_usage = Some(ContextUsage {
                    estimated_tokens,
                    context_length,
                    source,
                });
            }
            vec![]
        }
        DomainEvent::SubagentDone { run_id, status, .. } => {
            state.compaction_requests.remove(&Some(run_id));
            state.flush_subagent_partial_text(run_id);
            if let Some(conversation) = state.subagent_conversations.get_mut(&run_id) {
                conversation.finished_at = Some(Instant::now());
                conversation.outcome = Some(SubagentOutcome::from_status(&status));
            }
            if status == STATUS_CANCELLED {
                for tool_id in state.kill_unfinished_subagent_tools(run_id) {
                    if matches!(state.focus, Focus::ToolApproval { tool_id: focused_id, .. } if focused_id == tool_id)
                    {
                        state.focus = Focus::MainInput;
                    }
                    state.resolve_possible_interactive_pause(tool_id);
                }
            }
            // Only auto-return if the user is currently viewing *this*
            // finished subagent's transcript — a different subagent may still be
            // running/queued, or the user may be looking at an unrelated
            // (earlier-finished) subagent's transcript, in which case this
            // event must not disturb `active_view`.
            let is_viewing_this_subagent = matches!(
                state.active_view,
                ConversationView::Subagent { run_id: viewed_run_id, .. } if viewed_run_id == run_id
            );
            if is_viewing_this_subagent && state.auto_switched_to_subagent {
                state.switch_to_root_view();
            }
            state.running_subagents.remove(&run_id);
            vec![]
        }
        DomainEvent::BubbleClickWindowExpired => {
            // Commit the deferred toggle from a single (non-double) click on
            // a spawn bubble's title — see the click handler in `update()`.
            // A no-op if a confirming second click already resolved (and
            // cleared) the arm first.
            if let Some((bubble_id, _)) = state.last_bubble_click.take() {
                state.toggle_tool_bubble_collapsed(bubble_id);
            }
            vec![]
        }
        DomainEvent::TypingSettled => {
            state.settle_typing();
            let was_visible = state.prompt_is_visible();
            state.resume_deferred_prompt();
            // A deferred clarify is reported through `queued_clarify_activated`.
            if !was_visible && state.is_approval_pending() {
                vec![Effect::ParkSlashOverlay]
            } else {
                vec![]
            }
        }
        DomainEvent::BubbleExpansionFeedbackShown => {
            state.pending_bubble_expansion = None;
            vec![]
        }
        DomainEvent::CompactionApplied {
            run_id,
            entry_index,
            summarized_messages,
        } => {
            let entries = match run_id {
                Some(run_id) => {
                    let Some(conversation) = state.subagent_conversations.get_mut(&run_id) else {
                        tracing::warn!(%run_id, "compaction marker has no owning subagent conversation");
                        return vec![];
                    };
                    &mut conversation.messages
                }
                None => &mut state.chat_entries,
            };
            // Mode-independent like the other transcript bookkeeping events:
            // a sweep applies compaction at a settled boundary, whatever the
            // view state happens to be. Duplicate emissions are dropped by
            // the boundary itself — it only ever grows.
            let already_marked = entries.iter().any(|entry| {
                matches!(
                    entry,
                    ChatEntry::CompactionMarker {
                        summarized_messages: existing
                    } if *existing == summarized_messages
                )
            });
            if !already_marked {
                let insert_at = entry_index.min(entries.len());
                entries.insert(
                    insert_at,
                    ChatEntry::CompactionMarker {
                        summarized_messages,
                    },
                );
            }
            vec![]
        }
        DomainEvent::ContextUsage {
            estimated_tokens,
            context_length,
            source,
        } => {
            // Mode-independent like the subagent-lifecycle events: a settled
            // turn reports the readout while the loop is idle, and a summary
            // outcome can do so without touching the mode.
            state.context_usage = Some(ContextUsage {
                estimated_tokens,
                context_length,
                source,
            });
            vec![]
        }
        other => match state.mode.clone() {
            TuiMode::Idle => domain_idle(state, other),
            TuiMode::Pending => domain_pending(state, other),
            TuiMode::Streaming => domain_streaming(state, other),
            TuiMode::ClarifyPending { .. } => domain_clarify_pending(state, other),
            TuiMode::Aborting => domain_aborting(state, other),
            TuiMode::DoublePressAbort {
                pressed_at,
                return_to,
                armed_by,
            } => domain_double_press(state, other, pressed_at, return_to, armed_by),
        },
    }
}
fn domain_idle(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::StreamStarted => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Streaming;
            state.turn_buffer.partial_assistant_text.clear();
            state.turn_buffer.partial_thinking_text.clear();
            vec![]
        }
        DomainEvent::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => push_tool_pending_bubble(
            state,
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        ),
        DomainEvent::ClarifyPending {
            id,
            question,
            context,
            choices,
            run_id,
        } => push_clarify_pending_bubble(state, id, question, context, choices, run_id),
        DomainEvent::Error { message, run_id } => {
            if run_id.is_none() {
                state.mode = TuiMode::Idle;
            }
            state
                .target_messages_mut(run_id)
                .push(ChatEntry::ErrorLine { message });
            vec![]
        }
        DomainEvent::SteerQueued { run_id } => {
            // No driver routes `Prompt` to a subagent today (see
            // `UiUpdate::SteerQueued::run_id`), so `run_id: Some(_)` is
            // unreachable in practice; guard anyway rather than silently
            // flipping the root's own indicator for a subagent steer.
            if run_id.is_none() {
                state.steer_indicator = true;
            }
            vec![]
        }
        DomainEvent::SteerApplied { run_id } => {
            // See `DomainEvent::SteerQueued` above: only the root's own
            // steer queue/indicator is meaningful today.
            if run_id.is_none() {
                state.commit_queued_steer_messages(UserMessageState::Steered);
                state.steer_indicator = false;
            }
            vec![]
        }
        DomainEvent::AgentIdle => {
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            vec![]
        }
        DomainEvent::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
                run_id,
            );
            vec![]
        }
        DomainEvent::ToolProgress {
            id,
            title_arguments,
        } => {
            update_tool_bubble_progress(state, id, title_arguments);
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => {
            update_tool_bubble_to_done(
                state,
                ToolDonePayload {
                    id,
                    name,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    run_id,
                },
            );
            vec![]
        }
        DomainEvent::ToolKilled { id, run_id } => {
            update_tool_bubble_to_killed(state, id, run_id);
            vec![]
        }
        DomainEvent::ToolDenied { id } => {
            update_tool_bubble_to_denied(state, id);
            if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
                state.focus = Focus::MainInput;
            }
            state.resolve_possible_interactive_pause(id);
            vec![]
        }
        DomainEvent::ConfirmExpired => vec![],
        _ => vec![],
    }
}

fn domain_pending(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::StreamStarted => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Streaming;
            state.turn_buffer.partial_assistant_text.clear();
            state.turn_buffer.partial_thinking_text.clear();
            vec![]
        }
        DomainEvent::StreamDone => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::StreamCancelled => {
            state.flush_partial_assistant_message(true);
            state.commit_queued_steer_messages(UserMessageState::Cancelled);
            state.steer_indicator = false;
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::Error { message, run_id } => {
            if run_id.is_none() {
                state.mode = TuiMode::Idle;
            }
            state
                .target_messages_mut(run_id)
                .push(ChatEntry::ErrorLine { message });
            vec![]
        }
        DomainEvent::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => push_tool_pending_bubble(
            state,
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        ),
        DomainEvent::ClarifyPending {
            id,
            question,
            context,
            choices,
            run_id,
        } => push_clarify_pending_bubble(state, id, question, context, choices, run_id),
        DomainEvent::SteerQueued { run_id } => {
            // No driver routes `Prompt` to a subagent today (see
            // `UiUpdate::SteerQueued::run_id`), so `run_id: Some(_)` is
            // unreachable in practice; guard anyway rather than silently
            // flipping the root's own indicator for a subagent steer.
            if run_id.is_none() {
                state.steer_indicator = true;
            }
            vec![]
        }
        DomainEvent::SteerApplied { run_id } => {
            // See `DomainEvent::SteerQueued` above: only the root's own
            // steer queue/indicator is meaningful today.
            if run_id.is_none() {
                state.commit_queued_steer_messages(UserMessageState::Steered);
                state.steer_indicator = false;
            }
            vec![]
        }
        DomainEvent::AgentIdle => {
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            vec![]
        }
        DomainEvent::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
                run_id,
            );
            vec![]
        }
        DomainEvent::ToolProgress {
            id,
            title_arguments,
        } => {
            update_tool_bubble_progress(state, id, title_arguments);
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => {
            update_tool_bubble_to_done(
                state,
                ToolDonePayload {
                    id,
                    name,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    run_id,
                },
            );
            vec![]
        }
        DomainEvent::ToolKilled { id, run_id } => {
            update_tool_bubble_to_killed(state, id, run_id);
            vec![]
        }
        DomainEvent::ToolDenied { id } => {
            update_tool_bubble_to_denied(state, id);
            if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
                state.focus = Focus::MainInput;
            }
            state.resolve_possible_interactive_pause(id);
            vec![]
        }
        DomainEvent::ConfirmExpired => vec![],
        _ => vec![],
    }
}

fn domain_streaming(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::StreamChunk(text) => {
            // A response chunk following pending thinking text means the
            // model just finished a thinking segment and switched to reply
            // text — `push_response_chunk` flushes that segment into its own
            // bubble first, rather than merging it with whatever thinking
            // segment follows later in the same turn.
            state
                .turn_buffer
                .push_response_chunk(&text, &mut state.chat_entries);
            if state.conversation.scroll_locked {
                state.conversation.scroll_offset = 0;
            }
            vec![]
        }
        DomainEvent::StreamThinkingChunk(text) => {
            // Mirrors the flush above: `push_thinking_chunk` flushes any
            // pending reply segment so far into its own bubble when the
            // model switches back to thinking.
            state
                .turn_buffer
                .push_thinking_chunk(&text, &mut state.chat_entries);
            if state.conversation.scroll_locked {
                state.conversation.scroll_offset = 0;
            }
            vec![]
        }
        DomainEvent::StreamDone => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::StreamCancelled => {
            state.flush_partial_assistant_message(true);
            state.commit_queued_steer_messages(UserMessageState::Cancelled);
            state.steer_indicator = false;
            if matches!(state.focus, Focus::ToolApproval { .. }) {
                state.focus = Focus::MainInput;
            }
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => push_tool_pending_bubble(
            state,
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        ),
        DomainEvent::ClarifyPending {
            id,
            question,
            context,
            choices,
            run_id,
        } => push_clarify_pending_bubble(state, id, question, context, choices, run_id),
        DomainEvent::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
                run_id,
            );
            vec![]
        }
        DomainEvent::ToolProgress {
            id,
            title_arguments,
        } => {
            update_tool_bubble_progress(state, id, title_arguments);
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => {
            update_tool_bubble_to_done(
                state,
                ToolDonePayload {
                    id,
                    name,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    run_id,
                },
            );
            vec![]
        }
        DomainEvent::ToolKilled { id, run_id } => {
            update_tool_bubble_to_killed(state, id, run_id);
            vec![]
        }
        DomainEvent::ToolDenied { id } => {
            update_tool_bubble_to_denied(state, id);
            if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
                state.focus = Focus::MainInput;
            }
            state.resolve_possible_interactive_pause(id);
            vec![]
        }
        DomainEvent::Error { message, run_id } => {
            if run_id.is_none() {
                state.mode = TuiMode::Idle;
            }
            state
                .target_messages_mut(run_id)
                .push(ChatEntry::ErrorLine { message });
            vec![]
        }
        DomainEvent::SteerQueued { run_id } => {
            // No driver routes `Prompt` to a subagent today (see
            // `UiUpdate::SteerQueued::run_id`), so `run_id: Some(_)` is
            // unreachable in practice; guard anyway rather than silently
            // flipping the root's own indicator for a subagent steer.
            if run_id.is_none() {
                state.steer_indicator = true;
            }
            vec![]
        }
        DomainEvent::SteerApplied { run_id } => {
            // See `DomainEvent::SteerQueued` above: only the root's own
            // steer queue/indicator is meaningful today.
            if run_id.is_none() {
                state.commit_queued_steer_messages(UserMessageState::Steered);
                state.steer_indicator = false;
            }
            vec![]
        }
        DomainEvent::AgentIdle => {
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            vec![]
        }
        DomainEvent::ConfirmExpired => vec![],
        _ => vec![],
    }
}

fn domain_clarify_pending(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::StreamStarted => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Streaming;
            state.turn_buffer.partial_assistant_text.clear();
            state.turn_buffer.partial_thinking_text.clear();
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => {
            update_tool_bubble_to_done(
                state,
                ToolDonePayload {
                    id,
                    name,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    run_id,
                },
            );
            // Only the clarify call's own completion ends the question; other
            // tools (e.g. from parallel subagents) finishing must not drop it.
            if matches!(state.mode, TuiMode::ClarifyPending { id: clarify_id, .. } if clarify_id == id)
            {
                state.mode = TuiMode::Pending;
            }
            vec![]
        }
        DomainEvent::AgentIdle | DomainEvent::StreamCancelled => {
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            vec![]
        }
        // Tool lifecycle events of other agents (e.g. parallel subagents)
        // keep flowing while a question is open; dropping them would lose
        // their bubbles, titles and queued approvals.
        event @ (DomainEvent::ToolPending { .. }
        | DomainEvent::ClarifyPending { .. }
        | DomainEvent::ToolStart { .. }
        | DomainEvent::ToolProgress { .. }
        | DomainEvent::ToolKilled { .. }
        | DomainEvent::ToolDenied { .. }) => domain_pending(state, event),
        _ => vec![],
    }
}

fn domain_aborting(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    tracing::debug!(event = ?std::mem::discriminant(&event), "domain_aborting: received event");
    match event {
        DomainEvent::StreamCancelled => {
            tracing::info!(
                aborting_exit = state.aborting_exit,
                "aborting: stream cancelled → idle"
            );
            state.flush_partial_assistant_message(true);
            state.commit_queued_steer_messages(UserMessageState::Cancelled);
            state.steer_indicator = false;
            if matches!(state.focus, Focus::ToolApproval { .. }) {
                state.focus = Focus::MainInput;
            }
            state.mode = TuiMode::Idle;
            if std::mem::replace(&mut state.aborting_exit, false) {
                vec![Effect::Quit]
            } else {
                vec![]
            }
        }
        DomainEvent::AgentIdle => {
            // The agent drops its queued steers on stop; mirror that here.
            state.commit_queued_steer_messages(UserMessageState::Cancelled);
            state.steer_indicator = false;
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            if std::mem::replace(&mut state.aborting_exit, false) {
                vec![Effect::Quit]
            } else {
                vec![]
            }
        }
        DomainEvent::ToolPending { id, .. } => {
            // While aborting, deny immediately without entering approval sub-mode.
            vec![Effect::DenyTool {
                id,
                reason: String::new(),
            }]
        }
        DomainEvent::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
                run_id,
            );
            vec![]
        }
        DomainEvent::ToolProgress {
            id,
            title_arguments,
        } => {
            update_tool_bubble_progress(state, id, title_arguments);
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => {
            update_tool_bubble_to_done(
                state,
                ToolDonePayload {
                    id,
                    name,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    run_id,
                },
            );
            vec![]
        }
        DomainEvent::ToolKilled { id, run_id } => {
            tracing::info!(%id, aborting_exit = state.aborting_exit, "aborting: tool killed → idle");
            update_tool_bubble_to_killed(state, id, run_id);
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            if std::mem::replace(&mut state.aborting_exit, false) {
                vec![Effect::Quit]
            } else {
                vec![]
            }
        }
        DomainEvent::ToolDenied { id } => {
            update_tool_bubble_to_denied(state, id);
            vec![]
        }
        DomainEvent::Error { message, run_id } => {
            if run_id.is_none() {
                state.mode = TuiMode::Idle;
            }
            state
                .target_messages_mut(run_id)
                .push(ChatEntry::ErrorLine { message });
            vec![]
        }
        DomainEvent::StreamDone => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::StreamChunk(text) => {
            state
                .turn_buffer
                .push_response_chunk(&text, &mut state.chat_entries);
            vec![]
        }
        DomainEvent::StreamThinkingChunk(text) => {
            state
                .turn_buffer
                .push_thinking_chunk(&text, &mut state.chat_entries);
            vec![]
        }
        DomainEvent::ConfirmExpired => vec![],
        _ => vec![],
    }
}

fn domain_double_press(
    state: &mut AppState,
    event: DomainEvent,
    pressed_at: Instant,
    return_to: Box<TuiMode>,
    armed_by: ConfirmKey,
) -> Vec<Effect> {
    match event {
        DomainEvent::ConfirmExpired => {
            // Confirm window expired — restore the underlying mode.
            state.mode = *return_to;
            vec![]
        }
        other_event => {
            // Temporarily restore the underlying mode so domain handlers can
            // process the event. If the handler didn't change the mode (i.e.
            // it was a side-effect-only update like StreamChunk or ToolDone),
            // reinstate DoublePressAbort so the confirm window remains active.
            // If the handler did change mode (e.g. StreamDone → Idle), the
            // new mode stays.
            state.mode = *return_to.clone();
            let effects = route_domain(state, other_event);
            if state.mode == *return_to {
                state.mode = TuiMode::DoublePressAbort {
                    pressed_at,
                    return_to,
                    armed_by,
                };
            }
            effects
        }
    }
}
