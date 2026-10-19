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

//! Pure in-memory application of an edit to file content.

use super::super::ToolError;
use super::result::EditApplied;
use super::{EditFileArgs, EditMode};

/// Apply the edit to `original_content`, returning the patched string and outcome metadata.
pub(super) fn apply_edit(
    original_content: &str,
    args: &EditFileArgs,
) -> Result<(String, EditApplied), ToolError> {
    let path = &args.path;
    match &args.mode {
        EditMode::SearchReplace => {
            let old_string = args.old_string.as_deref().ok_or_else(|| {
                ToolError::failed("edit", "`old_string` is required for `search_replace` mode")
            })?;
            let new_string = &args.new_string;
            if !original_content.contains(old_string) {
                return Err(ToolError::failed(
                    "edit",
                    format!("`old_string` not found in '{path}'"),
                ));
            }
            if args.replace_all {
                let replacements_made = original_content.matches(old_string).count();
                let patched = original_content.replace(old_string, new_string.as_str());
                Ok((patched, EditApplied::SearchReplace { replacements_made }))
            } else {
                let patched = original_content.replacen(old_string, new_string.as_str(), 1);
                Ok((
                    patched,
                    EditApplied::SearchReplace {
                        replacements_made: 1,
                    },
                ))
            }
        }
        EditMode::LineRangeReplace => {
            let start_line = args.start_line.ok_or_else(|| {
                ToolError::failed(
                    "edit",
                    "`start_line` is required for `line_range_replace` mode",
                )
            })?;
            let end_line = args.end_line.ok_or_else(|| {
                ToolError::failed(
                    "edit",
                    "`end_line` is required for `line_range_replace` mode",
                )
            })?;
            let new_string = &args.new_string;
            if start_line > end_line {
                return Err(ToolError::failed(
                    "edit",
                    format!("`start_line` ({start_line}) must not exceed `end_line` ({end_line})"),
                ));
            }
            let mut lines: Vec<&str> = original_content.split('\n').collect();
            let trailing_newline = original_content.ends_with('\n');
            if trailing_newline && lines.last() == Some(&"") {
                lines.pop();
            }
            let total_lines = lines.len();
            if start_line == 0 || start_line > total_lines || end_line > total_lines {
                return Err(ToolError::failed(
                    "edit",
                    format!(
                        "line range {start_line}–{end_line} is out of bounds (file has {total_lines} lines)"
                    ),
                ));
            }
            let start_index = start_line - 1;
            let end_index = end_line;
            let mut result_lines: Vec<&str> = lines[..start_index].to_vec();
            if !new_string.is_empty() {
                for line in new_string.split('\n') {
                    result_lines.push(line);
                }
            }
            result_lines.extend_from_slice(&lines[end_index..]);
            let mut result = result_lines.join("\n");
            if trailing_newline {
                result.push('\n');
            }
            Ok((
                result,
                EditApplied::LineRangeReplace {
                    start_line,
                    end_line,
                },
            ))
        }
    }
}
