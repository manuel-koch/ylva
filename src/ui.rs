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
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
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
/// The user-message input field grows with content but is capped at this
/// fraction of the terminal height (e.g. 3 → one-third).  Adjust here to
/// change the maximum visible height of the input area.
const USER_MESSAGE_INPUT_HEIGHT_TERMINAL_FRACTION: u16 = 3;

// Assistant streaming bubble colors.
const ASSISTANT_STREAM_BORDER_COLOR: Color = Color::Blue;
const ASSISTANT_STREAM_BG_COLOR: Color = Color::Rgb(15, 20, 35);

// Tool bubble colors — magenta left border, dark magenta background tint.
const TOOL_BUBBLE_BORDER_COLOR: Color = Color::Magenta;
const TOOL_BUBBLE_BG_COLOR: Color = Color::Rgb(30, 0, 30);

// Help popup border, title, section headings, and scroll-indicator color.
const HELP_POPUP_BORDER_COLOR: Color = Color::Blue;
// Section heading foreground color inside the help popup content area.
const HELP_POPUP_HEADING_COLOR: Color = Color::Blue;

// Single hue used for all parts of the conversation-list scrollbar.
// The track and inactive arrows use the same colour with Modifier::DIM so the
// thumb and active arrows stand out without introducing a second colour.
const CHAT_SCROLLBAR_COLOR: Color = Color::Blue;
const SEPARATOR_LINE_COLOR: Color = Color::White;
const TOAST_BG_COLOR: Color = Color::Blue;
const TOAST_FG_COLOR: Color = Color::White;

/// Braille spinner animation frames, cycled while the agent is busy.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// ASCII-art "Ylva" wordmark shown while the conversation is empty so the
/// initial frame is never blank. Each line must be the same width.
const LOGO: [&str; 7] = [
    "██╗   ██╗██╗     ██╗   ██╗ █████╗ ",
    "╚██╗ ██╔╝██║     ██║   ██║██╔══██╗",
    " ╚████╔╝ ██║     ██║   ██║███████║",
    "  ╚██╔╝  ██║     ╚██╗ ██╔╝██╔══██║",
    "   ██║   ███████╗ ╚████╔╝ ██║  ██║",
    "   ╚═╝   ╚══════╝  ╚═══╝  ╚═╝  ╚═╝",
    "    Taming the pack of agents!    ",
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
    let max_input = (area.height / USER_MESSAGE_INPUT_HEIGHT_TERMINAL_FRACTION).max(3);
    let input_width = area.width.saturating_sub(1).max(1) as usize;
    let wrapped_rows = textarea
        .lines()
        .iter()
        .map(|line| textwrap::wrap(line, input_width).len().max(1))
        .sum::<usize>();
    let input_height = (wrapped_rows as u16).clamp(1, max_input);

    // When a tool approval is pending the approval title and buttons appear
    // ABOVE the input field so the approve/deny controls sit immediately
    // above where the user types a denial reason.
    //
    // The layout produces three variable slots (slot_a / slot_b / slot_c) that
    // are then mapped to named areas based on the current mode:
    //
    //   approval pending          │  normal mode
    //   ─────────────────────────────────────────
    //   slot_a → footer_row_1     │  slot_a → input_area
    //   slot_b → footer_row_2     │  slot_b → footer_row_1
    //   slot_c → input_area       │  slot_c → footer_row_2
    let approval_pending = state.pending_approval.is_some();
    let [chat_area, separator_area, slot_a, slot_b, slot_c] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(if approval_pending { 1 } else { input_height }),
            Constraint::Length(1),
            Constraint::Length(if approval_pending { input_height } else { 1 }),
        ])
        .areas(area);

    let (input_area, footer_row_1, footer_row_2) = if approval_pending {
        (slot_c, slot_a, slot_b)
    } else {
        (slot_a, slot_b, slot_c)
    };

    draw_chat(frame, chat_area, state);

    // Thin solid line separating the conversation area from the lower panel.
    let separator_line = "─".repeat(separator_area.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            separator_line,
            Style::default()
                .fg(SEPARATOR_LINE_COLOR)
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
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(USER_MESSAGE_BG_COLOR)),
    );
    frame.render_widget(&*textarea, input_area);

    // Mirror ratatui-textarea's next_scroll_top algorithm using the public
    // screen_cursor().row (wrapped screen row) to track which viewport window
    // the textarea is currently showing. This persists in TuiState across
    // frames so the state is correct regardless of cursor movement.
    if wrapped_rows > input_height as usize {
        let screen_row = textarea.screen_cursor().row;
        let height = input_height as usize;
        state.input_scroll_top = if screen_row < state.input_scroll_top {
            screen_row
        } else if state.input_scroll_top + height <= screen_row {
            screen_row + 1 - height
        } else {
            state.input_scroll_top
        };
    } else {
        state.input_scroll_top = 0;
    }

    // Show ↑/↓ based on the tracked viewport position, not the cursor row.
    if wrapped_rows > input_height as usize && input_area.height >= 1 && input_area.width >= 1 {
        let input_indicator_style = Style::default().fg(Color::Cyan);
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
                    .fg(Color::DarkGray)
                    .bg(USER_MESSAGE_BG_COLOR),
            )),
            placeholder_area,
        );
    }

    // Store the input rect for mouse click/drag handling in the event loop.
    state.input_area = input_area;

    let button_areas = if let Some((_, name, _)) = &state.pending_approval.clone() {
        draw_approval_footer(
            frame,
            footer_row_1,
            footer_row_2,
            name,
            input_is_empty,
            mouse_pos,
            hint,
        )
    } else {
        let keybinding_hint = if state.agent_mode != AgentMode::Idle && !input_is_empty {
            " enter: send  ctrl-c/esc: clear draft  F1: help"
        } else {
            " enter: send  ctrl-c/esc: cancel/quit  F1: help"
        };
        draw_normal_footer(
            frame,
            footer_row_1,
            footer_row_2,
            state,
            keybinding_hint,
            hint,
        );
        None
    };

    if state.show_help {
        draw_help_popup(frame, state);
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
    // bubble_row_width is 1 char narrower than row_width to leave a 1-char
    // margin between the right edge of each bubble and the scrollbar column.
    let bubble_row_width = row_width.saturating_sub(1);
    // wrap_width for plain (non-bubble) content — 1 char narrower to avoid
    // running flush against the right edge.
    let wrap_width = bubble_row_width.saturating_sub(1).max(1);

    let mut lines: Vec<Line> = Vec::new();
    // Queued (steer) user messages are deferred until after the in-progress
    // stream bubble so they appear visually after the current response.
    let mut deferred_queued_messages: Vec<String> = Vec::new();
    // Pending tool bubbles (awaiting approval) are deferred to the very end
    // of the conversation so they stay close to the approve/deny footer
    // regardless of how many earlier entries exist.
    let mut deferred_pending_tool_bubbles: Vec<(String, ToolBubbleState)> = Vec::new();
    // Show the logo wordmark only when the conversation is empty; once
    // the user has submitted a message the logo is hidden.
    if state.chat_entries.is_empty() {
        append_logo(&mut lines);
    } else {
        lines.push(Line::default());
    }
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
                let user_width = bubble_row_width.saturating_sub(3).max(1);
                append_user_lines(&mut lines, text, *state, user_width, bubble_row_width);
            }
            ChatEntry::AssistantMessage { text, interrupted } => {
                let msg_width = bubble_row_width.saturating_sub(3).max(1);
                append_assistant_lines(&mut lines, text, *interrupted, msg_width, bubble_row_width);
            }
            ChatEntry::ToolBubble { name, state, .. } => {
                // Pending tool bubbles are deferred so they render at the bottom,
                // close to the approve/deny footer buttons.
                if matches!(state, ToolBubbleState::Pending { .. }) {
                    deferred_pending_tool_bubbles.push((name.clone(), state.clone()));
                    continue;
                }
                append_tool_bubble_lines(&mut lines, name, state, wrap_width, bubble_row_width);
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
        let stream_width = bubble_row_width.saturating_sub(3).max(1);
        append_streaming_lines(
            &mut lines,
            &state.partial_assistant_text.clone(),
            stream_width,
            bubble_row_width,
        );
        // Match the blank separator that the committed-entry loop appends so
        // the gap between the bubble and the input field is consistent whether
        // streaming is in progress or finished.
        lines.push(Line::default());
    }

    // Flush deferred queued steer messages after the stream bubble so they
    // appear visually after the current in-progress response.
    for queued_text in &deferred_queued_messages {
        let user_width = bubble_row_width.saturating_sub(3).max(1);
        append_user_lines(
            &mut lines,
            queued_text,
            UserMessageState::Queued,
            user_width,
            bubble_row_width,
        );
        lines.push(Line::default());
    }

    // Flush pending-approval tool bubbles last so they always sit at the
    // bottom of the conversation, directly above the approve/deny footer.
    for (name, bubble_state) in &deferred_pending_tool_bubbles {
        append_tool_bubble_lines(&mut lines, name, bubble_state, wrap_width, bubble_row_width);
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
        if !sel.is_empty() {
            let (start_line, start_col, end_line, end_col) = sel.normalized();
            for (index, line) in lines.iter_mut().enumerate() {
                if index < start_line || index > end_line {
                    continue;
                }
                // Determine which column range to highlight on this line.
                // Middle lines are fully highlighted (0..MAX); the start and
                // end lines are highlighted only within the selection columns.
                let col_start = if index == start_line { start_col } else { 0 };
                let col_end = if index == end_line {
                    end_col
                } else {
                    usize::MAX
                };
                apply_selection_highlight_to_line(line, col_start, col_end, CHAT_SELECTION_BG);
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

    // Show a custom scrollbar only when the content overflows the viewport.
    if total_lines > visible {
        render_chat_scrollbar(
            frame,
            scrollbar_area,
            row_offset,
            max_offset,
            total_lines,
            visible,
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
fn render_chat_scrollbar(
    frame: &mut Frame,
    scrollbar_area: Rect,
    row_offset: usize,
    max_offset: usize,
    total_lines: usize,
    visible: usize,
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
            Style::default().fg(CHAT_SCROLLBAR_COLOR)
        } else {
            Style::default()
                .fg(CHAT_SCROLLBAR_COLOR)
                .add_modifier(Modifier::DIM)
        };
        let symbol = if is_thumb { "┃" } else { "│" };
        frame.render_widget(
            Paragraph::new(symbol).style(style),
            Rect::new(scrollbar_area.x, track_y + row as u16, 1, 1),
        );
    }

    if has_arrows {
        let arrow_active = Style::default().fg(CHAT_SCROLLBAR_COLOR);
        let arrow_dim = Style::default()
            .fg(CHAT_SCROLLBAR_COLOR)
            .add_modifier(Modifier::DIM);
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

/// Apply `background` color to character columns `[col_start, col_end)` on
/// `line`, splitting existing spans at the selection boundaries as needed.
/// Pass `col_end = usize::MAX` to highlight through to the end of the line.
fn apply_selection_highlight_to_line(
    line: &mut Line<'_>,
    col_start: usize,
    col_end: usize,
    background: Color,
) {
    if col_start >= col_end {
        return;
    }
    let original_spans: Vec<_> = std::mem::take(&mut line.spans);
    let mut new_spans: Vec<Span<'static>> = Vec::new();
    let mut char_pos = 0usize;

    for span in original_spans {
        let style = span.style;
        let text: String = span.content.into_owned();
        let span_char_len = text.chars().count();
        let span_end = char_pos + span_char_len;
        let effective_end = col_end.min(span_end);

        if span_end <= col_start || char_pos >= col_end {
            new_spans.push(Span::styled(text, style));
        } else {
            let overlap_start = col_start.saturating_sub(char_pos);
            let overlap_end = effective_end.saturating_sub(char_pos).min(span_char_len);

            if overlap_start > 0 {
                let before: String = text.chars().take(overlap_start).collect();
                new_spans.push(Span::styled(before, style));
            }
            let selected: String = text
                .chars()
                .skip(overlap_start)
                .take(overlap_end - overlap_start)
                .collect();
            if !selected.is_empty() {
                new_spans.push(Span::styled(
                    selected,
                    style.patch(Style::default().bg(background)),
                ));
            }
            if overlap_end < span_char_len {
                let after: String = text.chars().skip(overlap_end).collect();
                new_spans.push(Span::styled(after, style));
            }
        }
        char_pos = span_end;
    }
    line.spans = new_spans
        .into_iter()
        .map(|s| Span::styled(s.content, s.style))
        .collect();
}

/// Render the Ylva wordmark, horizontally centered. Only shown when the
/// conversation is still empty (no messages submitted yet).
fn append_logo(lines: &mut Vec<Line>) {
    lines.push(Line::default());
    lines.push(Line::default());
    let style = Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::BOLD);
    for row in LOGO {
        lines.push(Line::styled(row.to_string(), style).alignment(Alignment::Center));
    }
    // One blank line, then build-info centered below the wordmark.
    lines.push(Line::default());
    let build_info_text = format!("v{}  {}", BUILD_INFO.version, BUILD_INFO.git_commit);
    lines.push(
        Line::styled(build_info_text, Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center),
    );
    // A blank spacer separates the wordmark block from the conversation.
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
    keybinding_hint: &str,
    hint: Option<&str>,
) {
    let mode_word = match state.agent_mode {
        AgentMode::Idle => "Idle",
        AgentMode::Pending => "Sending…",
        AgentMode::Streaming => "Streaming…",
        AgentMode::ToolPending => "Running Tool…",
        AgentMode::Aborting => "Aborting…",
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
        format!(" {spinner_prefix}{mode_word}  ⋮ 1 steer queued")
    } else {
        format!(" {spinner_prefix}{mode_word}")
    };
    frame.render_widget(
        Paragraph::new(Line::styled(row_1_text, Style::default().fg(Color::Yellow))),
        row_1,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            keybinding_hint.to_string(),
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
    input_is_empty: bool,
    mouse_pos: Option<(u16, u16)>,
    hint: Option<&str>,
) -> Option<ButtonAreas> {
    let row_1_text = if let Some(hint_text) = hint {
        hint_text.to_string()
    } else {
        format!(" Tool '{tool_name}' requests your approval:")
    };
    frame.render_widget(
        Paragraph::new(Line::styled(row_1_text, Style::default().fg(Color::Yellow))),
        row_1,
    );

    // When the user is typing a denial reason, Enter will deny rather than
    // approve — reflect this by disabling the Approve button and adding
    // "(enter)" to the Deny button label.
    let approve_label = " [ ✓ Approve (enter) ] ";
    let deny_label = if input_is_empty {
        " [ ✗ Deny (esc) ] "
    } else {
        " [ ✗ Deny (enter/esc) ] "
    };
    let approve_width = approve_label.chars().count() as u16;
    let deny_width = deny_label.chars().count() as u16;

    // Indent buttons by 1 char so they align with the leading space of the
    // approval title above.
    let [_, buttons_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(1), Constraint::Fill(1)])
        .areas(row_2);
    let [approve_area, _, deny_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(approve_width),
            Constraint::Length(2),
            Constraint::Length(deny_width),
        ])
        .areas(buttons_area);

    let pointer = mouse_pos.map(|(col, row)| ratatui::layout::Position::new(col, row));
    let approve_hovered = pointer.is_some_and(|pos| approve_area.contains(pos));
    let deny_hovered = pointer.is_some_and(|pos| deny_area.contains(pos));

    // Approve button is disabled (greyed out, no hover) when the user has
    // typed a denial reason — pressing Enter at that point denies instead.
    let approve_style = if !input_is_empty {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM)
    } else if approve_hovered {
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
    let x = area.x + area.width.saturating_sub(popup_width + 1);
    let y = area.y + area.height.saturating_sub(popup_height + 1);
    let popup = Rect::new(x, y, popup_width, popup_height);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().fg(TOAST_FG_COLOR).bg(TOAST_BG_COLOR));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(Line::styled(
            content,
            Style::default()
                .fg(TOAST_FG_COLOR)
                .bg(TOAST_BG_COLOR)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        inner,
    );
}

fn draw_help_popup(frame: &mut Frame, state: &mut TuiState) {
    let area = frame.area();
    let popup_width = (area.width * 75 / 100).max(20);
    let popup_height = (area.height * 70 / 100).max(10);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup = Rect::new(x, y, popup_width, popup_height);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(HELP_POPUP_BORDER_COLOR))
        .title(" Help")
        .title_alignment(Alignment::Left)
        .padding(Padding::uniform(1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Build content lines grouped into sections. Each section starts with a
    // heading line followed by its entries; sections are separated by a blank.
    // No leading space on heading text — the popup padding provides the margin.
    let heading_style = Style::default()
        .fg(HELP_POPUP_HEADING_COLOR)
        .add_modifier(Modifier::BOLD);

    struct Section<'a> {
        heading: &'a str,
        entries: &'a [(&'a str, &'a str)],
    }

    let sections = [
        Section {
            heading: "General",
            entries: &[
                ("F1", "Toggle this help"),
                ("ctrl-c (x1)", "Show \"Press again to quit\" hint (idle)"),
                ("ctrl-c (x2, <=2s)", "Quit TUI"),
                (
                    "ctrl-c / esc (x1)",
                    "Show \"Press again to cancel\" hint (busy, empty input)",
                ),
                (
                    "ctrl-c / esc (x2, <=2s)",
                    "Interrupt generation (busy, empty input)",
                ),
                ("esc", "Close popup"),
            ],
        },
        Section {
            heading: "Draft message",
            entries: &[
                ("enter", "Send message; queue steer if busy"),
                ("shift-enter / alt-enter", "Insert newline"),
                ("ctrl-c / esc", "Clear input field (busy, non-empty input)"),
                ("esc", "Clear input (idle)"),
                ("↑ (on first line)", "Load previous message from history"),
                ("↓ (on last line)", "Load next message from history"),
                (
                    "home / fn-left",
                    "Line start; at col 0: end of previous line",
                ),
                (
                    "end / fn-right",
                    "Line end; at line end: start of next line",
                ),
            ],
        },
        Section {
            heading: "Help popup",
            entries: &[
                ("↑ / ↓", "Scroll up / down one line"),
                ("← / →", "Scroll left / right by 4 columns"),
                ("page-up / page-down", "Scroll up / down"),
                ("mouse wheel", "Scroll up / down"),
            ],
        },
        Section {
            heading: "Build",
            entries: &[],
        },
    ];

    let mut all_lines: Vec<Line> = Vec::new();
    for section in &sections {
        if !all_lines.is_empty() {
            all_lines.push(Line::default());
        }
        all_lines.push(Line::styled(section.heading, heading_style));
        if section.heading == "Build" {
            for bi_line in BUILD_INFO.render().lines() {
                all_lines.push(Line::from(bi_line.to_owned()));
            }
        } else {
            for (key, desc) in section.entries {
                all_lines.push(Line::from(format!("  {key:<40}{desc}")));
            }
        }
    }

    let total_lines = all_lines.len();
    let max_line_width = all_lines.iter().map(|l| l.width()).max().unwrap_or(0);

    let viewport_height = inner.height as usize;
    let viewport_width = inner.width as usize;

    // Clamp offsets so we never display empty space.
    let max_scroll_y = total_lines.saturating_sub(viewport_height);
    let max_scroll_x = max_line_width.saturating_sub(viewport_width);
    state.help_scroll_offset = state.help_scroll_offset.min(max_scroll_y);
    state.help_scroll_x = state.help_scroll_x.min(max_scroll_x);
    let scroll_y = state.help_scroll_offset;
    let scroll_x = state.help_scroll_x;

    frame.render_widget(
        Paragraph::new(all_lines).scroll((scroll_y as u16, scroll_x as u16)),
        inner,
    );

    // Scroll indicators sit in the 1-char padding strip between the border and
    // the content, so they never obscure text.
    //
    // Right padding column  = popup.x + popup.width  - 2
    // Left  padding column  = popup.x + 1
    // Top   padding row     = popup.y + 1
    // Bottom padding row    = popup.y + popup.height - 2
    let can_scroll_up = scroll_y > 0;
    let can_scroll_down = scroll_y < max_scroll_y;
    let can_scroll_left = scroll_x > 0;
    let can_scroll_right = scroll_x < max_scroll_x;
    let indicator_style = Style::default().fg(HELP_POPUP_BORDER_COLOR);

    let pad_col_right = popup.x + popup.width.saturating_sub(2);
    let pad_col_left = popup.x + 1;
    let pad_row_top = popup.y + 1;
    let pad_row_bottom = popup.y + popup.height.saturating_sub(2);
    let pad_row_mid = popup.y + popup.height / 2;

    if can_scroll_up || can_scroll_right {
        let symbol = match (can_scroll_up, can_scroll_right) {
            (true, true) => "↗",
            (true, false) => "↑",
            (false, true) => "→",
            (false, false) => unreachable!(),
        };
        frame.render_widget(
            Paragraph::new(symbol).style(indicator_style),
            Rect::new(pad_col_right, pad_row_top, 1, 1),
        );
    }
    if can_scroll_down {
        let symbol = if can_scroll_right { "↘" } else { "↓" };
        frame.render_widget(
            Paragraph::new(symbol).style(indicator_style),
            Rect::new(pad_col_right, pad_row_bottom, 1, 1),
        );
    }
    if can_scroll_left {
        frame.render_widget(
            Paragraph::new("←").style(indicator_style),
            Rect::new(pad_col_left, pad_row_mid, 1, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::apply_selection_highlight_to_line;
    use super::compute_scrollbar_thumb;
    use ratatui::style::Color;
    use ratatui::text::{Line, Span};

    const SEL_BG: Color = Color::Rgb(60, 80, 120);

    fn plain_line(text: &str) -> Line<'static> {
        Line::from(vec![Span::raw(text.to_owned())])
    }

    fn highlighted_in(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .filter(|s| s.style.bg == Some(SEL_BG))
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn selection_highlight_highlights_middle_chars_only() {
        let mut line = plain_line("Hello, world!");
        apply_selection_highlight_to_line(&mut line, 7, 12, SEL_BG);
        assert_eq!(highlighted_in(&line), "world");
    }

    #[test]
    fn selection_highlight_empty_range_leaves_line_unchanged() {
        let mut line = plain_line("Hello");
        apply_selection_highlight_to_line(&mut line, 3, 3, SEL_BG);
        assert!(
            line.spans.iter().all(|s| s.style.bg != Some(SEL_BG)),
            "no span should be highlighted for an empty range"
        );
    }

    #[test]
    fn selection_highlight_max_col_end_highlights_to_end_of_line() {
        let mut line = plain_line("Hello");
        apply_selection_highlight_to_line(&mut line, 2, usize::MAX, SEL_BG);
        assert_eq!(highlighted_in(&line), "llo");
    }

    #[test]
    fn selection_highlight_across_multiple_spans() {
        let mut line = Line::from(vec![
            Span::raw("foo".to_owned()),
            Span::raw("bar".to_owned()),
        ]);
        // Select "ob" spanning both spans: cols 2..4.
        apply_selection_highlight_to_line(&mut line, 2, 4, SEL_BG);
        assert_eq!(highlighted_in(&line), "ob");
    }

    #[test]
    fn scrollbar_thumb_sits_at_bottom_at_max_offset() {
        let total = 30usize;
        let visible = 10usize;
        let track_height = visible;
        let max_offset = total - visible;
        let (thumb_top, thumb_size) =
            compute_scrollbar_thumb(track_height, total, visible, max_offset, max_offset);
        assert_eq!(
            thumb_top + thumb_size,
            track_height,
            "thumb bottom must reach the last track row at max offset"
        );
    }

    #[test]
    fn scrollbar_thumb_sits_at_top_at_zero_offset() {
        let total = 30usize;
        let visible = 10usize;
        let track_height = visible;
        let max_offset = total - visible;
        let (thumb_top, _thumb_size) =
            compute_scrollbar_thumb(track_height, total, visible, 0, max_offset);
        assert_eq!(thumb_top, 0, "thumb must start at row 0 at zero offset");
    }

    #[test]
    fn scrollbar_thumb_never_overflows_track() {
        let total = 30usize;
        let visible = 10usize;
        let track_height = visible;
        let max_offset = total - visible;
        for offset in 0..=max_offset {
            let (thumb_top, thumb_size) =
                compute_scrollbar_thumb(track_height, total, visible, offset, max_offset);
            assert!(
                thumb_top + thumb_size <= track_height,
                "thumb overflows at offset {offset}: top={thumb_top} size={thumb_size}"
            );
        }
    }
}
