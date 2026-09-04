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

//! View layer — `render()` entry point for the Ylva TUI.
//!
//! This module is excluded from the coverage gate (it needs a live terminal);
//! visual correctness is verified manually.
//!
//! `render` owns the projection from [`AppState`] to the legacy [`TuiState`]
//! snapshot consumed by [`crate::ui::draw`], and syncs the view-geometry
//! fields written back by `ui::draw` into `AppState` after each frame.

use ratatui::Frame;
use ratatui_textarea::TextArea;

use crate::tui_reducer_state::{
    AppState, BannerDragSelection, ChatEntry, ConversationView, Focus, TuiMode,
};
use crate::tui_render_state::TuiState;
use crate::ui::{self, ButtonAreas};

/// Draw one TUI frame.
///
/// Projects `state` into a [`TuiState`] snapshot, delegates all rendering to
/// [`crate::ui::draw`], then syncs the view-geometry fields written by
/// `ui::draw` back into `state.layout` and the scroll/render-cache fields.
///
/// Called inside `terminal.draw(|frame| { render::render(frame, …); })`.
pub fn render(
    frame: &mut Frame,
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
    hint: Option<&str>,
    mouse_pos: Option<(u16, u16)>,
) {
    let mut render_state = project_for_draw(state);
    let button_areas = ui::draw(frame, &mut render_state, textarea, hint, mouse_pos);
    sync_from_draw(state, render_state, button_areas);
}

/// Project the canonical [`AppState`] to the legacy [`TuiState`] snapshot
/// required by [`crate::ui::draw`].
///
/// The projection is a shallow clone of all view-model fields. Driver-owned
/// types (`TextArea`, channel handles) are not part of this snapshot.
fn project_for_draw(state: &AppState) -> TuiState {
    let pending_approval = if let Focus::ToolApproval {
        tool_id,
        ref tool_name,
    } = state.focus
    {
        Some((tool_id, tool_name.clone(), serde_json::Value::Null))
    } else {
        None
    };
    // A pending approval belongs to the subagent, not the root agent, when
    // its `tool_id` lives in `child_messages` rather than `chat_entries` —
    // this can be true even while `active_view == Root` (the user manually
    // navigated back to the root conversation via Ctrl+B while the
    // subagent's own tool call is still awaiting approval), in which case
    // the approval footer needs to say so explicitly (see
    // `ui::draw_approval_footer`), since the pending tool call is otherwise
    // invisible from the root transcript.
    let pending_approval_is_subagent = matches!(state.focus, Focus::ToolApproval { tool_id, .. }
    if state.child_messages.iter().any(
        |entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == tool_id)
    ));
    // Drill-in conversation model (Phase 6.10): while viewing the child, the
    // main chat area and in-flight stream text project from the child's own
    // transcript/accumulator instead of the root's — the root's own
    // stream/tool activity keeps running in the background but must not
    // bleed into the child transcript the user is currently looking at.
    let showing_child = matches!(state.active_view, ConversationView::Child { .. });
    let (chat_entries, partial_assistant_text, partial_thinking_text, thinking_started_at) =
        if showing_child {
            (
                state.child_messages.clone(),
                state.child_partial_text.clone(),
                String::new(),
                None,
            )
        } else {
            (
                state.chat_entries.clone(),
                state.partial_assistant_text.clone(),
                state.partial_thinking_text.clone(),
                state.thinking_started_at,
            )
        };
    // Drill-in conversation model (Phase 6.10): the child-view header
    // re-renders the *real* `spawn_subagent` `ToolBubble` from the root
    // conversation — same chrome, same collapsed state, same task/label/
    // agent_type content — rather than a separately-maintained header
    // string. `AppState::header_bubble_id` is the single source of truth for
    // which bubble that is (also used by `presenter::route_mouse` to route
    // header clicks to the same bubble's collapse toggle).
    let child_view_bubble = state.header_bubble_id().and_then(|bubble_id| {
        state.chat_entries.iter().find_map(|entry| match entry {
            ChatEntry::ToolBubble {
                id,
                name,
                state: bubble_state,
                collapsed,
            } if *id == bubble_id => {
                Some((*id, name.clone(), (**bubble_state).clone(), *collapsed))
            }
            _ => None,
        })
    });
    // Input is disabled only while looking at a subagent that is running but
    // not currently paused for its own approval/clarify — a paused subagent
    // routes `Enter` through the normal approval/clarify flow instead.
    let input_disabled_hint = if showing_child
        && state.current_child.is_some()
        && !matches!(state.focus, Focus::ToolApproval { .. })
        && !matches!(state.mode, TuiMode::ClarifyPending { .. })
    {
        Some("Subagent is working…".to_string())
    } else {
        None
    };
    TuiState {
        agent_mode: state.agent_mode(),
        chat_entries,
        partial_assistant_text,
        partial_thinking_text,
        thinking_started_at,
        partial_thinking_collapsed: state.partial_thinking_collapsed,
        queued_steer_texts: state.queued_steer_texts.clone(),
        show_help: state.show_help(),
        steer_indicator: state.steer_indicator,
        scroll_offset: state.scroll_offset,
        scroll_locked: state.scroll_locked,
        chat_area_height: state.chat_area_height,
        pending_approval,
        pending_approval_is_subagent,
        pending_clarify: if let crate::tui_reducer_state::TuiMode::ClarifyPending {
            question,
            context,
            choices,
            ..
        } = &state.mode
        {
            Some((question.clone(), choices.clone(), context.clone()))
        } else {
            None
        },
        pending_approval_input_snapshot: state.pending_approval_input_snapshot.clone(),
        input_history: state.input_history.clone(),
        history_index: state.history_index,
        draft: state.draft.clone(),
        input_area: state.layout.input_area.rect,
        chat_area: state.layout.chat_area.rect,
        clarify_banner: ratatui::layout::Rect::default(),
        help_popup_rect: state.layout.help_popup.rect,
        chat_rendered_lines: state.chat_rendered_lines.clone(),
        chat_rendered_line_chrome_widths: state.chat_rendered_line_chrome_widths.clone(),
        chat_selection: state.chat_selection,
        chat_drag_active: state.chat_drag_active,
        clarify_banner_drag: match (
            state.clarify_choice_mousedown,
            state.clarify_banner_drag_end,
        ) {
            (Some((c1, r1)), Some((c2, r2))) => {
                let ((start_row, start_col), (end_row, end_col)) = if (r1, c1) <= (r2, c2) {
                    ((r1, c1), (r2, c2))
                } else {
                    ((r2, c2), (r1, c1))
                };
                Some(BannerDragSelection {
                    start_row,
                    start_col,
                    end_row,
                    end_col,
                })
            }
            _ => None,
        },
        toast: state.toast.clone(),
        spinner_frame: state.spinner_frame,
        help_scroll_offset: state.help_scroll_offset,
        help_scroll_x: state.help_scroll_x,
        clarify_scroll_offset: state.clarify_scroll_offset,
        input_scroll_top: state.input_scroll_top,
        bubble_title_rows: state.bubble_title_rows.clone(),
        thinking_title_rows: state.thinking_title_rows.clone(),
        child_view_bubble,
        header_bubble_scroll_offset: state.header_bubble_scroll_offset,
        header_bubble_area: ratatui::layout::Rect::default(),
        input_disabled_hint,
    }
}

/// Sync the view-geometry fields that [`crate::ui::draw`] writes back into
/// the canonical [`AppState`] after each frame.
fn sync_from_draw(state: &mut AppState, rendered: TuiState, button_areas: Option<ButtonAreas>) {
    state.layout.input_area.rect = rendered.input_area;
    state.layout.chat_area.rect = rendered.chat_area;
    state.layout.clarify_banner = rendered.clarify_banner;
    state.layout.header_bubble = rendered.header_bubble_area;
    state.header_bubble_scroll_offset = rendered.header_bubble_scroll_offset;
    state.layout.help_popup.rect = rendered.help_popup_rect;
    if let Some(buttons) = button_areas {
        state.layout.approve_button = buttons.approve;
        state.layout.deny_button = buttons.deny;
    } else {
        state.layout.approve_button = ratatui::layout::Rect::default();
        state.layout.deny_button = ratatui::layout::Rect::default();
    }
    state.input_scroll_top = rendered.input_scroll_top;
    state.scroll_offset = rendered.scroll_offset;
    state.scroll_locked = rendered.scroll_locked;
    state.bubble_title_rows = rendered.bubble_title_rows;
    state.thinking_title_rows = rendered.thinking_title_rows;
    state.chat_rendered_lines = rendered.chat_rendered_lines;
    state.chat_rendered_line_chrome_widths = rendered.chat_rendered_line_chrome_widths;
    state.help_scroll_offset = rendered.help_scroll_offset;
    state.help_scroll_x = rendered.help_scroll_x;
    state.clarify_scroll_offset = rendered.clarify_scroll_offset;
    state.toast = rendered.toast;
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::tui_reducer_state::{ChatEntry, ChildRunInfo, ToolCallState};

    /// Pushes a `spawn_subagent` `ToolBubble` into `state.chat_entries` (the
    /// root transcript) whose title arguments mirror `run_id`'s
    /// `ChildRunInfo`, so `project_for_draw` can find it as the child-view
    /// header source.
    fn push_spawn_bubble(state: &mut AppState, bubble_id: Uuid, agent_type: &str, task: &str) {
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: bubble_id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({ "agent_type": agent_type, "task": task }),
                content_sections: vec![],
                title_arguments: vec![
                    ("agent_type".to_string(), serde_json::json!(agent_type)),
                    ("task".to_string(), serde_json::json!(task)),
                ],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
    }

    #[test]
    fn project_for_draw_uses_root_transcript_and_no_header_by_default() {
        let mut state = AppState::new();
        state.chat_entries.push(ChatEntry::AssistantMessage {
            text: "root reply".to_string(),
            interrupted: false,
        });
        state.child_messages.push(ChatEntry::AssistantMessage {
            text: "child reply".to_string(),
            interrupted: false,
        });

        let projected = project_for_draw(&state);

        assert_eq!(projected.chat_entries.len(), 1);
        assert!(
            matches!(&projected.chat_entries[0], ChatEntry::AssistantMessage { text, .. } if text == "root reply")
        );
        assert!(projected.child_view_bubble.is_none());
        assert!(projected.input_disabled_hint.is_none());
    }

    #[test]
    fn project_for_draw_sources_child_transcript_and_header_bubble_when_viewing_child() {
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        let bubble_id = Uuid::new_v4();
        push_spawn_bubble(&mut state, bubble_id, "reviewer", "look into flaky test");
        state.current_child = Some(ChildRunInfo {
            run_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.child_messages.push(ChatEntry::AssistantMessage {
            text: "child reply".to_string(),
            interrupted: false,
        });
        state.switch_to_child_view();

        let projected = project_for_draw(&state);

        assert_eq!(projected.chat_entries.len(), 1);
        assert!(
            matches!(&projected.chat_entries[0], ChatEntry::AssistantMessage { text, .. } if text == "child reply")
        );
        let (header_bubble_id, header_name, _state, collapsed) = projected
            .child_view_bubble
            .expect("header bubble must be set while viewing the child");
        assert_eq!(header_bubble_id, bubble_id);
        assert_eq!(
            header_name,
            crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME
        );
        assert!(
            !collapsed,
            "must mirror the root bubble's own collapsed state"
        );
        // The child is running (no pending approval/clarify) and tracked, so
        // input must show the disabled hint rather than accepting keystrokes.
        assert!(projected.input_disabled_hint.is_some());
    }

    #[test]
    fn project_for_draw_does_not_disable_input_while_child_is_paused() {
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        state.current_child = Some(ChildRunInfo {
            run_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.switch_to_child_view();
        state.focus = Focus::ToolApproval {
            tool_id: Uuid::new_v4(),
            tool_name: "write".to_string(),
        };

        let projected = project_for_draw(&state);

        assert!(
            projected.input_disabled_hint.is_none(),
            "input must accept the approval response while the child is paused"
        );
    }

    #[test]
    fn project_for_draw_flags_pending_approval_belonging_to_a_subagent() {
        let mut state = AppState::new();
        let tool_id = Uuid::new_v4();
        state.child_messages.push(ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
            }),
            collapsed: true,
        });
        state.focus = Focus::ToolApproval {
            tool_id,
            tool_name: "bash".to_string(),
        };

        let projected = project_for_draw(&state);

        assert!(projected.pending_approval_is_subagent);
    }

    #[test]
    fn project_for_draw_does_not_flag_a_root_pending_approval_as_a_subagent_approval() {
        let mut state = AppState::new();
        let tool_id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
            }),
            collapsed: true,
        });
        state.focus = Focus::ToolApproval {
            tool_id,
            tool_name: "bash".to_string(),
        };

        let projected = project_for_draw(&state);

        assert!(!projected.pending_approval_is_subagent);
    }
}
