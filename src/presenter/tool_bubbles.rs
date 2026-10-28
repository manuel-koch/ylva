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

//! Pure tool bubbles event handling.

use crate::agent::tools::clarify::ClarifyTool;
use crate::tui_reducer_state::{AppState, ChatEntry, Effect, Focus, QueuedClarify, ToolCallState};
use uuid::Uuid;

/// Shared handling for `DomainEvent::ToolPending`, used identically by
/// `domain_idle`, `domain_pending`, and `domain_streaming`: creates the
/// `Pending` bubble in whichever transcript `run_id` targets (root or a
/// subagent) and queues it in `pending_approval_order`. When no other approval
/// is on screen it arms `Focus::ToolApproval` and auto-switches the drill-in
/// view to wherever this pending call lives (see
/// `AppState::note_interactive_pause`). Otherwise it waits its turn behind
/// the prompt already shown, which is re-armed when that one resolves.
pub(super) fn push_tool_pending_bubble(
    state: &mut AppState,
    id: Uuid,
    name: String,
    arguments: serde_json::Value,
    content_sections: Vec<crate::agent::tools::Section>,
    title_arguments: Vec<(String, serde_json::Value)>,
    run_id: Option<Uuid>,
) -> Vec<Effect> {
    state.flush_target_partial_text(run_id);
    state
        .target_messages_mut(run_id)
        .push(ChatEntry::ToolBubble {
            id,
            name: name.clone(),
            state: Box::new(ToolCallState::Pending {
                arguments,
                content_sections,
                title_arguments,
            }),
            collapsed: true,
        });
    let already_awaiting_approval = state.has_outstanding_interaction();
    state.pending_approval_order.push(id);
    // A prompt that arrives while the user types stays queued until
    // `DomainEvent::TypingSettled`, so keystrokes meant for the draft cannot
    // resolve it.
    if already_awaiting_approval || state.is_typing() {
        return vec![];
    }
    state.focus = Focus::ToolApproval {
        tool_id: id,
        tool_name: name,
    };
    // The approval footer occupies the rows directly above the input, the
    // same rows any slash overlay draws itself onto, and the approval also
    // takes over keyboard routing — so the park effect hides the open overlay
    // (notably a `/models` picker's already-fetched catalog) until
    // `RestoreSlashOverlay` brings it back when the approval resolves. The
    // draft stays in the input field.
    state.prompt_button_focus = None;
    state.note_interactive_pause(run_id);
    vec![Effect::ParkSlashOverlay]
}

/// Shared handling for `DomainEvent::ClarifyPending`, used identically by
/// `domain_idle`, `domain_pending`, and `domain_streaming`. See
/// `push_tool_pending_bubble` for the `run_id` routing/auto-switch rationale.
pub(super) fn push_clarify_pending_bubble(
    state: &mut AppState,
    id: Uuid,
    question: String,
    context: Option<String>,
    choices: Option<Vec<String>>,
    run_id: Option<Uuid>,
) -> Vec<Effect> {
    let mut bubble_arguments = serde_json::json!({ "question": question });
    if let Some(ref ctx) = context {
        bubble_arguments["context"] = serde_json::Value::String(ctx.clone());
    }
    if let Some(ref choices_list) = choices {
        bubble_arguments["choices"] = serde_json::json!(choices_list);
    }
    // Flush any thinking/assistant text accumulated before this clarify call
    // so it lands ahead of the clarify bubble in chronological order (mirrors
    // the ToolStart/ToolPending flush).
    state.flush_target_partial_text(run_id);
    state
        .target_messages_mut(run_id)
        .push(ChatEntry::ToolBubble {
            id,
            name: ClarifyTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: bubble_arguments,
                content_sections: vec![],
                title_arguments: vec![(
                    "question".to_string(),
                    serde_json::Value::String(question.clone()),
                )],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
    let request = QueuedClarify {
        id,
        question,
        context,
        choices,
        run_id,
    };
    // Strict arrival order: behind another prompt, or while the user types,
    // wait.
    if state.has_outstanding_interaction() || state.is_typing() {
        state.pending_approval_order.push(id);
        state.queued_clarifies.insert(id, request);
        return vec![];
    }
    state.show_clarify(request);
    // The banner sits on the same rows above the input as the approval
    // footer, so the overlay is parked the same way.
    vec![Effect::ParkSlashOverlay]
}
// ─── Tool bubble update helpers ───────────────────────────────────────────────

pub(super) fn update_tool_bubble_to_running(
    state: &mut AppState,
    id: Uuid,
    name: String,
    arguments: serde_json::Value,
    content_sections: Vec<crate::agent::tools::Section>,
    title_arguments: Vec<(String, serde_json::Value)>,
    run_id: Option<Uuid>,
) {
    if name == crate::agent::tools::bash::BashTool::NAME {
        state
            .bash_started_at
            .entry(id)
            .or_insert_with(std::time::Instant::now);
    }
    if let Some(entry) = state.find_tool_bubble_mut(id) {
        if let ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        } = entry
        {
            **bubble_state = ToolCallState::Running {
                arguments: arguments.clone(),
                content_sections: content_sections.clone(),
                title_arguments: title_arguments.clone(),
                progress_arguments: vec![],
            };
        }
    } else {
        // First appearance of this tool call (auto-run tool with no approval
        // step): flush any thinking/assistant text accumulated *before* this
        // call so it lands as its own bubble ahead of the tool bubble in
        // `chat_entries`, matching actual chronological order. Without this,
        // thinking that happens both before and after the tool call would
        // stay merged into one bubble and get flushed only at `StreamDone`,
        // making it look like the tool ran before the model ever "thought".
        state.flush_target_partial_text(run_id);
        state
            .target_messages_mut(run_id)
            .push(ChatEntry::ToolBubble {
                id,
                name,
                state: Box::new(ToolCallState::Running {
                    arguments,
                    content_sections,
                    title_arguments,
                    progress_arguments: vec![],
                }),
                collapsed: true,
            });
    }
    // Clear ToolApproval focus if it was for this tool.
    if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
        state.focus = Focus::MainInput;
    }
    // A tool moving from Pending to Running means its approval step (if any)
    // is over — if this was the subagent's interactive pause we auto-switched
    // for, auto-return to Root now that the subagent is unblocked again.
    state.resolve_possible_interactive_pause(id);
}

/// Patches `progress_arguments` on an existing `Running` tool bubble without
/// otherwise changing its state — used for interim status updates (e.g.
/// `spawn_subagent` reporting `status=spawned`/`queued` + `run_id`) that
/// don't mean the call itself is done. Deliberately separate from
/// `title_arguments` (the call's real, static arguments): `Done` has no
/// `progress_arguments` field, so these interim values are naturally
/// dropped once the call truly finishes instead of lingering stale on the
/// terminal bubble (see `ToolCallState::Running::progress_arguments`). A
/// no-op if the bubble isn't found or isn't currently `Running` (e.g. it
/// already reached a terminal state).
pub(super) fn update_tool_bubble_progress(
    state: &mut AppState,
    id: Uuid,
    title_arguments: Vec<(String, serde_json::Value)>,
) {
    let new_multiline_content = title_arguments
        .iter()
        .any(|(_, value)| value.as_str().is_some_and(|value| value.contains('\n')));
    let mut body_changed = new_multiline_content;
    if let Some(ChatEntry::ToolBubble {
        state: bubble_state,
        ..
    }) = state.find_tool_bubble_title_mut(id)
    {
        if let ToolCallState::Running {
            progress_arguments, ..
        } = bubble_state.as_mut()
        {
            body_changed |= progress_arguments
                .iter()
                .any(|(_, value)| value.as_str().is_some_and(|value| value.contains('\n')));
            *progress_arguments = title_arguments;
        }
    }
    if body_changed {
        // Multiline title values also supply expandable content sections.
        let _ = state.find_tool_bubble_mut(id);
    }
}

/// Payload for [`update_tool_bubble_to_done`], grouped into a struct to keep
/// the function signature down to a reasonable arity (see
/// `DomainEvent::ToolDone`, which this mirrors field-for-field plus `id`).
pub(super) struct ToolDonePayload {
    pub(super) id: Uuid,
    pub(super) name: String,
    pub(super) result_section: crate::tui_reducer_state::ResultSection,
    pub(super) content_sections: Vec<crate::agent::tools::Section>,
    pub(super) summary: Vec<(String, String)>,
    pub(super) duration: std::time::Duration,
    pub(super) run_id: Option<Uuid>,
}

pub(super) fn update_tool_bubble_to_done(state: &mut AppState, payload: ToolDonePayload) {
    let ToolDonePayload {
        id,
        name,
        result_section,
        content_sections,
        summary,
        duration,
        run_id,
    } = payload;
    let (previous_arguments, previous_title_arguments) = state
        .find_tool_bubble_mut(id)
        .and_then(|entry| {
            if let ChatEntry::ToolBubble {
                state: bubble_state,
                ..
            } = entry
            {
                match bubble_state.as_ref() {
                    ToolCallState::Running {
                        arguments,
                        title_arguments,
                        ..
                    }
                    | ToolCallState::Pending {
                        arguments,
                        title_arguments,
                        ..
                    } => Some((arguments.clone(), title_arguments.clone())),
                    _ => None,
                }
            } else {
                None
            }
        })
        .unwrap_or((serde_json::Value::Null, vec![]));
    if let Some(entry) = state.find_tool_bubble_mut(id) {
        if let ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        } = entry
        {
            **bubble_state = ToolCallState::Done {
                arguments: previous_arguments,
                result_section,
                content_sections,
                summary,
                duration,
                title_arguments: previous_title_arguments,
            };
        }
    } else {
        state
            .target_messages_mut(run_id)
            .push(ChatEntry::ToolBubble {
                id,
                name,
                state: Box::new(ToolCallState::Done {
                    arguments: serde_json::Value::Null,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    title_arguments: vec![],
                }),
                collapsed: true,
            });
    }
    state.resolve_possible_interactive_pause(id);
}

pub(super) fn update_tool_bubble_to_killed(state: &mut AppState, id: Uuid, run_id: Option<Uuid>) {
    if let Some(entry) = state.find_tool_bubble_mut(id) {
        if let ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        } = entry
        {
            **bubble_state = ToolCallState::Killed;
        }
    } else {
        state
            .target_messages_mut(run_id)
            .push(ChatEntry::ToolBubble {
                id,
                name: String::new(),
                state: Box::new(ToolCallState::Killed),
                collapsed: true,
            });
    }
    if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
        state.focus = Focus::MainInput;
    }
    state.resolve_possible_interactive_pause(id);
}

pub(super) fn update_tool_bubble_to_denied(state: &mut AppState, id: Uuid) {
    if let Some(ChatEntry::ToolBubble {
        state: bubble_state,
        ..
    }) = state.find_tool_bubble_mut(id)
    {
        let (preserved_arguments, preserved_content_sections, preserved_title_arguments) =
            match bubble_state.as_ref() {
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
                    arguments.clone(),
                    content_sections.clone(),
                    title_arguments.clone(),
                ),
                _ => (serde_json::Value::Null, vec![], vec![]),
            };
        **bubble_state = ToolCallState::Denied {
            arguments: preserved_arguments,
            error: "Denied by user".to_string(),
            content_sections: preserved_content_sections,
            title_arguments: preserved_title_arguments,
        };
    }
    state.resolve_possible_interactive_pause(id);
}
