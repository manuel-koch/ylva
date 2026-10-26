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

//! Inline styles, words and display-width aware wrapping helpers.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::inline_code_fg;

/// The inline formatting in effect for a run of text: bold/italic modifiers
/// and whether it originates from an inline code span (distinct color).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct InlineStyle {
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) code: bool,
}

impl InlineStyle {
    pub(super) fn to_style(self) -> Style {
        let mut style = Style::default();
        if self.code {
            style = style.fg(inline_code_fg());
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
pub(super) struct Word {
    pub(super) text: String,
    pub(super) style: InlineStyle,
}

/// A table-cell word or word fragment. Unlike [`Word`], this records whether
/// a space is needed before the fragment so an unbreakable word can be split
/// across rows without inserting spaces into its content.
#[derive(Clone)]
pub(super) struct TableWord {
    pub(super) text: String,
    pub(super) style: InlineStyle,
    pub(super) space_before: bool,
}

pub(super) fn terminal_display_width(text: &str) -> usize {
    Line::from(text).width()
}

/// The part of `text` drawn between display columns `start_column` (inclusive)
/// and `end_column` (exclusive). A wide character belongs to the selection when
/// its first cell is inside; zero-width characters follow the character they
/// attach to.
pub(crate) fn slice_by_display_columns(
    text: &str,
    start_column: usize,
    end_column: usize,
) -> String {
    let mut column = 0;
    let mut included = start_column == 0 && end_column > 0;
    let mut slice = String::new();
    for character in text.chars() {
        let mut buffer = [0; 4];
        let width = Span::raw(&*character.encode_utf8(&mut buffer)).width();
        if width > 0 {
            included = column >= start_column && column < end_column;
            column += width;
        }
        if included {
            slice.push(character);
        }
    }
    slice
}

/// Splits `text` on whitespace into [`Word`]s, appending them to `buffer`.
/// Consecutive whitespace collapses (matching Markdown's own treatment of
/// inline whitespace).
pub(super) fn push_words(buffer: &mut Vec<Word>, text: &str, style: InlineStyle) {
    for word in text.split_whitespace() {
        buffer.push(Word {
            text: word.to_string(),
            style,
        });
    }
}

/// Greedily wraps `words` into lines of at most `width` columns, one space
/// between words, splitting oversized words at display-width boundaries.
pub(super) fn wrap_words(words: &[Word], width: usize) -> Vec<Vec<Word>> {
    let width = width.max(1);
    let mut lines: Vec<Vec<Word>> = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    let mut current_len: usize = 0;
    for word in words {
        let word_len = terminal_display_width(&word.text);
        let extra_space = usize::from(!current.is_empty());
        if !current.is_empty() && current_len + extra_space + word_len > width {
            lines.push(std::mem::take(&mut current));
            current_len = 0;
        }
        if word_len > width {
            let mut remaining = word.text.as_str();
            while !remaining.is_empty() {
                let (chunk, consumed_bytes) = take_display_width_prefix(remaining, width);
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                    current_len = 0;
                }
                let chunk_width = terminal_display_width(&chunk);
                if consumed_bytes == remaining.len() {
                    current.push(Word {
                        text: chunk,
                        style: word.style,
                    });
                    current_len = chunk_width;
                } else {
                    lines.push(vec![Word {
                        text: chunk,
                        style: word.style,
                    }]);
                }
                remaining = &remaining[consumed_bytes..];
            }
            continue;
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

/// Wraps table-cell words to `width`, splitting an unbreakable word when
/// necessary. Spaces between original words are preserved, while fragments
/// from the same word remain adjacent.
pub(super) fn wrap_table_words(words: &[Word], width: usize) -> Vec<Vec<TableWord>> {
    let width = width.max(1);
    let mut lines: Vec<Vec<TableWord>> = Vec::new();
    let mut current: Vec<TableWord> = Vec::new();
    let mut current_len = 0usize;

    for word in words {
        let mut remaining = word.text.as_str();
        let mut needs_space = !current.is_empty();

        while !remaining.is_empty() {
            let space_len = usize::from(needs_space && !current.is_empty());
            if !current.is_empty() && current_len + space_len >= width {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
                needs_space = false;
            }

            let space_len = usize::from(needs_space && !current.is_empty());
            let available = width.saturating_sub(current_len + space_len).max(1);
            let (chunk, consumed_bytes) = take_display_width_prefix(remaining, available);

            let space_before = needs_space && !current.is_empty();
            let chunk_width = terminal_display_width(&chunk);
            current.push(TableWord {
                text: chunk,
                style: word.style,
                space_before,
            });
            current_len += space_len + chunk_width;
            needs_space = false;
            remaining = &remaining[consumed_bytes..];

            if !remaining.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

pub(super) fn take_display_width_prefix(text: &str, width: usize) -> (String, usize) {
    let mut consumed_bytes = 0usize;
    let mut consumed_width = 0usize;

    for (byte_offset, character) in text.char_indices() {
        let character_width = terminal_display_width(&character.to_string());
        if consumed_bytes > 0 && consumed_width + character_width > width {
            break;
        }
        consumed_bytes = byte_offset + character.len_utf8();
        consumed_width += character_width;
        if consumed_width >= width {
            break;
        }
    }

    if consumed_bytes == 0 {
        let Some(character) = text.chars().next() else {
            return (String::new(), 0);
        };
        (character.to_string(), character.len_utf8())
    } else {
        (text[..consumed_bytes].to_string(), consumed_bytes)
    }
}
