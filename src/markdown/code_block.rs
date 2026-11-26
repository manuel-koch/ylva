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

//! Syntax-highlighted fenced code block rendering.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::highlight::{
    active_theme, highlighted_lines_cached, syntect_fg_span, wrap_highlighted_line, HighlightedLine,
};
use crate::terminal_text::terminal_display_width;
use crate::theme::markdown_dim_fg;

const CODE_TAB_WIDTH: usize = 4;

/// Renders a fenced code block using syntect syntax highlighting.
/// Each source line is prefixed with a right-aligned line number and a dim
/// `│` separator. Falls back to plain-text rendering when no grammar matches.
pub(super) fn push_highlighted_code_block(
    out: &mut Vec<Line<'static>>,
    content: &str,
    language: Option<&str>,
    width: usize,
) {
    let trimmed = content.strip_suffix('\n').unwrap_or(content);
    if let Some(lang) = language.map(str::trim).filter(|s| !s.is_empty()) {
        out.push(Line::from(Span::styled(
            lang.to_string(),
            Style::default()
                .fg(markdown_dim_fg())
                .add_modifier(Modifier::ITALIC),
        )));
    }
    if trimmed.is_empty() {
        out.push(Line::default());
        return;
    }
    let line_count = trimmed.lines().count();
    let gutter_width = line_count.to_string().len();
    let gutter_style = Style::default().fg(markdown_dim_fg());
    // "{:>gutter_width$} │ " — digits, one space, the "│" glyph, one space.
    let gutter_prefix_width = gutter_width + 3;
    let continuation_gutter = Span::styled(" ".repeat(gutter_prefix_width), gutter_style);
    // Room left for code tokens once the gutter has been carved off. Lines
    // that don't fit are wrapped (never clipped) so they never render past
    // the bubble's right edge; see wrap_highlighted_line for the rationale.
    let content_width = width.saturating_sub(gutter_prefix_width).max(1);
    let ext = language
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(normalize_lang_ext);
    let highlighted = highlighted_lines_cached(trimmed, ext.as_deref(), active_theme());
    for (line_index, mut highlighted_line) in highlighted.into_iter().enumerate() {
        expand_code_tabs(&mut highlighted_line);
        let line_number = format!("{:>gutter_width$} │ ", line_index + 1);
        let gutter = Span::styled(line_number, gutter_style);
        for (row_index, row_tokens) in wrap_highlighted_line(highlighted_line, content_width)
            .into_iter()
            .enumerate()
        {
            let mut spans = Vec::with_capacity(row_tokens.len() + 1);
            spans.push(if row_index == 0 {
                gutter.clone()
            } else {
                continuation_gutter.clone()
            });
            spans.extend(
                row_tokens
                    .into_iter()
                    .map(|(style, token)| syntect_fg_span(style, token)),
            );
            out.push(Line::from(spans));
        }
    }
}

fn expand_code_tabs(line: &mut HighlightedLine) {
    if !line.iter().any(|(_, token)| token.contains('\t')) {
        return;
    }

    // Ratatui does not draw tabs. Expand them relative to the source line,
    // across syntax spans and before the gutter or visual wraps are added.
    let mut column = 0;
    for (_, token) in line {
        let Some((first, rest)) = token.split_once('\t') else {
            column += terminal_display_width(token);
            continue;
        };
        column += terminal_display_width(first);
        let mut expanded = String::from(first);
        for segment in rest.split('\t') {
            let spaces = CODE_TAB_WIDTH - column % CODE_TAB_WIDTH;
            expanded.extend(std::iter::repeat_n(' ', spaces));
            expanded.push_str(segment);
            column += spaces + terminal_display_width(segment);
        }
        *token = expanded;
    }
}

/// Maps a Markdown language tag to a file extension that syntect recognises.
pub(super) fn normalize_lang_ext(lang: &str) -> String {
    let lower = lang.to_ascii_lowercase();
    match lower.as_str() {
        "javascript" | "js" => "js".to_string(),
        "typescript" | "ts" => "ts".to_string(),
        "python" | "py" => "py".to_string(),
        "rust" | "rs" => "rs".to_string(),
        "shell" | "bash" | "sh" | "zsh" => "sh".to_string(),
        "yaml" | "yml" => "yaml".to_string(),
        "toml" => "toml".to_string(),
        "json" => "json".to_string(),
        "c" => "c".to_string(),
        "cpp" | "c++" => "cpp".to_string(),
        _ => lower,
    }
}
