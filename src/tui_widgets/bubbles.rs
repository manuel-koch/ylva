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

//! Shared message bubble chrome and prose rendering.

use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::markdown::render_markdown;
use crate::theme::{
    assistant_stream_border_color, interrupted_color, thinking_border_color,
    user_message_border_color, user_message_cancelled_border_color,
    user_message_queued_border_color, user_message_steered_border_color,
};
use crate::tui_reducer_state::UserMessageState;

/// Common visual scaffolding shared by every chat bubble renderer
/// (assistant, user, streaming-assistant, and tool bubbles): a colored
/// left-border bar and a 2-space margin between the bar and content.
/// Bubbles are rendered with no background fill and no top/bottom padding
/// row, so the conversation reads as a condensed, neutral list of entries
/// distinguished only by their left-border color.
pub(super) struct BubbleStyle {
    bar: Span<'static>,
    margin: Span<'static>,
}

impl BubbleStyle {
    pub(super) fn prefix_spans(&self) -> [Span<'static>; 2] {
        [self.bar.clone(), self.margin.clone()]
    }

    pub(super) fn new(border_color: Color) -> Self {
        Self {
            bar: Span::styled("▌", Style::default().fg(border_color)),
            margin: Span::raw("  "),
        }
    }

    /// Pushes a blank bar-only row, used as a separator between distinct
    /// sections within a bubble's body (e.g. between a tool call's arguments
    /// and its result). `extra_style` is patched the same way as
    /// [`BubbleStyle::content_line`] so separator rows stay visually
    /// consistent with the bubble's content rows (e.g. both dimmed for a
    /// queued/cancelled message).
    pub(super) fn push_separator_line(&self, lines: &mut Vec<Line<'static>>, extra_style: Style) {
        lines
            .push(Line::from(vec![self.bar.clone(), self.margin.clone()]).patch_style(extra_style));
    }

    /// Assembles one content row: bar + margin + `spans`. `extra_style` is
    /// patched onto the assembled line so it can override
    /// foreground/modifiers (e.g. dim for queued/cancelled messages) while
    /// preserving each span's own inline styling (bold, inline code, ...),
    /// since `patch_style` only overrides unset style attributes.
    pub(super) fn content_line(
        &self,
        spans: Vec<Span<'static>>,
        extra_style: Style,
    ) -> Line<'static> {
        let mut full_spans = vec![self.bar.clone(), self.margin.clone()];
        full_spans.extend(spans);
        Line::from(full_spans).patch_style(extra_style)
    }

    /// Renders `text` as Markdown wrapped to `width`, pushing one
    /// [`BubbleStyle::content_line`] per resulting line with `extra_style`
    /// applied to each.
    fn push_markdown_lines(
        &self,
        lines: &mut Vec<Line<'static>>,
        text: &str,
        width: usize,
        extra_style: Style,
    ) {
        let mut wrapped = render_markdown(text, width);
        if wrapped.is_empty() {
            wrapped.push(Line::from(""));
        }
        for md_line in wrapped {
            lines.push(self.content_line(md_line.spans, extra_style));
        }
    }
}

/// Render a committed assistant message as a blue-bordered bubble.
/// Interrupted messages use a `DarkGray` border and append a "…" suffix.
pub(super) fn append_assistant_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    interrupted: bool,
    width: usize,
) {
    let border_color = if interrupted {
        interrupted_color()
    } else {
        assistant_stream_border_color()
    };
    let style = BubbleStyle::new(border_color);
    let text_style = if interrupted {
        Style::default().fg(interrupted_color())
    } else {
        Style::default()
    };

    style.push_markdown_lines(lines, text, width, text_style);
    if interrupted {
        let ellipsis = "…";
        lines.push(style.content_line(
            vec![Span::styled(
                ellipsis,
                Style::default().fg(interrupted_color()),
            )],
            Style::default(),
        ));
    }
}

/// Render in-progress assistant text as a blue-bordered bubble with a blinking
/// cursor on the last line. Mirrors the layout of `append_user_lines`.
pub(super) fn append_streaming_lines(lines: &mut Vec<Line<'static>>, text: &str, width: usize) {
    let style = BubbleStyle::new(assistant_stream_border_color());

    let mut wrapped = render_markdown(text, width);
    if wrapped.is_empty() {
        wrapped.push(Line::from(""));
    }
    let last_index = wrapped.len().saturating_sub(1);
    for (i, md_line) in wrapped.into_iter().enumerate() {
        let mut spans = md_line.spans;
        if i == last_index {
            // Append blinking cursor on the last content line.
            spans.push(Span::styled(
                "▊",
                Style::default()
                    .fg(assistant_stream_border_color())
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(style.content_line(spans, Style::default()));
    }
}

/// Render a committed thinking bubble (extended-thinking/reasoning output),
/// distinct from the assistant reply. When `collapsed`, renders only a single
/// summary line `▶ Thought for {duration}`; when expanded, the content is
/// rendered via `render_markdown` under a `▼ Thought for {duration}` title
/// line. `duration` is the wall-clock time spent thinking.
///
/// Records the bubble's absolute row range in `thinking_title_rows` (keyed by
/// `entry_index`, since `ChatEntry::ThinkingMessage` carries no `Uuid`) so
/// mouse clicks can route to `AppState::toggle_thinking_collapsed`.
pub(super) fn append_thinking_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    collapsed: bool,
    duration: std::time::Duration,
    entry_index: usize,
    thinking_title_rows: &mut Vec<(usize, usize, usize)>,
    width: usize,
) {
    let style = BubbleStyle::new(thinking_border_color());
    let dim = Style::default().add_modifier(Modifier::DIM);
    let title_text = format!("Thought for {:.3}s", duration.as_secs_f64());

    let bubble_start_row = lines.len();

    if collapsed {
        let summary = format!("▶ {title_text}");
        lines.push(style.content_line(vec![Span::styled(summary, dim)], dim));
    } else {
        // Body lines are indented two spaces, matching the "▼ " title prefix
        // width, so they read as nested under the title rather than flush
        // with the bubble's left border. Wrap narrower to account for it.
        let body_width = width.saturating_sub(2).max(1);
        let mut wrapped = render_markdown(text, body_width);
        if wrapped.is_empty() {
            wrapped.push(Line::from(""));
        }
        let mut first = true;
        for md_line in wrapped {
            let mut spans: Vec<Span<'static>> = Vec::new();
            if first {
                spans.push(Span::styled("▼ ", dim));
                spans.push(Span::styled(title_text.clone(), dim));
                lines.push(style.content_line(spans, dim));
                first = false;
            }
            spans = vec![Span::styled("  ", dim)];
            spans.extend(
                md_line
                    .spans
                    .into_iter()
                    .map(|span| Span::styled(span.content, dim.patch(span.style))),
            );
            lines.push(style.content_line(spans, dim));
        }
    }

    let bubble_end_row = lines.len().saturating_sub(1);
    thinking_title_rows.push((bubble_start_row, bubble_end_row, entry_index));
}

/// Render in-progress thinking text as a cyan-bordered bubble with a blinking
/// cursor, mirroring `append_streaming_lines`. Honors `collapsed` (mirrored
/// from `AppState::partial_thinking_collapsed`) so the bubble can be toggled
/// before it is flushed into a `ChatEntry::ThinkingMessage` — clicking its
/// title routes through `thinking_title_rows`, keyed by the
/// `STREAMING_THINKING_ENTRY_INDEX` sentinel since there is no real
/// `chat_entries` index yet. The title shows elapsed whole seconds computed
/// from `thinking_started_at`; subsecond redraws leave the readout unchanged.
pub(super) fn append_streaming_thinking_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    thinking_started_at: Option<Instant>,
    collapsed: bool,
    thinking_title_rows: &mut Vec<(usize, usize, usize)>,
    width: usize,
) {
    let style = BubbleStyle::new(thinking_border_color());
    let dim = Style::default().add_modifier(Modifier::DIM);
    let elapsed_seconds = thinking_started_at
        .map(|started_at| started_at.elapsed().as_secs())
        .unwrap_or_default();
    let title_text = format!("Thinking… ({elapsed_seconds}s)");

    let bubble_start_row = lines.len();

    if collapsed {
        let summary = format!("▶ {title_text}");
        lines.push(style.content_line(vec![Span::styled(summary, dim)], dim));
    } else {
        lines.push(style.content_line(
            vec![Span::styled("▼ ", dim), Span::styled(title_text, dim)],
            dim,
        ));

        // Body lines are indented two spaces to match the "▼ " title prefix,
        // mirroring `append_thinking_lines`'s flushed rendering.
        let body_width = width.saturating_sub(2).max(1);
        let mut wrapped = render_markdown(text, body_width);
        if wrapped.is_empty() {
            wrapped.push(Line::from(""));
        }
        let last_index = wrapped.len().saturating_sub(1);
        for (i, md_line) in wrapped.into_iter().enumerate() {
            let mut spans: Vec<Span<'static>> = vec![Span::styled("  ", dim)];
            spans.extend(
                md_line
                    .spans
                    .into_iter()
                    .map(|span| Span::styled(span.content, dim.patch(span.style))),
            );
            if i == last_index {
                spans.push(Span::styled(
                    "▊",
                    Style::default()
                        .fg(thinking_border_color())
                        .add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(style.content_line(spans, dim));
        }
    }

    let bubble_end_row = lines.len().saturating_sub(1);
    thinking_title_rows.push((
        bubble_start_row,
        bubble_end_row,
        crate::tui_reducer_state::STREAMING_THINKING_ENTRY_INDEX,
    ));
}

/// Render a user message with a colored left bar and Markdown content.
/// Cancelled messages append a dimmed "Not sent." line.
pub(super) fn append_user_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    state: UserMessageState,
    width: usize,
) {
    let (border_color, dim) = match state {
        UserMessageState::Sent => (user_message_border_color(), false),
        UserMessageState::Queued => (user_message_queued_border_color(), true),
        UserMessageState::Steered => (user_message_steered_border_color(), false),
        UserMessageState::Cancelled => (user_message_cancelled_border_color(), true),
    };
    let style = BubbleStyle::new(border_color);
    let text_style = if dim {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };

    style.push_markdown_lines(lines, text, width, text_style);

    if state == UserMessageState::Cancelled {
        let label = "Not sent.";
        lines.push(style.content_line(
            vec![Span::styled(
                label,
                Style::default()
                    .fg(interrupted_color())
                    .add_modifier(Modifier::DIM),
            )],
            Style::default(),
        ));
    }
}

#[cfg(test)]
mod tests;
