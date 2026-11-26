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

use ratatui::text::{Line, Span};

pub(crate) fn terminal_display_width(text: &str) -> usize {
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

#[cfg(test)]
mod tests {
    use super::slice_by_display_columns;

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
}
