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

//! Plain-text wrapping helpers shared by widgets.

/// Splits `text` into lines of exactly `width` characters, keeping every
/// character (including spaces), so a caret index maps to a fixed cell. A
/// final empty line gives the caret room behind a full last line.
pub(crate) fn character_wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let characters: Vec<char> = text.chars().collect();
    let mut lines: Vec<String> = characters
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect();
    if characters.len().is_multiple_of(width) {
        lines.push(String::new());
    }
    lines
}

/// Greedy word wrap of `text` to `width` columns; never returns no lines.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_width = word.chars().count();
        let current_width = current.chars().count();
        if !current.is_empty() && current_width + 1 + word_width > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        while current.chars().count() > width {
            let rest: String = current.chars().skip(width).collect();
            lines.push(current.chars().take(width).collect());
            current = rest;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

pub(crate) fn indented(text: &str, first: &str, width: usize) -> Vec<String> {
    indent_lines(
        wrap(text, width.saturating_sub(first.chars().count())),
        first,
    )
}

pub(crate) fn indent_lines(lines: Vec<String>, first: &str) -> Vec<String> {
    let rest = " ".repeat(first.chars().count());
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| format!("{}{line}", if index == 0 { first } else { &rest }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_wrap_keeps_spaces_and_reserves_a_caret_line_after_a_full_row() {
        assert_eq!(character_wrap("ab cd", 2), vec!["ab", " c", "d"]);
        assert_eq!(character_wrap("abcd", 2), vec!["ab", "cd", ""]);
        assert_eq!(character_wrap("", 3), vec![""]);
    }

    #[test]
    fn wrap_breaks_at_words_and_splits_overlong_words() {
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc"]);
        assert_eq!(wrap("abcdefg", 3), vec!["abc", "def", "g"]);
        assert_eq!(wrap("", 3), vec![String::new()]);
    }
}
