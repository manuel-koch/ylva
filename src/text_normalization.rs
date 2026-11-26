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

/// Trim the ends and collapse each run of whitespace to one space.
pub(crate) fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Mark each carriage return and newline visibly, and replace tabs with spaces.
pub(crate) fn sanitize_inline_title(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\n' | '\r' => '↵',
            '\t' => ' ',
            character => character,
        })
        .collect()
}

/// Replace controls and Unicode line and paragraph separators with spaces.
pub(crate) fn single_line_display(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() || character == '\u{2028}' || character == '\u{2029}' {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// Drop exactly one leading UTF-8 byte-order mark, if present.
pub(crate) fn strip_byte_order_mark(contents: &str) -> &str {
    contents.strip_prefix('\u{feff}').unwrap_or(contents)
}
