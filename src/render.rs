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
//! Test-backend fixtures verify rendering and state synchronization; this module
//! participates in the aggregate coverage gate.
//!
//! Production frames borrow conversation content and move derived selection
//! buffers through a small legacy widget shell, then sync geometry to `AppState`.

use ratatui::Frame;
use ratatui_textarea::TextArea;

use crate::agent::tools::ToolRegistry;
use crate::tui_reducer_state::{
    ActiveSubagentCounts, AppState, BannerDragSelection, BubbleExpansionTarget, ChatEntry,
    ConversationView, Focus,
};
use crate::tui_render_state::TuiState;
use crate::tui_widgets::{self, ButtonAreas};

/// Draw one TUI frame.
///
/// Borrows the active transcript, delegates rendering to the shared widgets,
/// and syncs view geometry and moved selection buffers back to `state`.
/// The `registry` carries no rendering state of its own: tool bubbles look up
/// their display label in it while drawing (`Tool::title()`, e.g.
/// `Web Search (Brave)`) — the same on-demand lookup the CLI status line
/// performs — so a title is never copied into the view-model.
///
/// Called inside `terminal.draw(|frame| { render::render(frame, …); })`.
pub fn render(
    frame: &mut Frame,
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
    hint: Option<&str>,
    mouse_pos: Option<(u16, u16)>,
    registry: &ToolRegistry,
) {
    render_internal(frame, state, textarea, hint, mouse_pos, registry, false);
}

pub(crate) fn render_expansion_feedback(
    frame: &mut Frame,
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
    hint: Option<&str>,
    mouse_pos: Option<(u16, u16)>,
    registry: &ToolRegistry,
) {
    render_internal(frame, state, textarea, hint, mouse_pos, registry, true);
}

fn render_internal(
    frame: &mut Frame,
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
    hint: Option<&str>,
    mouse_pos: Option<(u16, u16)>,
    registry: &ToolRegistry,
    expansion_feedback: bool,
) {
    let feedback_scroll = expansion_feedback.then_some((
        state.conversation.scroll_offset,
        state.conversation.scroll_locked,
    ));
    if !state.render_cache_tracking {
        state.render_cache.clear();
    }
    let mut render_state = project_snapshot(state, false);
    render_state.conversation.chat_rendered_lines =
        std::mem::take(&mut state.conversation.chat_rendered_lines);
    render_state.conversation.chat_rendered_line_prefix_widths =
        std::mem::take(&mut state.conversation.chat_rendered_line_prefix_widths);
    render_state.child_view.header_rendered_lines =
        std::mem::take(&mut state.child_view.header_rendered_lines);
    render_state.child_view.header_rendered_line_prefix_widths =
        std::mem::take(&mut state.child_view.header_rendered_line_prefix_widths);
    let mut cache = std::mem::take(&mut state.render_cache);
    let mut source = tui_widgets::ConversationSource::from_app(state);
    if expansion_feedback {
        source.expansion_feedback = state.pending_bubble_expansion;
    }
    cache.activate(source.run_id);
    let button_areas = tui_widgets::draw_borrowed(
        frame,
        &mut render_state,
        textarea,
        hint,
        mouse_pos,
        registry,
        &mut tui_widgets::BorrowedConversation {
            source: &source,
            cache: &mut cache,
        },
    );
    if let Some(target) = source.expansion_feedback {
        draw_expansion_indicator(frame, &render_state, target);
    }
    state.render_cache = cache;
    sync_from_draw(state, render_state, button_areas);
    if let Some((offset, locked)) = feedback_scroll {
        state.conversation.scroll_offset = offset;
        state.conversation.scroll_locked = locked;
    }
}

const EXPANSION_FEEDBACK_LABEL: &str = " Expanding… ";

fn draw_expansion_indicator(frame: &mut Frame, state: &TuiState, target: BubbleExpansionTarget) {
    use ratatui::{
        layout::Rect,
        style::{Modifier, Style},
        widgets::Paragraph,
    };

    let (area, title_row) = match target {
        BubbleExpansionTarget::Tool(id)
            if state
                .child_view
                .child_view_bubble
                .as_ref()
                .is_some_and(|bubble| bubble.0 == id) =>
        {
            (state.child_view.header_bubble_area, Some(0))
        }
        _ => {
            let absolute_row = match target {
                BubbleExpansionTarget::Tool(id) => state
                    .conversation
                    .bubble_title_rows
                    .iter()
                    .find(|(_, _, bubble_id)| *bubble_id == id)
                    .map(|(start, ..)| *start),
                BubbleExpansionTarget::Thinking(index) => state
                    .conversation
                    .thinking_title_rows
                    .iter()
                    .find(|(_, _, entry_index)| *entry_index == index)
                    .map(|(start, ..)| *start),
            };
            (
                state.conversation.chat_area,
                absolute_row.and_then(|row| row.checked_sub(state.conversation.scroll_offset)),
            )
        }
    };
    let Some(row) = title_row.filter(|row| *row < usize::from(area.height)) else {
        return;
    };
    let width = (ratatui::text::Line::from(EXPANSION_FEEDBACK_LABEL).width() as u16)
        .min(area.width.saturating_sub(3));
    if width == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(EXPANSION_FEEDBACK_LABEL).style(
            Style::default()
                .fg(crate::theme::NOTICE_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(area.right() - width, area.y + row as u16, width, 1),
    );
}

/// Project the canonical [`AppState`] to the legacy [`TuiState`] snapshot
/// required by [`crate::tui_widgets::draw`].
///
/// Owned compatibility projection used by snapshot tests, not production frames.
#[cfg(test)]
fn project_for_draw(state: &AppState) -> TuiState {
    project_snapshot(state, true)
}

fn project_snapshot(state: &AppState, include_content: bool) -> TuiState {
    let pending_approval = if let Focus::ToolApproval {
        tool_id,
        ref tool_name,
    } = state.focus
    {
        Some((tool_id, tool_name.clone(), serde_json::Value::Null))
    } else {
        None
    };
    // A pending approval belongs to a subagent, not the root agent, when
    // its `tool_id` lives in any tracked child's own transcript rather than
    // `chat_entries` — this can be true even while `active_view == Root`
    // (the user manually navigated back to the root conversation via Esc
    // (empty draft) while a subagent's own tool call is still awaiting
    // approval), in which case the approval footer needs to say so
    // explicitly (see `tui_widgets::draw_approval_footer`), since the
    // pending tool call is otherwise invisible from the root transcript.
    let pending_approval_is_subagent = matches!(state.focus, Focus::ToolApproval { tool_id, .. }
    if state.child_conversations.values().any(|conversation| conversation.messages.iter().any(
        |entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == tool_id)
    )));
    // Drill-in conversation model (Phase 6.10): while viewing a child, the
    // main chat area and in-flight stream text project from that child's own
    // transcript/accumulator instead of the root's — the root's own
    // stream/tool activity (and any other child's) keeps running in the
    // background but must not bleed into the transcript the user is
    // currently looking at.
    let showing_child_run_id = match &state.active_view {
        ConversationView::Child { run_id, .. } => Some(*run_id),
        ConversationView::Root => None,
    };
    let active_subagent_counts = if showing_child_run_id.is_some() {
        ActiveSubagentCounts::default()
    } else {
        state.active_subagent_counts()
    };
    let (
        chat_entries,
        partial_assistant_text,
        partial_thinking_text,
        thinking_started_at,
        partial_thinking_collapsed,
    ) = if let Some(conversation) =
        showing_child_run_id.and_then(|run_id| state.child_conversations.get(&run_id))
    {
        (
            if include_content {
                conversation.messages.clone()
            } else {
                Vec::new()
            },
            if include_content {
                conversation.turn_buffer.partial_assistant_text.clone()
            } else {
                String::new()
            },
            if include_content {
                conversation.turn_buffer.partial_thinking_text.clone()
            } else {
                String::new()
            },
            conversation.turn_buffer.thinking_started_at,
            conversation.turn_buffer.partial_thinking_collapsed,
        )
    } else {
        (
            if include_content {
                state.chat_entries.clone()
            } else {
                Vec::new()
            },
            if include_content {
                state.turn_buffer.partial_assistant_text.clone()
            } else {
                String::new()
            },
            if include_content {
                state.turn_buffer.partial_thinking_text.clone()
            } else {
                String::new()
            },
            state.turn_buffer.thinking_started_at,
            state.turn_buffer.partial_thinking_collapsed,
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
            } if *id == bubble_id => Some((
                *id,
                name.clone(),
                if include_content {
                    (**bubble_state).clone()
                } else {
                    crate::tui_reducer_state::ToolCallState::Killed
                },
                *collapsed,
            )),
            _ => None,
        })
    });
    // Keep the root/parent status separately: the drilled-into child's own
    // streaming/tool-running/idle status, not the root's — see
    // `AppState::agent_mode_for_child` for why `agent_mode()` alone is wrong
    // here (it would gate the child's in-flight thinking/response bubble on
    // the root's `TuiMode` instead of the child's).
    let parent_agent_mode = state.agent_mode();
    let agent_mode = match &state.active_view {
        ConversationView::Child { run_id, .. } => state.agent_mode_for_child(*run_id),
        ConversationView::Root => parent_agent_mode,
    };
    let engine_display = showing_child_run_id
        .and_then(|run_id| state.child_engine_displays.get(&run_id))
        .map(|child_display| {
            let mut child_display = child_display.clone();
            if child_display.inherits_parent_runtime {
                child_display.engine_name = state.engine_display.engine_name.clone();
                child_display.provider_connection =
                    state.engine_display.provider_connection.clone();
                child_display.model = state.engine_display.model.clone();
                child_display.reasoning_effort = state.engine_display.reasoning_effort.clone();
                if let Some(compaction) = &mut child_display.compaction_engine {
                    if compaction.follows_conversation {
                        compaction.engine_name = child_display.engine_name.clone();
                        compaction.provider_connection = child_display.provider_connection.clone();
                        compaction.model = child_display.model.clone();
                        if let Some(parent_compaction) = state
                            .engine_display
                            .compaction_engine
                            .as_ref()
                            .filter(|compaction| compaction.follows_conversation)
                        {
                            compaction.context_length = parent_compaction.context_length;
                        }
                        compaction.shares_conversation = true;
                    } else {
                        compaction.shares_conversation = compaction.shares_conversation
                            && !compaction.engine_name.is_empty()
                            && compaction.engine_name == child_display.engine_name;
                    }
                }
            }
            child_display
        })
        .unwrap_or_else(|| state.engine_display.clone());
    TuiState {
        display_metadata: state.display_metadata.clone(),
        parent_agent_mode,
        agent_mode,
        compacting: state
            .compaction_requests
            .contains_key(&showing_child_run_id),
        active_subagent_counts,
        engine_display,
        chat_entries,
        subagent_started_at: state.subagent_started_at(),
        partial_assistant_text,
        partial_thinking_text,
        thinking_started_at,
        partial_thinking_collapsed,
        queued_steer_texts: if include_content && showing_child_run_id.is_none() {
            state.queued_steer_texts.clone()
        } else {
            Vec::new()
        },
        steer_indicator: state.steer_indicator && showing_child_run_id.is_none(),
        pending_approval,
        pending_approval_is_subagent,
        input_history: if include_content {
            state.input_history.clone()
        } else {
            Vec::new()
        },
        history_index: state.history_index,
        context_usage_text: state
            .active_context_usage()
            .map(crate::tui_reducer_state::format_context_usage_readout),
        draft: state.draft.clone(),
        input_area: state.layout.input_area.rect,
        toast: state.toast.clone(),
        spinner_frame: state.spinner_frame,
        input_scroll_top: state.input_scroll_top,
        slash_overlay: state.slash_overlay.clone(),
        slash_overlay_drag: state.slash_overlay_drag_selection(),
        slash_overlay_layout: state.layout.slash_overlay,
        help_popup: crate::tui_render_state::HelpPopupState {
            show_help: state.show_help(),
            help_scroll_offset: state.help_popup.help_scroll_offset,
            help_scroll_x: state.help_popup.help_scroll_x,
            help_popup_rect: state.layout.help_popup.rect,
            help_content_area: state.layout.help_popup.content_area,
            help_rendered_lines: state.help_popup.help_rendered_lines.clone(),
            help_selection: state.help_popup.help_selection,
            help_drag_active: state.help_popup.help_drag_active,
        },
        clarify_banner_state: crate::tui_render_state::ClarifyBannerState {
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
            clarify_scroll_offset: state.clarify_banner_state.clarify_scroll_offset,
            clarify_banner_drag: match (
                state.clarify_banner_state.clarify_choice_mousedown,
                state.clarify_banner_state.clarify_banner_drag_end,
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
            clarify_banner: ratatui::layout::Rect::default(),
        },
        child_view: crate::tui_render_state::ChildViewState {
            child_view_bubble,
            header_bubble_scroll_offset: state.child_view.header_bubble_scroll_offset,
            header_bubble_area: ratatui::layout::Rect::default(),
            header_rendered_lines: if include_content {
                state.child_view.header_rendered_lines.clone()
            } else {
                Vec::new()
            },
            header_rendered_line_prefix_widths: if include_content {
                state.child_view.header_rendered_line_prefix_widths.clone()
            } else {
                Vec::new()
            },
            header_selection: state.child_view.header_selection,
            header_drag_active: state.child_view.header_drag_active,
        },
        conversation: crate::tui_render_state::ConversationListState {
            scroll_offset: state.conversation.scroll_offset,
            scroll_locked: state.conversation.scroll_locked,
            chat_area_height: state.conversation.chat_area_height,
            chat_area: state.layout.chat_area.rect,
            chat_scrollbar: state.layout.chat_scrollbar,
            chat_rendered_lines: if include_content {
                state.conversation.chat_rendered_lines.clone()
            } else {
                Vec::new()
            },
            chat_rendered_line_prefix_widths: if include_content {
                state.conversation.chat_rendered_line_prefix_widths.clone()
            } else {
                Vec::new()
            },
            chat_selection: state.conversation.chat_selection,
            chat_drag_active: state.conversation.chat_drag_active,
            bubble_title_rows: state.conversation.bubble_title_rows.clone(),
            thinking_title_rows: state.conversation.thinking_title_rows.clone(),
        },
    }
}

/// Sync the view-geometry fields that [`crate::tui_widgets::draw`] writes back into
/// the canonical [`AppState`] after each frame.
fn sync_from_draw(state: &mut AppState, rendered: TuiState, button_areas: Option<ButtonAreas>) {
    state.layout.input_area.rect = rendered.input_area;
    state.layout.chat_area.rect = rendered.conversation.chat_area;
    state.layout.chat_scrollbar = rendered.conversation.chat_scrollbar;
    state.layout.clarify_banner = rendered.clarify_banner_state.clarify_banner;
    state.layout.header_bubble = rendered.child_view.header_bubble_area;
    state.child_view.header_bubble_scroll_offset = rendered.child_view.header_bubble_scroll_offset;
    state.child_view.header_rendered_lines = rendered.child_view.header_rendered_lines;
    state.child_view.header_rendered_line_prefix_widths =
        rendered.child_view.header_rendered_line_prefix_widths;
    state.child_view.header_selection = rendered.child_view.header_selection;
    state.child_view.header_drag_active = rendered.child_view.header_drag_active;
    state.layout.help_popup.rect = rendered.help_popup.help_popup_rect;
    state.layout.help_popup.content_area = rendered.help_popup.help_content_area;
    state.layout.slash_overlay = rendered.slash_overlay_layout;
    if let Some(buttons) = button_areas {
        state.layout.approve_button = buttons.approve;
        state.layout.deny_button = buttons.deny;
    } else {
        state.layout.approve_button = ratatui::layout::Rect::default();
        state.layout.deny_button = ratatui::layout::Rect::default();
    }
    state.input_scroll_top = rendered.input_scroll_top;
    state.conversation.scroll_offset = rendered.conversation.scroll_offset;
    state.conversation.scroll_locked = rendered.conversation.scroll_locked;
    state.conversation.bubble_title_rows = rendered.conversation.bubble_title_rows;
    state.conversation.thinking_title_rows = rendered.conversation.thinking_title_rows;
    // Resolve a pending "scroll back to the drilled-in-from bubble"
    // navigation (armed by `AppState::switch_to_root_view`) now that
    // `bubble_title_rows` reflects this frame's *root* transcript (the very
    // frame after the switch, since `active_view` already flipped to `Root`
    // before this draw call ran) rather than the child's. Resolving by the
    // bubble's current row — instead of trusting the raw offset saved at
    // drill-in time — stays correct even if the root conversation grew while
    // the child view was open. Left unresolved (and silently dropped) if the
    // bubble is no longer present, leaving whatever `saved_root_scroll`
    // placeholder was already restored.
    if let Some(target_id) = state.pending_scroll_to_bubble.take() {
        if let Some((start_row, ..)) = state
            .conversation
            .bubble_title_rows
            .iter()
            .find(|(_, _, id)| *id == target_id)
        {
            state.conversation.scroll_offset = *start_row;
            state.conversation.scroll_locked = false;
        }
    }
    state.conversation.chat_rendered_lines = rendered.conversation.chat_rendered_lines;
    state.conversation.chat_rendered_line_prefix_widths =
        rendered.conversation.chat_rendered_line_prefix_widths;
    state.help_popup.help_scroll_offset = rendered.help_popup.help_scroll_offset;
    state.help_popup.help_scroll_x = rendered.help_popup.help_scroll_x;
    state.help_popup.help_rendered_lines = rendered.help_popup.help_rendered_lines;
    state.help_popup.help_selection = rendered.help_popup.help_selection;
    state.help_popup.help_drag_active = rendered.help_popup.help_drag_active;
    state.clarify_banner_state.clarify_scroll_offset =
        rendered.clarify_banner_state.clarify_scroll_offset;
    state.toast = rendered.toast;
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::tui_reducer_state::{ChatEntry, ChildRunInfo, ToolCallState};

    #[test]
    fn expansion_feedback_reuses_collapsed_layouts_before_building_large_bodies() {
        use crate::tui_reducer_state::{ResultSection, STREAMING_THINKING_ENTRY_INDEX};
        use ratatui::{backend::TestBackend, Terminal};
        use std::time::Duration;

        for surface in [
            "root-tool",
            "child-tool",
            "header",
            "thinking",
            "child-thinking",
            "streaming",
        ] {
            let mut state = AppState::new();
            state.enable_render_cache();
            let bubble_id = Uuid::new_v4();
            let content = "Cold expansion body marker\n\n".repeat(2_000);
            let entry = if surface.ends_with("thinking") {
                ChatEntry::ThinkingMessage {
                    text: content.clone(),
                    collapsed: true,
                    duration: Duration::from_secs(1),
                }
            } else {
                ChatEntry::ToolBubble {
                    id: bubble_id,
                    name: if surface == "header" {
                        "spawn_subagent"
                    } else {
                        "read"
                    }
                    .to_string(),
                    state: Box::new(ToolCallState::Done {
                        arguments: serde_json::json!({}),
                        title_arguments: Vec::new(),
                        content_sections: Vec::new(),
                        result_section: ResultSection::plain(content.clone()),
                        summary: Vec::new(),
                        duration: Duration::from_secs(1),
                    }),
                    collapsed: true,
                }
            };
            if surface.starts_with("child-") || surface == "header" {
                let run_id = Uuid::new_v4();
                let mut entry = Some(entry);
                let header_bubble_id = if surface == "header" {
                    bubble_id
                } else {
                    Uuid::new_v4()
                };
                if surface == "header" {
                    state.chat_entries.push(entry.take().unwrap());
                } else {
                    push_spawn_bubble(&mut state, header_bubble_id, "reviewer", "Review");
                }
                state.start_child_run(ChildRunInfo {
                    run_id,
                    bubble_id: header_bubble_id,
                    agent_type: "reviewer".to_string(),
                    task: "Review".to_string(),
                    label: "Review".to_string(),
                });
                if surface != "header" {
                    state
                        .child_conversations
                        .get_mut(&run_id)
                        .unwrap()
                        .messages
                        .push(entry.take().unwrap());
                }
                state.switch_to_child_view(run_id);
            } else if surface == "streaming" {
                state.turn_buffer.partial_thinking_text = content;
                state.turn_buffer.partial_thinking_collapsed = true;
            } else {
                state.chat_entries.push(entry);
            }
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            let mut textarea = TextArea::default();
            let registry = ToolRegistry::new();
            terminal
                .draw(|frame| render(frame, &mut state, &mut textarea, None, None, &registry))
                .unwrap();
            let body_builds = state.render_cache.body_layout_builds;
            match surface {
                "header" => state.toggle_header_bubble_collapsed(),
                "thinking" | "child-thinking" => state.toggle_thinking_collapsed(0),
                "streaming" => state.toggle_partial_thinking_collapsed(),
                _ => state.toggle_tool_bubble_collapsed(bubble_id),
            }
            let target = if surface.ends_with("thinking") {
                BubbleExpansionTarget::Thinking(0)
            } else if surface == "streaming" {
                BubbleExpansionTarget::Thinking(STREAMING_THINKING_ENTRY_INDEX)
            } else {
                BubbleExpansionTarget::Tool(bubble_id)
            };
            assert_eq!(state.pending_bubble_expansion, Some(target), "{surface}");
            let scroll_before_feedback = (
                state.conversation.scroll_offset,
                state.conversation.scroll_locked,
            );
            terminal
                .draw(|frame| {
                    render_expansion_feedback(
                        frame,
                        &mut state,
                        &mut textarea,
                        None,
                        None,
                        &registry,
                    )
                })
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains(EXPANSION_FEEDBACK_LABEL), "{surface}");
            assert!(!text.contains("Cold expansion body marker"), "{surface}");
            assert_eq!(
                state.render_cache.body_layout_builds, body_builds,
                "{surface}"
            );
            assert_eq!(
                (
                    state.conversation.scroll_offset,
                    state.conversation.scroll_locked
                ),
                scroll_before_feedback,
                "{surface}",
            );
            crate::presenter::update(
                &mut state,
                crate::presenter::TuiEvent::Domain(
                    crate::tui_reducer_state::DomainEvent::BubbleExpansionFeedbackShown,
                ),
                &crate::presenter::TextareaContext {
                    is_empty: true,
                    text: String::new(),
                    cursor_row: 0,
                    cursor_row_is_last: true,
                },
            );
            assert_eq!(state.pending_bubble_expansion, None);
            terminal
                .draw(|frame| render(frame, &mut state, &mut textarea, None, None, &registry))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("Cold expansion body marker"), "{surface}");
            assert!(!text.contains(EXPANSION_FEEDBACK_LABEL), "{surface}");
            assert!(
                state.render_cache.body_layout_builds > body_builds,
                "{surface}"
            );
        }
    }

    #[test]
    fn scrollbar_clicks_move_the_rendered_conversation_to_the_requested_position() {
        assert_scrollbar_click_positions(false, false);
    }

    #[test]
    fn scrollbar_clicks_move_the_rendered_child_conversation() {
        assert_scrollbar_click_positions(true, false);
    }

    #[test]
    fn scrollbar_clicks_work_with_the_owned_render_projection() {
        assert_scrollbar_click_positions(false, true);
    }

    fn assert_scrollbar_click_positions(child_view: bool, owned_projection: bool) {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{backend::TestBackend, Terminal};

        let mut state = AppState::new();
        let message = ChatEntry::AssistantMessage {
            text: (0..200).map(|index| format!("Line {index}\n\n")).collect(),
            interrupted: false,
        };
        if child_view {
            let run_id = Uuid::new_v4();
            let bubble_id = Uuid::new_v4();
            push_spawn_bubble(&mut state, bubble_id, "reviewer", "Review");
            state.start_child_run(ChildRunInfo {
                run_id,
                bubble_id,
                agent_type: "reviewer".to_string(),
                task: "Review".to_string(),
                label: "Review".to_string(),
            });
            state
                .child_conversations
                .get_mut(&run_id)
                .unwrap()
                .messages
                .push(message);
            state.switch_to_child_view(run_id);
        } else {
            state.chat_entries.push(message);
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut textarea = TextArea::default();
        let registry = ToolRegistry::new();
        let draw = |frame: &mut Frame, state: &mut AppState, textarea: &mut TextArea<'_>| {
            if owned_projection {
                let mut projected = project_for_draw(state);
                let buttons =
                    tui_widgets::draw(frame, &mut projected, textarea, None, None, &registry);
                sync_from_draw(state, projected, buttons);
            } else {
                render(frame, state, textarea, None, None, &registry);
            }
        };
        terminal
            .draw(|frame| draw(frame, &mut state, &mut textarea))
            .unwrap();
        let maximum = state.conversation.scroll_offset;
        assert!(maximum > 0);
        let area = state.layout.chat_area.rect;
        let track_height = usize::from(area.height - 2);
        for (row, expected) in [
            (area.y, 0),
            (area.y + 1, 0),
            (
                area.y + 1 + (track_height / 2) as u16,
                maximum * (track_height / 2) / (track_height - 1),
            ),
            (area.bottom() - 2, maximum),
            (area.bottom() - 1, maximum),
        ] {
            let effects = crate::presenter::update(
                &mut state,
                crate::presenter::TuiEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: area.right(),
                    row,
                    modifiers: KeyModifiers::NONE,
                }),
                &crate::presenter::TextareaContext {
                    is_empty: true,
                    text: String::new(),
                    cursor_row: 0,
                    cursor_row_is_last: true,
                },
            );
            assert!(effects.is_empty());
            assert_eq!(state.conversation.scroll_offset, expected, "row {row}");
            assert_eq!(state.conversation.scroll_locked, expected == maximum);
            assert!(!state.conversation.chat_drag_active);
            let effects = crate::presenter::update(
                &mut state,
                crate::presenter::TuiEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    column: area.right(),
                    row,
                    modifiers: KeyModifiers::NONE,
                }),
                &crate::presenter::TextareaContext {
                    is_empty: true,
                    text: String::new(),
                    cursor_row: 0,
                    cursor_row_is_last: true,
                },
            );
            assert!(effects.is_empty());
            terminal
                .draw(|frame| draw(frame, &mut state, &mut textarea))
                .unwrap();
            assert_eq!(state.conversation.scroll_offset, expected);
        }
        state.chat_entries.clear();
        if child_view {
            state
                .child_conversations
                .values_mut()
                .for_each(|conversation| conversation.messages.clear());
        }
        terminal
            .draw(|frame| draw(frame, &mut state, &mut textarea))
            .unwrap();
        assert!(state.layout.chat_scrollbar.rect.is_empty());
        assert_eq!(state.layout.chat_scrollbar.maximum_scroll_offset, 0);
    }

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
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state
            .child_conversations
            .get_mut(&run_id)
            .unwrap()
            .messages
            .push(ChatEntry::AssistantMessage {
                text: "child reply".to_string(),
                interrupted: false,
            });

        let projected = project_for_draw(&state);

        assert_eq!(projected.chat_entries.len(), 1);
        assert!(
            matches!(&projected.chat_entries[0], ChatEntry::AssistantMessage { text, .. } if text == "root reply")
        );
        assert!(projected.child_view.child_view_bubble.is_none());
    }

    #[test]
    fn project_for_draw_counts_active_root_subagents_but_not_finished_or_child_view_runs() {
        let mut state = AppState::new();
        let running_bubble_id = Uuid::new_v4();
        let queued_bubble_id = Uuid::new_v4();
        for (bubble_id, status) in [(running_bubble_id, "running"), (queued_bubble_id, "queued")] {
            state.chat_entries.push(ChatEntry::ToolBubble {
                id: bubble_id,
                name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
                state: Box::new(ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![(
                        "status".to_string(),
                        serde_json::Value::String(status.to_string()),
                    )],
                }),
                collapsed: true,
            });
        }
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({}),
                result_section: crate::tui_reducer_state::ResultSection::plain("finished"),
                content_sections: vec![],
                summary: vec![],
                duration: std::time::Duration::ZERO,
                title_arguments: vec![],
            }),
            collapsed: true,
        });

        let root_projection = project_for_draw(&state);
        assert_eq!(
            root_projection.active_subagent_counts,
            ActiveSubagentCounts {
                running: 1,
                queued: 1,
            }
        );

        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id: running_bubble_id,
            agent_type: "reviewer".to_string(),
            task: "inspect the change".to_string(),
            label: "Inspect change".to_string(),
        });
        state.switch_to_child_view(run_id);

        let child_projection = project_for_draw(&state);
        assert_eq!(
            child_projection.active_subagent_counts,
            ActiveSubagentCounts::default(),
            "the main-conversation count must not be shown in a child transcript"
        );
    }

    #[test]
    fn project_for_draw_selects_the_identity_of_the_visible_conversation() {
        let mut state = AppState::new();
        state.engine_display = crate::llm::engine::EngineDisplayInfo {
            engine_name: "openrouter".to_string(),
            provider_connection: "openrouter".to_string(),
            model: "root-model".to_string(),
            reasoning_effort: None,
            compaction_engine: None,
            inherits_parent_runtime: false,
        };

        let root_projection = project_for_draw(&state);
        assert_eq!(
            root_projection.engine_display.provider_connection,
            "openrouter"
        );
        assert_eq!(root_projection.engine_display.model, "root-model");

        let run_id = Uuid::new_v4();
        let bubble_id = Uuid::new_v4();
        push_spawn_bubble(&mut state, bubble_id, "reviewer", "look into flaky test");
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.child_engine_displays.insert(
            run_id,
            crate::llm::engine::EngineDisplayInfo {
                engine_name: "ollama/local2".to_string(),
                provider_connection: "ollama".to_string(),
                model: "child-model".to_string(),
                reasoning_effort: None,
                compaction_engine: None,
                inherits_parent_runtime: false,
            },
        );
        state.switch_to_child_view(run_id);

        let child_projection = project_for_draw(&state);
        assert_eq!(
            child_projection.engine_display.provider_connection,
            "ollama"
        );
        assert_eq!(child_projection.engine_display.model, "child-model");
    }

    #[test]
    fn inherited_child_projection_tracks_parent_engine_and_implicit_summarizer() {
        let run_id = Uuid::new_v4();
        let bubble_id = Uuid::new_v4();
        let mut state = AppState::new();
        state.engine_display = crate::llm::engine::EngineDisplayInfo {
            engine_name: "engine-b".to_string(),
            provider_connection: "mock".to_string(),
            model: "model-b".to_string(),
            reasoning_effort: Some("high".to_string()),
            compaction_engine: Some(crate::llm::engine::CompactionEngineDisplayInfo {
                engine_name: "engine-b".to_string(),
                provider_connection: "mock".to_string(),
                model: "model-b".to_string(),
                context_length: 65_536,
                follows_conversation: true,
                shares_conversation: true,
            }),
            inherits_parent_runtime: false,
        };
        push_spawn_bubble(&mut state, bubble_id, "reviewer", "inspect the change");
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "inspect the change".to_string(),
            label: "Inspect change".to_string(),
        });
        state.child_engine_displays.insert(
            run_id,
            crate::llm::engine::EngineDisplayInfo {
                engine_name: "engine-a".to_string(),
                provider_connection: "mock".to_string(),
                model: "model-a".to_string(),
                reasoning_effort: None,
                compaction_engine: Some(crate::llm::engine::CompactionEngineDisplayInfo {
                    engine_name: "engine-a".to_string(),
                    provider_connection: "mock".to_string(),
                    model: "model-a".to_string(),
                    context_length: 131_072,
                    follows_conversation: true,
                    shares_conversation: true,
                }),
                inherits_parent_runtime: true,
            },
        );
        state.switch_to_child_view(run_id);

        let child_projection = project_for_draw(&state);
        assert_eq!(child_projection.engine_display.engine_name, "engine-b");
        assert_eq!(child_projection.engine_display.model, "model-b");
        let summarizer = child_projection
            .engine_display
            .compaction_engine
            .expect("implicit child summarizer");
        assert_eq!(summarizer.engine_name, "engine-b");
        assert_eq!(summarizer.model, "model-b");
        assert_eq!(summarizer.context_length, 65_536);
        assert!(summarizer.follows_conversation);
        assert!(summarizer.shares_conversation);
    }

    #[test]
    fn project_for_draw_sources_child_transcript_and_header_bubble_when_viewing_child() {
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        let bubble_id = Uuid::new_v4();
        push_spawn_bubble(&mut state, bubble_id, "reviewer", "look into flaky test");
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state
            .child_conversations
            .get_mut(&run_id)
            .unwrap()
            .messages
            .push(ChatEntry::AssistantMessage {
                text: "child reply".to_string(),
                interrupted: false,
            });
        state.switch_to_child_view(run_id);

        let projected = project_for_draw(&state);

        assert_eq!(projected.chat_entries.len(), 1);
        assert!(
            matches!(&projected.chat_entries[0], ChatEntry::AssistantMessage { text, .. } if text == "child reply")
        );
        let (header_bubble_id, header_name, _state, collapsed) = projected
            .child_view
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
    }

    #[test]
    fn drilled_in_view_tints_chat_dividers_with_the_subagent_bubble_color() {
        // The header-to-chat and chat-to-input divider lines are tinted the
        // same pinkish/magenta as the `spawn_subagent` bubble's own border
        // while drilled into a child, unlike the root conversation's neutral
        // dividers — a persistent cue that survives scrolling (an indentation
        // approach was tried first but discarded as visually odd alongside
        // the bubble chrome; see docs/features.md's drill-in section).
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;
        use ratatui::Terminal;
        use ratatui_textarea::TextArea;

        fn divider_colors(state: &AppState) -> Vec<Color> {
            let mut projected = project_for_draw(state);
            let backend = TestBackend::new(60, 20);
            let mut terminal = Terminal::new(backend).expect("terminal");
            let mut textarea = TextArea::default();
            terminal
                .draw(|frame| {
                    crate::tui_widgets::draw(
                        frame,
                        &mut projected,
                        &mut textarea,
                        None,
                        None,
                        &ToolRegistry::new(),
                    );
                })
                .expect("draw");
            let buffer = terminal.backend().buffer().clone();
            let mut colors = Vec::new();
            for row in 0..20u16 {
                let mut row_is_divider = true;
                let mut row_color = None;
                for column in 0..60u16 {
                    let cell = buffer.cell((column, row)).unwrap();
                    if cell.symbol() != "─" {
                        row_is_divider = false;
                        break;
                    }
                    row_color = cell.style().fg;
                }
                if row_is_divider {
                    colors.push(row_color.expect("divider row must set an fg color"));
                }
            }
            colors
        }

        let root_state = AppState::new();
        assert_eq!(
            divider_colors(&root_state),
            vec![Color::White],
            "root view has a single, neutral chat/input divider"
        );

        let mut child_state = AppState::new();
        let run_id = Uuid::new_v4();
        let bubble_id = Uuid::new_v4();
        push_spawn_bubble(
            &mut child_state,
            bubble_id,
            "reviewer",
            "look into flaky test",
        );
        child_state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        child_state.switch_to_child_view(run_id);

        let child_colors = divider_colors(&child_state);
        assert_eq!(
            child_colors.len(),
            2,
            "child view has both a header-to-chat and a chat-to-input divider: {child_colors:?}"
        );
        assert!(
            child_colors.iter().all(|color| *color == Color::Magenta),
            "both dividers must be tinted like the subagent bubble border while drilled in: {child_colors:?}"
        );
    }

    #[test]
    fn drilled_in_view_tints_the_chat_scrollbar_with_the_subagent_bubble_color() {
        // The conversation scrollbar (track/thumb/arrows) picks up the same
        // pinkish/magenta tint as the chat dividers while drilled into a
        // child, instead of the root conversation's neutral scrollbar color.
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;
        use ratatui::Terminal;
        use ratatui_textarea::TextArea;

        fn scrollbar_colors(state: &AppState) -> Vec<Color> {
            let mut projected = project_for_draw(state);
            let backend = TestBackend::new(60, 20);
            let mut terminal = Terminal::new(backend).expect("terminal");
            let mut textarea = TextArea::default();
            terminal
                .draw(|frame| {
                    crate::tui_widgets::draw(
                        frame,
                        &mut projected,
                        &mut textarea,
                        None,
                        None,
                        &ToolRegistry::new(),
                    );
                })
                .expect("draw");
            let buffer = terminal.backend().buffer().clone();
            let mut colors = Vec::new();
            for row in 0..20u16 {
                for column in 0..60u16 {
                    let cell = buffer.cell((column, row)).unwrap();
                    if matches!(cell.symbol(), "┃" | "│" | "↑" | "↓") {
                        if let Some(color) = cell.style().fg {
                            colors.push(color);
                        }
                    }
                }
            }
            colors
        }

        // Enough messages to overflow a 20-row terminal and force the
        // scrollbar to actually render.
        let mut root_state = AppState::new();
        for index in 0..30 {
            root_state.chat_entries.push(ChatEntry::AssistantMessage {
                text: format!("root reply {index}"),
                interrupted: false,
            });
        }
        let root_colors = scrollbar_colors(&root_state);
        assert!(
            !root_colors.is_empty(),
            "root scrollbar must render once content overflows the viewport"
        );
        assert!(
            root_colors.iter().all(|color| *color == Color::White),
            "root scrollbar must stay the neutral color: {root_colors:?}"
        );

        let mut child_state = AppState::new();
        let run_id = Uuid::new_v4();
        let bubble_id = Uuid::new_v4();
        push_spawn_bubble(
            &mut child_state,
            bubble_id,
            "reviewer",
            "look into flaky test",
        );
        child_state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        let conversation = child_state.child_conversations.get_mut(&run_id).unwrap();
        for index in 0..30 {
            conversation.messages.push(ChatEntry::AssistantMessage {
                text: format!("child reply {index}"),
                interrupted: false,
            });
        }
        child_state.switch_to_child_view(run_id);

        let child_colors = scrollbar_colors(&child_state);
        assert!(
            !child_colors.is_empty(),
            "child scrollbar must render once content overflows the viewport"
        );
        assert!(
            child_colors.iter().all(|color| *color == Color::Magenta),
            "child scrollbar must be tinted like the subagent bubble border while drilled in: {child_colors:?}"
        );
    }

    #[test]
    fn project_for_draw_flags_pending_approval_belonging_to_a_subagent() {
        let mut state = AppState::new();
        let tool_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state
            .child_conversations
            .get_mut(&run_id)
            .unwrap()
            .messages
            .push(ChatEntry::ToolBubble {
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

    #[test]
    fn project_for_draw_reports_streaming_agent_mode_while_viewing_an_actively_running_child() {
        // Regression test: while a `spawn_subagent` bubble is `Running` in
        // the root transcript, the root's own `agent_mode()` maps to
        // `ToolRunning` (its `TuiMode` is `Idle`/`Streaming` but a root
        // `ToolBubble` is `Running`). Before this fix, `project_for_draw`
        // used that *root* `agent_mode()` unconditionally, so a drilled-into
        // child's own in-flight thinking/response text never rendered as a
        // streaming bubble (`tui_widgets::draw` only shows it when
        // `agent_mode == AgentMode::Streaming`) — it only appeared once
        // flushed into a committed `ChatEntry`, i.e. no visible
        // in-progress-thinking bubble for subagents (see
        // docs/known-test-issues.md).
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        let bubble_id = Uuid::new_v4();
        push_spawn_bubble(&mut state, bubble_id, "reviewer", "look into flaky test");
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.switch_to_child_view(run_id);
        state
            .child_conversations
            .get_mut(&run_id)
            .unwrap()
            .turn_buffer
            .partial_thinking_text = "pondering...".to_string();

        // Sanity check on the premise: the root's own agent_mode is
        // `ToolRunning`, not `Streaming`, because of the still-running
        // `spawn_subagent` bubble.
        assert_eq!(state.agent_mode(), crate::agent::AgentMode::ToolRunning);

        let projected = project_for_draw(&state);

        assert_eq!(projected.agent_mode, crate::agent::AgentMode::Streaming);
        assert_eq!(projected.partial_thinking_text, "pondering...");
    }

    #[test]
    fn project_for_draw_reports_idle_agent_mode_while_viewing_a_finished_child() {
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        let bubble_id = Uuid::new_v4();
        push_spawn_bubble(&mut state, bubble_id, "reviewer", "look into flaky test");
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.switch_to_child_view(run_id);
        // The child finished: `running_child` is cleared (mirrors
        // `ChildAgentDone` handling), but the transcript/view stays around.
        state.running_child = None;

        let projected = project_for_draw(&state);

        assert_eq!(projected.agent_mode, crate::agent::AgentMode::Idle);
    }

    #[test]
    fn footer_reports_parent_activity_when_viewing_finished_child_with_running_sibling() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use ratatui_textarea::TextArea;

        let mut state = AppState::new();
        let finished_run_id = Uuid::new_v4();
        let finished_bubble_id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: finished_bubble_id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({}),
                result_section: crate::tui_reducer_state::ResultSection::plain("finished"),
                content_sections: vec![],
                summary: vec![],
                duration: std::time::Duration::ZERO,
                title_arguments: vec![],
            }),
            collapsed: false,
        });
        state.start_child_run(ChildRunInfo {
            run_id: finished_run_id,
            bubble_id: finished_bubble_id,
            agent_type: "reviewer".to_string(),
            task: "finished task".to_string(),
            label: "Finished task".to_string(),
        });

        let running_run_id = Uuid::new_v4();
        let running_bubble_id = Uuid::new_v4();
        push_spawn_bubble(&mut state, running_bubble_id, "reviewer", "running task");
        state.start_child_run(ChildRunInfo {
            run_id: running_run_id,
            bubble_id: running_bubble_id,
            agent_type: "reviewer".to_string(),
            task: "running task".to_string(),
            label: "Running task".to_string(),
        });
        state.switch_to_child_view(finished_run_id);

        assert_eq!(state.agent_mode(), crate::agent::AgentMode::ToolRunning);
        let mut projected = project_for_draw(&state);
        assert_eq!(projected.agent_mode, crate::agent::AgentMode::Idle);

        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut textarea = TextArea::default();
        terminal
            .draw(|frame| {
                crate::tui_widgets::draw(
                    frame,
                    &mut projected,
                    &mut textarea,
                    None,
                    None,
                    &ToolRegistry::new(),
                );
            })
            .expect("draw");

        let buffer = terminal.backend().buffer().clone();
        let rendered: String = (0..20u16)
            .flat_map(|row| (0..60u16).map(move |column| (column, row)))
            .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
            .collect();
        assert!(
            rendered.contains("Subagent done, parent working"),
            "footer must report the running sibling parent instead of Idle: {rendered:?}"
        );
    }

    #[test]
    fn sync_from_draw_resolves_pending_scroll_to_bubble_to_its_current_row() {
        // Regression test: Esc (empty draft) must land on the drilled-in-from bubble's
        // *current* row, not a raw offset saved before the root
        // conversation grew while the child view was open.
        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
        state.pending_scroll_to_bubble = Some(bubble_id);
        state.conversation.scroll_offset = 3; // stale fallback, must be overridden below
        state.conversation.scroll_locked = true;

        let mut rendered = TuiState::new();
        rendered.conversation.bubble_title_rows = vec![(0, 4, Uuid::new_v4()), (5, 9, bubble_id)];
        rendered.conversation.scroll_offset = 3;
        rendered.conversation.scroll_locked = true;

        sync_from_draw(&mut state, rendered, None);

        assert_eq!(state.conversation.scroll_offset, 5);
        assert!(!state.conversation.scroll_locked);
        assert!(state.pending_scroll_to_bubble.is_none());
    }

    #[test]
    fn sync_from_draw_leaves_fallback_scroll_when_pending_bubble_is_no_longer_present() {
        // E.g. the drilled-into bubble scrolled out of `chat_entries`
        // entirely (should not happen in practice, but must degrade
        // gracefully rather than panic or silently jump elsewhere).
        let mut state = AppState::new();
        state.pending_scroll_to_bubble = Some(Uuid::new_v4());
        state.conversation.scroll_offset = 7;
        state.conversation.scroll_locked = false;

        let mut rendered = TuiState::new();
        rendered.conversation.bubble_title_rows = vec![(0, 4, Uuid::new_v4())];
        rendered.conversation.scroll_offset = 7;
        rendered.conversation.scroll_locked = false;

        sync_from_draw(&mut state, rendered, None);

        assert_eq!(state.conversation.scroll_offset, 7);
        assert!(!state.conversation.scroll_locked);
        assert!(state.pending_scroll_to_bubble.is_none());
    }

    /// Render one frame of `state` through `registry` into a `TestBackend`
    /// buffer and return its plain text, so a test can assert what a bubble's
    /// title line reads.
    fn rendered_chat_text(state: &AppState, registry: &ToolRegistry) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use ratatui_textarea::TextArea;

        const COLUMNS: u16 = 100;
        const ROWS: u16 = 24;

        let mut projected = project_for_draw(state);
        let mut terminal = Terminal::new(TestBackend::new(COLUMNS, ROWS)).expect("terminal");
        let mut textarea = TextArea::default();
        terminal
            .draw(|frame| {
                crate::tui_widgets::draw(
                    frame,
                    &mut projected,
                    &mut textarea,
                    None,
                    None,
                    registry,
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let mut rendered = String::new();
        for row in 0..ROWS {
            for column in 0..COLUMNS {
                rendered.push_str(buffer.cell((column, row)).expect("cell").symbol());
            }
            rendered.push('\n');
        }
        rendered
    }

    /// The registry `default_registry` builds from a brave-configured profile
    /// — the same `[web_search]` section the CLI hands its display registry.
    fn brave_web_search_registry() -> ToolRegistry {
        crate::agent::tools::default_registry(
            std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new())),
            crate::agent::tools::ToolRegistryConfig {
                web_search: crate::config::WebSearchConfig {
                    backend: "brave".to_string(),
                    backends: crate::config::WebSearchBackendsConfig {
                        searxng: None,
                        brave: Some(crate::config::BraveBackendConfig {
                            api_key: Some("test-subscription-token".to_string()),
                        }),
                    },
                    ..crate::config::WebSearchConfig::default()
                },
                ..crate::agent::tools::ToolRegistryConfig::default()
            },
        )
    }

    fn push_web_search_bubble(state: &mut AppState) {
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: crate::agent::tools::web_search::WebSearchTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({ "terms": "agent-harnesses" }),
                content_sections: vec![],
                title_arguments: vec![("terms".to_string(), serde_json::json!("agent-harnesses"))],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
    }

    #[test]
    fn tool_bubble_title_asks_the_tool_for_its_label_at_render_time() {
        let mut state = AppState::new();
        push_web_search_bubble(&mut state);

        let with_backend = rendered_chat_text(&state, &brave_web_search_registry());
        assert!(
            with_backend.contains("Web Search (Brave)"),
            "the bubble title must name the backend the registry's tool reports: {with_backend:?}"
        );

        // A registry without that tool (e.g. a directly-constructed one)
        // falls back to the title-cased name instead of inventing a backend.
        let without_backend = rendered_chat_text(&state, &ToolRegistry::new());
        assert!(
            without_backend.contains("Web Search"),
            "the fallback still titles the bubble: {without_backend:?}"
        );
        assert!(
            !without_backend.contains("Brave"),
            "the fallback must not claim a backend: {without_backend:?}"
        );
    }

    #[test]
    fn drill_in_header_keeps_the_subagent_title_despite_the_registries_label() {
        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
        push_spawn_bubble(&mut state, bubble_id, "reviewer", "look into flaky test");
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.switch_to_child_view(run_id);
        let mut registry = crate::agent::tools::default_registry(
            std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new())),
            crate::agent::tools::ToolRegistryConfig::default(),
        );
        registry.register(std::sync::Arc::new(
            crate::agent::tools::spawn_subagent::SpawnSubagentTool::new(false),
        ));

        let rendered = rendered_chat_text(&state, &registry);

        assert!(
            rendered.contains("Subagent (reviewer)"),
            "the header keeps its short title and still shows the subtype: {rendered:?}"
        );
        assert!(
            !rendered.contains("agent_type"),
            "the subtype is part of the title, not a prop: {rendered:?}"
        );
        assert!(
            !rendered.contains("Spawn Subagent"),
            "the header's shortened title must win over the tool's own label: {rendered:?}"
        );
    }
}
