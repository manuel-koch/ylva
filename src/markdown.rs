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

//! Markdown-to-`ratatui` rendering used by chat bubbles and the clarify
//! banner.
//!
//! [`render_markdown`] converts a Markdown string into a `Vec<Line<'static>>`
//! hard-wrapped at a fixed column width, suitable for embedding inside a
//! bordered chat bubble. It supports a minimum-viable subset of CommonMark:
//! paragraphs, **bold**/*italic*, `inline code`, fenced code blocks
//! (syntax-highlighted when a language tag is given), bullet and numbered
//! lists, headings, blockquotes, horizontal rules, and tables
//! (column-aligned, bold header row). Anything outside that subset (links,
//! images, HTML, ...) degrades to plain unstyled text instead of panicking
//! or dropping content.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

mod code_block;
mod table;
mod text;

use code_block::push_highlighted_code_block;
use table::flush_table;
pub(crate) use text::slice_by_display_columns;
use text::{push_words, terminal_display_width, wrap_words, InlineStyle, Word};

use crate::theme::MARKDOWN_DIM_FG;

/// Tracks the counter for an in-progress list so nested items number/bullet
/// correctly; `Bullet` lists never advance, `Ordered` increments per item.
enum ListKind {
    Bullet,
    Ordered(u64),
}

/// Convert `text` (Markdown) into styled, word-wrapped lines no wider than
/// `width` columns. Fenced code block lines are the one exception to
/// word-wrapping: their content is preserved verbatim (no word-boundary
/// reflow) to keep code formatting intact, but a line that is still too long
/// to fit is character-wrapped onto continuation rows — never clipped — so
/// it never renders past the bubble's right edge.
pub fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let parser = Parser::new_ext(text, Options::ENABLE_TABLES);

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut buffer: Vec<Word> = Vec::new();
    let mut style_stack: Vec<InlineStyle> = vec![InlineStyle::default()];
    let mut list_stack: Vec<ListKind> = Vec::new();
    let mut quote_depth: usize = 0;
    let mut pending_item_prefix: Option<String> = None;
    let mut item_continuation_indent: usize = 0;
    let mut heading_level: Option<usize> = None;
    let mut code_block_buffer: Option<String> = None;
    // Language tag from a fenced code block opening (e.g. "rust", "python").
    // Carried alongside code_block_buffer and used for syntax selection.
    let mut code_block_language: Option<String> = None;
    // Table state: cells accumulate into current_row_cells; completed rows
    // (with a flag marking header rows) collect in table_rows until End(Table)
    // triggers a single two-pass aligned flush.
    let mut current_row_cells: Vec<Vec<Word>> = Vec::new();
    let mut current_cell_buffer: Option<Vec<Word>> = None;
    let mut table_rows: Vec<(bool, Vec<Vec<Word>>)> = Vec::new();

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Strong => {
                    let mut s = *style_stack.last().unwrap_or(&InlineStyle::default());
                    s.bold = true;
                    style_stack.push(s);
                }
                Tag::Emphasis => {
                    let mut s = *style_stack.last().unwrap_or(&InlineStyle::default());
                    s.italic = true;
                    style_stack.push(s);
                }
                Tag::BlockQuote(_) => quote_depth += 1,
                Tag::Heading { level, .. } => {
                    push_spacer_if_needed(&mut out);
                    heading_level = Some(level as usize);
                }
                Tag::List(start) => {
                    if list_stack.is_empty() {
                        push_spacer_if_needed(&mut out);
                    }
                    list_stack.push(match start {
                        Some(n) => ListKind::Ordered(n),
                        None => ListKind::Bullet,
                    });
                }
                Tag::Item => {
                    let (prefix, indent) = list_item_prefix(&mut list_stack);
                    pending_item_prefix = Some(prefix);
                    item_continuation_indent = indent;
                }
                Tag::CodeBlock(kind) => {
                    push_spacer_if_needed(&mut out);
                    code_block_language = if let CodeBlockKind::Fenced(lang) = kind {
                        let trimmed = lang.to_string();
                        let trimmed = trimmed.trim().to_string();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed)
                        }
                    } else {
                        None
                    };
                    code_block_buffer = Some(String::new());
                }
                Tag::Table(_) => {
                    push_spacer_if_needed(&mut out);
                }
                Tag::TableHead => {}
                Tag::TableCell => current_cell_buffer = Some(Vec::new()),
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Strong | TagEnd::Emphasis => {
                    style_stack.pop();
                }
                TagEnd::Paragraph | TagEnd::Item | TagEnd::Heading(_) => {
                    flush_buffer(
                        &mut out,
                        &mut buffer,
                        width,
                        quote_depth,
                        &mut pending_item_prefix,
                        item_continuation_indent,
                        heading_level,
                    );
                    if matches!(tag_end, TagEnd::Heading(_)) {
                        heading_level = None;
                        push_spacer_if_needed(&mut out);
                    }
                    if matches!(tag_end, TagEnd::Item) {
                        item_continuation_indent = 0;
                    }
                    if matches!(tag_end, TagEnd::Paragraph) {
                        push_spacer_if_needed(&mut out);
                    }
                }
                TagEnd::BlockQuote(_) => quote_depth = quote_depth.saturating_sub(1),
                TagEnd::List(_) => {
                    list_stack.pop();
                    if list_stack.is_empty() {
                        push_spacer_if_needed(&mut out);
                    }
                }
                TagEnd::CodeBlock => {
                    if let Some(content) = code_block_buffer.take() {
                        push_highlighted_code_block(
                            &mut out,
                            &content,
                            code_block_language.take().as_deref(),
                            width,
                        );
                        push_spacer_if_needed(&mut out);
                    }
                }
                TagEnd::TableCell => {
                    if let Some(cell) = current_cell_buffer.take() {
                        current_row_cells.push(cell);
                    }
                }
                TagEnd::TableRow => {
                    table_rows.push((false, std::mem::take(&mut current_row_cells)));
                }
                TagEnd::TableHead => {
                    // Header cells live directly inside TableHead (no TableRow wrapper),
                    // so we flush the accumulated cells here.
                    if !current_row_cells.is_empty() {
                        table_rows.push((true, std::mem::take(&mut current_row_cells)));
                    }
                }
                TagEnd::Table => {
                    flush_table(&mut out, &table_rows, width);
                    table_rows.clear();
                    push_spacer_if_needed(&mut out);
                }
                _ => {}
            },
            Event::Text(text) => {
                let style = *style_stack.last().unwrap_or(&InlineStyle::default());
                if let Some(code_block_content) = code_block_buffer.as_mut() {
                    code_block_content.push_str(&text);
                } else if let Some(cell) = current_cell_buffer.as_mut() {
                    push_words(cell, &text, style);
                } else {
                    push_words(&mut buffer, &text, style);
                }
            }
            Event::Code(code) => {
                let mut style = *style_stack.last().unwrap_or(&InlineStyle::default());
                style.code = true;
                if let Some(cell) = current_cell_buffer.as_mut() {
                    push_words(cell, &code, style);
                } else {
                    push_words(&mut buffer, &code, style);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                let style = *style_stack.last().unwrap_or(&InlineStyle::default());
                push_words(&mut buffer, " ", style);
            }
            Event::Rule => {
                flush_buffer(
                    &mut out,
                    &mut buffer,
                    width,
                    quote_depth,
                    &mut pending_item_prefix,
                    item_continuation_indent,
                    heading_level,
                );
                out.push(Line::from(Span::raw("─".repeat(width))));
            }
            _ => {}
        }
    }
    // Flush any trailing buffered inline content — guards against malformed
    // input or tight-list edge cases that don't emit a closing block event.
    flush_buffer(
        &mut out,
        &mut buffer,
        width,
        quote_depth,
        &mut pending_item_prefix,
        item_continuation_indent,
        heading_level,
    );

    // Drop trailing blank lines — spacers serve as separators between blocks;
    // a spacer after the last block has nothing to separate from.
    while out.last().is_some_and(|l| l.spans.is_empty()) {
        out.pop();
    }

    out
}

/// Computes the prefix (`"• "` or `"N. "`) for a newly started list item and
/// advances the innermost list's counter. Returns the prefix and its display
/// width, used as the continuation-line indent for wrapped item text.
fn list_item_prefix(list_stack: &mut [ListKind]) -> (String, usize) {
    match list_stack.last_mut() {
        Some(ListKind::Ordered(n)) => {
            let prefix = format!("{n}. ");
            let indent = terminal_display_width(&prefix);
            *n += 1;
            (prefix, indent)
        }
        Some(ListKind::Bullet) => {
            let prefix = "• ".to_string();
            let indent = terminal_display_width(&prefix);
            (prefix, indent)
        }
        None => (String::new(), 0),
    }
}

/// Flushes `buffer` (the inline text accumulated since the last block
/// boundary) into `out` as one or more wrapped [`Line`]s, applying the
/// current block-level decoration: heading prefix + bold, list bullet/number
/// prefix, or blockquote `│` prefix. Leaves `buffer` empty afterwards.
fn flush_buffer(
    out: &mut Vec<Line<'static>>,
    buffer: &mut Vec<Word>,
    width: usize,
    quote_depth: usize,
    pending_item_prefix: &mut Option<String>,
    item_continuation_indent: usize,
    heading_level: Option<usize>,
) {
    if buffer.is_empty() {
        return;
    }

    let quote_prefix = "│ ".repeat(quote_depth);
    let quote_prefix_len = terminal_display_width(&quote_prefix);

    let heading_prefix = heading_level
        .map(|level| format!("{} ", "#".repeat(level)))
        .unwrap_or_default();
    let heading_prefix_len = terminal_display_width(&heading_prefix);

    // First-line prefix combines quote + heading + list-item markers; the
    // continuation-line indent replaces the list marker with matching blanks
    // so wrapped text stays aligned under the first line's content.
    let first_prefix_extra = heading_prefix_len
        + pending_item_prefix
            .as_ref()
            .map(|prefix| terminal_display_width(prefix))
            .unwrap_or(0);
    let cont_prefix_extra = heading_prefix_len + item_continuation_indent;

    let first_wrap_width = width
        .saturating_sub(quote_prefix_len + first_prefix_extra)
        .max(1);
    let cont_wrap_width = width
        .saturating_sub(quote_prefix_len + cont_prefix_extra)
        .max(1);
    // Wrap once at the narrower of the two so a single word list fits both;
    // in practice the two widths only differ by the list-marker width, which
    // is small enough that using the continuation width for all lines (and
    // only shrinking, never growing) keeps wrapping stable across items.
    let wrap_width = first_wrap_width.min(cont_wrap_width).max(1);

    let words = std::mem::take(buffer);
    let mut wrapped_lines = wrap_words(&words, wrap_width);
    if wrapped_lines.is_empty() {
        wrapped_lines.push(Vec::new());
    }

    for (index, line_words) in wrapped_lines.into_iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if quote_depth > 0 {
            spans.push(Span::styled(
                quote_prefix.clone(),
                Style::default().fg(MARKDOWN_DIM_FG),
            ));
        }
        if index == 0 {
            if !heading_prefix.is_empty() {
                spans.push(Span::styled(
                    heading_prefix.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ));
            }
            if let Some(prefix) = pending_item_prefix.take() {
                spans.push(Span::raw(prefix));
            }
        } else {
            let indent = heading_prefix_len + item_continuation_indent;
            if indent > 0 {
                spans.push(Span::raw(" ".repeat(indent)));
            }
        }

        let heading_style = if heading_level.is_some() {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        for (word_index, word) in line_words.iter().enumerate() {
            if word_index > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                word.text.clone(),
                word.style.to_style().patch(heading_style),
            ));
        }
        out.push(Line::from(spans));
    }
}

/// Appends a blank [`Line`] to `out` only when `out` is non-empty and its
/// last line is not already blank. Used to add visual spacing before/after
/// headings, tables, and lists without creating double blank lines.
fn push_spacer_if_needed(out: &mut Vec<Line<'static>>) {
    if !out.is_empty() && !out.last().is_some_and(|l| l.spans.is_empty()) {
        out.push(Line::default());
    }
}
#[cfg(test)]
mod tests;
