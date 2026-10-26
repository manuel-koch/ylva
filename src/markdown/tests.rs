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

use super::*;
use crate::theme::inline_code_fg;

#[test]
fn display_column_slices_respect_wide_and_zero_width_characters() {
    assert_eq!(slice_by_display_columns("abcdef", 1, 4), "bcd");
    assert_eq!(slice_by_display_columns("日本語", 0, 2), "日");
    assert_eq!(slice_by_display_columns("日本語", 2, 6), "本語");
    assert_eq!(slice_by_display_columns("a日b", 1, 3), "日");
    assert_eq!(slice_by_display_columns("e\u{301}x", 0, 1), "e\u{301}");
    assert_eq!(slice_by_display_columns("e\u{301}x", 1, 2), "x");
    assert_eq!(slice_by_display_columns("a😀b", 1, 3), "😀");
    assert_eq!(slice_by_display_columns("abc", 2, usize::MAX), "c");
    assert_eq!(slice_by_display_columns("abc", 5, 9), "");
}

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
fn unbreakable_inline_text_wraps_without_exceeding_requested_width() {
    let text = "abcdefghijklmnop";
    let width = 12;
    let lines = render_markdown(text, width);
    let rendered: Vec<String> = lines.iter().map(plain_text).collect();

    assert!(
        lines.iter().all(|line| line.width() <= width),
        "unbreakable inline text exceeded width {width}: {rendered:?}"
    );
    assert_eq!(rendered.concat(), text);
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
    assert_eq!(code_span.style.fg, Some(inline_code_fg()));
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
        assert_eq!(line.spans[0].style.fg, Some(markdown_dim_fg()));
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
    assert_eq!(label_span.style.fg, Some(markdown_dim_fg()));
    assert!(label_span.style.add_modifier.contains(Modifier::ITALIC));
    // Gutter span on code line is dim.
    assert_eq!(lines[1].spans[0].style.fg, Some(markdown_dim_fg()));
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
fn nested_loose_bullet_list_is_indented_without_blank_before_children() {
    let md = "- Task A:\n  - one\n  - two\n\n- Task B:\n  - three";
    let rendered: Vec<String> = render_markdown(md, 80).iter().map(plain_text).collect();
    assert_eq!(
        rendered,
        vec![
            "• Task A:",
            "  • one",
            "  • two",
            "",
            "• Task B:",
            "  • three",
        ]
    );
}

#[test]
fn nested_numbered_list_wraps_under_its_own_indent() {
    let rendered: Vec<String> = render_markdown("- a\n  1. one two three", 10)
        .iter()
        .map(plain_text)
        .collect();
    assert_eq!(rendered, vec!["• a", "  1. one", "     two", "     three"]);
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
fn wide_unicode_heading_respects_terminal_display_width() {
    let lines = render_markdown("### 📅 Implementation Progress Status", 36);

    assert!(
        lines.iter().all(|line| line.width() <= 36),
        "heading row exceeded its width: {lines:?}"
    );
}

#[test]
fn blockquote_gets_dim_prefix() {
    let lines = render_markdown("> quoted text", 80);
    let prefix_span = &lines[0].spans[0];
    assert_eq!(prefix_span.content.as_ref(), "│ ");
    assert_eq!(prefix_span.style.fg, Some(markdown_dim_fg()));
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
    assert_eq!(code_span.style.fg, Some(inline_code_fg()));
}

#[test]
fn table_rows_never_exceed_requested_width() {
    let md = "| abc | def |\n|-----|-----|\n| abc | def |";
    let lines = render_markdown(md, 8);

    for line in lines {
        let rendered = plain_text(&line);
        assert!(
            rendered.chars().count() <= 8,
            "table row exceeds requested width: {rendered:?}"
        );
    }
}

#[test]
fn table_column_shrink_is_proportional_not_winner_take_all() {
    // Regression for docs/backlog-issues.md: a narrow column must keep a
    // fair share of the available width instead of being crowded down to
    // near its bare minimum by one very wide column.
    let md = "| Area | Files |\n|------|-------|\n\
        | Core tool layer | src/agent/tools.rs , src/agent.rs , src/agent_factory.rs , src/agent/subagent_manager.rs |\n\
        | Built-in tools | bash.rs , read.rs , write.rs , edit.rs , glob.rs , search.rs , clarify.rs , spawn_subagent.rs , agent_finish.rs |\n\
        | Security / LLM / config | src/sandbox.rs , src/llm.rs , src/config.rs , src/config/profile.rs , src/config/agent.rs |";
    let lines = render_markdown(md, 103);
    let header = plain_text(&lines[0]);
    let col1_width = header.find('│').expect("separator glyph") - 1;
    // The first column's natural width (longest cell: "Security / LLM /
    // config") is 24; it must not be crushed down to its bare per-word
    // minimum (8, from "Built-in"/"Security") to feed the wide column.
    assert!(
        col1_width >= 18,
        "narrow column shrunk far below its fair share: width {col1_width} in {header:?}"
    );
}
