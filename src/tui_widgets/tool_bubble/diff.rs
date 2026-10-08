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

//! Unified-diff hunk coordinates.

/// Parse a unified-diff hunk header of the form `@@ -a[,b] +c[,d] @@`.
///
/// Returns `(old_start, old_count, new_start, new_count)` on success.
pub(super) fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_part, rest) = rest.split_once(' ')?;
    let (old_start, old_count) = parse_hunk_range(old_part)?;
    let rest = rest.strip_prefix('+')?;
    let (new_part, _) = rest.split_once(' ')?;
    let (new_start, new_count) = parse_hunk_range(new_part)?;
    Some((old_start, old_count, new_start, new_count))
}

/// Parse `"a,b"` or `"a"` into `(start, count)`. Count defaults to 1 when absent.
fn parse_hunk_range(s: &str) -> Option<(usize, usize)> {
    if let Some((start, count)) = s.split_once(',') {
        Some((start.parse().ok()?, count.parse().ok()?))
    } else {
        Some((s.parse().ok()?, 1))
    }
}

#[cfg(test)]
mod tests {
    // ── parse_hunk_header ──────────────────────────────────────────────────

    #[test]
    fn parse_hunk_header_full_form() {
        use super::parse_hunk_header;
        let result = parse_hunk_header("@@ -10,5 +12,3 @@");
        assert_eq!(result, Some((10, 5, 12, 3)));
    }

    #[test]
    fn parse_hunk_header_count_omitted_defaults_to_one() {
        use super::parse_hunk_header;
        let result = parse_hunk_header("@@ -1 +1 @@");
        assert_eq!(result, Some((1, 1, 1, 1)));
    }

    #[test]
    fn parse_hunk_header_mixed_omission() {
        use super::parse_hunk_header;
        let result = parse_hunk_header("@@ -3,2 +5 @@");
        assert_eq!(result, Some((3, 2, 5, 1)));
    }

    #[test]
    fn parse_hunk_header_returns_none_for_non_header_line() {
        use super::parse_hunk_header;
        assert!(parse_hunk_header("+added line").is_none());
        assert!(parse_hunk_header("-removed line").is_none());
        assert!(parse_hunk_header(" context line").is_none());
    }

    #[test]
    fn parse_hunk_header_with_trailing_context_label() {
        use super::parse_hunk_header;
        // similar::TextDiff may include a trailing function name after the second @@.
        let result = parse_hunk_header("@@ -7,4 +7,4 @@ fn example() {");
        assert_eq!(result, Some((7, 4, 7, 4)));
    }
}
