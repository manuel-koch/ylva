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

//! The `edit` tool: patch an existing file in-place.
//!
//! Two modes are supported:
//! - **SearchReplace**: replace the first occurrence of `old_string` with `new_string`.
//! - **LineRange**: replace the lines from `start_line` to `end_line` (1-based, inclusive)
//!   with `new_string`.
//!
//! The patched content and a unified diff are computed eagerly at call-construction time
//! (before approval). Approval is required via sandbox path check. On approval, the
//! patched content is written to disk.

use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use similar::TextDiff;
use tracing::Instrument;

use super::validate_args_with_schema;

use super::{
    DiffSection, Section, Tool, ToolCall, ToolCallResult, ToolError, DEFAULT_TOOL_TIMEOUT_SECS,
};
use crate::sandbox::{ApprovalCheck, SandboxContext};

/// Number of unchanged context lines shown above and below each changed hunk in the diff preview.
const DIFF_CONTEXT_LINES: usize = 3;

/// Selects the editing strategy for the `edit` tool.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditMode {
    /// Replace occurrences of `old_string` with `new_string`.
    SearchReplace,
    /// Replace lines `start_line` through `end_line` (1-based, inclusive) with `new_string`.
    LineRangeReplace,
}

/// `edit` tool arguments. All fields are at the top level; which fields are required
/// depends on `mode` (see each field's description).
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct EditFileArgs {
    /// Absolute or workspace-relative path of the file to edit. The file must already exist.
    pub path: String,
    /// Editing strategy: `search_replace` or `line_range_replace`.
    pub mode: EditMode,
    /// (`search_replace`) The exact text to search for in the file.
    #[serde(default)]
    pub old_string: Option<String>,
    /// Replacement text used by both modes. May be empty to delete the matched text or lines.
    pub new_string: String,
    /// (`search_replace`) When `true`, replace every occurrence; default replaces only the first.
    #[serde(default)]
    pub replace_all: bool,
    /// (`line_range_replace`) First line to replace (1-based).
    #[serde(default)]
    pub start_line: Option<usize>,
    /// (`line_range_replace`) Last line to replace (1-based, inclusive). Must be ≥ `start_line`.
    #[serde(default)]
    pub end_line: Option<usize>,
}

/// Structured outcome data captured at patch-application time, mode-specific.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditApplied {
    /// Outcome of a `SearchReplace` edit.
    SearchReplace {
        /// Number of replacements made (1 when `replace_all` is `false`, ≥ 1 otherwise).
        replacements_made: usize,
    },
    /// Outcome of a `LineRange` edit.
    LineRangeReplace {
        /// First line replaced (1-based).
        start_line: usize,
        /// Last line replaced (1-based, inclusive).
        end_line: usize,
    },
}

/// Result metadata returned to the LLM after a successful `edit` call.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EditResultMetadata {
    /// For `SearchReplace` mode: number of replacements made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacements_made: Option<usize>,
    /// For `LineRange` mode: first line replaced (1-based).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    /// For `LineRange` mode: last line replaced (1-based, inclusive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
}

/// Output of a successful `edit` tool invocation.
#[derive(Debug)]
pub struct EditResult {
    pub path: String,
    pub outcome: EditApplied,
    pub diff: String,
}

impl ToolCallResult for EditResult {
    fn text(&self) -> String {
        match &self.outcome {
            EditApplied::SearchReplace { replacements_made } => {
                format!(
                    "Edited '{}': {replacements_made} replacement(s) made",
                    self.path
                )
            }
            EditApplied::LineRangeReplace {
                start_line,
                end_line,
            } => {
                format!(
                    "Edited '{}': replaced lines {start_line}–{end_line}",
                    self.path
                )
            }
        }
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        let meta = match &self.outcome {
            EditApplied::SearchReplace { replacements_made } => EditResultMetadata {
                replacements_made: Some(*replacements_made),
                start_line: None,
                end_line: None,
            },
            EditApplied::LineRangeReplace {
                start_line,
                end_line,
            } => EditResultMetadata {
                replacements_made: None,
                start_line: Some(*start_line),
                end_line: Some(*end_line),
            },
        };
        serde_json::to_value(meta).ok()
    }

    fn metadata_schema(&self) -> Option<RootSchema> {
        Some(schema_for!(EditResultMetadata))
    }

    fn content_sections(&self) -> Vec<super::Section> {
        if self.diff.is_empty() {
            return vec![];
        }
        vec![super::Section::Diff(super::DiffSection {
            label: Some(self.path.clone()),
            content: self.diff.clone(),
        })]
    }
}

/// Invocation object for a single `edit` tool call.
///
/// Built eagerly in `create_call`: for the happy path both `patched_content` and `diff` are
/// pre-computed so the diff preview is available before the user approves. When a runtime
/// error occurs during that computation (file not found, `old_string` not found, …) the error
/// is stored in `error` and surfaced by `run()`; `pending_content_sections` returns nothing.
#[derive(Debug)]
pub struct EditCall {
    id: uuid::Uuid,
    raw_arguments: serde_json::Value,
    timeout_secs: u64,
    path: String,
    diff: String,
    patched_content: String,
    outcome: Option<EditApplied>,
    error: Option<ToolError>,
}

impl EditCall {
    fn ready(
        id: uuid::Uuid,
        raw_arguments: serde_json::Value,
        timeout_secs: u64,
        path: String,
        original_content: String,
        patched_content: String,
        outcome: EditApplied,
    ) -> Self {
        let diff = TextDiff::from_lines(&original_content, &patched_content)
            .unified_diff()
            .context_radius(DIFF_CONTEXT_LINES)
            .header(&format!("a/{path}"), &format!("b/{path}"))
            .to_string();
        EditCall {
            id,
            raw_arguments,
            timeout_secs,
            path,
            diff,
            patched_content,
            outcome: Some(outcome),
            error: None,
        }
    }

    fn failed(
        id: uuid::Uuid,
        raw_arguments: serde_json::Value,
        timeout_secs: u64,
        path: String,
        error: ToolError,
    ) -> Self {
        EditCall {
            id,
            raw_arguments,
            timeout_secs,
            path,
            diff: String::new(),
            patched_content: String::new(),
            outcome: None,
            error: Some(error),
        }
    }
}

impl ToolCall for EditCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }

    fn name(&self) -> &str {
        EditTool::NAME
    }

    fn arguments(&self) -> &serde_json::Value {
        &self.raw_arguments
    }

    fn argument_order(&self, key: &str) -> usize {
        EditTool::default().argument_order(key)
    }

    // `old_string`/`new_string` are already fully represented by the diff
    // section (`pending_content_sections`/`content_sections`), so keep them
    // out of the inline invocation summary to avoid showing the same content
    // twice. When the edit failed before a diff could be computed (e.g.
    // `old_string` not found), no diff section exists, so keep them visible
    // — they're the only place left to see what was searched for.
    fn is_body_argument(&self, key: &str) -> bool {
        self.error.is_none() && matches!(key, "old_string" | "new_string")
    }

    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    fn pending_content_sections(&self) -> Vec<Section> {
        if self.error.is_some() {
            return vec![];
        }
        vec![Section::Diff(DiffSection {
            label: Some(self.path.clone()),
            content: self.diff.clone(),
        })]
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
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
        if let Some(error) = self.error {
            return Box::pin(async move { Err(error) });
        }
        let serialized = serde_json::to_string(&self.raw_arguments).unwrap_or_default();
        let span = tracing::info_span!("tool_args", args = %serialized);
        let path = self.path.clone();
        let diff = self.diff.clone();
        let patched_content = self.patched_content.clone();
        let outcome = self
            .outcome
            .clone()
            .expect("outcome is set when error is None");
        Box::pin(
            async move {
                tokio::fs::write(&path, patched_content.as_bytes())
                    .await
                    .map_err(|error| {
                        ToolError::failed("edit", format!("failed to write '{path}': {error}"))
                    })?;
                Ok(Box::new(EditResult {
                    path,
                    outcome,
                    diff,
                }) as Box<dyn ToolCallResult>)
            }
            .instrument(span),
        )
    }
}

/// The `edit` tool: patch an existing file using search-replace or line-range replacement.
#[derive(Debug)]
pub struct EditTool {
    timeout_secs: u64,
    sandbox_context: SandboxContext,
}

impl EditTool {
    pub const NAME: &'static str = "edit";

    pub fn new(sandbox_context: SandboxContext) -> Self {
        EditTool {
            timeout_secs: 0,
            sandbox_context,
        }
    }

    pub fn with_timeout(timeout_secs: u64, sandbox_context: SandboxContext) -> Self {
        EditTool {
            timeout_secs,
            sandbox_context,
        }
    }
}

impl Default for EditTool {
    fn default() -> Self {
        use crate::sandbox::Sandbox;
        EditTool::new(SandboxContext::new(std::sync::Arc::new(
            std::sync::RwLock::new(Sandbox::new()),
        )))
    }
}

impl Tool for EditTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Edit an existing file (must already exist) by replacing text. `mode: search_replace` — \
        replace `old_string` with `new_string` (first occurrence only, unless `replace_all: true`). \
        `mode: line_range_replace` — replace lines `start_line`..`end_line` (1-based, inclusive) \
        with `new_string`. The `path` argument accepts `~` and `$ENVVAR`/`${ENVVAR}` tokens; \
        they are expanded before use."
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(EditFileArgs)
    }

    fn path_like_arguments(&self) -> &'static [&'static str] {
        &["path"]
    }

    fn validate_args(&self, arguments: &Value) -> Result<(), String> {
        validate_args_with_schema(&self.input_schema(), arguments)?;
        let mode = arguments
            .get("mode")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        match mode {
            "search_replace" => {
                if arguments
                    .get("old_string")
                    .map(|value| value.is_null())
                    .unwrap_or(true)
                {
                    return Err("`old_string` is required when mode is `search_replace`".into());
                }
            }
            "line_range_replace" => {
                if arguments
                    .get("start_line")
                    .map(|value| value.is_null())
                    .unwrap_or(true)
                {
                    return Err("`start_line` is required when mode is `line_range_replace`".into());
                }
                if arguments
                    .get("end_line")
                    .map(|value| value.is_null())
                    .unwrap_or(true)
                {
                    return Err("`end_line` is required when mode is `line_range_replace`".into());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        let path_str = match arguments.get("path").and_then(|v| v.as_str()) {
            Some(value) if !value.is_empty() => value,
            Some(_) => {
                return ApprovalCheck::AutoDenied {
                    reason: "Required argument 'path' must not be empty".to_string(),
                }
            }
            None => {
                return ApprovalCheck::AutoDenied {
                    reason: "Required argument 'path' is missing or not a string".to_string(),
                }
            }
        };
        let sandbox_verdict = self
            .sandbox_context
            .check_path(std::path::Path::new(path_str));
        let approval = match sandbox_verdict {
            None => ApprovalCheck::UserApprovalRequired,
            Some(verdict) => verdict,
        };
        tracing::debug!(
            tool = "edit",
            path = path_str,
            decision = match &approval {
                ApprovalCheck::AutoApproved => "auto-approved",
                ApprovalCheck::AutoDenied { .. } => "auto-denied",
                ApprovalCheck::UserApprovalRequired => "sandbox-abstain→user-approval-required",
            },
            "tool approval decision"
        );
        approval
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_TOOL_TIMEOUT_SECS
        } else {
            self.timeout_secs
        }
    }

    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
        Box::new(create_edit_call(id, arguments, self.timeout_secs()))
    }
}

/// Build the `EditCall` synchronously: read the file and compute the patch.
///
/// Runtime errors (missing file, old_string not found, …) are embedded in the returned
/// `EditCall` and surfaced when `run()` is eventually called.
fn create_edit_call(id: uuid::Uuid, arguments: serde_json::Value, timeout_secs: u64) -> EditCall {
    let args = match serde_json::from_value::<EditFileArgs>(arguments.clone()) {
        Ok(parsed) => parsed,
        Err(error) => {
            let path = arguments
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            return EditCall::failed(
                id,
                arguments,
                timeout_secs,
                path,
                ToolError::failed("edit", format!("invalid arguments: {error}")),
            );
        }
    };

    let path = args.path.clone();

    if !std::path::Path::new(&path).exists() {
        return EditCall::failed(
            id,
            arguments,
            timeout_secs,
            path.clone(),
            ToolError::failed(
                "edit",
                format!("file '{path}' does not exist; use `write` to create new files"),
            ),
        );
    }

    let original_content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => {
            return EditCall::failed(
                id,
                arguments,
                timeout_secs,
                path.clone(),
                ToolError::failed("edit", format!("failed to read '{path}': {error}")),
            );
        }
    };

    match apply_edit(&original_content, &args) {
        Ok((patched_content, outcome)) => EditCall::ready(
            id,
            arguments,
            timeout_secs,
            path,
            original_content,
            patched_content,
            outcome,
        ),
        Err(error) => EditCall::failed(id, arguments, timeout_secs, path, error),
    }
}

/// Apply the edit to `original_content`, returning the patched string and outcome metadata.
fn apply_edit(
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    fn make_args_search_replace(
        path: &str,
        old_string: &str,
        new_string: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "path": path,
            "mode": "search_replace",
            "old_string": old_string,
            "new_string": new_string,
        })
    }

    fn make_args_line_range(
        path: &str,
        start_line: usize,
        end_line: usize,
        new_string: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "path": path,
            "mode": "line_range_replace",
            "start_line": start_line,
            "end_line": end_line,
            "new_string": new_string,
        })
    }

    // ── SearchReplace ──────────────────────────────────────────────────────

    fn search_replace_args(
        path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
    ) -> EditFileArgs {
        EditFileArgs {
            path: path.to_string(),
            mode: EditMode::SearchReplace,
            new_string: new_string.to_string(),
            old_string: Some(old_string.to_string()),
            replace_all,
            start_line: None,
            end_line: None,
        }
    }

    fn line_range_args(
        path: &str,
        start_line: usize,
        end_line: usize,
        new_string: &str,
    ) -> EditFileArgs {
        EditFileArgs {
            path: path.to_string(),
            mode: EditMode::LineRangeReplace,
            new_string: new_string.to_string(),
            old_string: None,
            replace_all: false,
            start_line: Some(start_line),
            end_line: Some(end_line),
        }
    }

    #[test]
    fn search_replace_happy_path() {
        let content = "hello world\ngoodbye world\n";
        let (patched, outcome) = apply_edit(
            content,
            &search_replace_args("test.txt", "hello world", "hi world", false),
        )
        .unwrap();
        assert_eq!(patched, "hi world\ngoodbye world\n");
        assert!(matches!(
            outcome,
            EditApplied::SearchReplace {
                replacements_made: 1
            }
        ));
    }

    #[test]
    fn search_replace_diff_contains_old_and_new() {
        let original = "line one\nline two\n";
        let patched = "line one\nline TWO\n";
        let diff = TextDiff::from_lines(original, patched)
            .unified_diff()
            .context_radius(DIFF_CONTEXT_LINES)
            .header("a/test.txt", "b/test.txt")
            .to_string();
        assert!(diff.contains("-line two"));
        assert!(diff.contains("+line TWO"));
    }

    #[test]
    fn search_replace_not_found_returns_error() {
        let content = "hello world\n";
        let result = apply_edit(
            content,
            &search_replace_args("test.txt", "not present", "replacement", false),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn search_replace_replaces_only_first_occurrence() {
        let content = "foo bar foo\n";
        let (patched, outcome) = apply_edit(
            content,
            &search_replace_args("test.txt", "foo", "baz", false),
        )
        .unwrap();
        assert_eq!(patched, "baz bar foo\n");
        assert!(matches!(
            outcome,
            EditApplied::SearchReplace {
                replacements_made: 1
            }
        ));
    }

    #[test]
    fn search_replace_replace_all_replaces_every_occurrence() {
        let content = "foo bar foo baz foo\n";
        let (patched, outcome) = apply_edit(
            content,
            &search_replace_args("test.txt", "foo", "qux", true),
        )
        .unwrap();
        assert_eq!(patched, "qux bar qux baz qux\n");
        assert!(matches!(
            outcome,
            EditApplied::SearchReplace {
                replacements_made: 3
            }
        ));
    }

    // ── LineRange ──────────────────────────────────────────────────────────

    #[test]
    fn line_range_happy_path() {
        let content = "line1\nline2\nline3\n";
        let (patched, outcome) =
            apply_edit(content, &line_range_args("test.txt", 2, 2, "replaced")).unwrap();
        assert_eq!(patched, "line1\nreplaced\nline3\n");
        assert!(matches!(
            outcome,
            EditApplied::LineRangeReplace {
                start_line: 2,
                end_line: 2
            }
        ));
    }

    #[test]
    fn line_range_diff_present() {
        let original = "a\nb\nc\n";
        let patched = "a\nX\nc\n";
        let diff = TextDiff::from_lines(original, patched)
            .unified_diff()
            .context_radius(DIFF_CONTEXT_LINES)
            .header("a/f", "b/f")
            .to_string();
        assert!(diff.contains("-b"));
        assert!(diff.contains("+X"));
    }

    #[test]
    fn line_range_out_of_bounds_returns_error() {
        let content = "line1\nline2\n";
        let result = apply_edit(content, &line_range_args("test.txt", 1, 5, "x"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("out of bounds"));
    }

    #[test]
    fn line_range_start_greater_than_end_returns_error() {
        let content = "line1\nline2\n";
        let result = apply_edit(content, &line_range_args("test.txt", 3, 1, "x"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must not exceed"));
    }

    #[tokio::test]
    async fn nonexistent_file_returns_error() {
        let args = make_args_search_replace("/no/such/file.txt", "x", "y");
        let id = uuid::Uuid::new_v4();
        let call = create_edit_call(id, args, DEFAULT_TOOL_TIMEOUT_SECS);
        let result = Box::new(call).run().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[tokio::test]
    async fn nonexistent_file_via_line_range_returns_error() {
        let args = make_args_line_range("/no/such/file.txt", 1, 1, "x");
        let id = uuid::Uuid::new_v4();
        let call = create_edit_call(id, args, DEFAULT_TOOL_TIMEOUT_SECS);
        let result = Box::new(call).run().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn pending_content_sections_returns_diff_section() {
        let file = write_temp("hello\nworld\n");
        let path = file.path().to_str().unwrap().to_string();
        let args = make_args_search_replace(&path, "hello", "hi");
        let id = uuid::Uuid::new_v4();
        let call = create_edit_call(id, args, DEFAULT_TOOL_TIMEOUT_SECS);
        let sections = call.pending_content_sections();
        assert_eq!(sections.len(), 1);
        assert!(matches!(sections[0], Section::Diff(_)));
        if let Section::Diff(ref diff) = sections[0] {
            assert_eq!(diff.label, Some(path));
            assert!(diff.content.contains("-hello"));
            assert!(diff.content.contains("+hi"));
        }
    }

    #[test]
    fn title_arguments_excludes_old_and_new_string_when_diff_is_available() {
        // old_string/new_string are already fully represented by the diff
        // section, so they must not also appear in the inline title args.
        let file = write_temp("hello\nworld\n");
        let path = file.path().to_str().unwrap().to_string();
        let args = make_args_search_replace(&path, "hello", "hi");
        let call = create_edit_call(uuid::Uuid::new_v4(), args, DEFAULT_TOOL_TIMEOUT_SECS);
        let title_arguments = call.title_arguments();
        assert!(
            title_arguments
                .iter()
                .all(|(k, _)| k != "old_string" && k != "new_string"),
            "old_string/new_string should be excluded from title_arguments: {title_arguments:?}"
        );
        // Other scalar args (path, mode) remain visible.
        assert!(title_arguments.iter().any(|(k, _)| k == "path"));
        assert!(title_arguments.iter().any(|(k, _)| k == "mode"));
    }

    #[test]
    fn title_arguments_keeps_old_and_new_string_when_no_diff_was_computed() {
        // When old_string isn't found, no diff section is produced, so
        // old_string/new_string are the only place left to see what was
        // searched for — keep them in the title args.
        let file = write_temp("hello\nworld\n");
        let path = file.path().to_str().unwrap().to_string();
        let args = make_args_search_replace(&path, "not-present", "hi");
        let call = create_edit_call(uuid::Uuid::new_v4(), args, DEFAULT_TOOL_TIMEOUT_SECS);
        assert!(call.pending_content_sections().is_empty());
        let title_arguments = call.title_arguments();
        assert!(title_arguments.iter().any(|(k, _)| k == "old_string"));
        assert!(title_arguments.iter().any(|(k, _)| k == "new_string"));
    }

    #[tokio::test]
    async fn run_writes_patched_content() {
        let file = write_temp("alpha\nbeta\n");
        let path = file.path().to_str().unwrap().to_string();
        let args = make_args_search_replace(&path, "beta", "gamma");
        let id = uuid::Uuid::new_v4();
        let call = create_edit_call(id, args, DEFAULT_TOOL_TIMEOUT_SECS);
        Box::new(call).run().await.unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "alpha\ngamma\n");
    }

    // ── EditResult ─────────────────────────────────────────────────────────

    #[test]
    fn edit_result_text_search_replace() {
        let result = EditResult {
            path: "src/foo.rs".to_string(),
            outcome: EditApplied::SearchReplace {
                replacements_made: 2,
            },
            diff: String::new(),
        };
        assert!(result.text().contains("src/foo.rs"));
        assert!(result.text().contains("2"));
    }

    #[test]
    fn edit_result_text_line_range() {
        let result = EditResult {
            path: "src/foo.rs".to_string(),
            outcome: EditApplied::LineRangeReplace {
                start_line: 3,
                end_line: 5,
            },
            diff: String::new(),
        };
        assert!(result.text().contains("src/foo.rs"));
        assert!(result.text().contains("3"));
        assert!(result.text().contains("5"));
    }

    #[test]
    fn edit_result_metadata_search_replace() {
        let result = EditResult {
            path: "src/foo.rs".to_string(),
            outcome: EditApplied::SearchReplace {
                replacements_made: 1,
            },
            diff: String::new(),
        };
        let meta = result.metadata().unwrap();
        assert_eq!(meta["replacements_made"], serde_json::json!(1));
        assert!(result.metadata_schema().is_some());
    }

    #[test]
    fn edit_result_metadata_line_range() {
        let result = EditResult {
            path: "src/foo.rs".to_string(),
            outcome: EditApplied::LineRangeReplace {
                start_line: 2,
                end_line: 4,
            },
            diff: String::new(),
        };
        let meta = result.metadata().unwrap();
        assert_eq!(meta["start_line"], serde_json::json!(2));
        assert_eq!(meta["end_line"], serde_json::json!(4));
        assert!(meta.get("replacements_made").is_none() || meta["replacements_made"].is_null());
    }

    #[tokio::test]
    async fn edit_result_content_sections_returns_diff_after_run() {
        let file = write_temp("hello\nworld\n");
        let path = file.path().to_str().unwrap().to_string();
        let args = make_args_search_replace(&path, "hello", "hi");
        let call = create_edit_call(uuid::Uuid::new_v4(), args, DEFAULT_TOOL_TIMEOUT_SECS);
        let result = Box::new(call).run().await.unwrap();
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1, "diff section must be present after run");
        assert!(
            matches!(sections[0], crate::agent::tools::Section::Diff(_)),
            "section must be a Diff"
        );
        if let crate::agent::tools::Section::Diff(ref diff) = sections[0] {
            assert!(
                diff.content.contains("-hello"),
                "diff must show removed line"
            );
            assert!(diff.content.contains("+hi"), "diff must show added line");
            assert_eq!(diff.label.as_deref(), Some(&path as &str));
        }
    }

    // ── EditTool ───────────────────────────────────────────────────────────

    #[test]
    fn edit_tool_needs_approval_missing_path() {
        let tool = EditTool::default();
        let args = serde_json::json!({});
        assert!(matches!(
            tool.needs_approval(&args),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
    }

    #[test]
    fn edit_tool_needs_approval_empty_path() {
        let tool = EditTool::default();
        let args = serde_json::json!({ "path": "" });
        assert!(matches!(
            tool.needs_approval(&args),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
    }

    #[test]
    fn edit_tool_needs_approval_valid_path_requires_user_approval() {
        let tool = EditTool::default();
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "old_string": "foo",
            "new_string": "bar",
        });
        assert!(matches!(
            tool.needs_approval(&args),
            crate::sandbox::ApprovalCheck::UserApprovalRequired
        ));
    }

    #[test]
    fn edit_tool_validate_args_invalid_mode_args_returns_error() {
        let tool = EditTool::default();
        // path present but no mode field → schema validation fails
        let args = serde_json::json!({ "path": "src/lib.rs" });
        assert!(tool.validate_args(&args).is_err());
    }

    #[test]
    fn edit_tool_validate_args_search_replace_missing_old_string_returns_error() {
        let tool = EditTool::default();
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "mode": "search_replace",
            "new_string": "bar",
        });
        let error = tool.validate_args(&args).unwrap_err();
        assert!(error.contains("old_string"), "error was: {error}");
    }

    #[test]
    fn edit_tool_validate_args_line_range_replace_missing_start_line_returns_error() {
        let tool = EditTool::default();
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "mode": "line_range_replace",
            "new_string": "x",
            "end_line": 3,
        });
        let error = tool.validate_args(&args).unwrap_err();
        assert!(error.contains("start_line"), "error was: {error}");
    }

    #[test]
    fn edit_tool_validate_args_line_range_replace_missing_end_line_returns_error() {
        let tool = EditTool::default();
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "mode": "line_range_replace",
            "new_string": "x",
            "start_line": 1,
        });
        let error = tool.validate_args(&args).unwrap_err();
        assert!(error.contains("end_line"), "error was: {error}");
    }

    #[test]
    fn edit_tool_validate_args_valid_search_replace_passes() {
        let tool = EditTool::default();
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "mode": "search_replace",
            "old_string": "foo",
            "new_string": "bar",
        });
        assert!(tool.validate_args(&args).is_ok());
    }

    #[test]
    fn edit_tool_validate_args_valid_line_range_replace_passes() {
        let tool = EditTool::default();
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "mode": "line_range_replace",
            "new_string": "x",
            "start_line": 1,
            "end_line": 3,
        });
        assert!(tool.validate_args(&args).is_ok());
    }

    #[test]
    fn edit_tool_schema_property_order() {
        let tool = EditTool::default();
        // All fields are now top-level in `EditFileArgs`, so each receives a schema-driven rank.
        assert_eq!(tool.argument_order("path"), 0);
        assert_eq!(tool.argument_order("mode"), 1);
        assert_eq!(tool.argument_order("new_string"), 3);
        assert_eq!(tool.argument_order("old_string"), 2);
        assert_eq!(tool.argument_order("replace_all"), 4);
        assert_eq!(tool.argument_order("start_line"), 5);
        assert_eq!(tool.argument_order("end_line"), 6);
    }

    #[test]
    fn edit_call_argument_order_matches_tool() {
        let file = write_temp("hello\n");
        let path = file.path().to_str().unwrap().to_string();
        let args = make_args_search_replace(&path, "hello", "hi");
        let call = create_edit_call(uuid::Uuid::new_v4(), args, DEFAULT_TOOL_TIMEOUT_SECS);
        // path must be rank 0 so it renders as the keyless primary arg in the TUI.
        assert_eq!(call.argument_order("path"), 0);
        assert!(call.argument_order("path") < call.argument_order("mode"));
        assert!(call.argument_order("mode") < call.argument_order("old_string"));
    }

    #[test]
    fn edit_tool_with_timeout() {
        let tool = EditTool::with_timeout(
            10,
            crate::sandbox::SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
                crate::sandbox::Sandbox::new(),
            ))),
        );
        assert_eq!(tool.timeout_secs(), 10);
    }

    #[test]
    fn edit_tool_default_timeout() {
        let tool = EditTool::default();
        assert_eq!(tool.timeout_secs(), DEFAULT_TOOL_TIMEOUT_SECS);
    }

    #[test]
    fn edit_tool_name_and_description() {
        let tool = EditTool::default();
        assert_eq!(tool.name(), "edit");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn edit_tool_create_call_for_nonexistent_file_produces_error_call() {
        let tool = EditTool::default();
        let id = uuid::Uuid::new_v4();
        let args = make_args_search_replace("/no/such/file.txt", "x", "y");
        // create_call embeds the runtime error into the EditCall; verify it is well-formed.
        let call = tool.create_call(id, args);
        assert_eq!(call.name(), "edit");
        assert_eq!(call.id(), id);
    }

    #[tokio::test]
    async fn failing_edit_call_run_returns_error() {
        let tool = EditTool::default();
        let id = uuid::Uuid::new_v4();
        let args = make_args_search_replace("/no/such/file.txt", "x", "y");
        let call = tool.create_call(id, args);
        let result = call.run().await;
        assert!(result.is_err());
    }

    // ── EditCall trait methods ─────────────────────────────────────────────

    #[test]
    fn edit_call_trait_methods() {
        let file = write_temp("hello\n");
        let path = file.path().to_str().unwrap().to_string();
        let args = make_args_search_replace(&path, "hello", "hi");
        let id = uuid::Uuid::new_v4();
        let call = create_edit_call(id, args.clone(), DEFAULT_TOOL_TIMEOUT_SECS);
        assert_eq!(call.id(), id);
        assert_eq!(call.name(), "edit");
        assert_eq!(call.timeout_secs(), DEFAULT_TOOL_TIMEOUT_SECS);
        let redacted = call.non_default_redacted_arguments();
        assert!(!redacted.is_empty());
    }

    // ── Line range edge cases ──────────────────────────────────────────────

    #[test]
    fn line_range_zero_start_line_returns_error() {
        let content = "line1\nline2\n";
        let result = apply_edit(content, &line_range_args("test.txt", 0, 1, "x"));
        assert!(result.is_err());
    }

    #[test]
    fn line_range_delete_range_with_empty_new_string() {
        let content = "a\nb\nc\n";
        let (patched, _) = apply_edit(content, &line_range_args("test.txt", 2, 2, "")).unwrap();
        assert_eq!(patched, "a\nc\n");
    }

    #[test]
    fn line_range_multi_line_replacement() {
        let content = "a\nb\nc\n";
        let (patched, _) = apply_edit(content, &line_range_args("test.txt", 1, 2, "X\nY")).unwrap();
        assert_eq!(patched, "X\nY\nc\n");
    }

    #[test]
    fn edit_tool_non_default_redacted_arguments() {
        let tool = EditTool::default();
        let file = write_temp("hello\n");
        let path = file.path().to_str().unwrap().to_string();
        let args = make_args_search_replace(&path, "hello", "hi");
        let redacted = tool.non_default_redacted_arguments(&args);
        assert!(!redacted.is_empty());
    }
}
