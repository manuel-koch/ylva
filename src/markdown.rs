//! Markdown-to-`ratatui` rendering used by chat bubbles and the clarify
//! banner (see `docs/plans/phase6-terminal-superpowers.md`, Phase 6.6).
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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::highlight::{
    active_theme, highlighted_lines_cached, syntect_fg_span, wrap_highlighted_line,
};

/// Foreground color used for `inline code` spans.
pub const INLINE_CODE_FG: Color = Color::Rgb(224, 168, 82);
/// Foreground color used for blockquote `│` prefixes and fenced code blocks.
pub const MARKDOWN_DIM_FG: Color = Color::DarkGray;

/// The inline formatting in effect for a run of text: bold/italic modifiers
/// and whether it originates from an inline code span (distinct color).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    code: bool,
}

impl InlineStyle {
    fn to_style(self) -> Style {
        let mut style = Style::default();
        if self.code {
            style = style.fg(INLINE_CODE_FG);
        }
        let mut modifier = Modifier::empty();
        if self.bold {
            modifier |= Modifier::BOLD;
        }
        if self.italic {
            modifier |= Modifier::ITALIC;
        }
        style.add_modifier(modifier)
    }
}

/// A single whitespace-delimited word carrying its own inline style, used as
/// the unit of word-wrapping so that styled runs survive line breaks.
#[derive(Clone)]
struct Word {
    text: String,
    style: InlineStyle,
}

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
                if let Some(buf) = code_block_buffer.as_mut() {
                    buf.push_str(&text);
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
            let indent = prefix.chars().count();
            *n += 1;
            (prefix, indent)
        }
        Some(ListKind::Bullet) => ("• ".to_string(), 2),
        None => (String::new(), 0),
    }
}

/// Splits `text` on whitespace into [`Word`]s, appending them to `buffer`.
/// Consecutive whitespace collapses (matching Markdown's own treatment of
/// inline whitespace).
fn push_words(buffer: &mut Vec<Word>, text: &str, style: InlineStyle) {
    for word in text.split_whitespace() {
        buffer.push(Word {
            text: word.to_string(),
            style,
        });
    }
}

/// Greedily wraps `words` into lines of at most `width` columns, one space
/// between words.
fn wrap_words(words: &[Word], width: usize) -> Vec<Vec<Word>> {
    let mut lines: Vec<Vec<Word>> = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    let mut current_len: usize = 0;
    for word in words {
        let word_len = word.text.chars().count();
        let extra_space = usize::from(!current.is_empty());
        if !current.is_empty() && current_len + extra_space + word_len > width {
            lines.push(std::mem::take(&mut current));
            current_len = 0;
        }
        if !current.is_empty() {
            current_len += 1;
        }
        current_len += word_len;
        current.push(word.clone());
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
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
    let quote_prefix_len = quote_prefix.chars().count();

    let heading_prefix = heading_level
        .map(|level| format!("{} ", "#".repeat(level)))
        .unwrap_or_default();
    let heading_prefix_len = heading_prefix.chars().count();

    // First-line prefix combines quote + heading + list-item markers; the
    // continuation-line indent replaces the list marker with matching blanks
    // so wrapped text stays aligned under the first line's content.
    let first_prefix_extra = heading_prefix_len
        + pending_item_prefix
            .as_ref()
            .map(|p| p.chars().count())
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

/// Renders a fenced code block using syntect syntax highlighting.
/// Each source line is prefixed with a right-aligned line number and a dim
/// `│` separator. Falls back to plain-text rendering when no grammar matches.
fn push_highlighted_code_block(
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
                .fg(MARKDOWN_DIM_FG)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    if trimmed.is_empty() {
        out.push(Line::default());
        return;
    }
    let line_count = trimmed.lines().count();
    let gutter_width = line_count.to_string().len();
    let gutter_style = Style::default().fg(MARKDOWN_DIM_FG);
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
    for (line_index, hl_line) in highlighted.into_iter().enumerate() {
        let line_number = format!("{:>gutter_width$} │ ", line_index + 1);
        let gutter = Span::styled(line_number, gutter_style);
        for (row_index, row_tokens) in wrap_highlighted_line(hl_line, content_width)
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

/// Maps a Markdown language tag to a file extension that syntect recognises.
fn normalize_lang_ext(lang: &str) -> String {
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

/// Renders all accumulated table rows as column-aligned lines.
/// Two-pass: compute natural per-column max widths, then shrink proportionally
/// if the total exceeds `width` (minimum 1 char per column). Long cells are
/// word-wrapped to the capped column width, producing multi-terminal-line rows
/// so that no content is ever clipped.
/// Header rows are rendered bold and followed by a separator line.
fn flush_table(out: &mut Vec<Line<'static>>, rows: &[(bool, Vec<Vec<Word>>)], width: usize) {
    if rows.is_empty() {
        return;
    }
    let col_count = rows.iter().map(|(_, cells)| cells.len()).max().unwrap_or(0);
    if col_count == 0 {
        return;
    }
    // Pass 1: natural column widths, and the minimum width imposed by each
    // column's longest unbreakable word (whitespace is the only wrap point;
    // see `wrap_words`).
    let mut col_widths = vec![0usize; col_count];
    let mut col_min_widths = vec![1usize; col_count];
    for (_, cells) in rows {
        for (col_idx, cell) in cells.iter().enumerate() {
            col_widths[col_idx] = col_widths[col_idx].max(cell_display_width(cell));
            for word in cell {
                col_min_widths[col_idx] = col_min_widths[col_idx].max(word.text.chars().count());
            }
        }
    }
    // Shrink proportionally when columns overflow the available width, but
    // never below a column's longest unbreakable word — otherwise that word
    // is wider than its padded cell, and every column after it in that row
    // shifts out of alignment with the rest of the table.
    let separator_overhead = col_count.saturating_sub(1) * 3;
    let total_natural: usize = col_widths.iter().sum::<usize>() + separator_overhead;
    if total_natural > width {
        let budget = width.saturating_sub(separator_overhead).max(col_count);
        let total_original: usize = col_widths.iter().sum::<usize>().max(1);
        for (w, min_w) in col_widths.iter_mut().zip(col_min_widths.iter()) {
            *w = (*w * budget / total_original).max(*min_w);
        }
    }
    let separator_style = Style::default().fg(MARKDOWN_DIM_FG);
    let empty_words: Vec<Word> = Vec::new();
    for (is_header, cells) in rows {
        let extra = if *is_header {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        // Pass 2: wrap each cell to its (possibly capped) column width.
        let wrapped_cells: Vec<Vec<Vec<Word>>> = (0..col_count)
            .map(|col_idx| {
                let cell = cells.get(col_idx).map(Vec::as_slice).unwrap_or(&[]);
                wrap_words(cell, col_widths[col_idx].max(1))
            })
            .collect();
        let row_height = wrapped_cells
            .iter()
            .map(|c| c.len().max(1))
            .max()
            .unwrap_or(1);
        // Emit one terminal line per wrapped row-line.
        for line_idx in 0..row_height {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for col_idx in 0..col_count {
                if col_idx > 0 {
                    spans.push(Span::styled(" │ ", separator_style));
                }
                let cell_lines = wrapped_cells[col_idx].as_slice();
                let line_words = cell_lines.get(line_idx).unwrap_or(&empty_words);
                let cell_len = cell_display_width(line_words);
                for (word_idx, word) in line_words.iter().enumerate() {
                    if word_idx > 0 {
                        spans.push(Span::raw(" "));
                    }
                    spans.push(Span::styled(
                        word.text.clone(),
                        word.style.to_style().patch(extra),
                    ));
                }
                let pad = col_widths[col_idx].saturating_sub(cell_len);
                if pad > 0 {
                    spans.push(Span::raw(" ".repeat(pad)));
                }
            }
            out.push(Line::from(spans));
        }
        if *is_header {
            let table_width = col_widths.iter().sum::<usize>() + separator_overhead;
            out.push(Line::from(Span::styled(
                "─".repeat(table_width),
                separator_style,
            )));
        }
    }
}

/// Returns the display width of a table cell (sum of word lengths plus
/// one space between consecutive words).
fn cell_display_width(words: &[Word]) -> usize {
    words
        .iter()
        .enumerate()
        .map(|(i, w)| (if i > 0 { 1 } else { 0 }) + w.text.chars().count())
        .sum()
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
mod tests {
    use super::*;

    fn plain_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn plain_text_wraps_like_textwrap_regression_guard() {
        let lines = render_markdown("hello world this is plain text", 11);
        let rendered: Vec<String> = lines.iter().map(plain_text).collect();
        // Single paragraph: content lines + trailing spacer.
        assert_eq!(rendered, vec!["hello world", "this is", "plain text"]);
        // No styling should leak onto plain-text spans.
        for line in lines.iter().filter(|l| !l.spans.is_empty()) {
            for span in &line.spans {
                assert_eq!(span.style, Style::default());
            }
        }
    }

    #[test]
    fn two_paragraphs_separated_by_blank_line() {
        let lines = render_markdown("first paragraph\n\nsecond paragraph", 80);
        let rendered: Vec<String> = lines.iter().map(plain_text).collect();
        assert_eq!(rendered, vec!["first paragraph", "", "second paragraph"]);
    }

    #[test]
    fn bold_and_italic_apply_modifiers() {
        let lines = render_markdown("plain **bold** *italic*", 80);
        // Single paragraph → content line + trailing spacer.
        let content = &lines[0];
        let spans = &content.spans;
        let bold_span = spans.iter().find(|s| s.content.as_ref() == "bold").unwrap();
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
        let italic_span = spans
            .iter()
            .find(|s| s.content.as_ref() == "italic")
            .unwrap();
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn inline_code_gets_distinct_color() {
        let lines = render_markdown("run `cargo test` now", 80);
        let code_span = spans_of(&lines[0])
            .into_iter()
            .find(|s| s.content.as_ref() == "cargo")
            .unwrap();
        assert_eq!(code_span.style.fg, Some(INLINE_CODE_FG));
    }

    fn spans_of(line: &Line<'static>) -> Vec<Span<'static>> {
        line.spans.clone()
    }

    #[test]
    fn fenced_code_block_renders_verbatim_with_syntax_highlighting() {
        // Wide enough that neither source line needs to wrap.
        let lines = render_markdown("```\nfn main() {}\n    indented\n```", 80);
        let rendered: Vec<String> = lines.iter().map(plain_text).collect();
        // Unwrapped: no word-boundary reflow — code blocks are verbatim.
        // Trailing spacer added after the block.
        assert_eq!(rendered, vec!["1 │ fn main() {}", "2 │     indented"]);
        // Each non-blank line starts with a dim gutter span.
        for line in lines.iter().filter(|l| !l.spans.is_empty()) {
            assert_eq!(line.spans[0].style.fg, Some(MARKDOWN_DIM_FG));
        }
    }

    #[test]
    fn fenced_code_block_line_too_long_for_width_wraps_onto_continuation_rows() {
        // gutter_prefix_width = 4 ("1 │ "), leaving content_width = 8 at
        // width 12. A 10-char code line must split into two visual rows
        // instead of overflowing the bubble's right edge.
        let lines = render_markdown("```\nabcdefghij\n```", 12);
        let rendered: Vec<String> = lines.iter().map(plain_text).collect();
        assert_eq!(rendered, vec!["1 │ abcdefgh", "    ij"]);
        // No row exceeds the requested width.
        for line in &rendered {
            assert!(line.chars().count() <= 12, "line {line:?} exceeds width 12");
        }
        // The continuation row's gutter placeholder is blank, not repeating
        // the line number or gutter glyph.
        assert!(!rendered[1].contains('│'));
    }

    #[test]
    fn fenced_code_block_with_language_shows_label() {
        let lines = render_markdown("```rust\nlet x = 1;\n```", 80);
        let rendered: Vec<String> = lines.iter().map(plain_text).collect();
        // [0] language label, [1] code line with gutter.
        assert_eq!(rendered[0], "rust");
        assert_eq!(rendered[1], "1 │ let x = 1;");
        // Label is dim+italic.
        let label_span = &lines[0].spans[0];
        assert_eq!(label_span.style.fg, Some(MARKDOWN_DIM_FG));
        assert!(label_span.style.add_modifier.contains(Modifier::ITALIC));
        // Gutter span on code line is dim.
        assert_eq!(lines[1].spans[0].style.fg, Some(MARKDOWN_DIM_FG));
    }

    #[test]
    fn bullet_list_gets_prefix_and_indented_continuation() {
        let lines = render_markdown("- one two three four five", 10);
        let rendered: Vec<String> = lines.iter().map(plain_text).collect();
        assert_eq!(rendered[0], "• one two");
        assert_eq!(rendered[1], "  three");
        assert_eq!(rendered[2], "  four");
        assert_eq!(rendered[3], "  five");
    }

    #[test]
    fn numbered_list_gets_prefix() {
        let lines = render_markdown("1. first\n2. second", 80);
        let rendered: Vec<String> = lines.iter().map(plain_text).collect();
        // Trailing spacer line after the list.
        assert_eq!(rendered, vec!["1. first", "2. second"]);
    }

    #[test]
    fn heading_is_bold_and_prefixed() {
        let lines = render_markdown("## Title", 80);
        // The heading line plus a trailing spacer.
        assert!(!lines.is_empty());
        let rendered = plain_text(&lines[0]);
        assert_eq!(rendered, "## Title");
        for span in &lines[0].spans {
            assert!(span.style.add_modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn blockquote_gets_dim_prefix() {
        let lines = render_markdown("> quoted text", 80);
        let prefix_span = &lines[0].spans[0];
        assert_eq!(prefix_span.content.as_ref(), "│ ");
        assert_eq!(prefix_span.style.fg, Some(MARKDOWN_DIM_FG));
        assert_eq!(plain_text(&lines[0]), "│ quoted text");
    }

    #[test]
    fn horizontal_rule_spans_width() {
        let lines = render_markdown("above\n\n---\n\nbelow", 6);
        let rendered: Vec<String> = lines.iter().map(plain_text).collect();
        assert!(rendered.contains(&"─".repeat(6)));
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(render_markdown("", 80).is_empty());
    }

    #[test]
    fn paragraph_after_list_wraps_without_indent() {
        // Regression: item_continuation_indent must not leak into the
        // following paragraph, which would indent its wrapped continuation
        // lines by 2 spaces.
        let md = "- item\n\nsome long paragraph text that needs to wrap at this width";
        let lines = render_markdown(md, 20);
        // Find the paragraph lines (after the list spacer).
        let para_lines: Vec<String> = lines
            .iter()
            .map(plain_text)
            .filter(|t| !t.is_empty() && !t.starts_with('•'))
            .collect();
        for line in &para_lines {
            assert!(
                !line.starts_with("  "),
                "paragraph continuation line has unexpected leading indent: {line:?}"
            );
        }
    }

    #[test]
    fn unsupported_syntax_passes_through_as_plain_text() {
        // Links: no crash, no data loss — inner text is preserved.
        let lines = render_markdown("[a link](https://example.com)", 80);
        assert_eq!(plain_text(&lines[0]), "a link");
    }

    #[test]
    fn table_renders_cells_aligned_with_padding() {
        let md = "| Name | Value |\n|------|-------|\n| foo  | 42     |\n| bar  | 7      |";
        let lines = render_markdown(md, 80);
        // lines: header, separator, row1, row2 (trailing spacer stripped)
        assert_eq!(lines.len(), 4);
        // Col widths: "Name"=4, "Value"=5 → separator = 4+5+3 = 12
        let header = plain_text(&lines[0]);
        assert_eq!(header, "Name │ Value");
        // Header row spans are bold.
        let bold_spans: Vec<_> = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .collect();
        assert!(!bold_spans.is_empty());
        // Separator is exactly table_width ─ characters.
        let sep = plain_text(&lines[1]);
        assert!(sep.chars().all(|c| c == '─'));
        assert_eq!(sep.chars().count(), 12);
        // Body rows are padded to column width.
        assert_eq!(plain_text(&lines[2]), "foo  │ 42   ");
        assert_eq!(plain_text(&lines[3]), "bar  │ 7    ");
    }

    #[test]
    fn table_inline_code_in_cell_gets_code_color() {
        let md = "| A |\n|---|\n| `val` |";
        let lines = render_markdown(md, 80);
        // body row cell contains inline code span
        let code_span = lines[2]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "val")
            .expect("inline code span in table cell");
        assert_eq!(code_span.style.fg, Some(INLINE_CODE_FG));
    }
}
