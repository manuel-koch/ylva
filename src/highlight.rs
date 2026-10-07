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
use std::sync::atomic::{AtomicU8, Ordering};

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

/// Selectable syntax-highlighting theme, resolved from the `theme` key of the
/// selected profile (see `docs/configuration.md`).
///
/// Deliberately only two variants: both are bundled with syntect, so no theme
/// files ship with Ylva.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    #[default]
    Dark,
    Light,
}

impl ThemeName {
    /// The syntect bundle key this theme resolves to.
    pub fn syntect_key(self) -> &'static str {
        match self {
            ThemeName::Dark => "base16-ocean.dark",
            ThemeName::Light => "base16-ocean.light",
        }
    }
}

/// The theme every renderer highlights with, set once at startup from the
/// loaded profile.
///
/// A process-wide value rather than a parameter threaded through every render
/// function: the theme is chosen before the first frame and never changes
/// during a run, so passing it down ~20 rendering signatures would be pure
/// ceremony. The highlighting API itself still takes the theme explicitly, so
/// nothing below this line reads global state.
static ACTIVE_THEME: AtomicU8 = AtomicU8::new(0);

/// Select the theme every subsequent render highlights with.
pub fn set_active_theme(theme_name: ThemeName) {
    let encoded = match theme_name {
        ThemeName::Dark => 0,
        ThemeName::Light => 1,
    };
    ACTIVE_THEME.store(encoded, Ordering::Relaxed);
}

/// The theme selected by [`set_active_theme`], defaulting to
/// [`ThemeName::Dark`].
pub fn active_theme() -> ThemeName {
    match ACTIVE_THEME.load(Ordering::Relaxed) {
        1 => ThemeName::Light,
        _ => ThemeName::Dark,
    }
}

pub(crate) fn highlight_theme(theme_name: ThemeName) -> &'static Theme {
    let theme_set = THEME_SET.get_or_init(ThemeSet::load_defaults);
    &theme_set.themes[theme_name.syntect_key()]
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

/// Cache key for one highlighting result. The theme is part of the key: the
/// same content highlighted with a different theme is a different result, so
/// a theme switch must not return stale styled lines.
fn highlight_cache_key(content: &str, file_extension: Option<&str>, theme_name: ThemeName) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    file_extension.hash(&mut hasher);
    theme_name.syntect_key().hash(&mut hasher);
    hasher.finish()
}

/// Returns highlighted lines for `content`, using a content-addressed cache so
/// syntect is not re-run on every render frame.
///
/// `file_extension` is used for grammar selection (e.g. `Some("rs")` for
/// Rust). When `None` (or when no grammar matches the extension), syntect
/// attempts first-line detection and falls back to plain text.
pub fn highlighted_lines_cached(
    content: &str,
    file_extension: Option<&str>,
    theme_name: ThemeName,
) -> HighlightedLines {
    let key = highlight_cache_key(content, file_extension, theme_name);
    HIGHLIGHT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(lines) = cache.get(&key) {
            return lines.clone();
        }
        let ss = syntax_set();
        let theme = highlight_theme(theme_name);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_theme_names_resolve_to_a_distinct_bundled_syntect_theme() {
        let dark = highlight_theme(ThemeName::Dark);
        let light = highlight_theme(ThemeName::Light);
        assert_eq!(dark.name.as_deref(), Some("Base16 Ocean Dark"));
        assert_eq!(light.name.as_deref(), Some("Base16 Ocean Light"));
        assert_ne!(
            dark.settings.background, light.settings.background,
            "the two themes must not resolve to the same loaded theme"
        );
    }

    #[test]
    fn the_cache_key_distinguishes_themes() {
        assert_ne!(
            highlight_cache_key("let x = 1;", Some("rs"), ThemeName::Dark),
            highlight_cache_key("let x = 1;", Some("rs"), ThemeName::Light),
            "a theme switch must invalidate previously cached highlighted lines"
        );
    }

    #[test]
    fn the_same_content_is_styled_differently_per_theme() {
        let dark = highlighted_lines_cached("let x = 1;\n", Some("rs"), ThemeName::Dark);
        let light = highlighted_lines_cached("let x = 1;\n", Some("rs"), ThemeName::Light);
        let dark_foregrounds: Vec<_> = dark[0].iter().map(|(style, _)| style.foreground).collect();
        let light_foregrounds: Vec<_> =
            light[0].iter().map(|(style, _)| style.foreground).collect();
        assert_eq!(
            dark[0]
                .iter()
                .map(|(_, token)| token.clone())
                .collect::<Vec<_>>(),
            light[0]
                .iter()
                .map(|(_, token)| token.clone())
                .collect::<Vec<_>>(),
            "the same content tokenizes identically in both themes"
        );
        assert_ne!(
            dark_foregrounds, light_foregrounds,
            "but is styled with different colors"
        );
    }

    #[test]
    fn the_active_theme_defaults_to_dark_and_follows_the_last_selection() {
        assert_eq!(active_theme(), ThemeName::Dark);
        set_active_theme(ThemeName::Light);
        assert_eq!(active_theme(), ThemeName::Light);
        set_active_theme(ThemeName::Dark);
        assert_eq!(active_theme(), ThemeName::Dark);
    }
}
