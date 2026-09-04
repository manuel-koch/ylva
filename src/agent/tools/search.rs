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

//! `search` tool: text search using `ignore` + `regex` crates; no external
//! binary required.
//!
//! Results are formatted in grep style (`filepath:lineno: content` for
//! matches, `filepath-lineno- content` for context lines). One
//! [`SearchResultSection`] is emitted per matching file. Total output is capped
//! at [`SEARCH_OUTPUT_LIMIT_BYTES`].

use std::io::Read;
use std::path::{Path, PathBuf};

use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;

use super::{
    SearchResultLine, SearchResultSection, Section, Tool, ToolCall, ToolCallResult, ToolError,
    DEFAULT_TIMEOUT_SECS,
};
use crate::sandbox::{ApprovalCheck, SandboxContext};

/// Maximum total bytes in the combined search output. Output beyond this cap
/// is discarded; the result's `metadata()` will carry
/// `truncated` and `truncated_at_bytes`.
pub const SEARCH_OUTPUT_LIMIT_BYTES: usize = 100 * 1024;

/// Number of bytes inspected to detect binary files (null-byte heuristic).
const BINARY_PROBE_BYTES: usize = 8 * 1024;

// ── Argument schema ───────────────────────────────────────────────────────────

/// Arguments accepted by the `search` tool.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct SearchInputArgs {
    /// Regular expression pattern to search for.
    pub pattern: String,
    /// Directory (or file) to search within. Defaults to the current working
    /// directory when omitted.
    pub path: Option<String>,
    /// Optional glob pattern (e.g. `**/*.rs`, `*.txt`) that restricts which
    /// files are searched. Matched against each file's path relative to the
    /// search root. When omitted, all non-binary files under `path` are
    /// searched.
    pub glob: Option<String>,
    /// When `true`, the pattern is matched case-sensitively. Defaults to
    /// `false` (case-insensitive search).
    #[serde(default)]
    pub case_sensitive: bool,
    /// Number of lines of context to include before and after each match.
    /// Defaults to `0` (no context). Ignored when `only_matches` is `true`.
    #[serde(default)]
    pub context_lines: usize,
    /// When `true`, return only the paths of files that contain at least one
    /// match — no line content. Mutually exclusive with `only_matches`.
    #[serde(default)]
    pub files_with_matches: bool,
    /// When `true`, return only the substrings matched by the pattern (one
    /// entry per match occurrence, like `grep -o`). Context lines are ignored.
    /// Mutually exclusive with `files_with_matches`.
    #[serde(default)]
    pub only_matches: bool,
}

// ── Result ────────────────────────────────────────────────────────────────────

/// Structured metadata for a successful `search` invocation.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchResultMetadata {
    /// Number of files containing at least one match.
    pub matching_files: usize,
    /// Total matching lines across all files (absent in files-with-matches mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matching_lines: Option<usize>,
    /// Present and `true` when output exceeded the byte cap and was truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// Byte offset at which truncation began (only present when `truncated` is `true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated_at_bytes: Option<usize>,
}

/// Successful output from a `search` invocation.
#[derive(Debug)]
pub struct SearchResult {
    /// All matching sections, one per file that contained at least one match.
    pub sections: Vec<SearchResultSection>,
    /// `true` when the 100 KB output cap was reached and some output was
    /// discarded.
    pub truncated: bool,
    /// Byte count at which truncation occurred (only `Some` when `truncated`).
    pub truncated_at_bytes: Option<usize>,
    /// `true` when the search was run in files-with-matches mode (no line
    /// content collected, sections carry only file paths).
    pub files_with_matches: bool,
    /// Pre-formatted text for the LLM.
    text: String,
}

impl SearchResult {
    fn new(
        sections: Vec<SearchResultSection>,
        truncated: bool,
        truncated_at_bytes: Option<usize>,
        files_with_matches: bool,
    ) -> Self {
        let text = if files_with_matches {
            sections
                .iter()
                .map(|section| section.file_path.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            sections
                .iter()
                .flat_map(|section| {
                    section.lines.iter().map(|line| {
                        if line.is_match {
                            format!(
                                "{}:{}: {}",
                                section.file_path, line.line_number, line.content
                            )
                        } else {
                            format!(
                                "{}-{}- {}",
                                section.file_path, line.line_number, line.content
                            )
                        }
                    })
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        SearchResult {
            sections,
            truncated,
            truncated_at_bytes,
            files_with_matches,
            text,
        }
    }
}

impl ToolCallResult for SearchResult {
    fn text(&self) -> String {
        self.text.clone()
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        let matching_lines: usize = self
            .sections
            .iter()
            .flat_map(|s| s.lines.iter())
            .filter(|l| l.is_match)
            .count();
        let metadata = SearchResultMetadata {
            matching_files: self.sections.len(),
            matching_lines: (!self.files_with_matches).then_some(matching_lines),
            truncated: self.truncated.then_some(true),
            truncated_at_bytes: self.truncated.then_some(self.truncated_at_bytes).flatten(),
        };
        serde_json::to_value(metadata).ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schemars::schema_for!(SearchResultMetadata))
    }

    fn content_sections(&self) -> Vec<Section> {
        if self.files_with_matches {
            // Return a single plain-text section listing matched paths.
            if self.sections.is_empty() {
                return vec![];
            }
            let paths = self
                .sections
                .iter()
                .map(|section| section.file_path.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            return vec![Section::Content(super::ContentSection {
                label: None,
                content: paths,
                file_extension: None,
                line_number_offset: None,
            })];
        }
        self.sections
            .iter()
            .map(|section| Section::SearchResult(section.clone()))
            .collect()
    }
}

// ── ToolCall ──────────────────────────────────────────────────────────────────

/// Invocation object for a single `search` tool call.
pub struct SearchCall {
    id: uuid::Uuid,
    raw_arguments: Value,
    timeout_secs: u64,
}

impl ToolCall for SearchCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }

    fn name(&self) -> &str {
        SearchTool::NAME
    }

    fn arguments(&self) -> &Value {
        &self.raw_arguments
    }

    fn argument_order(&self, key: &str) -> usize {
        SearchTool::default().argument_order(key)
    }

    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, Value)> {
        self.raw_arguments
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        let serialized = serde_json::to_string(&self.raw_arguments).unwrap_or_default();
        let span = tracing::info_span!("tool_args", args = %serialized);
        Box::pin(
            async move {
                let args = serde_json::from_value::<SearchInputArgs>(self.raw_arguments).map_err(
                    |error| ToolError::failed("search", format!("invalid arguments: {error}")),
                )?;
                search_files(args)
                    .await
                    .map(|result| -> Box<dyn ToolCallResult> { Box::new(result) })
            }
            .instrument(span),
        )
    }
}

// ── Tool definition ───────────────────────────────────────────────────────────

/// The `search` tool: text search over a directory tree using `regex` and
/// `ignore`.
#[derive(Debug)]
pub struct SearchTool {
    timeout_secs: u64,
    sandbox_context: SandboxContext,
}

impl SearchTool {
    pub const NAME: &'static str = "search";

    pub fn new(sandbox_context: SandboxContext) -> Self {
        SearchTool {
            timeout_secs: 0,
            sandbox_context,
        }
    }

    pub fn with_timeout(timeout_secs: u64, sandbox_context: SandboxContext) -> Self {
        SearchTool {
            timeout_secs,
            sandbox_context,
        }
    }
}

impl Default for SearchTool {
    fn default() -> Self {
        use crate::sandbox::Sandbox;
        SearchTool::new(SandboxContext::new(std::sync::Arc::new(
            std::sync::RwLock::new(Sandbox::new()),
        )))
    }
}

impl Tool for SearchTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Search for a regular-expression pattern in files within a directory tree. \
        Results are formatted in grep style: `filepath:lineno: content` for matches \
        and `filepath-lineno- content` for context lines. Binary files are skipped \
        automatically. Total output is capped at 100 KB. \
        Optional `glob` (e.g. `**/*.rs`) restricts which files are searched. \
        Set `files_with_matches: true` to return only file paths (no line content). \
        Set `only_matches: true` to return only the matched substrings (one per \
        occurrence, like `grep -o`). `files_with_matches` and `only_matches` are \
        mutually exclusive."
    }

    fn input_schema(&self) -> schemars::schema::RootSchema {
        schema_for!(SearchInputArgs)
    }

    fn path_like_arguments(&self) -> &'static [&'static str] {
        &["path"]
    }

    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        match arguments.get("path").and_then(|v| v.as_str()) {
            Some(path_str) if !path_str.is_empty() => {
                let sandbox_verdict = self.sandbox_context.check_path(Path::new(path_str));
                let approval = match sandbox_verdict {
                    None => ApprovalCheck::AutoApproved,
                    Some(verdict) => verdict,
                };
                tracing::debug!(
                    tool = "search",
                    path = path_str,
                    sandbox = match &approval {
                        ApprovalCheck::AutoApproved => "abstain→auto-approved",
                        ApprovalCheck::AutoDenied { .. } => "auto-denied",
                        ApprovalCheck::UserApprovalRequired => "user-approval-required",
                    },
                    "tool approval decision"
                );
                approval
            }
            // No explicit path → defaults to cwd (inside workspace) → auto-approved.
            _ => ApprovalCheck::AutoApproved,
        }
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_TIMEOUT_SECS
        } else {
            self.timeout_secs
        }
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn create_call(&self, id: uuid::Uuid, arguments: Value) -> Box<dyn ToolCall> {
        Box::new(SearchCall {
            id,
            raw_arguments: arguments,
            timeout_secs: self.timeout_secs(),
        })
    }
}

// ── Core search logic ─────────────────────────────────────────────────────────

/// Returns `true` when the file at `path` appears to be binary by checking
/// whether the first [`BINARY_PROBE_BYTES`] bytes contain a null byte.
fn is_binary_file(path: &Path) -> bool {
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

/// Format a single output line, applying the cap and recording truncation.
/// Returns `false` when the cap is exceeded and the line was not appended.
fn append_line(
    buffer: &mut String,
    line: &str,
    total_bytes: &mut usize,
    truncated: &mut bool,
    truncated_at_bytes: &mut Option<usize>,
) -> bool {
    if *truncated {
        return false;
    }
    let needed = line.len() + 1; // +1 for '\n'
    if *total_bytes + needed > SEARCH_OUTPUT_LIMIT_BYTES {
        *truncated = true;
        *truncated_at_bytes = Some(*total_bytes);
        return false;
    }
    buffer.push_str(line);
    buffer.push('\n');
    *total_bytes += needed;
    true
}

async fn search_files(args: SearchInputArgs) -> Result<SearchResult, ToolError> {
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
fn search_blocking(
    regex: &regex::Regex,
    root: &Path,
    context_lines: usize,
    glob_pattern: Option<&glob::Pattern>,
    files_with_matches: bool,
    only_matches: bool,
) -> SearchResult {
    let mut sections: Vec<SearchResultSection> = Vec::new();
    let mut total_bytes: usize = 0;
    let mut truncated = false;
    let mut truncated_at_bytes: Option<usize> = None;

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
            let formatted = file_path_str.clone();
            let mut byte_count_buffer = String::new();
            if !append_line(
                &mut byte_count_buffer,
                &formatted,
                &mut total_bytes,
                &mut truncated,
                &mut truncated_at_bytes,
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
                        let formatted = format!("{file_path_str}:{lineno}: {match_text}");
                        let mut byte_count_buffer = String::new();
                        if !append_line(
                            &mut byte_count_buffer,
                            &formatted,
                            &mut total_bytes,
                            &mut truncated,
                            &mut truncated_at_bytes,
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
                let formatted = if is_match {
                    format!("{file_path_str}:{lineno}: {line}")
                } else {
                    format!("{file_path_str}-{lineno}- {line}")
                };
                let mut byte_count_buffer = String::new();
                if !append_line(
                    &mut byte_count_buffer,
                    &formatted,
                    &mut total_bytes,
                    &mut truncated,
                    &mut truncated_at_bytes,
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

    SearchResult::new(sections, truncated, truncated_at_bytes, files_with_matches)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    // ── approval ──────────────────────────────────────────────────────────────

    #[test]
    fn search_tool_no_path_is_auto_approved() {
        let tool = SearchTool::default();
        let args = serde_json::json!({ "pattern": "foo" });
        assert!(matches!(
            tool.needs_approval(&args),
            ApprovalCheck::AutoApproved
        ));
    }

    #[test]
    fn search_tool_workspace_path_is_auto_approved() {
        let tool = SearchTool::default();
        // cwd in tests is the project root; src/ is inside the workspace.
        let args = serde_json::json!({ "pattern": "fn main", "path": "src" });
        assert!(matches!(
            tool.needs_approval(&args),
            ApprovalCheck::AutoApproved
        ));
    }

    #[test]
    fn search_tool_outside_workspace_path_is_auto_denied() {
        let tool = SearchTool::default();
        let args = serde_json::json!({ "pattern": "secret", "path": "/etc" });
        assert!(matches!(
            tool.needs_approval(&args),
            ApprovalCheck::AutoDenied { .. }
        ));
    }

    // ── core search ───────────────────────────────────────────────────────────

    #[test]
    fn search_finds_match_in_file() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("hello.txt"), "hello world\nfoo bar\n").expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "hello".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: false,
                context_lines: 0,
                glob: None,
                files_with_matches: false,
                only_matches: false,
            })
            .await
            .expect("search ok");
            assert_eq!(result.sections.len(), 1);
            assert!(result.text().contains(":1: hello world"));
            assert!(!result.truncated);
        });
    }

    #[test]
    fn search_no_match_returns_empty_sections() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("a.txt"), "apple banana\n").expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "cherry".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: false,
                context_lines: 0,
                glob: None,
                files_with_matches: false,
                only_matches: false,
            })
            .await
            .expect("search ok");
            assert!(result.sections.is_empty());
        });
    }

    #[test]
    fn search_invalid_regex_returns_error() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let result = search_files(SearchInputArgs {
                pattern: "[unclosed".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: false,
                context_lines: 0,
                glob: None,
                files_with_matches: false,
                only_matches: false,
            })
            .await;
            assert!(result.is_err());
        });
    }

    #[test]
    fn search_case_insensitive_matches_regardless_of_case() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("b.txt"), "Hello World\n").expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "hello".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: false,
                context_lines: 0,
                glob: None,
                files_with_matches: false,
                only_matches: false,
            })
            .await
            .expect("search ok");
            assert_eq!(result.sections.len(), 1, "case-insensitive should match");

            let case_sensitive_result = search_files(SearchInputArgs {
                pattern: "hello".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: true,
                context_lines: 0,
                glob: None,
                files_with_matches: false,
                only_matches: false,
            })
            .await
            .expect("search ok");
            assert!(
                case_sensitive_result.sections.is_empty(),
                "case-sensitive should not match 'Hello'"
            );
        });
    }

    #[test]
    fn search_context_lines_included_in_output() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(
                dir.path().join("c.txt"),
                "line1\nline2\nMATCH\nline4\nline5\n",
            )
            .expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "MATCH".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: true,
                context_lines: 1,
                glob: None,
                files_with_matches: false,
                only_matches: false,
            })
            .await
            .expect("search ok");
            assert_eq!(result.sections.len(), 1);
            // Match line uses ':' separator; context lines use '-' separator.
            assert!(result.text().contains(":3: MATCH"), "match line present");
            assert!(
                result.text().contains("-2- line2"),
                "before-context present"
            );
            assert!(result.text().contains("-4- line4"), "after-context present");
        });
    }

    #[test]
    fn search_binary_file_is_skipped() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            // Write a file with a null byte — triggers binary detection.
            let mut binary_content = b"text before\x00binary after".to_vec();
            binary_content.extend_from_slice(b"text");
            std::fs::write(dir.path().join("binary.bin"), &binary_content).expect("write");
            // Also write a plain text file so the walker has something to visit.
            std::fs::write(dir.path().join("text.txt"), "text before binary after\n")
                .expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "text before".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: true,
                context_lines: 0,
                glob: None,
                files_with_matches: false,
                only_matches: false,
            })
            .await
            .expect("search ok");
            // Only the text file should match; the binary file must be skipped.
            assert_eq!(result.sections.len(), 1);
            assert!(result.sections[0].file_path.ends_with("text.txt"));
        });
    }

    #[test]
    fn search_output_truncated_at_100kb_cap() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            // Create enough content to exceed the 100 KB cap.
            // Each line is ~120 bytes; 1000 lines ≈ 120 KB.
            let line = "a".repeat(100) + " NEEDLE here\n";
            let content = line.repeat(1000);
            std::fs::write(dir.path().join("big.txt"), &content).expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "NEEDLE".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: true,
                context_lines: 0,
                glob: None,
                files_with_matches: false,
                only_matches: false,
            })
            .await
            .expect("search ok");
            assert!(result.truncated, "output must be truncated");
            assert!(
                result.truncated_at_bytes.is_some(),
                "truncated_at_bytes must be set"
            );
            // The metadata() must advertise the truncation.
            let meta = result
                .metadata()
                .expect("truncated result must have metadata");
            let obj = meta.as_object().expect("metadata is a JSON object");
            assert!(obj.contains_key("truncated"), "truncated key expected");
            assert_eq!(obj["truncated"], json!(true));
        });
    }

    #[test]
    fn search_section_label_is_file_path() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let file_path = dir.path().join("labeled.txt");
            std::fs::write(&file_path, "target line\n").expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "target".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: false,
                context_lines: 0,
                glob: None,
                files_with_matches: false,
                only_matches: false,
            })
            .await
            .expect("search ok");
            assert_eq!(result.sections.len(), 1);
            assert_eq!(
                result.sections[0].file_path,
                file_path.display().to_string()
            );
        });
    }

    #[test]
    fn search_relative_path_returns_relative_result_paths() {
        crate::test_support::run_locked(async {
            // Call search_blocking directly with a relative root so we can
            // assert path form without mutating the process-global cwd.
            // `src/` is guaranteed to exist and contain Rust files with `fn `.
            let regex = regex::Regex::new(r"\bfn \b").expect("regex");
            let result =
                search_blocking(&regex, std::path::Path::new("src"), 0, None, false, false);
            assert!(
                !result.sections.is_empty(),
                "expected at least one match in src/"
            );
            for section in &result.sections {
                assert!(
                    !std::path::Path::new(&section.file_path).is_absolute(),
                    "result path must be relative, got: {}",
                    section.file_path
                );
                assert!(
                    section.file_path.starts_with("src"),
                    "result path must be rooted at 'src', got: {}",
                    section.file_path
                );
            }
        });
    }

    #[test]
    fn search_glob_filters_to_matching_files_only() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("code.rs"), "needle in rust\n").expect("write");
            std::fs::write(dir.path().join("docs.txt"), "needle in text\n").expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "needle".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: true,
                context_lines: 0,
                glob: Some("*.rs".to_string()),
                files_with_matches: false,
                only_matches: false,
            })
            .await
            .expect("search ok");
            assert_eq!(result.sections.len(), 1, "only .rs file should be searched");
            assert!(
                result.sections[0].file_path.ends_with(".rs"),
                "matched file must be .rs"
            );
        });
    }

    #[test]
    fn search_invalid_glob_returns_error() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let result = search_files(SearchInputArgs {
                pattern: "needle".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: false,
                context_lines: 0,
                glob: Some("[bad".to_string()),
                files_with_matches: false,
                only_matches: false,
            })
            .await;
            assert!(result.is_err(), "invalid glob must return ToolError");
        });
    }

    #[test]
    fn search_glob_with_dotdot_segment_returns_error() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            for glob_str in ["../*.rs", "src/../*.rs", "../**/*.rs"] {
                let result = search_files(SearchInputArgs {
                    pattern: "needle".to_string(),
                    path: Some(dir.path().display().to_string()),
                    case_sensitive: false,
                    context_lines: 0,
                    glob: Some(glob_str.to_string()),
                    files_with_matches: false,
                    only_matches: false,
                })
                .await;
                assert!(
                    result.is_err(),
                    "glob '{glob_str}' with '..' must return ToolError"
                );
            }
        });
    }

    #[test]
    fn search_files_with_matches_returns_only_paths() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("a.rs"), "fn foo() {}\nfn bar() {}\n").expect("write");
            std::fs::write(dir.path().join("b.rs"), "no match here\n").expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "fn foo".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: true,
                context_lines: 0,
                glob: None,
                files_with_matches: true,
                only_matches: false,
            })
            .await
            .expect("search ok");
            assert_eq!(result.sections.len(), 1, "one matching file");
            // In files_with_matches mode the section has no content lines.
            assert!(
                result.sections[0].lines.is_empty(),
                "files_with_matches sections must have no content lines"
            );
            // text() output must contain only file paths.
            let text = result.text();
            assert!(
                text.contains("a.rs"),
                "path of matching file must appear in text"
            );
            assert!(
                !text.contains("fn foo"),
                "line content must not appear in text"
            );
        });
    }

    #[test]
    fn search_only_matches_returns_match_substrings() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("code.rs"), "foo bar foo\n").expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "foo".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: true,
                context_lines: 0,
                glob: None,
                files_with_matches: false,
                only_matches: true,
            })
            .await
            .expect("search ok");
            assert_eq!(result.sections.len(), 1, "one section for matching file");
            // "foo bar foo" has two occurrences of "foo".
            assert_eq!(
                result.sections[0].lines.len(),
                2,
                "two match occurrences on the line"
            );
            for line in &result.sections[0].lines {
                assert_eq!(line.content, "foo", "only the matched substring is stored");
            }
        });
    }

    #[test]
    fn search_only_matches_ignores_context_lines() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(
                dir.path().join("code.rs"),
                "context before\nfoo match\ncontext after\n",
            )
            .expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "foo".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: true,
                context_lines: 2,
                glob: None,
                files_with_matches: false,
                only_matches: true,
            })
            .await
            .expect("search ok");
            // context_lines must be ignored in only_matches mode.
            assert_eq!(
                result.sections[0].lines.len(),
                1,
                "only the matched substring"
            );
        });
    }

    #[test]
    fn search_files_with_matches_and_only_matches_returns_error() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("code.rs"), "foo\n").expect("write");
            let result = search_files(SearchInputArgs {
                pattern: "foo".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: true,
                context_lines: 0,
                glob: None,
                files_with_matches: true,
                only_matches: true,
            })
            .await;
            assert!(
                result.is_err(),
                "files_with_matches and only_matches are mutually exclusive"
            );
        });
    }

    #[test]
    fn search_input_args_all_properties_have_descriptions() {
        let schema = SearchTool::default().input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "SearchInputArgs");
    }
}
