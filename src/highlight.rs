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

//! Syntax-highlighting helpers shared by the TUI renderer (`tui_widgets.rs`) and the
//! Markdown renderer (`markdown.rs`).
//!
//! All syntect state is held in process-wide statics so the grammar/theme
//! data is loaded at most once per process.  Per-call results are memoised in
//! a thread-local content-addressed cache so the highlighting pass is skipped
//! on every subsequent render frame.

use std::cell::RefCell;
use std::collections::HashMap;

use ratatui::style::Color;
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SyntectStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Lazily initialised default syntax set — loaded once, reused across calls.
static SYNTAX_SET: std::sync::OnceLock<SyntaxSet> = std::sync::OnceLock::new();
/// Lazily initialised default theme set — loaded once, reused across calls.
static THEME_SET: std::sync::OnceLock<ThemeSet> = std::sync::OnceLock::new();

pub(crate) fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

pub(crate) fn highlight_theme() -> &'static Theme {
    let theme_set = THEME_SET.get_or_init(ThemeSet::load_defaults);
    &theme_set.themes["base16-ocean.dark"]
}

/// One token produced by syntect: a foreground style and the text fragment.
pub type HighlightedToken = (SyntectStyle, String);
/// One source line split into styled tokens.
pub type HighlightedLine = Vec<HighlightedToken>;
/// All lines of a highlighted source block.
pub type HighlightedLines = Vec<HighlightedLine>;

type HighlightCacheMap = HashMap<u64, HighlightedLines>;

thread_local! {
    static HIGHLIGHT_CACHE: RefCell<HighlightCacheMap> = RefCell::new(HashMap::new());
}

fn highlight_cache_key(content: &str, file_extension: Option<&str>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    file_extension.hash(&mut hasher);
    hasher.finish()
}

/// Returns highlighted lines for `content`, using a content-addressed cache so
/// syntect is not re-run on every render frame.
///
/// `file_extension` is used for grammar selection (e.g. `Some("rs")` for
/// Rust). When `None` (or when no grammar matches the extension), syntect
/// attempts first-line detection and falls back to plain text.
pub fn highlighted_lines_cached(content: &str, file_extension: Option<&str>) -> HighlightedLines {
    let key = highlight_cache_key(content, file_extension);
    HIGHLIGHT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(lines) = cache.get(&key) {
            return lines.clone();
        }
        let ss = syntax_set();
        let theme = highlight_theme();
        let syntax = file_extension
            .and_then(|ext| ss.find_syntax_by_extension(ext))
            .or_else(|| ss.find_syntax_by_first_line(content))
            .unwrap_or_else(|| ss.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, theme);
        // `SyntaxSet::load_defaults_newlines()` grammars expect each line
        // passed to `highlight_line` to include its trailing "\n" — some
        // contexts (e.g. line comments) only pop on a newline match. Using
        // `str::lines()` (which strips newlines) leaves the parser stuck in
        // that context after the first such line, coloring everything after
        // it as a comment. `LinesWithEndings` preserves the newline for
        // parsing; it's stripped back off each line's last token below so it
        // isn't rendered as literal content.
        let lines: HighlightedLines = syntect::util::LinesWithEndings::from(content)
            .map(|line| {
                let mut tokens: HighlightedLine = highlighter
                    .highlight_line(line, ss)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(style, token)| (style, token.to_string()))
                    .collect();
                if let Some((_, last_token)) = tokens.last_mut() {
                    if last_token.ends_with('\n') {
                        let trimmed_len = last_token.trim_end_matches('\n').len();
                        last_token.truncate(trimmed_len);
                    }
                }
                tokens
            })
            .collect();
        cache.insert(key, lines.clone());
        lines
    })
}

/// Converts a syntect foreground color and text token into a ratatui [`Span`]
/// with the given `bg` color applied as the background.  Only the foreground
/// is taken from syntect; the background is always `bg` so the token blends
/// into the surrounding bubble.
pub fn syntect_token_to_span(style: SyntectStyle, token: String, bg: Color) -> Span<'static> {
    let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
    Span::styled(token, ratatui::style::Style::default().fg(fg).bg(bg))
}

/// Like [`syntect_token_to_span`] but sets only the foreground color.
/// Use this when the background is applied by a parent widget's `patch_style`
/// (e.g. in the Markdown renderer where the bubble background is not known
/// at the point of span creation).
pub fn syntect_fg_span(style: SyntectStyle, token: String) -> Span<'static> {
    let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
    Span::styled(token, ratatui::style::Style::default().fg(fg))
}

/// Splits a single highlighted line's token-spans into multiple visual rows,
/// each at most `available_width` characters wide.  Splitting occurs at
/// character boundaries within tokens, not at word boundaries, to preserve
/// code layout.
pub fn wrap_highlighted_line(
    spans: HighlightedLine,
    available_width: usize,
) -> Vec<HighlightedLine> {
    if available_width == 0 {
        return vec![spans];
    }
    let mut rows: Vec<HighlightedLine> = Vec::new();
    let mut current_row: HighlightedLine = Vec::new();
    let mut current_width: usize = 0;

    for (style, token) in spans {
        let mut remaining: &str = &token;
        while !remaining.is_empty() {
            let available = available_width.saturating_sub(current_width);
            let token_chars = remaining.chars().count();
            if token_chars <= available {
                current_row.push((style, remaining.to_string()));
                current_width += token_chars;
                remaining = "";
            } else {
                let split_byte = remaining
                    .char_indices()
                    .nth(available)
                    .map(|(byte_idx, _)| byte_idx)
                    .unwrap_or(remaining.len());
                let (head, tail) = remaining.split_at(split_byte);
                if !head.is_empty() {
                    current_row.push((style, head.to_string()));
                }
                rows.push(std::mem::take(&mut current_row));
                current_width = 0;
                remaining = tail;
            }
        }
    }

    if !current_row.is_empty() {
        rows.push(current_row);
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}
