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

//! Selected line and character windows over validated UTF-8 chunks.

use super::{FileEncoding, ReadResult, READ_OUTPUT_LIMIT_BYTES};

#[derive(Default)]
pub(super) struct SourceLineCounter {
    newline_count: usize,
    last_character: Option<char>,
}

impl SourceLineCounter {
    pub(super) fn observe(&mut self, text: &str) {
        self.newline_count += text.bytes().filter(|byte| *byte == b'\n').count();
        self.last_character = text.chars().last().or(self.last_character);
    }

    pub(super) fn count(&self) -> usize {
        self.newline_count
            + usize::from(
                self.last_character
                    .is_some_and(|character| character != '\n'),
            )
    }
}

/// Assemble the selected line window of a `lines` read while the file streams
/// by.
///
/// Mirrors `text.split_terminator('\n')` + `join("\n")`: a trailing newline
/// does not create a final empty line, `"\n"` is one empty line, CRLF endings
/// keep their `'\r'`, and exactly one `'\n'` separates selected lines in the
/// joined content.
pub(super) struct LineWindowScanner {
    /// 0-based index of the first selected line.
    window_start: usize,
    /// 0-based index after the last selected line (`None` = to EOF).
    window_end: Option<usize>,
    /// 0-based index of the line currently being scanned.
    line_index: usize,
    /// Number of selected lines already started.
    collected_lines: usize,
    /// `true` while no byte of the current line has been seen yet.
    at_line_start: bool,
    /// `true` while the current line belongs to the selected window.
    collecting: bool,
    /// `true` once the current line is known to exist and has been counted.
    line_confirmed: bool,
    content: String,
    /// `true` when [`READ_OUTPUT_LIMIT_BYTES`] cut off selected content.
    truncated: bool,
}

impl LineWindowScanner {
    pub(super) fn new(offset: Option<usize>, limit: Option<usize>) -> Self {
        let window_start = offset.unwrap_or(0);
        Self {
            window_start,
            window_end: limit.map(|limit| window_start.saturating_add(limit)),
            line_index: 0,
            collected_lines: 0,
            at_line_start: true,
            collecting: false,
            line_confirmed: false,
            content: String::new(),
            truncated: false,
        }
    }

    /// `true` once every selected line has been collected — nothing more can
    /// be added, so callers may skip to the validation scan.
    fn window_completed(&self) -> bool {
        self.window_end
            .is_some_and(|window_end| self.line_index >= window_end)
    }

    fn in_window(&self) -> bool {
        self.line_index >= self.window_start
            && self
                .window_end
                .is_none_or(|window_end| self.line_index < window_end)
    }

    /// Count the current line and separate it from the previously collected
    /// one. A line exists once it has content or its `'\n'` terminator is
    /// seen; the empty remainder of a file ending in `'\n'` is not a line.
    ///
    /// Returns `false` when the output cap leaves no room for the separator.
    fn confirm_line(&mut self) -> bool {
        if self.line_confirmed {
            return true;
        }
        if self.collected_lines > 0 {
            if self.content.len() >= READ_OUTPUT_LIMIT_BYTES {
                self.truncated = true;
                return false;
            }
            self.content.push('\n');
        }
        self.line_confirmed = true;
        self.collected_lines += 1;
        true
    }

    /// Append as much of `segment` as fits the output cap, cutting on a
    /// UTF-8 char boundary. Returns `false` when content was left behind.
    fn push_segment(&mut self, segment: &str) -> bool {
        let remaining = READ_OUTPUT_LIMIT_BYTES.saturating_sub(self.content.len());
        let mut take = segment.len().min(remaining);
        while take > 0 && !segment.is_char_boundary(take) {
            take -= 1;
        }
        self.content.push_str(&segment[..take]);
        if take < segment.len() {
            self.truncated = true;
            return false;
        }
        true
    }

    /// Consume the next validated UTF-8 range of the file, keeping only the
    /// selected window. Once the output cap cut selected content the window
    /// stops growing, while the caller's validation scan runs to EOF.
    pub(super) fn process(&mut self, text: &str) {
        if self.window_completed() || self.truncated {
            return;
        }
        let mut rest = text;
        loop {
            let (segment, tail) = match rest.find('\n') {
                Some(index) => (&rest[..index], Some(&rest[index + 1..])),
                None => (rest, None),
            };
            let starts_line = self.at_line_start;
            if starts_line {
                self.at_line_start = false;
                self.collecting = self.in_window();
                self.line_confirmed = false;
            }
            if self.collecting && (!segment.is_empty() || tail.is_some()) && !self.confirm_line() {
                return;
            }
            if self.collecting && !segment.is_empty() && !self.push_segment(segment) {
                return;
            }
            match tail {
                Some(tail) => {
                    self.line_index += 1;
                    self.at_line_start = true;
                    self.collecting = false;
                    rest = tail;
                }
                None => {
                    if starts_line && segment.is_empty() {
                        // Nothing of this line was consumed — stay at its start.
                        self.at_line_start = true;
                        self.collecting = false;
                    }
                    return;
                }
            }
        }
    }

    pub(super) fn finish(
        self,
        path: String,
        source_size_bytes: usize,
        source_lines: usize,
    ) -> ReadResult {
        // Lines seen so far: completed lines plus a pending final line that
        // was not terminated by `'\n'`. At EOF this is the file's line count;
        // after the output cap stopped collection it is a lower bound that is
        // already ≥ the window start, so the `min` below still clamps to the
        // window start.
        let lines_seen = self.line_index + usize::from(!self.at_line_start);
        let starting_line = Some(self.window_start.min(lines_seen));
        let content = self.content;
        ReadResult {
            read_bytes: content.len(),
            source_size_bytes,
            source_lines: Some(source_lines),
            content,
            path,
            starting_line,
            encoding: FileEncoding::Utf8,
            text_preview: None,
            media_type: None,
            truncated: self.truncated,
        }
    }
}

/// Assemble the selected character window of a `chars` read while the file
/// streams by, counting UTF-8 code points from the start of the file.
pub(super) struct CharWindowScanner {
    /// 0-based index of the first selected character.
    window_start: usize,
    /// 0-based index after the last selected character (`None` = to EOF).
    window_end: Option<usize>,
    /// Number of code points seen so far.
    char_index: usize,
    content: String,
    /// `true` when [`READ_OUTPUT_LIMIT_BYTES`] cut off selected content.
    truncated: bool,
}

impl CharWindowScanner {
    pub(super) fn new(offset: Option<usize>, limit: Option<usize>) -> Self {
        let window_start = offset.unwrap_or(0);
        Self {
            window_start,
            window_end: limit.map(|limit| window_start.saturating_add(limit)),
            char_index: 0,
            content: String::new(),
            truncated: false,
        }
    }

    /// `true` once every selected character has been collected.
    fn window_completed(&self) -> bool {
        self.window_end
            .is_some_and(|window_end| self.char_index >= window_end)
    }

    /// Consume the next validated UTF-8 range of the file, keeping only the
    /// selected characters. Once the output cap cut selected content the
    /// window stops growing, while the caller's validation scan runs to EOF.
    pub(super) fn process(&mut self, text: &str) {
        if self.window_completed() || self.truncated {
            return;
        }
        for character in text.chars() {
            if self.char_index >= self.window_start
                && self
                    .window_end
                    .is_none_or(|window_end| self.char_index < window_end)
            {
                if self.content.len() + character.len_utf8() > READ_OUTPUT_LIMIT_BYTES {
                    self.truncated = true;
                    return;
                }
                self.content.push(character);
            }
            self.char_index += 1;
        }
    }

    pub(super) fn finish(
        self,
        path: String,
        source_size_bytes: usize,
        source_lines: usize,
    ) -> ReadResult {
        let content = self.content;
        ReadResult {
            read_bytes: content.len(),
            source_size_bytes,
            source_lines: Some(source_lines),
            content,
            path,
            starting_line: Some(0),
            encoding: FileEncoding::Utf8,
            text_preview: None,
            media_type: None,
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_scanners_match_whole_text_for_every_character_boundary() {
        for text in [
            "",
            "\n",
            "\n\n",
            "one\ntwo\n",
            "one\r\n\r\nlast",
            "a\u{e9}\n\u{1f600}z",
        ] {
            for split in (0..=text.len()).filter(|split| text.is_char_boundary(*split)) {
                for offset in [None, Some(0), Some(1), Some(2), Some(usize::MAX)] {
                    for limit in [None, Some(0), Some(1), Some(3), Some(usize::MAX)] {
                        let mut lines = LineWindowScanner::new(offset, limit);
                        let mut characters = CharWindowScanner::new(offset, limit);
                        let mut source_lines = SourceLineCounter::default();
                        for chunk in [&text[..split], "", &text[split..]] {
                            lines.process(chunk);
                            characters.process(chunk);
                            source_lines.observe(chunk);
                        }
                        let expected_lines = text
                            .split_terminator('\n')
                            .skip(offset.unwrap_or(0))
                            .take(limit.unwrap_or(usize::MAX))
                            .collect::<Vec<_>>()
                            .join("\n");
                        let expected_characters = text
                            .chars()
                            .skip(offset.unwrap_or(0))
                            .take(limit.unwrap_or(usize::MAX))
                            .collect::<String>();
                        assert_eq!(source_lines.count(), text.lines().count());
                        let line_result =
                            lines.finish("fixture".into(), text.len(), source_lines.count());
                        let character_result =
                            characters.finish("fixture".into(), text.len(), source_lines.count());
                        assert_eq!(
                            line_result.content, expected_lines,
                            "split={split} offset={offset:?} limit={limit:?}"
                        );
                        assert_eq!(
                            character_result.content, expected_characters,
                            "split={split} offset={offset:?} limit={limit:?}"
                        );
                        assert_eq!(line_result.source_lines, Some(text.lines().count()));
                        assert_eq!(character_result.source_size_bytes, text.len());
                        assert!(!line_result.truncated && !character_result.truncated);
                    }
                }
            }
        }
    }

    #[test]
    fn window_caps_keep_complete_characters_and_full_source_counts() {
        let text = format!(
            "{}\u{1f600}\nlast\n",
            "a".repeat(READ_OUTPUT_LIMIT_BYTES - 1)
        );
        let mut lines = LineWindowScanner::new(None, None);
        let mut characters = CharWindowScanner::new(None, None);
        let mut source_lines = SourceLineCounter::default();
        for chunk in [
            &text[..READ_OUTPUT_LIMIT_BYTES - 1],
            &text[READ_OUTPUT_LIMIT_BYTES - 1..],
        ] {
            lines.process(chunk);
            characters.process(chunk);
            source_lines.observe(chunk);
        }
        let line_result = lines.finish("fixture".into(), text.len(), source_lines.count());
        let character_result =
            characters.finish("fixture".into(), text.len(), source_lines.count());
        for result in [line_result, character_result] {
            assert_eq!(result.content, "a".repeat(READ_OUTPUT_LIMIT_BYTES - 1));
            assert!(result.truncated);
            assert_eq!(result.source_size_bytes, text.len());
            assert_eq!(result.source_lines, Some(2));
        }
    }
}
