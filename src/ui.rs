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
//! This module is excluded from the coverage gate (it needs a live terminal);
//! visual correctness is verified manually.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;
use ratatui_textarea::TextArea;

use crate::agent::AgentMode;
use crate::app::{ChatEntry, ToolBubbleState, TuiState, UserMessageState};
use crate::build_info::BUILD_INFO;

// User message border colors — extract into Theme struct in a later phase.
const USER_MESSAGE_BORDER_COLOR: Color = Color::Green;
// Bright RGB value so the queued border is visually distinct from plain Green
// even in terminals where Color::LightGreen and Color::Green look the same.
const USER_MESSAGE_QUEUED_BORDER_COLOR: Color = Color::Yellow;
// Applied steer: lighter green to stay distinct from both Sent (Green) and Queued (Yellow).
const USER_MESSAGE_STEERED_BORDER_COLOR: Color = Color::LightGreen;
const USER_MESSAGE_CANCELLED_BORDER_COLOR: Color = Color::DarkGray;
// Soft background shade applied to all user message blocks.
const USER_MESSAGE_BG_COLOR: Color = Color::Rgb(20, 20, 40);

// Assistant streaming bubble colors.
const ASSISTANT_STREAM_BORDER_COLOR: Color = Color::Blue;
const ASSISTANT_STREAM_BG_COLOR: Color = Color::Rgb(15, 20, 35);

// Tool bubble colors — magenta left border, dark magenta background tint.
const TOOL_BUBBLE_BORDER_COLOR: Color = Color::Magenta;
const TOOL_BUBBLE_BG_COLOR: Color = Color::Rgb(30, 0, 30);

/// Braille spinner animation frames, cycled while the agent is busy.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// ASCII-art "Ylva" wordmark rendered as the first element of the chat list so
/// the initial frame is never empty. Each line must be the same width.
const LOGO: [&str; 6] = [
    "██╗   ██╗██╗     ██╗   ██╗ █████╗ ",
    "╚██╗ ██╔╝██║     ██║   ██║██╔══██╗",
    " ╚████╔╝ ██║     ██║   ██║███████║",
    "  ╚██╔╝  ██║     ╚██╗ ██╔╝██╔══██║",
    "   ██║   ███████╗ ╚████╔╝ ██║  ██║",
    "   ╚═╝   ╚══════╝  ╚═══╝  ╚═╝  ╚═╝",
];

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
#[allow(clippy::too_many_arguments)]
pub fn draw(
    frame: &mut Frame,
    state: &mut TuiState,
    textarea: &mut TextArea<'_>,
    hint: Option<&str>,
    mouse_pos: Option<(u16, u16)>,
) -> Option<ButtonAreas> {
    let area = frame.area();
    // The input box grows to fit its (word-wrapped) content, capped at 1/3 of
    // the terminal height. Wrapped row count, not logical line count, drives
    // the height so long lines grow the box instead of scrolling it.
    let max_input = (area.height / 3).max(3);
    let input_width = area.width.saturating_sub(1).max(1) as usize;
    let wrapped_rows = textarea
        .lines()
        .iter()
        .map(|line| textwrap::wrap(line, input_width).len().max(1))
        .sum::<usize>();
    let input_height = (wrapped_rows as u16).clamp(1, max_input);

    let [chat_area, _, input_area, footer_row_1, footer_row_2] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

    draw_chat(frame, chat_area, state);

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
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(USER_MESSAGE_BG_COLOR)),
    );
    frame.render_widget(&*textarea, input_area);

    // Store the input rect for mouse click/drag handling in the event loop.
    state.input_area = input_area;

    let button_areas = if let Some((_, name, _)) = &state.pending_approval {
        draw_approval_footer(frame, footer_row_1, footer_row_2, name, mouse_pos)
    } else {
        draw_normal_footer(frame, footer_row_1, footer_row_2, state, hint);
        None
    };

    if state.show_help {
        draw_help_popup(frame);
    }

    // Dismiss any toast whose display window has elapsed, then render it.
    state.clear_expired_toast();
    if let Some((message, _)) = state.toast.clone() {
        draw_toast(frame, area, &message);
    }

    button_areas
}

fn draw_chat(frame: &mut Frame, area: Rect, state: &mut TuiState) {
    // Split horizontally first so all widths are derived from content_area —
    // this ensures trailing-pad spans reach the right edge of content_area
    // and leave no bare cells that cause rendering artifacts.
    let [content_area, scrollbar_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(1)])
        .areas(area);

    // Save the content rect so the event loop can hit-test mouse events.
    state.chat_area = content_area;

    // row_width is the full column count of content_area; bubble helpers pad
    // lines to this width so the background fills every cell on each row.
    let row_width = content_area.width as usize;
    // wrap_width for plain (non-bubble) content — 1 char narrower to avoid
    // running flush against the right edge.
    let wrap_width = row_width.saturating_sub(1).max(1);

    let mut lines: Vec<Line> = Vec::new();
    // Queued (steer) user messages are deferred until after the in-progress
    // stream bubble so they appear visually after the current response.
    let mut deferred_queued_messages: Vec<String> = Vec::new();
    // The logo wordmark is always the first element of the chat list.
    append_logo(&mut lines);
    for entry in &state.chat_entries {
        match entry {
            ChatEntry::UserMessage {
                text,
                state: UserMessageState::Queued,
            } => {
                // Defer queued steer bubbles; they will be appended after the
                // in-progress streaming section below.
                deferred_queued_messages.push(text.clone());
                // Skip the blank separator here; it is added when we flush.
                continue;
            }
            ChatEntry::UserMessage { text, state } => {
                // User bubbles carry a 1-char left border bar plus a 2-char
                // margin, so wrap to a narrower width to keep lines on-screen.
                let user_width = row_width.saturating_sub(3).max(1);
                append_user_lines(&mut lines, text, *state, user_width, row_width);
            }
            ChatEntry::AssistantMessage { text, interrupted } => {
                let msg_width = row_width.saturating_sub(3).max(1);
                append_assistant_lines(&mut lines, text, *interrupted, msg_width, row_width);
            }
            ChatEntry::ToolBubble { name, state, .. } => {
                append_tool_bubble_lines(&mut lines, name, state, wrap_width, row_width);
            }
            ChatEntry::ErrorLine { message } => {
                for wrapped in textwrap::wrap(message, wrap_width) {
                    lines.push(Line::styled(
                        wrapped.into_owned(),
                        Style::default().fg(Color::Red),
                    ));
                }
            }
        }
        // Blank separator between entries so the conversation breathes.
        lines.push(Line::default());
    }

    if state.agent_mode == AgentMode::Streaming && !state.partial_assistant_text.is_empty() {
        let stream_width = row_width.saturating_sub(3).max(1);
        append_streaming_lines(
            &mut lines,
            &state.partial_assistant_text.clone(),
            stream_width,
            row_width,
        );
        // Match the blank separator that the committed-entry loop appends so
        // the gap between the bubble and the input field is consistent whether
        // streaming is in progress or finished.
        lines.push(Line::default());
    }

    // Flush deferred queued steer messages after the stream bubble so they
    // appear visually after the current in-progress response.
    for queued_text in &deferred_queued_messages {
        let user_width = row_width.saturating_sub(3).max(1);
        append_user_lines(
            &mut lines,
            queued_text,
            UserMessageState::Queued,
            user_width,
            row_width,
        );
        lines.push(Line::default());
    }

    // Extract plain text of every line for selection/copy support.
    state.chat_rendered_lines = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();

    // Apply selection highlight to lines in the selected range.
    const CHAT_SELECTION_BG: Color = Color::Rgb(60, 80, 120);
    if let Some(sel) = state.chat_selection {
        let (start_line, _, end_line, _) = sel.normalized();
        for (index, line) in lines.iter_mut().enumerate() {
            if index >= start_line && index <= end_line {
                for span in line.spans.iter_mut() {
                    span.style = span.style.patch(Style::default().bg(CHAT_SELECTION_BG));
                }
            }
        }
    }

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
    let row_offset = if state.scroll_locked {
        // Keep scroll_offset in sync with the rendered bottom so that the
        // first scroll_up(1) call starts from the actual bottom position
        // rather than 0, which would otherwise cause the view to jump to
        // the top on the very first mouse-wheel or PageUp event.
        state.scroll_offset = max_offset;
        max_offset
    } else {
        // Clamp the manual offset to the content height so scrolling past the
        // end (or up past the top) cannot leave the offset silently drifting.
        state.scroll_offset = state.scroll_offset.min(max_offset);
        // Scrolling back down to the very bottom re-enables auto-scroll.
        if state.scroll_offset >= max_offset {
            state.scroll_locked = true;
        }
        state.scroll_offset
    };

    let paragraph = Paragraph::new(lines.clone()).scroll((row_offset as u16, 0));

    frame.render_widget(paragraph, content_area);

    // Show a scrollbar only when the content actually overflows the viewport.
    if total_lines > visible {
        // `content_length` must be `max_offset + 1` (the number of scrollable
        // positions), not the total line count, or ratatui's thumb proportion
        // never reaches the very bottom of the track when the viewport is more
        // than one row.
        let mut scrollbar_state = ScrollbarState::new(max_offset + 1)
            .position(row_offset)
            .viewport_content_length(visible);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            scrollbar_area,
            &mut scrollbar_state,
        );
    }
}

/// Render the Ylva wordmark, horizontally centered, as the leading chat lines.
fn append_logo(lines: &mut Vec<Line>) {
    let style = Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::BOLD);
    for row in LOGO {
        lines.push(Line::styled(row.to_string(), style).alignment(Alignment::Center));
    }
    // A blank spacer separates the wordmark from the conversation.
    lines.push(Line::default());
}

/// Render a user message as wrapped lines with a colored left bar and soft
/// background: a 1-char top/bottom pad and a 2-char margin between the bar
/// and the text. Cancelled messages get a dimmed "Not sent." line.
///
/// `width` is the maximum text column count (after bar + margin).
/// Render a committed assistant message as a blue-bordered bubble.
/// Interrupted messages use a `DarkGray` border and append a "…" suffix.
fn append_assistant_lines(
    lines: &mut Vec<Line>,
    text: &str,
    interrupted: bool,
    width: usize,
    row_width: usize,
) {
    let border_color = if interrupted {
        Color::DarkGray
    } else {
        ASSISTANT_STREAM_BORDER_COLOR
    };
    let bar = Span::styled(
        "▌",
        Style::default()
            .fg(border_color)
            .bg(ASSISTANT_STREAM_BG_COLOR),
    );
    let margin = Span::styled("  ", Style::default().bg(ASSISTANT_STREAM_BG_COLOR));
    let text_style = if interrupted {
        Style::default()
            .fg(Color::DarkGray)
            .bg(ASSISTANT_STREAM_BG_COLOR)
    } else {
        Style::default().bg(ASSISTANT_STREAM_BG_COLOR)
    };
    let row_bg = Style::default().bg(ASSISTANT_STREAM_BG_COLOR);
    let trailing_pad = |text_len: usize| -> Span<'static> {
        let used = 3 + text_len;
        let pad = row_width.saturating_sub(used);
        Span::styled(
            " ".repeat(pad),
            Style::default().bg(ASSISTANT_STREAM_BG_COLOR),
        )
    };

    lines.push(Line::from(vec![bar.clone(), margin.clone(), trailing_pad(0)]).patch_style(row_bg));

    let mut wrapped = textwrap::wrap(text, width);
    if wrapped.is_empty() {
        wrapped.push(std::borrow::Cow::Borrowed(""));
    }
    for line in wrapped.into_iter() {
        let line_str = line.into_owned();
        let line_len = line_str.chars().count();
        let content = Span::styled(line_str, text_style);
        lines.push(
            Line::from(vec![
                bar.clone(),
                margin.clone(),
                content,
                trailing_pad(line_len),
            ])
            .patch_style(row_bg),
        );
    }
    if interrupted {
        let ellipsis = "…";
        lines.push(
            Line::from(vec![
                bar.clone(),
                margin.clone(),
                Span::styled(
                    ellipsis,
                    Style::default()
                        .fg(Color::DarkGray)
                        .bg(ASSISTANT_STREAM_BG_COLOR),
                ),
                trailing_pad(ellipsis.len()),
            ])
            .patch_style(row_bg),
        );
    }

    lines.push(Line::from(vec![bar, margin, trailing_pad(0)]).patch_style(row_bg));
}

/// `row_width` is the full content-area column count; every row is padded with
/// Render in-progress assistant text as a blue-bordered bubble with a blinking
/// cursor on the last line. Mirrors the layout of `append_user_lines`.
fn append_streaming_lines(lines: &mut Vec<Line>, text: &str, width: usize, row_width: usize) {
    let bar = Span::styled(
        "▌",
        Style::default()
            .fg(ASSISTANT_STREAM_BORDER_COLOR)
            .bg(ASSISTANT_STREAM_BG_COLOR),
    );
    let margin = Span::styled("  ", Style::default().bg(ASSISTANT_STREAM_BG_COLOR));
    let text_style = Style::default().bg(ASSISTANT_STREAM_BG_COLOR);
    let row_bg = Style::default().bg(ASSISTANT_STREAM_BG_COLOR);
    let trailing_pad = |text_len: usize| -> Span<'static> {
        let used = 3 + text_len;
        let pad = row_width.saturating_sub(used);
        Span::styled(
            " ".repeat(pad),
            Style::default().bg(ASSISTANT_STREAM_BG_COLOR),
        )
    };

    // Top padding.
    lines.push(Line::from(vec![bar.clone(), margin.clone(), trailing_pad(0)]).patch_style(row_bg));

    let mut wrapped = textwrap::wrap(text, width);
    if wrapped.is_empty() {
        wrapped.push(std::borrow::Cow::Borrowed(""));
    }
    let last_index = wrapped.len().saturating_sub(1);
    for (i, line) in wrapped.into_iter().enumerate() {
        let line_str = line.into_owned();
        let line_len = line_str.chars().count();
        let content = Span::styled(line_str, text_style);
        if i == last_index {
            // Append blinking cursor on the last content line.
            let cursor = Span::styled(
                "▊",
                Style::default()
                    .fg(ASSISTANT_STREAM_BORDER_COLOR)
                    .bg(ASSISTANT_STREAM_BG_COLOR)
                    .add_modifier(Modifier::BOLD),
            );
            let cursor_len = 1;
            lines.push(
                Line::from(vec![
                    bar.clone(),
                    margin.clone(),
                    content,
                    cursor,
                    trailing_pad(line_len + cursor_len),
                ])
                .patch_style(row_bg),
            );
        } else {
            lines.push(
                Line::from(vec![
                    bar.clone(),
                    margin.clone(),
                    content,
                    trailing_pad(line_len),
                ])
                .patch_style(row_bg),
            );
        }
    }

    // Bottom padding.
    lines.push(Line::from(vec![bar, margin, trailing_pad(0)]).patch_style(row_bg));
}

/// trailing spaces to that width so the `USER_MESSAGE_BG_COLOR` background
/// fills the entire terminal row — not just the cells that contain characters.
fn append_user_lines(
    lines: &mut Vec<Line>,
    text: &str,
    state: UserMessageState,
    width: usize,
    row_width: usize,
) {
    let (border_color, dim) = match state {
        UserMessageState::Sent => (USER_MESSAGE_BORDER_COLOR, false),
        UserMessageState::Queued => (USER_MESSAGE_QUEUED_BORDER_COLOR, true),
        UserMessageState::Steered => (USER_MESSAGE_STEERED_BORDER_COLOR, false),
        UserMessageState::Cancelled => (USER_MESSAGE_CANCELLED_BORDER_COLOR, true),
    };
    let bar = Span::styled(
        "▌",
        Style::default().fg(border_color).bg(USER_MESSAGE_BG_COLOR),
    );
    // 2-char margin between the left border and the content.
    let margin = Span::styled("  ", Style::default().bg(USER_MESSAGE_BG_COLOR));
    let text_style = if dim {
        Style::default()
            .bg(USER_MESSAGE_BG_COLOR)
            .add_modifier(Modifier::DIM)
    } else {
        Style::default().bg(USER_MESSAGE_BG_COLOR)
    };
    // Line-level background fills the rest of the row after the spans.
    let row_bg = Style::default().bg(USER_MESSAGE_BG_COLOR);
    // Trailing pad fills cells after the last span so the background reaches
    // the right edge of the content area. bar(1) + margin(2) = 3 prefix cols.
    let trailing_pad = |text_len: usize| -> Span<'static> {
        let used = 3 + text_len;
        let pad = row_width.saturating_sub(used);
        Span::styled(" ".repeat(pad), Style::default().bg(USER_MESSAGE_BG_COLOR))
    };

    // 1-char top padding keeps the bubble from crowding the entry above.
    lines.push(Line::from(vec![bar.clone(), margin.clone(), trailing_pad(0)]).patch_style(row_bg));

    let mut wrapped = textwrap::wrap(text, width);
    if wrapped.is_empty() {
        wrapped.push(std::borrow::Cow::Borrowed(""));
    }
    for line in wrapped.into_iter() {
        let line_str = line.into_owned();
        let line_len = line_str.chars().count();
        let content = Span::styled(line_str, text_style);
        lines.push(
            Line::from(vec![
                bar.clone(),
                margin.clone(),
                content,
                trailing_pad(line_len),
            ])
            .patch_style(row_bg),
        );
    }
    if state == UserMessageState::Cancelled {
        let label = "Not sent.";
        let label_len = label.len();
        lines.push(
            Line::from(vec![
                bar.clone(),
                margin.clone(),
                Span::styled(
                    label,
                    Style::default()
                        .fg(Color::DarkGray)
                        .bg(USER_MESSAGE_BG_COLOR)
                        .add_modifier(Modifier::DIM),
                ),
                trailing_pad(label_len),
            ])
            .patch_style(row_bg),
        );
    }
    // 1-char bottom padding separates the bubble from the entry below.
    lines.push(Line::from(vec![bar, margin, trailing_pad(0)]).patch_style(row_bg));
}

fn append_tool_bubble_lines(
    lines: &mut Vec<Line>,
    name: &str,
    state: &ToolBubbleState,
    wrap_width: usize,
    row_width: usize,
) {
    let bar = Span::styled(
        "▌",
        Style::default()
            .fg(TOOL_BUBBLE_BORDER_COLOR)
            .bg(TOOL_BUBBLE_BG_COLOR),
    );
    let margin = Span::styled("  ", Style::default().bg(TOOL_BUBBLE_BG_COLOR));
    let row_bg = Style::default().bg(TOOL_BUBBLE_BG_COLOR);
    let trailing_pad = |text_len: usize| -> Span<'static> {
        let used = 3 + text_len;
        let pad = row_width.saturating_sub(used);
        Span::styled(" ".repeat(pad), Style::default().bg(TOOL_BUBBLE_BG_COLOR))
    };

    // Top padding.
    lines.push(Line::from(vec![bar.clone(), margin.clone(), trailing_pad(0)]).patch_style(row_bg));

    // Title line showing state and tool name.
    let title = match state {
        ToolBubbleState::Pending { .. } => format!("[tool pending] {name}"),
        ToolBubbleState::Running { .. } => format!("[tool] {name}"),
        ToolBubbleState::Done { .. } => format!("[tool done] {name}"),
        ToolBubbleState::Error { .. } => format!("[tool error] {name}"),
        ToolBubbleState::Killed => format!("[tool killed] {name}"),
        ToolBubbleState::Denied { .. } => format!("[tool denied] {name}"),
    };
    let title_len = title.chars().count();
    lines.push(
        Line::from(vec![
            bar.clone(),
            margin.clone(),
            Span::styled(
                title,
                Style::default()
                    .fg(TOOL_BUBBLE_BORDER_COLOR)
                    .bg(TOOL_BUBBLE_BG_COLOR),
            ),
            trailing_pad(title_len),
        ])
        .patch_style(row_bg),
    );

    // Arguments line for states that carry them.
    let maybe_args = match state {
        ToolBubbleState::Pending { arguments }
        | ToolBubbleState::Running { arguments }
        | ToolBubbleState::Done { arguments, .. } => Some(arguments),
        _ => None,
    };
    if let Some(arguments) = maybe_args {
        let args_text = serde_json::to_string(arguments).unwrap_or_default();
        let truncated: String = args_text.chars().take(wrap_width).collect();
        let truncated_len = truncated.chars().count();
        lines.push(
            Line::from(vec![
                bar.clone(),
                margin.clone(),
                Span::styled(
                    truncated,
                    Style::default()
                        .fg(Color::DarkGray)
                        .bg(TOOL_BUBBLE_BG_COLOR),
                ),
                trailing_pad(truncated_len),
            ])
            .patch_style(row_bg),
        );
    }

    // Result for Done state.
    if let ToolBubbleState::Done { result, .. } = state {
        for wrapped_line in textwrap::wrap(result, wrap_width) {
            let line_str = wrapped_line.into_owned();
            let line_len = line_str.chars().count();
            lines.push(
                Line::from(vec![
                    bar.clone(),
                    margin.clone(),
                    Span::styled(line_str, Style::default().bg(TOOL_BUBBLE_BG_COLOR)),
                    trailing_pad(line_len),
                ])
                .patch_style(row_bg),
            );
        }
    }

    // Error message for Error or Denied state.
    let maybe_error = match state {
        ToolBubbleState::Error { error } | ToolBubbleState::Denied { error } => Some(error),
        _ => None,
    };
    if let Some(error) = maybe_error {
        for wrapped_line in textwrap::wrap(error, wrap_width) {
            let line_str = wrapped_line.into_owned();
            let line_len = line_str.chars().count();
            lines.push(
                Line::from(vec![
                    bar.clone(),
                    margin.clone(),
                    Span::styled(
                        line_str,
                        Style::default().fg(Color::Red).bg(TOOL_BUBBLE_BG_COLOR),
                    ),
                    trailing_pad(line_len),
                ])
                .patch_style(row_bg),
            );
        }
    }

    // Bottom padding.
    lines.push(Line::from(vec![bar, margin, trailing_pad(0)]).patch_style(row_bg));
}

fn draw_normal_footer(
    frame: &mut Frame,
    row_1: Rect,
    row_2: Rect,
    state: &TuiState,
    hint: Option<&str>,
) {
    let mode_word = match state.agent_mode {
        AgentMode::Idle => "Idle",
        AgentMode::Pending => "Sending…",
        AgentMode::Streaming => "Streaming",
        AgentMode::ToolPending => "ToolPending",
    };
    let spinner_prefix = if state.agent_mode != AgentMode::Idle {
        let index = state.spinner_frame as usize % SPINNER_FRAMES.len();
        format!("{} ", SPINNER_FRAMES[index])
    } else {
        String::new()
    };
    let row_1_text = if let Some(hint) = hint {
        hint.to_string()
    } else if state.steer_indicator {
        format!(" {spinner_prefix}[mode] {mode_word}  ⋮ 1 steer queued")
    } else {
        format!(" {spinner_prefix}[mode] {mode_word}")
    };
    frame.render_widget(
        Paragraph::new(Line::styled(row_1_text, Style::default().fg(Color::Yellow))),
        row_1,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            " enter: send  ctrl-c/esc: cancel/quit  F1: help",
            Style::default().fg(Color::DarkGray),
        )),
        row_2,
    );
}
/// Returns the button `Rect`s for mouse hit-testing.
fn draw_approval_footer(
    frame: &mut Frame,
    row_1: Rect,
    row_2: Rect,
    tool_name: &str,
    mouse_pos: Option<(u16, u16)>,
) -> Option<ButtonAreas> {
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(" Tool '{tool_name}' requests your approval:"),
            Style::default().fg(Color::Yellow),
        )),
        row_1,
    );

    let approve_label = " [ ✓ Approve (Enter) ] ";
    let deny_label = " [ ✗ Deny (Esc) ] ";
    let approve_width = approve_label.chars().count() as u16;
    let deny_width = deny_label.chars().count() as u16;

    let [approve_area, _, deny_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(approve_width),
            Constraint::Length(2),
            Constraint::Length(deny_width),
        ])
        .areas(row_2);

    let pointer = mouse_pos.map(|(col, row)| ratatui::layout::Position::new(col, row));
    let approve_hovered = pointer.is_some_and(|pos| approve_area.contains(pos));
    let deny_hovered = pointer.is_some_and(|pos| deny_area.contains(pos));

    let approve_style = if approve_hovered {
        Style::default()
            .fg(Color::Black)
            .bg(Color::LightGreen)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Black).bg(Color::Green)
    };
    let deny_style = if deny_hovered {
        Style::default()
            .fg(Color::Black)
            .bg(Color::LightRed)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Black).bg(Color::Red)
    };

    frame.render_widget(
        Paragraph::new(Line::styled(approve_label, approve_style)).alignment(Alignment::Left),
        approve_area,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(deny_label, deny_style)).alignment(Alignment::Left),
        deny_area,
    );

    Some(ButtonAreas {
        approve: approve_area,
        deny: deny_area,
    })
}

/// Render a transient notification in the bottom-centre of the screen.
/// The popup is 3 rows tall (border + text + border) and wide enough for the
/// message plus 4 chars of horizontal padding.
fn draw_toast(frame: &mut Frame, area: Rect, message: &str) {
    let content = format!(" ✓ {message} ");
    let popup_width = (content.chars().count() as u16 + 2).min(area.width);
    let popup_height = 3u16;
    let x = area.x + area.width.saturating_sub(popup_width) / 2;
    // Position above the two footer rows.
    let y = area.y + area.height.saturating_sub(popup_height + 2);
    let popup = Rect::new(x, y, popup_width, popup_height);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Black).bg(Color::Green));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(Line::styled(
            content,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        inner,
    );
}

fn draw_help_popup(frame: &mut Frame) {
    let area = frame.area();
    let popup_width = (area.width * 60 / 100).max(20);
    let popup_height = (area.height * 70 / 100).max(10);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup = Rect::new(x, y, popup_width, popup_height);

    frame.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title("Help — Ylva");
    let inner = block.inner(popup);
    // Draw the border so the popup is visually distinct from the content it
    // overlays (Clear erases what is underneath).
    frame.render_widget(block.clone(), popup);

    let keybindings = "\
Enter                       Send message; queue steer if busy
Shift+Enter / Alt+Enter     Insert newline
Ctrl+C / Esc (x1)           Show \"Press again to cancel\" hint (while streaming)
Ctrl+C / Esc (x2, <=2s)     Interrupt generation
Ctrl+C (x1)                 Show \"Press again to quit\" hint (idle)
Ctrl+C (x2, <=2s)           Quit TUI
Esc                         Clear input (idle)
Esc                         Close popup
F1                          Toggle this help
Up / Down                   Input history (on boundary lines)
Home / Fn+Left              Line start; at col 0: end of previous line
End / Fn+Right              Line end; at line end: start of next line
PageUp / PageDown           Scroll chat
Mouse wheel                 Scroll chat
Enter                       Approve tool (approval mode)
Esc                         Deny tool (approval mode)";

    let build_info = BUILD_INFO.render();

    let [keybind_area, build_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Fill(1), Constraint::Length(6)])
        .areas(inner);

    let kb_title = Block::default().title(" Keybindings ");
    frame.render_widget(
        Paragraph::new(keybindings).block(kb_title).scroll((0, 0)),
        keybind_area,
    );
    let bi_title = Block::default().title(" Build ");
    frame.render_widget(
        Paragraph::new(build_info).block(bi_title).scroll((0, 0)),
        build_area,
    );

    frame.render_widget(Paragraph::new("F1 or Esc to close"), popup);
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget};

    /// `content_length` must be `max_offset + 1` (the number of scrollable
    /// positions), not the total line count, or the thumb never reaches the
    /// bottom of the track when the viewport spans more than one row.
    #[test]
    fn vertical_scrollbar_thumb_reaches_bottom_at_max_offset() {
        let total = 30usize;
        let visible = 10usize;
        let max_offset = total - visible;
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, visible as u16));
        let mut state = ScrollbarState::new(max_offset + 1)
            .position(max_offset)
            .viewport_content_length(visible);
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_symbol("█")
            .track_symbol(Some("│"))
            .begin_symbol(None)
            .end_symbol(None)
            .render(buffer.area, &mut buffer, &mut state);
        assert_eq!(
            buffer[(0, visible as u16 - 1)].symbol(),
            "█",
            "the thumb must sit on the last row of the track at max offset"
        );
    }

    #[test]
    fn vertical_scrollbar_thumb_is_at_top_at_zero_offset() {
        let total = 30usize;
        let visible = 10usize;
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, visible as u16));
        let mut state = ScrollbarState::new(total - visible + 1)
            .position(0)
            .viewport_content_length(visible);
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_symbol("█")
            .track_symbol(Some("│"))
            .begin_symbol(None)
            .end_symbol(None)
            .render(buffer.area, &mut buffer, &mut state);
        assert_eq!(buffer[(0, 0)].symbol(), "█");
    }
}
