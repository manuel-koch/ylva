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

//! File-system tools: `read`, `write`, `glob` (blueprint §7 Phase 3).
//!
//! All file I/O uses `tokio::fs`; execution is bounded upstream by
//! [`crate::agent::DEFAULT_TOOL_TIMEOUT_SECS`] (5s). Typed argument structs
//! derive both `JsonSchema` (for the wire schema) and `Deserialize` (for
//! parsing incoming arguments).
//!
//! **`timeout_secs` sentinel (Q6):** each tool carries a `timeout_secs: u64`
//! override where `0` selects [`DEFAULT_TIMEOUT_SECS`]. A timeout is **always
//! enforced — there is no "no-timeout" value**; `0` merely selects the module
//! default.

use std::path::Path;

use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;

use super::{
    ContentSection, Section, Tool, ToolCall, ToolCallResult, ToolError, DEFAULT_TIMEOUT_SECS,
};
use crate::sandbox::{ApprovalCheck, SandboxContext};

/// Return `Ok(value_str)` when `args[key]` is a non-empty string, or
/// `Err(AutoDenied)` with an explanatory reason.
///
/// Used inside `needs_approval` implementations to reject malformed tool
/// calls before any sandbox or user-approval logic runs.
fn require_string_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ApprovalCheck> {
    match args.get(key).and_then(|v| v.as_str()) {
        Some(value) if !value.is_empty() => Ok(value),
        Some(_) => Err(ApprovalCheck::AutoDenied {
            reason: format!("Required argument '{key}' must not be empty"),
        }),
        None => Err(ApprovalCheck::AutoDenied {
            reason: format!("Required argument '{key}' is missing or not a string"),
        }),
    }
}

fn default_true() -> bool {
    true
}

/// The unit that `ReadFileArgs::offset`/`limit` count, chosen by the model.
///
/// `Bytes` is the default and the only meaningful mode for binary files; an
/// encoding must be known before a "char"/line can be defined, so `Lines`/
/// `Chars` lossy-decode UTF-8 first (no encoding detection is done — out of
/// scope for Phase 3). `Chars` counts UTF-8 code points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadMode {
    #[default]
    Bytes,
    Lines,
    Chars,
}

/// `read` arguments: a path plus an optional `mode` (unit selector) and
/// `offset`/`limit` that count that unit.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct ReadFileArgs {
    /// Absolute or workspace-relative path of the file to read.
    pub path: String,
    #[serde(default)]
    pub mode: ReadMode,
    /// 0-based index of the first unit (byte/line/char) to read; the first
    /// line (or byte/char) is at `offset: 0`.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Maximum number of units (bytes/lines/chars) to read, counted from
    /// `offset`. Omitted means "until the end".
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `write` arguments: a target path and the content to create/overwrite.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct WriteFileArgs {
    /// Absolute or workspace-relative path of the file to create or overwrite.
    pub path: String,
    /// Full text content to write. Overwrites any existing content at `path`.
    pub content: String,
}

/// `glob` arguments: a glob pattern resolved against the current directory.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct GlobInputArgs {
    /// Glob pattern to match against file-system paths (e.g. `**/*.rs`, `src/**/mod.rs`).
    /// `*` matches within one path segment; `**` crosses directory boundaries;
    /// `?` matches a single character; `[abc]` matches a character class.
    pub pattern: String,
    /// When `true` (default), paths ignored by `.gitignore` rules are excluded.
    /// Set to `false` to include gitignored paths.
    #[serde(default = "default_true")]
    pub git_ignore: bool,
    /// When `true`, return only the total match count in result metadata (`matches`)
    /// without listing any paths. Useful for counting without fetching all paths.
    #[serde(default)]
    pub only_count: bool,
    /// When `true`, restrict results to regular files only (excludes directories and
    /// symlinks). Use this instead of post-filtering to count or list only files.
    /// Mutually exclusive with `only_dirs`.
    /// When both `only_files` and `only_dirs` are `false` (the default), all path
    /// types — files, directories, and symlinks — are included in results.
    #[serde(default)]
    pub only_files: bool,
    /// When `true`, restrict results to directories only (excludes files and symlinks).
    /// Mutually exclusive with `only_files`.
    /// When both `only_files` and `only_dirs` are `false` (the default), all path
    /// types — files, directories, and symlinks — are included in results.
    #[serde(default)]
    pub only_dirs: bool,
    /// Maximum number of paths to return. When the cap is hit, excess paths are
    /// excluded and `ignored_matches` in result metadata records the overflow count.
    #[serde(default)]
    pub limit: Option<usize>,
    /// When `true`, paths whose name starts with `.` are excluded.
    /// Default `false` — hidden files are included, matching `find . -type f` behaviour.
    #[serde(default)]
    pub dot_ignore: bool,
}

/// Structured metadata for a successful `read` tool invocation.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ReadResultMetadata {
    /// Total bytes in the file content returned.
    pub read_bytes: usize,
    /// Total lines in the file content returned.
    pub read_lines: usize,
}

/// Output of a successful `read` tool invocation.
#[derive(Debug)]
pub struct ReadResult {
    /// File content returned to the LLM.
    pub content: String,
    /// Path of the file that was read.
    pub path: String,
    /// 0-based index of the first line in `content` within the source file.
    pub starting_line: Option<usize>,
}

impl ToolCallResult for ReadResult {
    fn text(&self) -> String {
        self.content.clone()
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        let metadata = ReadResultMetadata {
            read_bytes: self.content.len(),
            read_lines: self.content.lines().count(),
        };
        serde_json::to_value(metadata).ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schemars::schema_for!(ReadResultMetadata))
    }

    fn content_sections(&self) -> Vec<Section> {
        if self.content.is_empty() {
            return vec![];
        }
        let file_extension = std::path::Path::new(&self.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        vec![Section::Content(ContentSection {
            label: None,
            content: self.content.clone(),
            file_extension,
            line_number_offset: self.starting_line,
        })]
    }
}

/// Structured metadata for a successful `write` tool invocation.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WriteResultMetadata {
    /// Total bytes written to disk.
    pub bytes_written: usize,
}

/// Output of a successful `write` tool invocation.
#[derive(Debug)]
pub struct WriteResult {
    /// Path that was written.
    pub path: String,
    /// Number of bytes written to the file.
    pub bytes_written: usize,
    /// Content that was written to the file.
    pub content: String,
    /// Pre-formatted LLM-facing confirmation text.
    text: String,
}

impl WriteResult {
    fn new(path: impl Into<String>, content: impl Into<String>, bytes_written: usize) -> Self {
        let path = path.into();
        let content = content.into();
        WriteResult {
            text: format!("wrote {bytes_written} bytes to '{path}'"),
            path,
            bytes_written,
            content,
        }
    }
}

impl ToolCallResult for WriteResult {
    fn text(&self) -> String {
        self.text.clone()
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        let metadata = WriteResultMetadata {
            bytes_written: self.bytes_written,
        };
        serde_json::to_value(metadata).ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schemars::schema_for!(WriteResultMetadata))
    }

    fn content_sections(&self) -> Vec<Section> {
        if self.content.is_empty() {
            return vec![];
        }
        let file_extension = std::path::Path::new(&self.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        vec![Section::Content(ContentSection {
            label: None,
            content: self.content.clone(),
            file_extension,
            line_number_offset: Some(0),
        })]
    }
}

/// Structured metadata for a successful `glob` tool invocation.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GlobResultMetadata {
    /// Net path count after all filters (`git_ignore`, `dot_ignore`, `only_files`,
    /// `only_dirs`) and limit; unaffected by `only_count`.
    pub matches: usize,
    /// Paths excluded by any active filter or limit (only present when non-zero).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored_matches: Option<usize>,
}

/// Output of a successful `glob` tool invocation.
#[derive(Debug)]
pub struct GlobResult {
    /// Paths included in this response. Empty when `only_count=true`.
    pub matched_paths: Vec<String>,
    /// Net path count after all filters (git_ignore, dot_ignore, only_files, only_dirs) and
    /// limit. Unaffected by `only_count` — counts paths regardless of whether they are
    /// included in `matched_paths`.
    pub matches: usize,
    /// Paths excluded by any filter or limit. `matches + ignored_matches` equals the raw
    /// candidate count before any filtering.
    pub ignored_matches: usize,
}

impl GlobResult {
    fn new(matched_paths: Vec<String>, matches: usize, ignored_matches: usize) -> Self {
        GlobResult {
            matched_paths,
            matches,
            ignored_matches,
        }
    }
}

impl ToolCallResult for GlobResult {
    fn text(&self) -> String {
        if self.matched_paths.is_empty() && self.matches > 0 {
            // only_count mode: no paths in response but the LLM needs the number.
            return self.matches.to_string();
        }
        self.matched_paths.join("\n")
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        let metadata = GlobResultMetadata {
            matches: self.matches,
            ignored_matches: (self.ignored_matches > 0).then_some(self.ignored_matches),
        };
        serde_json::to_value(metadata).ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schemars::schema_for!(GlobResultMetadata))
    }

    fn content_sections(&self) -> Vec<Section> {
        let text = self.matched_paths.join("\n");
        if text.is_empty() {
            return vec![];
        }
        vec![Section::Content(ContentSection {
            label: None,
            content: text,
            file_extension: None,
            line_number_offset: Some(0),
        })]
    }
}

// ── ToolCall objects ──────────────────────────────────────────────────────────

/// Invocation object for a single `read` tool call.
pub struct ReadCall {
    id: uuid::Uuid,
    raw_arguments: serde_json::Value,
    timeout_secs: u64,
}

impl ToolCall for ReadCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }
    fn name(&self) -> &str {
        "read"
    }
    fn arguments(&self) -> &serde_json::Value {
        &self.raw_arguments
    }
    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }
    fn argument_order(&self, key: &str) -> usize {
        ReadTool::default().argument_order(key)
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
        let tool = ReadTool::default();
        self.raw_arguments
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .filter(|(key, value)| !tool.is_default_argument(key.as_str(), value))
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
                let args = serde_json::from_value::<ReadFileArgs>(self.raw_arguments).map_err(
                    |error| ToolError::failed("read", format!("invalid arguments: {error}")),
                )?;
                read_file(args)
                    .await
                    .map(|o| -> Box<dyn ToolCallResult> { Box::new(o) })
            }
            .instrument(span),
        )
    }
}

/// Invocation object for a single `write` tool call.
pub struct WriteCall {
    id: uuid::Uuid,
    raw_arguments: serde_json::Value,
    timeout_secs: u64,
}

impl ToolCall for WriteCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }
    fn name(&self) -> &str {
        "write"
    }
    fn arguments(&self) -> &serde_json::Value {
        &self.raw_arguments
    }
    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }
    fn is_body_argument(&self, key: &str) -> bool {
        key == "content"
    }
    fn argument_order(&self, key: &str) -> usize {
        WriteTool::default().argument_order(key)
    }
    fn pending_content_sections(&self) -> Vec<Section> {
        let content = self
            .raw_arguments
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if content.is_empty() {
            return vec![];
        }
        let path = self
            .raw_arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let file_extension = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        vec![Section::Content(ContentSection {
            label: None,
            content: content.to_string(),
            file_extension,
            line_number_offset: Some(0),
        })]
    }
    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
        self.raw_arguments
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .map(|(key, value)| match key.as_str() {
                "content" => {
                    let content = value.as_str().unwrap_or_default();
                    (
                        key.clone(),
                        Value::String(format!(
                            "REDACTED(size={},lines={})",
                            content.len(),
                            content.lines().count()
                        )),
                    )
                }
                _ => (key.clone(), value.clone()),
            })
            .collect()
    }
    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        let redacted: serde_json::Map<String, Value> =
            self.non_default_redacted_arguments().into_iter().collect();
        let serialized = serde_json::to_string(&redacted).unwrap_or_default();
        let span = tracing::info_span!("tool_args", args = %serialized);
        Box::pin(
            async move {
                let args = serde_json::from_value::<WriteFileArgs>(self.raw_arguments).map_err(
                    |error| ToolError::failed("write", format!("invalid arguments: {error}")),
                )?;
                write_file(args)
                    .await
                    .map(|o| -> Box<dyn ToolCallResult> { Box::new(o) })
            }
            .instrument(span),
        )
    }
}

/// Invocation object for a single `glob` tool call.
pub struct GlobCall {
    id: uuid::Uuid,
    raw_arguments: serde_json::Value,
    timeout_secs: u64,
}

impl ToolCall for GlobCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }
    fn name(&self) -> &str {
        "glob"
    }
    fn arguments(&self) -> &serde_json::Value {
        &self.raw_arguments
    }
    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }
    fn argument_order(&self, key: &str) -> usize {
        GlobTool::default().argument_order(key)
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
        let tool = GlobTool::default();
        self.raw_arguments
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .filter(|(key, value)| !tool.is_default_argument(key.as_str(), value))
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
                let args = serde_json::from_value::<GlobInputArgs>(self.raw_arguments).map_err(
                    |error| ToolError::failed("glob", format!("invalid arguments: {error}")),
                )?;
                glob_files(&args)
                    .await
                    .map(|o| -> Box<dyn ToolCallResult> { Box::new(o) })
            }
            .instrument(span),
        )
    }
}

// ── Tool definitions ──────────────────────────────────────────────────────────

/// The `read` tool: return the contents of a file, optionally trimmed by
/// `mode`-counted `offset`/`limit`.
#[derive(Debug)]
pub struct ReadTool {
    timeout_secs: u64,
    sandbox_context: SandboxContext,
}

impl ReadTool {
    pub const NAME: &'static str = "read";

    pub fn new(sandbox_context: SandboxContext) -> Self {
        ReadTool {
            timeout_secs: 0,
            sandbox_context,
        }
    }

    pub fn with_timeout(timeout_secs: u64, sandbox_context: SandboxContext) -> Self {
        ReadTool {
            timeout_secs,
            sandbox_context,
        }
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        use crate::sandbox::Sandbox;
        ReadTool::new(SandboxContext::new(std::sync::Arc::new(
            std::sync::RwLock::new(Sandbox::new()),
        )))
    }
}

impl Tool for ReadTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Read a file's contents. `offset`/`limit` count the unit selected by `mode`: `bytes` (default, raw and binary-safe), `lines` (split on newline), or `chars` (UTF-8 code points). `offset` is 0-based: line 1 is `offset: 0`, line 2 is `offset: 1`, and so on."
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(ReadFileArgs)
    }

    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        let path_str = match require_string_arg(arguments, "path") {
            Ok(value) => value,
            Err(denied) => return denied,
        };
        // When the sandbox has no opinion, reads are auto-approved.
        // Explicit sandbox verdicts (deny or user-approval) are honoured as-is.
        let sandbox_verdict = self
            .sandbox_context
            .check_path(std::path::Path::new(path_str));
        let approval = match sandbox_verdict {
            None => ApprovalCheck::AutoApproved,
            Some(verdict) => verdict,
        };
        tracing::debug!(
            tool = "read",
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

    fn is_default_argument(&self, key: &str, value: &Value) -> bool {
        match (key, value) {
            ("mode", Value::String(mode)) => mode == "bytes",
            ("offset", Value::Null) => true,
            ("offset", Value::Number(number)) => number.as_u64() == Some(0),
            ("limit", Value::Null) => true,
            _ => false,
        }
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_TIMEOUT_SECS
        } else {
            self.timeout_secs
        }
    }

    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
        Box::new(ReadCall {
            id,
            raw_arguments: arguments,
            timeout_secs: self.timeout_secs(),
        })
    }
}

/// The `write` tool: create or overwrite a file.
#[derive(Debug)]
pub struct WriteTool {
    timeout_secs: u64,
    sandbox_context: SandboxContext,
}

impl WriteTool {
    pub const NAME: &'static str = "write";

    pub fn new(sandbox_context: SandboxContext) -> Self {
        WriteTool {
            timeout_secs: 0,
            sandbox_context,
        }
    }

    pub fn with_timeout(timeout_secs: u64, sandbox_context: SandboxContext) -> Self {
        WriteTool {
            timeout_secs,
            sandbox_context,
        }
    }
}

impl Default for WriteTool {
    fn default() -> Self {
        use crate::sandbox::Sandbox;
        WriteTool::new(SandboxContext::new(std::sync::Arc::new(
            std::sync::RwLock::new(Sandbox::new()),
        )))
    }
}

impl Tool for WriteTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Create or overwrite a file with the given content."
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(WriteFileArgs)
    }

    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        let path_str = match require_string_arg(arguments, "path") {
            Ok(value) => value,
            Err(denied) => return denied,
        };
        if let Err(denied) = require_string_arg(arguments, "content") {
            return denied;
        }
        // When the sandbox has no opinion, writes require explicit user approval.
        // Explicit sandbox verdicts (deny or allow) are honoured as-is.
        let sandbox_verdict = self
            .sandbox_context
            .check_path(std::path::Path::new(path_str));
        let approval = match sandbox_verdict {
            None => ApprovalCheck::UserApprovalRequired,
            Some(verdict) => verdict,
        };
        tracing::debug!(
            tool = "write",
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
            .map(|(key, value)| match key {
                "content" => {
                    let content = value.as_str().unwrap_or_default();
                    (
                        key.to_string(),
                        Value::String(format!(
                            "REDACTED(size={},lines={})",
                            content.len(),
                            content.lines().count()
                        )),
                    )
                }
                _ => (key.to_string(), value.clone()),
            })
            .collect()
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_TIMEOUT_SECS
        } else {
            self.timeout_secs
        }
    }

    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
        Box::new(WriteCall {
            id,
            raw_arguments: arguments,
            timeout_secs: self.timeout_secs(),
        })
    }
}

/// The `glob` tool: enumerate pathnames matching a pattern.
#[derive(Debug)]
pub struct GlobTool {
    timeout_secs: u64,
    sandbox_context: SandboxContext,
}

impl GlobTool {
    pub const NAME: &'static str = "glob";

    pub fn new(sandbox_context: SandboxContext) -> Self {
        GlobTool {
            timeout_secs: 0,
            sandbox_context,
        }
    }

    pub fn with_timeout(timeout_secs: u64, sandbox_context: SandboxContext) -> Self {
        GlobTool {
            timeout_secs,
            sandbox_context,
        }
    }
}

impl Default for GlobTool {
    fn default() -> Self {
        use crate::sandbox::Sandbox;
        GlobTool::new(SandboxContext::new(std::sync::Arc::new(
            std::sync::RwLock::new(Sandbox::new()),
        )))
    }
}

impl Tool for GlobTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "List paths matching a glob-wildcard pattern (`*`, `?`, `**`, `[...]`). Patterns resolve relative to cwd; absolute patterns supported. Hidden files (dot-prefixed) are included by default — use `dot_ignore: true` to exclude them. `git_ignore` (default `true`) skips gitignored paths via `ignore::WalkBuilder`."
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(GlobInputArgs)
    }

    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        let pattern_str = match require_string_arg(arguments, "pattern") {
            Ok(value) => value,
            Err(denied) => return denied,
        };
        let root = pattern_root(pattern_str);
        let sandbox_verdict = self.sandbox_context.check_path(std::path::Path::new(&root));
        let approval = match sandbox_verdict {
            None => ApprovalCheck::AutoApproved,
            Some(verdict) => verdict,
        };
        tracing::debug!(
            tool = "glob",
            pattern = pattern_str,
            root = root,
            sandbox = match &approval {
                ApprovalCheck::AutoApproved => "abstain→auto-approved",
                ApprovalCheck::AutoDenied { .. } => "auto-denied",
                ApprovalCheck::UserApprovalRequired => "user-approval-required",
            },
            "tool approval decision"
        );
        approval
    }

    fn is_default_argument(&self, key: &str, value: &Value) -> bool {
        match (key, value) {
            ("git_ignore", Value::Bool(git_ignore)) => *git_ignore,
            ("only_count", Value::Bool(false)) => true,
            ("only_files", Value::Bool(false)) => true,
            ("only_dirs", Value::Bool(false)) => true,
            ("dot_ignore", Value::Bool(false)) => true,
            ("limit", Value::Null) => true,
            _ => false,
        }
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_TIMEOUT_SECS
        } else {
            self.timeout_secs
        }
    }

    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
        Box::new(GlobCall {
            id,
            raw_arguments: arguments,
            timeout_secs: self.timeout_secs(),
        })
    }
}

async fn read_file(args: ReadFileArgs) -> Result<ReadResult, ToolError> {
    tracing::debug!("reading file");
    // TODO: should we really read the whole file into memory
    //       and get the chunks afterwards ?
    //       Could eat much memory on really big files !?
    let bytes = tokio::fs::read(&args.path).await.map_err(|error| {
        tracing::error!(error = %error,"failed to read");
        ToolError::failed("read", format!("failed to read '{}': {error}", args.path))
    })?;
    match args.mode {
        ReadMode::Bytes => {
            let start = args.offset.unwrap_or(0).min(bytes.len());
            let mut slice = &bytes[start..];
            if let Some(limit) = args.limit {
                slice = &slice[..limit.min(slice.len())];
            }
            let content = String::from_utf8_lossy(slice).to_string();
            tracing::debug!(
                size = content.len(),
                lines = content.lines().count(),
                "read file"
            );
            Ok(ReadResult {
                content,
                path: args.path,
                starting_line: Some(0),
            })
        }
        ReadMode::Lines => {
            let text = String::from_utf8_lossy(&bytes).to_string();
            let lines: Vec<&str> = text.split_terminator('\n').collect();
            let start = args.offset.unwrap_or(0).min(lines.len());
            let end = args
                .limit
                .map(|limit| (start + limit).min(lines.len()))
                .unwrap_or(lines.len());
            let content = if start < end {
                lines[start..end].join("\n")
            } else {
                String::new()
            };
            tracing::debug!(
                size = content.len(),
                lines = content.lines().count(),
                "read file"
            );
            Ok(ReadResult {
                content,
                path: args.path,
                starting_line: Some(start),
            })
        }
        ReadMode::Chars => {
            let text = String::from_utf8_lossy(&bytes).to_string();
            let chars: Vec<char> = text.chars().collect();
            let start = args.offset.unwrap_or(0).min(chars.len());
            let end = args
                .limit
                .map(|limit| (start + limit).min(chars.len()))
                .unwrap_or(chars.len());
            let content: String = if start < end {
                chars[start..end].iter().collect()
            } else {
                String::new()
            };
            tracing::debug!(
                size = content.len(),
                lines = content.lines().count(),
                "read file"
            );
            Ok(ReadResult {
                content,
                path: args.path,
                starting_line: Some(0),
            })
        }
    }
}

async fn write_file(args: WriteFileArgs) -> Result<WriteResult, ToolError> {
    tracing::debug!("writing file");
    if let Some(parent) = Path::new(&args.path).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                tracing::error!(
                    error = %error,
                    path = %parent.display(),
                    "failed to create directory"
                );
                ToolError::failed(
                    "write",
                    format!("failed to create '{}': {error}", parent.display()),
                )
            })?;
        }
    }
    let bytes_written = args.content.len();
    tokio::fs::write(&args.path, &args.content)
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "failed to write");
            ToolError::failed("write", format!("failed to write '{}': {error}", args.path))
        })?;
    tracing::debug!("wrote file");
    Ok(WriteResult::new(args.path, args.content, bytes_written))
}

fn pattern_root(pattern: &str) -> String {
    let first_glob_position = pattern
        .char_indices()
        .find(|(_, character)| matches!(character, '*' | '?' | '['))
        .map(|(index, _)| index);
    let Some(first_glob_position) = first_glob_position else {
        return ".".to_string();
    };
    let slash_position = pattern[..first_glob_position].rfind('/');
    match slash_position {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(position) => pattern[..position].to_string(),
    }
}

fn normalize_glob_match_path(path: &Path) -> String {
    let path_string = path.display().to_string();
    if path_string == "." {
        String::new()
    } else if let Some(stripped_prefix) = path_string.strip_prefix("./") {
        stripped_prefix.to_string()
    } else {
        path_string
    }
}

fn entry_matches_filters(entry: &ignore::DirEntry, args: &GlobInputArgs) -> bool {
    if args.only_files {
        return entry.file_type().map(|file_type| file_type.is_file()) == Some(true);
    }
    if args.only_dirs {
        return entry.file_type().map(|file_type| file_type.is_dir()) == Some(true);
    }
    true
}

async fn glob_files(args: &GlobInputArgs) -> Result<GlobResult, ToolError> {
    if args.only_files && args.only_dirs {
        return Err(ToolError::failed(
            "glob",
            "arguments 'only_files' and 'only_dirs' cannot both be true",
        ));
    }
    tracing::debug!(pattern = %args.pattern, "glob searching");
    let (mut sorted, candidate_count): (Vec<String>, usize) = if args.git_ignore {
        let compiled_pattern = glob::Pattern::new(&args.pattern).map_err(|error| {
            ToolError::failed(
                "glob",
                format!("invalid pattern '{}': {error}", args.pattern),
            )
        })?;
        let root = pattern_root(&args.pattern);
        let mut candidate_count: usize = 0;
        let mut collected_paths: Vec<String> = Vec::new();
        let walker = ignore::WalkBuilder::new(&root)
            .git_ignore(true)
            .require_git(false)
            .hidden(args.dot_ignore)
            .build();
        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            candidate_count += 1;
            if !entry_matches_filters(&entry, args) {
                continue;
            }
            let entry_path = entry.path();
            let match_path = normalize_glob_match_path(entry_path);
            if match_path.is_empty() || !compiled_pattern.matches(&match_path) {
                continue;
            }
            collected_paths.push(entry_path.display().to_string());
        }
        (collected_paths, candidate_count)
    } else {
        let match_options = glob::MatchOptions {
            require_literal_leading_dot: args.dot_ignore,
            ..Default::default()
        };
        let all_matches: Vec<_> = glob::glob_with(&args.pattern, match_options)
            .map_err(|error| {
                ToolError::failed(
                    "glob",
                    format!("invalid pattern '{}': {error}", args.pattern),
                )
            })?
            .filter_map(Result::ok)
            .collect();
        let candidate_count = all_matches.len();
        let paths = all_matches
            .into_iter()
            .filter(|path| {
                if args.only_files {
                    return path.is_file();
                }
                if args.only_dirs {
                    return path.is_dir();
                }
                true
            })
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        (paths, candidate_count)
    };
    sorted.sort();
    if let Some(limit) = args.limit {
        if sorted.len() > limit {
            sorted.truncate(limit);
        }
    }
    let matches = sorted.len();
    let ignored = candidate_count - matches;
    if args.only_count {
        sorted.clear();
    }
    tracing::debug!(matches, ignored, "glob searched");
    Ok(GlobResult::new(sorted, matches, ignored))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::capture_logs;
    use serde_json::json;
    use tempfile::TempDir;

    fn glob_args(pattern: impl Into<String>) -> GlobInputArgs {
        GlobInputArgs {
            pattern: pattern.into(),
            git_ignore: true,
            only_count: false,
            only_files: false,
            only_dirs: false,
            limit: None,
            dot_ignore: false,
        }
    }

    #[test]
    fn read_tool_auto_approves_workspace_path_and_has_default_timeout() {
        let tool = ReadTool::default();
        let args = serde_json::json!({ "path": "src/main.rs" });
        // Phase 6.0: reads inside the workspace are auto-approved by the sandbox;
        // the cwd is the project root in tests, so "src/main.rs" is in-workspace.
        assert!(matches!(
            tool.needs_approval(&args),
            crate::sandbox::ApprovalCheck::AutoApproved
        ));
        assert_eq!(tool.timeout_secs(), DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn write_tool_requires_approval() {
        let tool = WriteTool::default();
        let args = serde_json::json!({ "path": "src/main.rs", "content": "hello" });
        assert!(matches!(
            tool.needs_approval(&args),
            crate::sandbox::ApprovalCheck::UserApprovalRequired
        ));
    }

    #[test]
    fn write_tool_outside_workspace_is_auto_denied() {
        let tool = WriteTool::default();
        // /tmp is outside the project workspace root (cwd), so the sandbox denies it.
        let args = serde_json::json!({ "path": "/tmp/evil.sh", "content": "rm -rf /" });
        assert!(matches!(
            tool.needs_approval(&args),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
    }

    #[test]
    fn read_tool_missing_path_is_auto_denied() {
        let tool = ReadTool::default();
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({})),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({ "path": "" })),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({ "path": 42 })),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
    }

    #[test]
    fn write_tool_missing_required_args_are_auto_denied() {
        let tool = WriteTool::default();
        // Missing path.
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({ "content": "hello" })),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
        // Missing content.
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({ "path": "out.txt" })),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
        // Both missing.
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({})),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
    }

    #[test]
    fn write_tool_empty_content_is_auto_denied() {
        // An empty string for `content` is treated as a malformed call, consistent
        // with how an empty `path` is handled.
        let tool = WriteTool::default();
        let args = serde_json::json!({ "path": "src/empty.rs", "content": "" });
        assert!(matches!(
            tool.needs_approval(&args),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
    }

    #[test]
    fn glob_tool_missing_pattern_is_auto_denied() {
        let tool = GlobTool::default();
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({})),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({ "pattern": "" })),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
    }

    #[test]
    fn undeclared_tool_timeout_uses_default() {
        assert_eq!(ReadTool::default().timeout_secs(), DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn read_tool_schema_includes_mode_selector() {
        let schema = ReadTool::default().input_schema();
        let value = serde_json::to_value(&schema).expect("schema serializes");
        let required = value["required"].as_array().expect("required is an array");
        assert_eq!(required.len(), 1, "only `path` is required");
        assert_eq!(required[0], serde_json::json!("path"));
        assert_eq!(
            value["properties"]["mode"]["$ref"],
            serde_json::json!("#/definitions/ReadMode")
        );
        let variants = value["definitions"]["ReadMode"]["enum"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .expect("mode enum present");
        assert_eq!(variants, vec!["bytes", "lines", "chars"]);
    }

    #[test]
    fn read_schema_documents_offset_as_zero_based() {
        let schema = ReadTool::default().input_schema();
        let value = serde_json::to_value(&schema).expect("schema serializes");
        let offset_description = value["properties"]["offset"]["description"]
            .as_str()
            .expect("offset property description");
        assert!(
            offset_description.contains("0-based"),
            "offset schema must state its 0-based convention, got: {offset_description}"
        );
    }
    #[test]
    fn read_returns_whole_file_content() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, "hello world").expect("write");
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: ReadMode::Bytes,
                offset: None,
                limit: None,
            })
            .await
            .expect("read ok");
            assert_eq!(result.text(), "hello world");
        });
    }

    #[test]
    fn read_missing_file_is_error() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let result = read_file(ReadFileArgs {
                path: dir.path().join("nope.txt").display().to_string(),
                mode: ReadMode::Bytes,
                offset: None,
                limit: None,
            })
            .await;
            assert!(result.is_err());
        });
    }

    #[test]
    fn read_bytes_honors_offset_and_limit() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, "0123456789").expect("write");
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: ReadMode::Bytes,
                offset: Some(2),
                limit: Some(4),
            })
            .await
            .expect("read ok");
            assert_eq!(result.text(), "2345");
        });
    }

    #[test]
    fn read_lines_honors_offset_and_limit() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, "one\ntwo\nthree\nfour").expect("write");
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: ReadMode::Lines,
                offset: Some(1),
                limit: Some(2),
            })
            .await
            .expect("read ok");
            assert_eq!(result.text(), "two\nthree");
        });
    }

    #[test]
    fn read_chars_counts_utf8_code_points() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            // `é` is 2 bytes but 1 UTF-8 code point.
            std::fs::write(&path, "héllo").expect("write");
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: ReadMode::Chars,
                offset: Some(1),
                limit: Some(3),
            })
            .await
            .expect("read ok");
            assert_eq!(result.text(), "éll");
        });
    }

    #[test]
    fn write_creates_file_and_returns_byte_count() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("sub").join("out.txt");
            let result = write_file(WriteFileArgs {
                path: path.display().to_string(),
                content: "abc".to_string(),
            })
            .await
            .expect("write ok");
            assert!(result.text().contains("wrote 3 bytes"));
            let content = std::fs::read_to_string(&path).expect("read back");
            assert_eq!(content, "abc");
        });
    }

    #[test]
    fn glob_with_git_ignore_false_matches_files() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let sub = dir.path().join("sub");
            std::fs::create_dir_all(&sub).expect("mkdir");
            for name in ["a.txt", "b.txt"] {
                std::fs::write(dir.path().join(name), name).expect("write");
            }
            std::fs::write(sub.join("c.txt"), "c").expect("write");

            let matches = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: false,
                ..glob_args("")
            })
            .await
            .expect("glob ok");
            let text = matches.text();
            let lines: Vec<&str> = text.lines().collect();
            assert!(lines.iter().any(|l: &&str| l.ends_with("a.txt")));
            assert!(lines.iter().any(|l: &&str| l.ends_with("b.txt")));
            assert!(!lines.iter().any(|l: &&str| l.ends_with("c.txt")));
        });
    }

    #[test]
    fn glob_with_git_ignore_true_matches_files() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("a.txt"), "a").expect("write");
            std::fs::write(dir.path().join("ignored.txt"), "ignored").expect("write");
            std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").expect("write");

            let result = glob_files(&glob_args(format!("{}/*.txt", dir.path().display())))
                .await
                .expect("glob ok");
            assert!(result
                .matched_paths
                .iter()
                .any(|path| path.ends_with("a.txt")));
            assert!(!result
                .matched_paths
                .iter()
                .any(|path| path.ends_with("ignored.txt")));
        });
    }

    #[test]
    fn glob_only_files_excludes_dirs() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let subdirectory = dir.path().join("sub");
            std::fs::create_dir_all(&subdirectory).expect("mkdir");
            std::fs::write(dir.path().join("a.txt"), "a").expect("write");
            std::fs::write(subdirectory.join("b.txt"), "b").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/**", dir.path().display()),
                only_files: true,
                git_ignore: false,
                ..glob_args("")
            })
            .await
            .expect("glob ok");
            assert!(result
                .matched_paths
                .iter()
                .all(|path| std::path::Path::new(path).is_file()));
        });
    }

    #[test]
    fn glob_only_dirs_excludes_files() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let subdirectory = dir.path().join("sub");
            std::fs::create_dir_all(&subdirectory).expect("mkdir");
            std::fs::write(dir.path().join("a.txt"), "a").expect("write");
            std::fs::write(subdirectory.join("b.txt"), "b").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/**", dir.path().display()),
                only_dirs: true,
                git_ignore: false,
                ..glob_args("")
            })
            .await
            .expect("glob ok");
            assert!(!result.matched_paths.is_empty());
            assert!(result
                .matched_paths
                .iter()
                .all(|path| std::path::Path::new(path).is_dir()));
        });
    }

    #[test]
    fn glob_contradictory_only_files_and_dirs_is_error() {
        crate::test_support::run_locked(async {
            let result = glob_files(&GlobInputArgs {
                pattern: "*.txt".to_string(),
                only_files: true,
                only_dirs: true,
                ..glob_args("")
            })
            .await;
            assert!(result.is_err());
        });
    }

    #[test]
    fn glob_limit_caps_results_and_sets_truncated_metadata() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            for index in 0..5 {
                std::fs::write(dir.path().join(format!("{index}.txt")), format!("{index}"))
                    .expect("write");
            }

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: false,
                limit: Some(3),
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert_eq!(result.matched_paths.len(), 3);
            assert_eq!(result.matches, 3);
            assert_eq!(
                result.metadata(),
                Some(json!({
                    "matches": 3,
                    "ignored_matches": 2,
                }))
            );
        });
    }

    #[test]
    fn glob_only_count_returns_empty_content_with_total_in_metadata() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            for index in 0..5 {
                std::fs::write(dir.path().join(format!("{index}.txt")), format!("{index}"))
                    .expect("write");
            }

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: false,
                only_count: true,
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert_eq!(result.text(), "5");
            assert!(result.matched_paths.is_empty());
            assert_eq!(result.matches, 5);
            assert_eq!(
                result.metadata(),
                Some(json!({
                    "matches": 5,
                }))
            );
        });
    }

    #[test]
    fn glob_needs_approval_auto_approves_inside_workspace() {
        let tool = GlobTool::default();
        let args = serde_json::json!({ "pattern": "src/**/*.rs" });
        assert!(matches!(
            tool.needs_approval(&args),
            ApprovalCheck::AutoApproved
        ));
    }

    #[test]
    fn glob_needs_approval_denies_outside_workspace() {
        let tool = GlobTool::default();
        let args = serde_json::json!({ "pattern": "/tmp/**" });
        assert!(matches!(
            tool.needs_approval(&args),
            ApprovalCheck::AutoDenied { .. }
        ));
    }

    #[test]
    fn glob_invalid_pattern_is_error() {
        crate::test_support::run_locked(async {
            let result = glob_files(&glob_args("[")).await;
            assert!(result.is_err());
        });
    }

    #[test]
    fn read_tool_orders_path_first_and_marks_its_defaults() {
        let tool = ReadTool::default();
        assert_eq!(tool.argument_order("path"), 0);
        assert!(tool.argument_order("mode") < tool.argument_order("offset"));
        assert!(tool.is_default_argument("mode", &Value::from("bytes")));
        assert!(!tool.is_default_argument("mode", &Value::from("lines")));
        assert!(tool.is_default_argument("offset", &Value::Null));
        assert!(tool.is_default_argument("offset", &Value::from(0)));
        assert!(!tool.is_default_argument("offset", &Value::from(3)));
        assert!(tool.is_default_argument("limit", &Value::Null));
        assert!(!tool.is_default_argument("limit", &Value::from(5)));
    }

    #[test]
    fn read_tool_non_default_arguments_drop_defaults() {
        let tool = ReadTool::default();
        let args = serde_json::json!({
            "path": "a.txt",
            "mode": "bytes",
            "offset": 0,
            "limit": 3,
        });
        let keys: Vec<&str> = tool
            .non_default_arguments(&args)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, vec!["path", "limit"]);
    }

    #[test]
    fn write_and_glob_order_their_primary_argument_first() {
        assert!(WriteTool::default().argument_order("path") == 0);
        assert!(
            WriteTool::default().argument_order("path")
                < WriteTool::default().argument_order("content")
        );
        let glob_tool = GlobTool::default();
        assert_eq!(glob_tool.argument_order("pattern"), 0);
        assert!(glob_tool.is_default_argument("git_ignore", &Value::Bool(true)));
        assert!(glob_tool.is_default_argument("only_count", &Value::Bool(false)));
        assert!(glob_tool.is_default_argument("only_files", &Value::Bool(false)));
        assert!(glob_tool.is_default_argument("only_dirs", &Value::Bool(false)));
        assert!(glob_tool.is_default_argument("limit", &Value::Null));
        assert!(!glob_tool.is_default_argument("git_ignore", &Value::Bool(false)));
    }

    /// A `tracing` log target that appends to an in-memory buffer
    /// synchronously in the calling thread, plus a `capture_logs` helper that
    /// runs an async body under it; both live in shared test-support
    /// (`src/test_support.rs`), SSOT.
    #[test]
    fn read_file_logs_start_and_result_size() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "hello world").expect("seed file");
        let log = capture_logs(async {
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: ReadMode::Bytes,
                offset: None,
                limit: None,
            })
            .await;
            assert!(result.is_ok());
        });
        assert!(
            log.contains("reading file"),
            "read_file must log its start: {log}"
        );
        assert!(log.contains("read file"), "read_file must log: {log}");
        assert!(
            log.contains("size="),
            "read_file must log the returned content size: {log}"
        );
        assert!(
            log.contains("lines="),
            "read_file must log the returned content line count: {log}"
        );
    }

    #[test]
    fn write_file_logs_completion_but_not_content() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("out.txt");
        let log = capture_logs(async {
            let result = write_file(WriteFileArgs {
                path: path.display().to_string(),
                content: "TOP-SECRET-DATA".to_string(),
            })
            .await;
            assert!(result.is_ok());
        });
        assert!(
            log.contains("writing file"),
            "write_file must log its start: {log}"
        );
        assert!(
            log.contains("wrote file"),
            "write_file must log its completion: {log}"
        );
        assert!(
            !log.contains("TOP-SECRET-DATA"),
            "write_file must never log the written content: {log}"
        );
    }

    #[test]
    fn glob_files_logs_pattern_and_match_count() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "a").expect("seed file");
        std::fs::write(dir.path().join("b.txt"), "b").expect("seed file");
        let args = GlobInputArgs {
            pattern: format!("{}/*.txt", dir.path().display()),
            git_ignore: false,
            ..glob_args("")
        };
        let log = capture_logs(async move {
            let result = glob_files(&args).await;
            assert!(result.is_ok());
        });
        assert!(
            log.contains("glob searched"),
            "glob_files must log its result: {log}"
        );
        assert!(
            log.contains("matches=2"),
            "glob_files must log the match count: {log}"
        );
    }

    #[test]
    fn read_tool_run_records_non_default_arguments_in_span() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "hello world").expect("seed file");
        let log = capture_logs(async {
            let call = ReadTool::default().create_call(
                uuid::Uuid::nil(),
                serde_json::json!({
                    "path": path.display().to_string(),
                    "mode": "bytes",
                    "offset": 0,
                }),
            );
            let result = call.run().await;
            assert!(result.is_ok());
        });
        assert!(
            log.contains("tool_args"),
            "run must open an args span: {log}"
        );
        assert!(
            log.contains("\"path\""),
            "the non-default path argument must be recorded: {log}"
        );
    }

    #[test]
    fn write_tool_run_redacts_content_in_span_arguments() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("out.txt");
        let log = capture_logs(async {
            let call = WriteTool::default().create_call(
                uuid::Uuid::nil(),
                serde_json::json!({
                    "path": path.display().to_string(),
                    "content": "TOP-SECRET-DATA",
                }),
            );
            let result = call.run().await;
            assert!(result.is_ok());
        });
        assert!(
            log.contains("tool_args"),
            "run must open an args span: {log}"
        );
        assert!(
            log.contains("\"path\""),
            "the non-default path argument must be recorded: {log}"
        );
        assert!(
            log.contains("REDACTED(size=15,lines=1)"),
            "the write content must be replaced by a size/lines summary: {log}"
        );
        assert!(
            !log.contains("TOP-SECRET-DATA"),
            "the write content must never be recorded in the span: {log}"
        );
    }

    #[test]
    fn read_non_default_redacted_arguments_drop_defaults_and_keep_path() {
        let tool = ReadTool::default();
        let arguments = serde_json::json!({
            "path": "a.txt",
            "mode": "bytes",
            "offset": 0,
            "limit": serde_json::Value::Null,
        });
        let redacted = tool.non_default_redacted_arguments(&arguments);
        let keys: Vec<&str> = redacted.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, vec!["path"]);
    }

    #[test]
    fn write_non_default_redacted_arguments_redact_content() {
        let tool = WriteTool::default();
        let arguments = serde_json::json!({
            "path": "out.txt",
            "content": "TOP-SECRET-DATA",
        });
        let redacted = tool.non_default_redacted_arguments(&arguments);
        let by_key: std::collections::HashMap<String, Value> = redacted.into_iter().collect();
        assert_eq!(by_key.get("path"), Some(&Value::String("out.txt".into())));
        assert_eq!(
            by_key.get("content"),
            Some(&Value::String("REDACTED(size=15,lines=1)".into()))
        );
    }

    #[test]
    fn read_tool_done_content_sections_have_line_numbers() {
        crate::test_support::run_locked(async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test.rs");
            std::fs::write(&path, "fn main() {}\nfn foo() {}\n").unwrap();
            let result = read_file(ReadFileArgs {
                path: path.to_str().unwrap().to_string(),
                mode: ReadMode::Bytes,
                offset: None,
                limit: None,
            })
            .await
            .unwrap();
            let sections = result.content_sections();
            assert_eq!(sections.len(), 1, "expected one body section");
            let section = &sections[0];
            let content_section = match section {
                super::Section::Content(cs) => cs,
                other => panic!("expected Section::Content, got {other:?}"),
            };
            assert_eq!(
                content_section.line_number_offset,
                Some(0),
                "bytes mode must carry line_number_offset Some(0)"
            );
            assert_eq!(content_section.file_extension.as_deref(), Some("rs"));
        });
    }

    #[test]
    fn read_result_empty_content_has_no_sections() {
        let result = ReadResult {
            content: String::new(),
            path: "empty.txt".to_string(),
            starting_line: None,
        };
        assert!(result.content_sections().is_empty());
    }

    #[test]
    fn write_result_empty_content_has_no_sections() {
        let result = WriteResult::new("empty.txt", "", 0);
        assert!(result.content_sections().is_empty());
    }

    #[test]
    fn glob_result_content_sections_empty_and_nonempty() {
        let empty_result = GlobResult::new(vec![], 0, 0);
        assert!(empty_result.content_sections().is_empty());

        let result = GlobResult::new(vec!["src/main.rs".to_string()], 1, 0);
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        assert!(matches!(sections[0], super::Section::Content(_)));
    }

    #[test]
    fn read_call_tool_call_methods() {
        let id = uuid::Uuid::new_v4();
        let args = serde_json::json!({ "path": "src/main.rs" });
        let call = ReadTool::default().create_call(id, args.clone());
        assert_eq!(call.id(), id);
        assert_eq!(call.name(), "read");
        assert_eq!(call.arguments(), &args);
        assert_eq!(call.timeout_secs(), DEFAULT_TIMEOUT_SECS);
        assert_eq!(call.argument_order("path"), 0);
        let redacted = call.non_default_redacted_arguments();
        assert!(redacted.iter().any(|(key, _)| key == "path"));
    }

    #[test]
    fn write_call_tool_call_methods() {
        let id = uuid::Uuid::new_v4();
        let args = serde_json::json!({ "path": "out.txt", "content": "hello" });
        let call = WriteTool::default().create_call(id, args.clone());
        assert_eq!(call.id(), id);
        assert_eq!(call.name(), "write");
        assert_eq!(call.arguments(), &args);
        assert_eq!(call.argument_order("path"), 0);
        assert!(call.is_body_argument("content"));
        assert!(!call.is_body_argument("path"));
        let redacted = call.non_default_redacted_arguments();
        let content_entry = redacted.iter().find(|(key, _)| key == "content");
        assert!(
            content_entry.map(|(_, v)| v.as_str().unwrap_or("").starts_with("REDACTED"))
                == Some(true)
        );
        let sections = call.pending_content_sections();
        assert_eq!(sections.len(), 1);
    }

    #[test]
    fn write_call_pending_content_sections_empty_for_empty_content() {
        let args = serde_json::json!({ "path": "out.txt", "content": "" });
        let call = WriteTool::default().create_call(uuid::Uuid::new_v4(), args);
        assert!(call.pending_content_sections().is_empty());
    }

    #[test]
    fn glob_call_tool_call_methods() {
        let id = uuid::Uuid::new_v4();
        let args = serde_json::json!({ "pattern": "**/*.rs" });
        let call = GlobTool::default().create_call(id, args.clone());
        assert_eq!(call.id(), id);
        assert_eq!(call.name(), "glob");
        assert_eq!(call.arguments(), &args);
        assert_eq!(call.timeout_secs(), DEFAULT_TIMEOUT_SECS);
        assert_eq!(call.argument_order("pattern"), 0);
        let redacted = call.non_default_redacted_arguments();
        assert!(redacted.iter().any(|(key, _)| key == "pattern"));
    }

    #[test]
    fn tools_with_timeout_constructor() {
        let make_ctx = || {
            SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
                crate::sandbox::Sandbox::new(),
            )))
        };
        let read_tool = ReadTool::with_timeout(10, make_ctx());
        assert_eq!(read_tool.timeout_secs(), 10);
        let write_tool = WriteTool::with_timeout(20, make_ctx());
        assert_eq!(write_tool.timeout_secs(), 20);
        let glob_tool = GlobTool::with_timeout(30, make_ctx());
        assert_eq!(glob_tool.timeout_secs(), 30);
    }

    #[test]
    fn glob_default_includes_hidden_files() {
        // dot_ignore defaults to false — hidden files must appear in results.
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("visible.txt"), "v").expect("write");
            std::fs::write(dir.path().join(".hidden.txt"), "h").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: false,
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert!(
                result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with("visible.txt")),
                "visible file must be present"
            );
            assert!(
                result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with(".hidden.txt")),
                "hidden file must be present when dot_ignore=false"
            );
        });
    }

    #[test]
    fn glob_dot_ignore_excludes_hidden_files() {
        // dot_ignore=true must exclude any path component starting with '.'.
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("visible.txt"), "v").expect("write");
            std::fs::write(dir.path().join(".hidden.txt"), "h").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: false,
                dot_ignore: true,
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert!(
                result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with("visible.txt")),
                "visible file must be present"
            );
            assert!(
                !result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with(".hidden.txt")),
                "hidden file must be absent when dot_ignore=true"
            );
        });
    }

    #[test]
    fn glob_dot_ignore_excludes_hidden_files_git_ignore_mode() {
        // dot_ignore=true must also exclude hidden files in the git_ignore=true path.
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("visible.txt"), "v").expect("write");
            std::fs::write(dir.path().join(".hidden.txt"), "h").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: true,
                dot_ignore: true,
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert!(
                result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with("visible.txt")),
                "visible file must be present"
            );
            assert!(
                !result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with(".hidden.txt")),
                "hidden file must be absent when dot_ignore=true"
            );
        });
    }

    #[test]
    fn read_input_args_all_properties_have_descriptions() {
        let schema = ReadTool::default().input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "ReadFileArgs");
    }

    #[test]
    fn write_input_args_all_properties_have_descriptions() {
        let schema = WriteTool::default().input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "WriteFileArgs");
    }

    #[test]
    fn glob_input_args_all_properties_have_descriptions() {
        let schema = GlobTool::default().input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "GlobInputArgs");
    }
}
