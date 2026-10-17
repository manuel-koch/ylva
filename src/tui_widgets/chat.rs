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

//! Conversation painting, legacy layout and scrollbars.

mod cache;
mod selection;

pub(super) use cache::{draw_cached_chat, prepare_cached_header};
pub(super) use selection::{
    apply_selection_highlight_to_line, line_alignment_offset, rendered_line_selection_data,
    terminal_display_width,
};

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::agent::tools::ToolRegistry;
use crate::build_info::BUILD_INFO;
use crate::theme::{
    BUILD_INFO_COLOR, CHAT_SCROLLBAR_COLOR, LOGO_COLOR, SUBAGENT_VIEW_SEPARATOR_COLOR,
};
use crate::tui_reducer_state::{ChatEntry, ToolCallState, UserMessageState};
use crate::tui_render_state::TuiState;

use super::bubbles::{append_streaming_lines, append_streaming_thinking_lines, append_user_lines};
use super::tool_bubble::{append_tool_bubble_lines, ToolBubbleArgs};
use cache::{build_entry, highlight_visible_rows};

/// ASCII-art "Ylva" wordmark shown while the conversation is empty so the
/// initial frame is never blank. Each line must be the same width.
const LOGO: [&str; 7] = [
    "██╗   ██╗██╗     ██╗   ██╗ █████╗ ",
    "╚██╗ ██╔╝██║     ██║   ██║██╔══██╗",
    " ╚████╔╝ ██║     ██║   ██║███████║",
    "  ╚██╔╝  ██║     ╚██╗ ██╔╝██╔══██║",
    "   ██║   ███████╗ ╚████╔╝ ██║  ██║",
    "   ╚═╝   ╚══════╝  ╚═══╝  ╚═╝  ╚═╝",
    "  Taming the wild pack of agents! ",
];

pub(super) fn draw_chat(
    frame: &mut Frame,
    area: Rect,
    state: &mut TuiState,
    registry: &ToolRegistry,
) {
    // Split horizontally first so all widths are derived from content_area —
    // this ensures trailing-pad spans reach the right edge of content_area
    // and leave no bare cells that cause rendering artifacts.
    let [content_area, scrollbar_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(1)])
        .areas(area);

    // Save the content rect so the event loop can hit-test mouse events.
    state.conversation.chat_area = content_area;

    // While viewing a subagent's drill-in transcript, the conversation
    // scrollbar is tinted the same pinkish/magenta as the chat/input
    // dividers and the `spawn_subagent` bubble's own border, instead of the
    // root conversation's neutral scrollbar color.
    let in_subagent_view = state.subagent_view.subagent_view_bubble.is_some();
    let scrollbar_color = if in_subagent_view {
        SUBAGENT_VIEW_SEPARATOR_COLOR
    } else {
        CHAT_SCROLLBAR_COLOR
    };

    // row_width is the full column count of content_area; bubble helpers pad
    // lines to this width so the background fills every cell on each row.
    let row_width = content_area.width as usize;
    // bubble_row_width is 1 char narrower than row_width to leave a 1-char
    // margin between the right edge of each bubble and the scrollbar column.
    let bubble_row_width = row_width.saturating_sub(1);

    let mut lines: Vec<Line> = Vec::new();
    // Pending tool bubbles (awaiting approval) are deferred to the very end
    // of the conversation so they stay close to the approve/deny footer
    // regardless of how many earlier entries exist.
    let mut deferred_pending_tool_bubbles: Vec<(uuid::Uuid, String, ToolCallState, bool)> =
        Vec::new();
    // Rebuilt every frame; assigned to state after the loop (avoids a borrow
    // conflict between iterating state.chat_entries and mutating state).
    let mut bubble_title_rows_out: Vec<(usize, usize, uuid::Uuid)> = Vec::new();
    let mut thinking_title_rows_out: Vec<(usize, usize, usize)> = Vec::new();
    // Show the logo wordmark only when the root conversation is empty at
    // startup; never in the subagent drill-in view — an in-progress subagent
    // with no tokens yet is not "a fresh session", it's mid-flight work, so
    // the empty-state wordmark would be misleading there.
    if state.chat_entries.is_empty() && state.subagent_view.subagent_view_bubble.is_none() {
        append_logo(&mut lines, &state.display_metadata.profile_name);
    } else {
        lines.push(Line::default());
    }
    for (entry_index, entry) in state.chat_entries.iter().enumerate() {
        match entry {
            ChatEntry::ToolBubble {
                name,
                state: bubble_state,
                id,
                collapsed,
            } => {
                // Pending tool bubbles are deferred so they render at the bottom,
                // close to the approve/deny footer buttons.
                if matches!(bubble_state.as_ref(), ToolCallState::Pending { .. }) {
                    deferred_pending_tool_bubbles.push((
                        *id,
                        name.clone(),
                        bubble_state.as_ref().clone(),
                        *collapsed,
                    ));
                    continue;
                }
                let bubble_tool = registry.get(name);
                let display_title_override = bubble_tool.as_ref().map(|tool| tool.title());
                let subtype_argument = bubble_tool
                    .as_ref()
                    .and_then(|tool| tool.subtype_argument());
                append_tool_bubble_lines(
                    &mut lines,
                    ToolBubbleArgs {
                        id: *id,
                        name,
                        state: bubble_state.as_ref(),
                        collapsed: *collapsed,
                        spinner_frame: state.spinner_frame,
                        elapsed_seconds: state
                            .subagent_started_at
                            .get(id)
                            .map(|started| started.elapsed().as_secs()),
                        display_title_override: display_title_override.as_deref(),
                        subtype_argument,
                        path_like_arguments: bubble_tool
                            .as_ref()
                            .map_or(&[], |tool| tool.path_like_arguments()),
                    },
                    &mut bubble_title_rows_out,
                    bubble_row_width.saturating_sub(4).max(1),
                );
            }
            _ => {
                let offset = lines.len();
                let collapsed = matches!(
                    entry,
                    ChatEntry::ThinkingMessage {
                        collapsed: true,
                        ..
                    }
                );
                let layout = build_entry(entry, entry_index, content_area.width, collapsed);
                thinking_title_rows_out.extend(
                    layout
                        .thinking
                        .into_iter()
                        .map(|(start, end, index)| (start + offset, end + offset, index)),
                );
                lines.extend(layout.rows);
                continue;
            }
        }
        // Blank separator between entries so the conversation breathes.
        lines.push(Line::default());
    }

    // Gated on the presence of pending text, not on `agent_mode == Streaming`:
    // a running tool bubble (e.g. a still-in-flight `spawn_subagent` subagent)
    // makes `agent_mode()` report `ToolRunning` even while the same root
    // loop is mid-stream on its *next* iteration (see `AppState::agent_mode`
    // and `AppState::has_running_tool_bubble`). Gating strictly on
    // `Streaming` left that in-flight text invisible until it was flushed
    // into a fully-committed bubble — it "popped in" with no visible
    // streaming progress. `partial_thinking_text`/`partial_assistant_text`
    // are always cleared by the relevant `flush_partial_*` call before any
    // non-streaming mode (Idle/ToolPending/ClarifyPending) is entered, so
    // non-empty here reliably means a stream is actually in progress.
    if !state.partial_thinking_text.is_empty() {
        let stream_width = bubble_row_width.saturating_sub(4).max(1);
        append_streaming_thinking_lines(
            &mut lines,
            &state.partial_thinking_text.clone(),
            state.thinking_started_at,
            state.partial_thinking_collapsed,
            &mut thinking_title_rows_out,
            stream_width,
        );
        lines.push(Line::default());
    }

    // Same reasoning as the thinking-bubble gate above: presence of pending
    // text, not the footer-facing `agent_mode`, is what tells us a stream is
    // actually in progress.
    if !state.partial_assistant_text.is_empty() {
        let stream_width = bubble_row_width.saturating_sub(4).max(1);
        append_streaming_lines(
            &mut lines,
            &state.partial_assistant_text.clone(),
            stream_width,
        );
        // Match the blank separator that the committed-entry loop appends so
        // the gap between the bubble and the input field is consistent whether
        // streaming is in progress or finished.
        lines.push(Line::default());
    }

    // Flush staged (not-yet-committed) queued steer messages after the
    // stream bubble so they appear visually after the current in-progress
    // response/thinking/tool-call content. See `AppState::queue_steer_message`.
    for queued_text in &state.queued_steer_texts {
        let user_width = bubble_row_width.saturating_sub(4).max(1);
        append_user_lines(
            &mut lines,
            queued_text,
            UserMessageState::Queued,
            user_width,
        );
        lines.push(Line::default());
    }

    // Flush pending-approval tool bubbles last so they always sit at the
    // bottom of the conversation, directly above the approve/deny footer.
    for (bubble_id, name, bubble_state, collapsed) in &deferred_pending_tool_bubbles {
        let pending_tool = registry.get(name);
        let display_title_override = pending_tool.as_ref().map(|tool| tool.title());
        let subtype_argument = pending_tool
            .as_ref()
            .and_then(|tool| tool.subtype_argument());
        append_tool_bubble_lines(
            &mut lines,
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
                path_like_arguments: pending_tool
                    .as_ref()
                    .map_or(&[], |tool| tool.path_like_arguments()),
            },
            &mut bubble_title_rows_out,
            bubble_row_width.saturating_sub(4).max(1),
        );
        lines.push(Line::default());
    }

    state.conversation.bubble_title_rows = bubble_title_rows_out;
    state.conversation.thinking_title_rows = thinking_title_rows_out;

    // Extract plain text of every line for selection/copy support.
    let (rendered_lines, prefix_widths) = rendered_line_selection_data(&lines, content_area.width);
    state.conversation.chat_rendered_lines = rendered_lines;
    state.conversation.chat_rendered_line_prefix_widths = prefix_widths;

    // Strip any trailing blank line: the static layout spacer (Constraint::Length(1)
    // between chat_area and input_area) owns the visual gap, so keeping a
    // blank inside the scrollable content would create a double gap when
    // scrolled to the bottom.
    if lines.last().is_some_and(|line| line.spans.is_empty()) {
        lines.pop();
    }

    let total_lines = lines.len();
    let visible = area.height as usize;
    // Upper scroll bound: how far the viewport can be offset from the top.
    let max_offset = total_lines.saturating_sub(visible);
    state.conversation.chat_scrollbar = crate::tui_reducer_state::ChatScrollbarLayout {
        rect: if max_offset > 0 {
            scrollbar_area
        } else {
            Rect::default()
        },
        maximum_scroll_offset: max_offset,
    };
    let row_offset = if state.conversation.scroll_locked {
        // Keep scroll_offset in sync with the rendered bottom so that the
        // first scroll_up(1) call starts from the actual bottom position
        // rather than 0, which would otherwise cause the view to jump to
        // the top on the very first mouse-wheel or PageUp event.
        state.conversation.scroll_offset = max_offset;
        max_offset
    } else {
        // Clamp the manual offset to the content height so scrolling past the
        // end (or up past the top) cannot leave the offset silently drifting.
        state.conversation.scroll_offset = state.conversation.scroll_offset.min(max_offset);
        // Scrolling back down to the very bottom re-enables auto-scroll.
        if state.conversation.scroll_offset >= max_offset {
            state.conversation.scroll_locked = true;
        }
        state.conversation.scroll_offset
    };

    let mut lines: Vec<_> = lines.into_iter().skip(row_offset).take(visible).collect();
    highlight_visible_rows(
        &mut lines,
        row_offset,
        state.conversation.chat_selection,
        content_area.width,
    );
    let paragraph = Paragraph::new(lines);

    frame.render_widget(paragraph, content_area);

    // Show a custom scrollbar only when the content overflows the viewport.
    if total_lines > visible {
        render_chat_scrollbar(
            frame,
            scrollbar_area,
            row_offset,
            max_offset,
            total_lines,
            visible,
            scrollbar_color,
        );
    }
}

/// Compute the thumb span for the chat scrollbar track.
///
/// Returns `(thumb_top, thumb_size)` — both in track-relative row units within
/// `[0, track_height)`. The thumb always occupies at least one row and never
/// extends beyond the track.
fn compute_scrollbar_thumb(
    track_height: usize,
    total_lines: usize,
    visible: usize,
    row_offset: usize,
    max_offset: usize,
) -> (usize, usize) {
    let thumb_size = ((track_height * visible) / total_lines)
        .max(1)
        .min(track_height);
    let thumb_top = if max_offset == 0 {
        0
    } else {
        (row_offset * (track_height - thumb_size)) / max_offset
    };
    (thumb_top, thumb_size)
}

/// Render the custom chat scrollbar into `scrollbar_area` (1 column wide).
///
/// Layout (when height ≥ 4):
///   row 0                  → ↑ (Blue when can scroll up, DarkGray otherwise)
///   rows 1 .. height-2     → thin │ track (DarkGray), ┃ thumb (Gray)
///   row height-1           → ↓ (Blue when can scroll down, DarkGray otherwise)
///
/// On very short areas (< 4 rows) the track fills the whole column with no
/// arrow rows.
pub(super) fn render_chat_scrollbar(
    frame: &mut Frame,
    scrollbar_area: Rect,
    row_offset: usize,
    max_offset: usize,
    total_lines: usize,
    visible: usize,
    color: Color,
) {
    let height = scrollbar_area.height as usize;
    if height == 0 {
        return;
    }
    let has_arrows = height >= 4;
    let (track_y, track_height) = if has_arrows {
        (scrollbar_area.y + 1, height - 2)
    } else {
        (scrollbar_area.y, height)
    };
    let (thumb_top, thumb_size) =
        compute_scrollbar_thumb(track_height, total_lines, visible, row_offset, max_offset);

    for row in 0..track_height {
        let is_thumb = row >= thumb_top && row < thumb_top + thumb_size;
        let style = if is_thumb {
            Style::default().fg(color)
        } else {
            Style::default().fg(color).add_modifier(Modifier::DIM)
        };
        let symbol = if is_thumb { "┃" } else { "│" };
        frame.render_widget(
            Paragraph::new(symbol).style(style),
            Rect::new(scrollbar_area.x, track_y + row as u16, 1, 1),
        );
    }

    if has_arrows {
        let arrow_active = Style::default().fg(color);
        let arrow_dim = Style::default().fg(color).add_modifier(Modifier::DIM);
        frame.render_widget(
            Paragraph::new("↑").style(if row_offset > 0 {
                arrow_active
            } else {
                arrow_dim
            }),
            Rect::new(scrollbar_area.x, scrollbar_area.y, 1, 1),
        );
        frame.render_widget(
            Paragraph::new("↓").style(if row_offset < max_offset {
                arrow_active
            } else {
                arrow_dim
            }),
            Rect::new(
                scrollbar_area.x,
                scrollbar_area.y + scrollbar_area.height - 1,
                1,
                1,
            ),
        );
    }
}

/// Render the Ylva wordmark, horizontally centered. Only shown when the
/// conversation is still empty (no messages submitted yet).
fn append_logo(lines: &mut Vec<Line>, profile_name: &str) {
    lines.push(Line::default());
    lines.push(Line::default());
    let style = Style::default().fg(LOGO_COLOR).add_modifier(Modifier::BOLD);
    for row in LOGO {
        lines.push(Line::styled(row.to_string(), style).alignment(Alignment::Center));
    }
    // One blank line, then build-info centered below the wordmark.
    lines.push(Line::default());
    let build_info_text = format!("v{}  {}", BUILD_INFO.version, BUILD_INFO.git_commit);
    lines.push(
        Line::styled(build_info_text, Style::default().fg(BUILD_INFO_COLOR))
            .alignment(Alignment::Center),
    );
    let profile_text = format!("profile: {profile_name}");
    lines.push(
        Line::styled(profile_text, Style::default().fg(BUILD_INFO_COLOR))
            .alignment(Alignment::Center),
    );
    // A blank spacer separates the wordmark block from the conversation.
    lines.push(Line::default());
}

#[cfg(test)]
mod tests;
