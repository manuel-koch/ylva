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

//! Regex search over the filesystem: binary detection, walking and line collection.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::super::{SearchResultLine, SearchResultSection, ToolError};
use super::result::SearchResult;
use super::{
    SearchInputArgs, BINARY_PROBE_BYTES, SEARCH_OUTPUT_LIMIT_BYTES, SEARCH_OUTPUT_LIMIT_LINES,
};

/// Returns `true` when the file at `path` appears to be binary by checking
/// whether the first [`BINARY_PROBE_BYTES`] bytes contain a null byte.
pub(super) fn is_binary_file(path: &Path) -> bool {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut reader = std::io::BufReader::new(file);
    let mut probe = vec![0u8; BINARY_PROBE_BYTES];
    let bytes_read = match reader.read(&mut probe) {
        Ok(n) => n,
        Err(_) => return false,
    };
    probe[..bytes_read].contains(&0u8)
}

/// Record a single output line against the line-count and byte-count caps.
/// `content_len` is the byte length of the line's content about to be
/// recorded (checked before it is added, so a single oversized line cannot
/// blow past [`SEARCH_OUTPUT_LIMIT_BYTES`]).
/// Returns `false` when either cap is exceeded and the line must be discarded.
pub(super) fn record_line(
    line_count: &mut usize,
    byte_count: &mut usize,
    content_len: usize,
    truncated: &mut bool,
) -> bool {
    if *truncated {
        return false;
    }
    if *line_count >= SEARCH_OUTPUT_LIMIT_LINES
        || *byte_count + content_len > SEARCH_OUTPUT_LIMIT_BYTES
    {
        *truncated = true;
        return false;
    }
    *line_count += 1;
    *byte_count += content_len;
    true
}

pub(super) async fn search_files(args: SearchInputArgs) -> Result<SearchResult, ToolError> {
    tracing::debug!(pattern = %args.pattern, "searching files");

    let regex = {
        let mut builder = regex::RegexBuilder::new(&args.pattern);
        builder.case_insensitive(!args.case_sensitive);
        builder.build().map_err(|error| {
            ToolError::failed(
                "search",
                format!("invalid pattern '{}': {error}", args.pattern),
            )
        })?
    };

    if args.files_with_matches && args.only_matches {
        return Err(ToolError::failed(
            "search",
            "'files_with_matches' and 'only_matches' are mutually exclusive",
        ));
    }

    let glob_pattern: Option<glob::Pattern> = match &args.glob {
        Some(glob_str) => {
            if glob_str.split('/').any(|seg| seg == "..") {
                return Err(ToolError::failed(
                    "search",
                    "glob must not contain '..' segments",
                ));
            }
            Some(glob::Pattern::new(glob_str).map_err(|error| {
                ToolError::failed("search", format!("invalid glob '{}': {error}", glob_str))
            })?)
        }
        None => None,
    };

    let search_root: PathBuf = match &args.path {
        Some(path_str) => PathBuf::from(path_str),
        None => PathBuf::from("."),
    };

    // Use a blocking task for the synchronous ignore + regex walk.
    let context_lines = args.context_lines;
    let files_with_matches = args.files_with_matches;
    let only_matches = args.only_matches;
    let result = tokio::task::spawn_blocking(move || {
        search_blocking(
            &regex,
            &search_root,
            context_lines,
            glob_pattern.as_ref(),
            files_with_matches,
            only_matches,
        )
    })
    .await
    .map_err(|error| ToolError::failed("search", format!("search task failed: {error}")))?;

    tracing::debug!(
        files = result.sections.len(),
        truncated = result.truncated,
        "search complete"
    );
    Ok(result)
}

/// Blocking search logic executed inside [`tokio::task::spawn_blocking`].
pub(super) fn search_blocking(
    regex: &regex::Regex,
    root: &Path,
    context_lines: usize,
    glob_pattern: Option<&glob::Pattern>,
    files_with_matches: bool,
    only_matches: bool,
) -> SearchResult {
    let mut sections: Vec<SearchResultSection> = Vec::new();
    let mut line_count: usize = 0;
    let mut byte_count: usize = 0;
    let mut truncated = false;

    let walker = ignore::WalkBuilder::new(root).git_ignore(true).build();

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        if truncated {
            break;
        }

        // Only search regular files.
        let file_type = match entry.file_type() {
            Some(ft) => ft,
            None => continue,
        };
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();

        // Apply glob filter: match against path relative to the search root so
        // that patterns like `**/*.rs` work regardless of the root's form.
        if let Some(pattern) = glob_pattern {
            let match_path = path.strip_prefix(root).unwrap_or(path);
            let match_options = glob::MatchOptions {
                case_sensitive: true,
                require_literal_separator: true,
                require_literal_leading_dot: false,
            };
            if !pattern.matches_path_with(match_path, match_options) {
                continue;
            }
        }

        if is_binary_file(path) {
            tracing::trace!(path = %path.display(), "skipping binary file");
            continue;
        }

        let content = match std::fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                tracing::warn!(path = %path.display(), error = %error, "could not read file");
                continue;
            }
        };

        let file_path_str = path.display().to_string();
        let lines: Vec<&str> = content.lines().collect();

        // Collect 1-based line numbers of matching lines.
        let match_line_numbers: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| regex.is_match(line))
            .map(|(index, _)| index + 1)
            .collect();

        if match_line_numbers.is_empty() {
            continue;
        }

        let file_extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase());

        if files_with_matches {
            // Only record the file path; skip all line content.
            if !record_line(
                &mut line_count,
                &mut byte_count,
                file_path_str.len(),
                &mut truncated,
            ) {
                break;
            }
            sections.push(SearchResultSection {
                file_path: file_path_str,
                file_extension: None,
                lines: vec![],
            });
            continue;
        }

        if only_matches {
            // Emit one SearchResultLine per regex match occurrence (grep -o).
            // Context lines are ignored.
            let mut search_result_lines: Vec<SearchResultLine> = Vec::new();
            'outer: for &lineno in &match_line_numbers {
                if let Some(line) = lines.get(lineno - 1) {
                    for matched in regex.find_iter(line) {
                        let match_text = matched.as_str().to_string();
                        if !record_line(
                            &mut line_count,
                            &mut byte_count,
                            match_text.len(),
                            &mut truncated,
                        ) {
                            break 'outer;
                        }
                        search_result_lines.push(SearchResultLine {
                            line_number: lineno,
                            is_match: true,
                            content: match_text,
                        });
                    }
                }
            }
            if !search_result_lines.is_empty() {
                sections.push(SearchResultSection {
                    file_path: file_path_str,
                    file_extension,
                    lines: search_result_lines,
                });
            }
            continue;
        }

        // Default mode: full lines with optional context.

        // Build the set of line numbers to emit (matches + context).
        // Each entry: (line_number_1based, is_match).
        let mut output_lines: Vec<(usize, bool)> = Vec::new();
        for &match_lineno in &match_line_numbers {
            let context_start = match_lineno.saturating_sub(context_lines);
            let context_end = (match_lineno + context_lines).min(lines.len());
            for lineno in context_start..=context_end {
                if lineno == 0 {
                    continue;
                }
                let is_match = match_line_numbers.contains(&lineno);
                // Avoid duplicate entries when context ranges overlap.
                if output_lines.last().map(|(n, _)| *n) != Some(lineno) {
                    output_lines.push((lineno, is_match));
                }
            }
        }
        // Ensure strictly ascending order (context ranges from adjacent matches
        // may produce duplicates; deduplicate while preserving order).
        output_lines.dedup_by_key(|(lineno, _)| *lineno);
        output_lines.sort_by_key(|(lineno, _)| *lineno);
        output_lines.dedup_by_key(|(lineno, _)| *lineno);

        let mut search_result_lines = Vec::new();
        for (lineno, is_match) in output_lines {
            if let Some(line) = lines.get(lineno - 1) {
                let search_result_line = SearchResultLine {
                    line_number: lineno,
                    is_match,
                    content: (*line).to_string(),
                };
                if !record_line(
                    &mut line_count,
                    &mut byte_count,
                    search_result_line.content.len(),
                    &mut truncated,
                ) {
                    break;
                }
                search_result_lines.push(search_result_line);
            }
        }

        if !search_result_lines.is_empty() {
            sections.push(SearchResultSection {
                file_path: file_path_str,
                file_extension,
                lines: search_result_lines,
            });
        }
    }

    SearchResult::new(sections, truncated, files_with_matches)
}
