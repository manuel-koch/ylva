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

//! ratatui layout and widgets for the Ylva TUI.
//!
//! Test-backend fixtures verify buffers and rendering geometry. This facade and
//! all widget subagents participate in the aggregate coverage gate.

mod bubbles;
mod chat;
mod footer;
mod help;
mod overlays;
mod tool_bubble;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use ratatui_textarea::TextArea;

use crate::agent::tools::ToolRegistry;
use crate::conversation_render_cache::ConversationRenderCache;
use crate::markdown::render_markdown;
use crate::theme::{
    CHAT_SELECTION_BG, INPUT_BORDER_COLOR, MUTED_TEXT_COLOR, SEPARATOR_LINE_COLOR,
    SUBAGENT_VIEW_SEPARATOR_COLOR, USER_MESSAGE_BG_COLOR,
};
use crate::tui_reducer_state::BubbleExpansionTarget;
use crate::tui_reducer_state::ChatEntry;
use crate::tui_render_state::TuiState;

use chat::{
    apply_selection_highlight_to_line, draw_cached_chat, draw_chat, line_alignment_offset,
    prepare_cached_header, render_chat_scrollbar, rendered_line_selection_data,
};
use footer::{
    draw_approval_footer, draw_normal_footer, footer_keybinding_hints, help_popup_footer_hints,
};
use help::draw_help_popup;
use overlays::{draw_clarify_banner, draw_slash_overlay, draw_toast};
use tool_bubble::{append_tool_bubble_lines, ToolBubbleArgs};

pub(crate) struct ConversationSource<'a> {
    pub expansion_feedback: Option<BubbleExpansionTarget>,
    pub run_id: Option<uuid::Uuid>,
    entries: &'a [ChatEntry],
    turn: &'a crate::tui_reducer_state::TurnBuffer,
    queued: &'a [String],
    header: Option<(usize, &'a ChatEntry)>,
    revisions: &'a std::collections::HashMap<(Option<uuid::Uuid>, usize), u64>,
    title_revisions: &'a std::collections::HashMap<uuid::Uuid, u64>,
    stream_revision: u64,
    queued_revision: u64,
}

impl<'a> ConversationSource<'a> {
    pub fn from_app(state: &'a crate::tui_reducer_state::AppState) -> Self {
        let run_id = match state.active_view {
            crate::tui_reducer_state::ConversationView::Root => None,
            crate::tui_reducer_state::ConversationView::Subagent { run_id, .. } => Some(run_id),
        };
        let subagent = run_id.and_then(|run_id| state.subagent_conversations.get(&run_id));
        Self {
            expansion_feedback: None,
            run_id,
            entries: subagent.map_or(state.chat_entries.as_slice(), |subagent| subagent.messages.as_slice()),
            turn: subagent.map_or(&state.turn_buffer, |subagent| &subagent.turn_buffer),
            queued: if run_id.is_none() { &state.queued_steer_texts } else { &[] },
            header: state.header_bubble_id().and_then(|bubble_id| {
                state.chat_entries.iter().enumerate().find(
                    |(_, entry)| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == bubble_id),
                )
            }),
            revisions: &state.render_revisions,
            title_revisions: &state.render_title_revisions,
            stream_revision: state.render_stream_revisions.get(&run_id).copied().unwrap_or_default(),
            queued_revision: state.queued_render_revision,
        }
    }
}

pub(crate) struct BorrowedConversation<'a> {
    pub source: &'a ConversationSource<'a>,
    pub cache: &'a mut ConversationRenderCache,
}

/// The user-message input field grows with content but is capped at this
/// fraction of the terminal height. Adjust here to change the maximum visible
/// height of the input area.
const USER_MESSAGE_INPUT_HEIGHT_TERMINAL_FRACTION: u16 = 3;

/// Braille spinner animation frames, cycled while the agent is busy.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Clickable approval button rects returned by `draw` when in approval sub-mode.
pub struct ButtonAreas {
    pub approve: Rect,
    pub deny: Rect,
}

/// Draw the full TUI frame. Returns `Some(ButtonAreas)` when in tool approval
/// sub-mode (`pending_approval` is `Some`), `None` otherwise.
///
/// `state` is taken mutably so the manual scroll offset can be clamped to the
/// rendered content height (preventing silent over-scroll).
pub fn draw(
    frame: &mut Frame,
    state: &mut TuiState,
    textarea: &mut TextArea<'_>,
    hint: Option<&str>,
    mouse_pos: Option<(u16, u16)>,
    registry: &ToolRegistry,
) -> Option<ButtonAreas> {
    draw_internal(frame, state, textarea, hint, mouse_pos, registry, None)
}

pub(crate) fn draw_borrowed(
    frame: &mut Frame,
    state: &mut TuiState,
    textarea: &mut TextArea<'_>,
    hint: Option<&str>,
    mouse_pos: Option<(u16, u16)>,
    registry: &ToolRegistry,
    conversation: &mut BorrowedConversation<'_>,
) -> Option<ButtonAreas> {
    draw_internal(
        frame,
        state,
        textarea,
        hint,
        mouse_pos,
        registry,
        Some(conversation),
    )
}

fn draw_internal(
    frame: &mut Frame,
    state: &mut TuiState,
    textarea: &mut TextArea<'_>,
    hint: Option<&str>,
    mouse_pos: Option<(u16, u16)>,
    registry: &ToolRegistry,
    mut conversation: Option<&mut BorrowedConversation<'_>>,
) -> Option<ButtonAreas> {
    let full_area = frame.area();
    // Drill-in conversation model (Phase 6.10): while viewing a subagent
    // conversation, reserve a header above the chat area that re-renders the
    // real `spawn_subagent` tool bubble from the root conversation (same
    // chrome, same collapsed/expanded state, same task/label/agent_type
    // content) — so the drill-in view always shows exactly what the user
    // clicked into, without a separately-maintained synthetic header text.
    // Absent while viewing `Root`. A 1-char scrollbar column is reserved on
    // the right (mirroring the chat area's own content/scrollbar split) and
    // a separator row is rendered below it, mirroring the separator between
    // the chat area and the input/footer panel — the header must not be
    // visually mistaken for part of the scrollable conversation-list below.
    let mut header_lines: Vec<Line<'static>> = Vec::new();
    if conversation.is_none() {
        if let Some((bubble_id, name, bubble_state, collapsed)) =
            &state.subagent_view.subagent_view_bubble
        {
            let header_row_width = full_area.width.saturating_sub(1) as usize;
            let mut discarded_title_rows = Vec::new();
            // The header sits directly above the subagent's own transcript, so
            // "Spawn" (relevant only from the root conversation's point of view)
            // is dropped in favor of the shorter "Subagent". Every other tool
            // bubble asks its own tool for the label, like the CLI status line.
            let header_tool = registry.get(name);
            let display_title_override =
                if name == crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME {
                    Some("Subagent".to_string())
                } else {
                    header_tool.as_ref().map(|tool| tool.title())
                };
            let subtype_argument = header_tool
                .as_ref()
                .and_then(|tool| tool.subtype_argument());
            append_tool_bubble_lines(
                &mut header_lines,
                ToolBubbleArgs {
                    id: *bubble_id,
                    name,
                    state: bubble_state,
                    collapsed: *collapsed,
                    spinner_frame: state.spinner_frame,
                    elapsed_seconds: state
                        .subagent_started_at
                        .get(bubble_id)
                        .map(|started| started.elapsed().as_secs()),
                    display_title_override: display_title_override.as_deref(),
                    subtype_argument,
                    path_like_arguments: header_tool
                        .as_ref()
                        .map_or(&[], |tool| tool.path_like_arguments()),
                },
                &mut discarded_title_rows,
                header_row_width.saturating_sub(4).max(1),
            );
        }
    }
    let header_total = if let Some(conversation) = conversation.as_mut() {
        prepare_cached_header(
            conversation,
            state,
            registry,
            full_area.width.saturating_sub(1),
        );
        conversation.cache.header.height
    } else {
        header_lines.len()
    };
    let (header_area, area) = if header_total == 0 {
        (None, full_area)
    } else {
        // Cap the header at 40 % of the terminal, matching the clarify-banner
        // convention, so an expanded bubble with a long result never crowds
        // out the chat area entirely.
        let max_header_height = (full_area.height as usize * 40 / 100).max(1) as u16;
        let header_height = header_total.min(max_header_height as usize) as u16;
        let [header_area, header_separator_area, rest] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .areas(full_area);
        (Some((header_area, header_separator_area)), rest)
    };
    // The input box grows to fit its (word-wrapped) content, capped at 1/3 of
    // the terminal height. Wrapped row count, not logical line count, drives
    // the height so long lines grow the box instead of scrolling it.
    let max_input = (area.height / USER_MESSAGE_INPUT_HEIGHT_TERMINAL_FRACTION).max(3);
    let input_width = area.width.saturating_sub(1).max(1);
    let wrapped_rows = {
        let mut measurement = TextArea::from(textarea.lines().to_vec());
        measurement.set_wrap_mode(textarea.wrap_mode());
        measurement.set_tab_length(textarea.tab_length());
        let measurement_area = Rect::new(0, 0, input_width, 1);
        ratatui::widgets::Widget::render(
            &measurement,
            measurement_area,
            &mut ratatui::buffer::Buffer::empty(measurement_area),
        );
        measurement.move_cursor(ratatui_textarea::CursorMove::Bottom);
        measurement.move_cursor(ratatui_textarea::CursorMove::End);
        measurement.screen_cursor().row + 1
    };
    let input_height = wrapped_rows.min(max_input as usize).max(1) as u16;

    // When a tool approval is pending the approval title and buttons appear
    // ABOVE the input field so the approve/deny controls sit immediately
    // above where the user types a denial reason.
    //
    // The footer is one area whose height (2 wide / 3 narrow rows) comes from
    // `footer::footer_height`; its row split happens inside the footer
    // drawing functions. In approval mode the footer sits above the input.
    let approval_pending = state.pending_approval.is_some();
    let clarify_banner_content_height = state
        .clarify_banner_state
        .pending_clarify
        .as_ref()
        .map(|(question, choices, context)| {
            let context_rows = context.as_deref().map_or(0, |ctx| {
                render_markdown(ctx, area.width as usize).len() as u16
            });
            let context_spacer = u16::from(context.is_some());
            let question_rows = render_markdown(question, area.width as usize).len().max(1) as u16;
            context_rows
                + context_spacer
                + question_rows
                + choices.as_ref().map_or(0, Vec::len) as u16
        })
        .unwrap_or(0);
    // Cap the visible height so the banner never consumes more than 40 % of
    // the terminal.  When content is taller the user can scroll.
    let clarify_banner_height = if clarify_banner_content_height > 0 {
        let max_banner_height = (area.height * 40 / 100).max(3);
        clarify_banner_content_height.min(max_banner_height)
    } else {
        0
    };
    let footer_height = footer::footer_height(area.width);
    let (chat_area, separator_area, input_area, footer_area, clarify_area) = if approval_pending {
        let [chat_area, separator_area, footer_area, input_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(footer_height),
                Constraint::Length(input_height),
            ])
            .areas(area);
        (chat_area, separator_area, input_area, footer_area, None)
    } else if clarify_banner_height > 0 {
        let [chat_area, separator_area, clarify_area, input_area, footer_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(clarify_banner_height),
                Constraint::Length(input_height),
                Constraint::Length(footer_height),
            ])
            .areas(area);
        (
            chat_area,
            separator_area,
            input_area,
            footer_area,
            Some(clarify_area),
        )
    } else {
        let [chat_area, separator_area, input_area, footer_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(input_height),
                Constraint::Length(footer_height),
            ])
            .areas(area);
        (chat_area, separator_area, input_area, footer_area, None)
    };

    if let Some((header_area, header_separator_area)) = header_area {
        let [header_content_area, header_scrollbar_area] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Fill(1), Constraint::Length(1)])
            .areas(header_area);
        state.subagent_view.header_bubble_area = header_content_area;

        let total_lines = header_total;
        let visible = header_content_area.height as usize;
        let max_offset = total_lines.saturating_sub(visible);
        // Clamp (rather than trust) the stored offset: the bubble's content
        // height can shrink between frames (e.g. collapsing it, or a shorter
        // subagent result replacing a longer in-progress one), which would
        // otherwise leave the offset pointing past the end of the content.
        let row_offset = state
            .subagent_view
            .header_bubble_scroll_offset
            .min(max_offset);
        state.subagent_view.header_bubble_scroll_offset = row_offset;

        if let Some(conversation) = conversation.as_mut() {
            header_lines = conversation.cache.header.visible_rows(row_offset, visible);
            conversation.cache.painted_rows += header_lines.len();
        } else {
            let (rendered_lines, prefix_widths) =
                rendered_line_selection_data(&header_lines, header_content_area.width);
            state.subagent_view.header_rendered_lines = rendered_lines;
            state.subagent_view.header_rendered_line_prefix_widths = prefix_widths;
            header_lines = header_lines
                .into_iter()
                .skip(row_offset)
                .take(visible)
                .collect();
        }

        if let Some(selection) = state.subagent_view.header_selection {
            if !selection.is_empty() {
                let (start_line, start_col, end_line, end_col) = selection.normalized();
                for (index, line) in header_lines.iter_mut().enumerate() {
                    let index = index + row_offset;
                    if index < start_line || index > end_line {
                        continue;
                    }
                    let column_start = if index == start_line { start_col } else { 0 };
                    let column_end = if index == end_line {
                        end_col
                    } else {
                        usize::MAX
                    };
                    let alignment_offset = line_alignment_offset(line, header_content_area.width);
                    apply_selection_highlight_to_line(
                        line,
                        column_start.saturating_sub(alignment_offset),
                        column_end.saturating_sub(alignment_offset),
                        CHAT_SELECTION_BG,
                    );
                }
            }
        }

        let paragraph = Paragraph::new(header_lines);
        frame.render_widget(paragraph, header_content_area);

        // Show the scroll indicators/scrollbar only when the bubble's full
        // content overflows the clamped header height. Tinted
        // `SUBAGENT_VIEW_SEPARATOR_COLOR` — this scrollbar only ever renders
        // while the header bubble (i.e. the subagent view) is showing.
        if total_lines > visible {
            render_chat_scrollbar(
                frame,
                header_scrollbar_area,
                row_offset,
                max_offset,
                total_lines,
                visible,
                SUBAGENT_VIEW_SEPARATOR_COLOR,
            );
        }

        // Separator between the header bubble and the chat area below it —
        // mirrors the separator between the chat area and the input/footer
        // panel, so the header never reads as part of the scrollable
        // conversation-list. Tinted `SUBAGENT_VIEW_SEPARATOR_COLOR` (always the
        // case here — this branch only runs while a header bubble, i.e. the
        // subagent view, is showing) rather than the neutral root-view color.
        let header_separator_line = "─".repeat(header_separator_area.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                header_separator_line,
                Style::default()
                    .fg(SUBAGENT_VIEW_SEPARATOR_COLOR)
                    .add_modifier(Modifier::DIM),
            ))),
            header_separator_area,
        );
    } else {
        state.subagent_view.header_bubble_area = Rect::default();
        state.subagent_view.header_rendered_lines.clear();
        state
            .subagent_view
            .header_rendered_line_prefix_widths
            .clear();
    }

    if let Some(conversation) = conversation.as_mut() {
        draw_cached_chat(frame, chat_area, state, registry, conversation);
    } else {
        draw_chat(frame, chat_area, state, registry);
    }

    // Thin solid line separating the conversation area from the lower panel.
    // Tinted the same pinkish/magenta as the header separator while viewing a
    // subagent's drill-in transcript, so the cue survives scrolling even once
    // the (non-scrolling) header bubble is out of view — the neutral color is
    // reserved for the root conversation.
    let in_subagent_view = state.subagent_view.subagent_view_bubble.is_some();
    let separator_color = if in_subagent_view {
        SUBAGENT_VIEW_SEPARATOR_COLOR
    } else {
        SEPARATOR_LINE_COLOR
    };
    let separator_line = "─".repeat(separator_area.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            separator_line,
            Style::default()
                .fg(separator_color)
                .add_modifier(Modifier::DIM),
        ))),
        separator_area,
    );

    // The input box is styled like a sent message: a colored left border, no
    // full frame, no "Input" title. The current mode/steer/approval state is
    // conveyed by the footer, not a border title.
    // Use `▌` (half-block) for the left border so it matches the thick bar
    // rendered by user-message bubbles.
    textarea.set_block(
        Block::default()
            .borders(Borders::LEFT)
            .border_set(ratatui::symbols::border::Set {
                vertical_left: "▌",
                top_left: "▌",
                bottom_left: "▌",
                ..ratatui::symbols::border::PLAIN
            })
            .border_style(
                Style::default()
                    .fg(INPUT_BORDER_COLOR)
                    .add_modifier(Modifier::DIM),
            )
            .style(Style::default().bg(USER_MESSAGE_BG_COLOR)),
    );
    frame.render_widget(&*textarea, input_area);

    // InViewport clamps an origin cursor to the actual rendered viewport top.
    // Probe a clone so cursor movement and draft replacement cannot desync it.
    let mut viewport_probe = textarea.clone();
    viewport_probe.move_cursor(ratatui_textarea::CursorMove::Jump(0, 0));
    viewport_probe.move_cursor(ratatui_textarea::CursorMove::InViewport);
    state.input_scroll_top = viewport_probe.screen_cursor().row;

    // Show ↑/↓ based on the tracked viewport position, not the cursor row.
    if wrapped_rows > input_height as usize && input_area.height >= 1 && input_area.width >= 1 {
        let input_indicator_style = Style::default()
            .fg(INPUT_BORDER_COLOR)
            .add_modifier(Modifier::DIM);
        if state.input_scroll_top > 0 {
            frame.render_widget(
                Paragraph::new("↑").style(input_indicator_style),
                Rect::new(input_area.x + input_area.width - 1, input_area.y, 1, 1),
            );
        }
        if state.input_scroll_top + (input_height as usize) < wrapped_rows && input_area.height > 1
        {
            frame.render_widget(
                Paragraph::new("↓").style(input_indicator_style),
                Rect::new(
                    input_area.x + input_area.width - 1,
                    input_area.y + input_area.height - 1,
                    1,
                    1,
                ),
            );
        }
    }

    // When a tool approval is pending and the input field is empty, overlay a
    // dimmed placeholder hint so the user knows the field accepts a denial reason.
    // The overlay sits one column to the right of the `▌` border character.
    let input_is_empty = textarea.lines() == [""];
    if approval_pending && input_is_empty && input_area.width > 1 && input_area.height > 0 {
        let placeholder_area = Rect {
            x: input_area.x + 1,
            y: input_area.y,
            width: input_area.width - 1,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::styled(
                "Enter reason for denial here…",
                Style::default()
                    .fg(MUTED_TEXT_COLOR)
                    .bg(USER_MESSAGE_BG_COLOR),
            )),
            placeholder_area,
        );
    }

    draw_slash_overlay(
        frame,
        input_area,
        &state.slash_overlay,
        state.slash_overlay_drag,
        &state.engine_display,
        &mut state.slash_overlay_layout,
    );

    // Store the input rect for mouse click/drag handling in the event loop.
    state.input_area = input_area;
    // Store the clarify banner rect for mouse click-to-choose handling.
    state.clarify_banner_state.clarify_banner = clarify_area.unwrap_or_default();

    if let (Some(clarify_area), Some((question, choices, context))) = (
        clarify_area,
        state.clarify_banner_state.pending_clarify.as_ref(),
    ) {
        draw_clarify_banner(
            frame,
            clarify_area,
            question,
            choices.as_ref(),
            context.as_deref(),
            state.clarify_banner_state.clarify_banner_drag,
            &mut state.clarify_banner_state.clarify_scroll_offset,
            clarify_banner_content_height as usize,
        );
    }

    let footer_source = conversation
        .as_ref()
        .map(|conversation| conversation.source);
    let button_areas = if state.help_popup.show_help {
        draw_normal_footer(
            frame,
            footer_area,
            state,
            help_popup_footer_hints(state),
            hint,
            footer_source,
        );
        None
    } else if let Some((_, name, _)) = &state.pending_approval.clone() {
        draw_approval_footer(
            frame,
            footer_area,
            state,
            name,
            state.pending_approval_is_subagent,
            input_is_empty,
            mouse_pos,
            hint,
        )
    } else {
        let footer_hints = footer_keybinding_hints(state, input_is_empty);
        draw_normal_footer(frame, footer_area, state, footer_hints, hint, footer_source);
        None
    };

    if state.help_popup.show_help {
        draw_help_popup(frame, state);
    }

    // Dismiss any toast whose display window has elapsed, then render it.
    state.clear_expired_toast();
    if let Some((message, _)) = state.toast.clone() {
        draw_toast(frame, area, &message);
    }

    button_areas
}
