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

//! Terminal-column selection over rendered rows.

use ratatui::layout::Alignment;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Apply `background` color to terminal columns `[col_start, col_end)` on
/// `line`, splitting existing spans at the selection boundaries as needed.
/// Pass `col_end = usize::MAX` to highlight through to the end of the line.
pub(in crate::tui_widgets) fn apply_selection_highlight_to_line(
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
    let mut column = 0usize;

    for span in original_spans {
        let style = span.style;
        let text: String = span.content.into_owned();
        let text_span = Span::raw(text.as_str());
        let span_end = column + text_span.width();
        if span_end <= col_start || column >= col_end {
            new_spans.push(Span::styled(text, style));
        } else {
            let mut before = String::new();
            let mut selected = String::new();
            let mut after = String::new();
            for grapheme in text_span.styled_graphemes(Style::default()) {
                let width = Span::raw(grapheme.symbol).width();
                if column < col_end && column + width > col_start {
                    selected.push_str(grapheme.symbol);
                } else if column < col_start {
                    before.push_str(grapheme.symbol);
                } else {
                    after.push_str(grapheme.symbol);
                }
                column += width;
            }
            if !before.is_empty() {
                new_spans.push(Span::styled(before, style));
            }
            if !selected.is_empty() {
                new_spans.push(Span::styled(
                    selected,
                    style.patch(Style::default().bg(background)),
                ));
            }
            if !after.is_empty() {
                new_spans.push(Span::styled(after, style));
            }
        }
        column = span_end;
    }
    line.spans = new_spans;
}

pub(in crate::tui_widgets) fn terminal_display_width(text: &str) -> usize {
    Line::from(text).width()
}

pub(in crate::tui_widgets) fn line_alignment_offset(line: &Line<'_>, area_width: u16) -> usize {
    let line_width = line.width();
    match line.alignment.unwrap_or(Alignment::Left) {
        Alignment::Left => 0,
        Alignment::Center => (area_width as usize / 2).saturating_sub(line_width / 2),
        Alignment::Right => (area_width as usize).saturating_sub(line_width),
    }
}

/// Build the plain-text selection cache for rendered lines and record the
/// leading bubble chrome needed to map terminal columns back to content.
pub(in crate::tui_widgets) fn rendered_line_selection_data(
    lines: &[Line<'_>],
    area_width: u16,
) -> (Vec<String>, Vec<usize>) {
    let mut rendered_lines = Vec::with_capacity(lines.len());
    let mut prefix_widths = Vec::with_capacity(lines.len());
    for line in lines {
        let raw: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        // Body content lines are `▌  ` (bar + margin = 3 chars) followed by a
        // 2-char structural gutter `  `, making 5 chars of chrome before real
        // content. Title and separator lines have only the 3-char `▌  ` prefix.
        let chrome = if raw.starts_with("▌    ") {
            5.min(raw.chars().count())
        } else if raw.starts_with('▌') {
            3.min(raw.chars().count())
        } else {
            0
        };
        let content_start_byte = raw
            .char_indices()
            .nth(chrome)
            .map_or(raw.len(), |(byte_pos, _)| byte_pos);
        rendered_lines.push(raw[content_start_byte..].trim_end_matches(' ').to_string());
        prefix_widths.push(chrome + line_alignment_offset(line, area_width));
    }
    (rendered_lines, prefix_widths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::CHAT_SELECTION_BG;

    fn plain_line(text: &str) -> Line<'static> {
        Line::from(vec![Span::raw(text.to_owned())])
    }

    fn highlighted_in(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .filter(|s| s.style.bg == Some(CHAT_SELECTION_BG))
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn selection_highlight_highlights_middle_chars_only() {
        let mut line = plain_line("Hello, world!");
        apply_selection_highlight_to_line(&mut line, 7, 12, CHAT_SELECTION_BG);
        assert_eq!(highlighted_in(&line), "world");
    }

    #[test]
    fn selection_highlight_empty_range_leaves_line_unchanged() {
        let mut line = plain_line("Hello");
        apply_selection_highlight_to_line(&mut line, 3, 3, CHAT_SELECTION_BG);
        assert!(
            line.spans
                .iter()
                .all(|s| s.style.bg != Some(CHAT_SELECTION_BG)),
            "no span should be highlighted for an empty range"
        );
    }

    #[test]
    fn selection_highlight_max_col_end_highlights_to_end_of_line() {
        let mut line = plain_line("Hello");
        apply_selection_highlight_to_line(&mut line, 2, usize::MAX, CHAT_SELECTION_BG);
        assert_eq!(highlighted_in(&line), "llo");
    }

    #[test]
    fn selection_highlight_across_multiple_spans() {
        let mut line = Line::from(vec![
            Span::raw("foo".to_owned()),
            Span::raw("bar".to_owned()),
        ]);
        // Select "ob" spanning both spans: cols 2..4.
        apply_selection_highlight_to_line(&mut line, 2, 4, CHAT_SELECTION_BG);
        assert_eq!(highlighted_in(&line), "ob");
    }

    #[test]
    fn selection_highlight_preserves_wide_graphemes_and_original_styles() {
        let foreground = Color::Green;
        let mut line = Line::from(vec![
            Span::styled("A日".to_owned(), Style::default().fg(foreground)),
            Span::raw("e\u{301}Z".to_owned()),
        ]);
        apply_selection_highlight_to_line(&mut line, 2, 4, CHAT_SELECTION_BG);
        assert_eq!(highlighted_in(&line), "日e\u{301}");
        assert_eq!(line.width(), 5);
        assert!(line
            .spans
            .iter()
            .filter(|span| span.content.contains('日'))
            .all(|span| span.style.fg == Some(foreground)));
    }

    #[test]
    fn selection_data_preserves_alignment_offsets_and_bubble_chrome() {
        let lines = [
            Line::from("logo").alignment(Alignment::Center),
            Line::from("status").alignment(Alignment::Right),
            Line::from("▌    日 body  "),
            Line::from("▌  title"),
        ];
        let (selection, prefixes) = rendered_line_selection_data(&lines, 20);
        assert_eq!(selection, ["logo", "status", "日 body", "title"]);
        assert_eq!(prefixes, [8, 14, 5, 3]);
    }
}
