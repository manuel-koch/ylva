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

//! Tool content/search/diff sections and completed results.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::highlight::{
    active_theme, highlighted_lines_cached, syntect_fg_span, wrap_highlighted_line,
};
use crate::markdown::render_markdown;
use crate::theme::{
    CONTENT_COLOR, DIFF_ADDED_COLOR, DIFF_CONTEXT_COLOR, DIFF_REMOVED_COLOR, MUTED_TEXT_COLOR,
};
use crate::tui_reducer_state::ToolCallState;

use super::super::bubbles::BubbleStyle;
use super::super::chat::terminal_display_width;
use super::diff::parse_hunk_header;
use super::ToolBubbleArgs;

/// Returns a derived label for a tool result, or `None` for plain output.
fn result_section_title(tool_name: &str, section_id: &str) -> Option<String> {
    use crate::agent::tools::clarify::ClarifyTool;
    use crate::tui_reducer_state::result_section_ids;
    match (tool_name, section_id) {
        (ClarifyTool::NAME, result_section_ids::USER_ANSWER) => Some("User answered:".to_string()),
        _ => None,
    }
}

/// Renders a tool's result-section `content` (e.g. a clarify tool's
/// `USER_ANSWER` result) as Markdown, optionally prefixed on the first line
/// with an underlined `title` label and indented on continuation lines to
/// align under the label. Mirrors the layout other bubble content uses via
/// [`render_markdown`] so inline styling (bold, inline code, ...) in the
/// result is preserved.
fn append_result_section_lines(
    lines: &mut Vec<Line<'static>>,
    style: &BubbleStyle,
    title: Option<&str>,
    content: &str,
    wrap_width: usize,
) {
    let prefix = title.map(|t| format!("{t} ")).unwrap_or_default();
    let prefix_width = terminal_display_width(&prefix);
    let content_wrap_width = wrap_width.saturating_sub(2 + prefix_width).max(1);
    // DIM is patched onto each span individually (not passed as `content_line`'s
    // line-level `extra_style`) so it only dims the label/content text —
    // patching it at the line level would also dim the bubble's left-border
    // bar, since unset modifiers are unioned in, not overridden, when a span
    // has no modifier of its own.
    let dim = Style::default()
        .fg(CONTENT_COLOR)
        .add_modifier(Modifier::DIM);
    let mut md_lines = render_markdown(content, content_wrap_width);
    if md_lines.is_empty() {
        md_lines.push(Line::from(""));
    }
    let mut first = true;
    for md_line in md_lines {
        // Extra indent block matching the standard bar+margin, so the
        // label/content aligns one level deeper than ordinary bubble text.
        let mut spans: Vec<Span<'static>> = vec![Span::styled("  ", dim)];
        if first && !prefix.is_empty() {
            let label = title.unwrap_or_default().to_string();
            spans.push(Span::styled(label, dim.add_modifier(Modifier::UNDERLINED)));
            spans.push(Span::styled(" ", dim));
        } else if !prefix.is_empty() {
            spans.push(Span::styled(" ".repeat(prefix_width), dim));
        }
        // `dim` is the base (fg White + DIM, matching other section titles
        // like "Context:"/"Choices:"); each markdown span's own style (e.g.
        // inline-code color, bold) patches on top so per-span styling still
        // overrides the White fallback, while DIM always applies.
        spans.extend(
            md_line
                .spans
                .into_iter()
                .map(|span| Span::styled(span.content, dim.patch(span.style))),
        );
        lines.push(style.content_line(spans, Style::default()));
        first = false;
    }
}

pub(super) fn append_tool_body(
    lines: &mut Vec<Line<'static>>,
    bubble: ToolBubbleArgs<'_>,
    style: &BubbleStyle,
    extra_content_sections: &[crate::agent::tools::Section],
    wrap_width: usize,
) {
    let ToolBubbleArgs {
        name,
        state,
        collapsed,
        ..
    } = bubble;
    let [bar, margin] = style.prefix_spans();
    let content_sections = bubble.content_sections(extra_content_sections);
    // Body (body sections + result) — hidden when collapsed.
    if !collapsed {
        if !content_sections.is_empty() {
            let show_labels = content_sections.len() > 1;
            for (section_index, section) in content_sections.iter().enumerate() {
                // Empty line before every section; separates sections from each
                // other and from the title area above the body.
                let _ = section_index;
                lines.push(Line::from(vec![bar.clone(), margin.clone()]));
                match section {
                    crate::agent::tools::Section::Content(content_section) => {
                        // Label header (only when multiple sections).
                        if show_labels {
                            if let Some(label) = &content_section.label {
                                let label_str = format!("{label}:");
                                let dim_style = Style::default()
                                    .fg(CONTENT_COLOR)
                                    .add_modifier(Modifier::DIM);
                                lines.push(Line::from(vec![
                                    bar.clone(),
                                    margin.clone(),
                                    Span::styled("  ", dim_style),
                                    Span::styled(
                                        label_str,
                                        dim_style.add_modifier(Modifier::UNDERLINED),
                                    ),
                                ]));
                            }
                        }

                        // Markdown-flagged sections (e.g. `spawn_subagent`'s `task`/
                        // `briefing`) render through the project's own Markdown
                        // renderer (headings, bullets, bold, ...) instead of the
                        // syntect code-highlighting path below, since they carry
                        // prose, not source code.
                        if content_section.file_extension.as_deref() == Some("md") {
                            let content_width = wrap_width.saturating_sub(2).max(1);
                            let mut md_lines =
                                render_markdown(&content_section.content, content_width);
                            if md_lines.is_empty() {
                                md_lines.push(Line::from(""));
                            }
                            for md_line in md_lines {
                                let mut spans: Vec<Span<'static>> = vec![
                                    bar.clone(),
                                    margin.clone(),
                                    Span::styled("  ", Style::default()),
                                ];
                                spans.extend(md_line.spans);
                                lines.push(Line::from(spans));
                            }
                            continue;
                        }

                        // Determine line-number gutter width.
                        let line_count = content_section.content.lines().count();
                        let offset = content_section.line_number_offset.unwrap_or(0);
                        let last_line_num = offset + line_count;
                        let digits = last_line_num.to_string().len();

                        // Fetch highlighted lines from cache (content-addressed, computed once).
                        let all_highlighted = highlighted_lines_cached(
                            &content_section.content,
                            content_section.file_extension.as_deref(),
                            active_theme(),
                        );

                        for (line_index, highlighted) in all_highlighted.into_iter().enumerate() {
                            let line_num = offset + line_index + 1; // 1-based display

                            let gutter_prefix_len = if content_section.line_number_offset.is_some()
                            {
                                2 + digits + 2 // "  " + digits + ": "
                            } else {
                                2 // "  " indent
                            };

                            // Available width for actual code content after the gutter.
                            let content_width = wrap_width.saturating_sub(gutter_prefix_len).max(1);

                            // Wrap this syntect line into visual rows that fit within content_width.
                            let visual_rows = wrap_highlighted_line(highlighted, content_width);

                            for (row_index, row_spans) in visual_rows.into_iter().enumerate() {
                                let mut spans: Vec<Span<'static>> =
                                    vec![bar.clone(), margin.clone()];

                                // Gutter only on the first visual row; continuation rows get indent.
                                if row_index == 0 {
                                    if content_section.line_number_offset.is_some() {
                                        let gutter_str = format!("  {line_num:>digits$}: ");
                                        spans.push(Span::styled(
                                            gutter_str,
                                            Style::default().fg(MUTED_TEXT_COLOR),
                                        ));
                                    } else {
                                        spans.push(Span::styled("  ", Style::default()));
                                    }
                                } else {
                                    // Continuation row: blank gutter placeholder.
                                    let indent = " ".repeat(gutter_prefix_len);
                                    spans.push(Span::styled(indent, Style::default()));
                                }

                                // Convert syntect tokens to ratatui spans.
                                for (style, token) in row_spans {
                                    spans.push(syntect_fg_span(style, token));
                                }

                                lines.push(Line::from(spans));
                            }
                        }
                    }
                    crate::agent::tools::Section::Diff(diff_section) => {
                        // Label header (only when multiple sections).
                        if show_labels {
                            if let Some(label) = &diff_section.label {
                                let label_str = format!("{label}:");
                                let dim_style = Style::default()
                                    .fg(CONTENT_COLOR)
                                    .add_modifier(Modifier::DIM);
                                lines.push(Line::from(vec![
                                    bar.clone(),
                                    margin.clone(),
                                    Span::styled("  ", dim_style),
                                    Span::styled(
                                        label_str,
                                        dim_style.add_modifier(Modifier::UNDERLINED),
                                    ),
                                ]));
                            }
                        }

                        // Pre-scan hunk headers to determine gutter column width.
                        let gutter_col_width = diff_section
                            .content
                            .lines()
                            .filter_map(parse_hunk_header)
                            .flat_map(|(old_start, old_count, new_start, new_count)| {
                                [
                                    old_start.saturating_add(old_count).saturating_sub(1),
                                    new_start.saturating_add(new_count).saturating_sub(1),
                                ]
                            })
                            .max()
                            .unwrap_or(1)
                            .to_string()
                            .len()
                            .max(1);
                        // "  {left:>w} {right:>w} " — 2 + w + 1 + w + 1 = 4 + 2w chars.
                        let gutter_len = 4 + 2 * gutter_col_width;

                        // Render unified diff with a two-column line-number gutter.
                        // Left column: old-file line number (blank for `+` lines).
                        // Right column: new-file line number (blank for `-` lines).
                        let mut old_line: usize = 1;
                        let mut new_line: usize = 1;
                        for diff_line in diff_section.content.lines() {
                            if let Some((old_start, _, new_start, _)) = parse_hunk_header(diff_line)
                            {
                                old_line = old_start;
                                new_line = new_start;
                                // Render hunk header dim across the full width.
                                let text_str = format!("  {diff_line}");
                                lines.push(Line::from(vec![
                                    bar.clone(),
                                    margin.clone(),
                                    Span::styled(
                                        text_str,
                                        Style::default()
                                            .fg(MUTED_TEXT_COLOR)
                                            .add_modifier(Modifier::DIM),
                                    ),
                                ]));
                                continue;
                            }

                            let (line_color, left_num, right_num) = if diff_line.starts_with('+') {
                                let n = new_line;
                                new_line += 1;
                                (DIFF_ADDED_COLOR, None, Some(n))
                            } else if diff_line.starts_with('-') {
                                let n = old_line;
                                old_line += 1;
                                (DIFF_REMOVED_COLOR, Some(n), None)
                            } else {
                                let o = old_line;
                                let n = new_line;
                                old_line += 1;
                                new_line += 1;
                                (DIFF_CONTEXT_COLOR, Some(o), Some(n))
                            };

                            let left_str = left_num
                                .map(|n| format!("{n:>gutter_col_width$}"))
                                .unwrap_or_else(|| " ".repeat(gutter_col_width));
                            let right_str = right_num
                                .map(|n| format!("{n:>gutter_col_width$}"))
                                .unwrap_or_else(|| " ".repeat(gutter_col_width));
                            let gutter_str = format!("  {left_str} {right_str} ");

                            let rest = diff_line.get(1..).unwrap_or("");
                            // Content area: gutter + "+"/"-"/" " (1 char) + " " (1 char).
                            let content_width = wrap_width.saturating_sub(gutter_len + 2).max(1);
                            let wrapped_rest = textwrap::wrap(rest, content_width);
                            for (wrap_index, wrapped_chunk) in wrapped_rest.iter().enumerate() {
                                let prefix = if diff_line.starts_with('+') {
                                    "+"
                                } else if diff_line.starts_with('-') {
                                    "-"
                                } else {
                                    " "
                                };
                                let (gutter_span, content_str) = if wrap_index == 0 {
                                    (
                                        Span::styled(
                                            gutter_str.clone(),
                                            Style::default().fg(MUTED_TEXT_COLOR),
                                        ),
                                        format!("{prefix} {wrapped_chunk}"),
                                    )
                                } else {
                                    (
                                        Span::styled(" ".repeat(gutter_len), Style::default()),
                                        format!("  {wrapped_chunk}"),
                                    )
                                };
                                lines.push(Line::from(vec![
                                    bar.clone(),
                                    margin.clone(),
                                    gutter_span,
                                    Span::styled(content_str, Style::default().fg(line_color)),
                                ]));
                            }
                        }
                    }
                    crate::agent::tools::Section::SearchResult(search_section) => {
                        // Always show the file path label — line numbers are
                        // meaningless without the filename, even when there is
                        // only one section.
                        let label_str = format!("  {}:", search_section.file_path);
                        lines.push(Line::from(vec![
                            bar.clone(),
                            margin.clone(),
                            Span::styled(
                                label_str,
                                Style::default()
                                    .fg(CONTENT_COLOR)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));

                        let gutter_width = search_section
                            .lines
                            .iter()
                            .map(|l| l.line_number)
                            .max()
                            .map(|n| n.to_string().len())
                            .unwrap_or(1);

                        for search_result_line in &search_section.lines {
                            // Path already shown in the label; use only the
                            // line number as the gutter prefix, right-aligned
                            // to the widest number in this section.
                            let prefix = if search_result_line.is_match {
                                format!("  {:>gutter_width$}: ", search_result_line.line_number)
                            } else {
                                format!("  {:>gutter_width$}- ", search_result_line.line_number)
                            };
                            let prefix_len = terminal_display_width(&prefix);
                            let content_width = wrap_width.saturating_sub(prefix_len).max(1);
                            let highlighted_lines = highlighted_lines_cached(
                                &search_result_line.content,
                                search_section.file_extension.as_deref(),
                                active_theme(),
                            );
                            let highlighted_line =
                                highlighted_lines.into_iter().next().unwrap_or_default();
                            let visual_rows =
                                wrap_highlighted_line(highlighted_line, content_width);

                            for (row_index, row_spans) in visual_rows.into_iter().enumerate() {
                                let mut spans: Vec<Span<'static>> =
                                    vec![bar.clone(), margin.clone()];

                                if row_index == 0 {
                                    let prefix_style = if search_result_line.is_match {
                                        Style::default().fg(MUTED_TEXT_COLOR)
                                    } else {
                                        Style::default()
                                            .fg(MUTED_TEXT_COLOR)
                                            .add_modifier(Modifier::DIM)
                                    };
                                    spans.push(Span::styled(prefix.clone(), prefix_style));
                                } else {
                                    spans.push(Span::styled(
                                        " ".repeat(prefix_len),
                                        Style::default(),
                                    ));
                                }

                                for (style, token) in row_spans {
                                    let fg = Color::Rgb(
                                        style.foreground.r,
                                        style.foreground.g,
                                        style.foreground.b,
                                    );
                                    spans.push(Span::styled(token, Style::default().fg(fg)));
                                }

                                lines.push(Line::from(spans));
                            }
                        }
                    }
                }
            }

            // After content sections, show the result only when it carries a derived title
            // (e.g. USER_ANSWER). Plain results are already represented by their content sections.
            if let ToolCallState::Done { result_section, .. } = state {
                if let Some(title) = result_section_title(name, &result_section.id) {
                    if !result_section.content.is_empty() {
                        style.push_separator_line(lines, Style::default());
                        append_result_section_lines(
                            lines,
                            style,
                            Some(&title),
                            &result_section.content,
                            wrap_width,
                        );
                    }
                }
            }
        } else if let ToolCallState::Done { result_section, .. } = state {
            // No body sections — render the result, with a title prefix when one is derived
            // for this (tool, section-id) pair, or as plain text otherwise.
            if !result_section.content.is_empty() {
                let title = result_section_title(name, &result_section.id);
                append_result_section_lines(
                    lines,
                    style,
                    title.as_deref(),
                    &result_section.content,
                    wrap_width,
                );
            }
        }
    }
}
