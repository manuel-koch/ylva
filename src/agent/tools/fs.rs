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
use serde::Deserialize;
use serde_json::Value;
use tracing::Instrument;

use super::{ContentSection, Tool, ToolCall, ToolCallResult, ToolError, DEFAULT_TIMEOUT_SECS};

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
    pub path: String,
    pub content: String,
}

/// `glob` arguments: a glob pattern resolved against the current directory.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct GlobArgs {
    pub pattern: String,
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
    fn text(&self) -> &str {
        &self.content
    }

    fn properties(&self) -> Vec<(&'static str, String)> {
        vec![
            ("read_bytes", format!("{}", self.content.len())),
            ("read_lines", format!("{}", self.content.lines().count())),
        ]
    }

    fn content_sections(&self) -> Vec<ContentSection> {
        if self.content.is_empty() {
            return vec![];
        }
        let file_extension = std::path::Path::new(&self.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        vec![ContentSection {
            label: None,
            content: self.content.clone(),
            file_extension,
            line_number_offset: self.starting_line,
        }]
    }
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
    fn text(&self) -> &str {
        &self.text
    }

    fn properties(&self) -> Vec<(&'static str, String)> {
        vec![("bytes_written", format!("{}", self.bytes_written))]
    }

    fn content_sections(&self) -> Vec<ContentSection> {
        if self.content.is_empty() {
            return vec![];
        }
        let file_extension = std::path::Path::new(&self.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        vec![ContentSection {
            label: None,
            content: self.content.clone(),
            file_extension,
            line_number_offset: Some(0),
        }]
    }
}

/// Output of a successful `glob` tool invocation.
#[derive(Debug)]
pub struct GlobResult {
    /// Matched paths, sorted.
    pub matched_paths: Vec<String>,
    /// Newline-joined paths — the text returned to the LLM.
    text: String,
}

impl GlobResult {
    fn new(matched_paths: Vec<String>) -> Self {
        let text = matched_paths.join("\n");
        GlobResult {
            matched_paths,
            text,
        }
    }
}

impl ToolCallResult for GlobResult {
    fn text(&self) -> &str {
        &self.text
    }

    fn properties(&self) -> Vec<(&'static str, String)> {
        vec![("matches", self.matched_paths.len().to_string())]
    }

    fn content_sections(&self) -> Vec<ContentSection> {
        if self.text.is_empty() {
            return vec![];
        }
        vec![ContentSection {
            label: None,
            content: self.text.clone(),
            file_extension: None,
            line_number_offset: Some(0),
        }]
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
    fn pending_content_sections(&self) -> Vec<ContentSection> {
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
        vec![ContentSection {
            label: None,
            content: content.to_string(),
            file_extension,
            line_number_offset: Some(0),
        }]
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
                let args =
                    serde_json::from_value::<GlobArgs>(self.raw_arguments).map_err(|error| {
                        ToolError::failed("glob", format!("invalid arguments: {error}"))
                    })?;
                glob_files(&args.pattern)
                    .await
                    .map(|o| -> Box<dyn ToolCallResult> { Box::new(o) })
            }
            .instrument(span),
        )
    }
}

// ── Tool definitions ──────────────────────────────────────────────────────────

/// The `read` tool: return the contents of a file, optionally trimmed by
/// `mode`-counted `offset`/`limit`. `needs_approval` is false — an
/// **interim/fake** read-only marker (Q5): the real Phase 6 policy is
/// path-based, and an out-of-workspace read may then require approval.
#[derive(Debug, Default)]
pub struct ReadTool {
    timeout_secs: u64,
}

impl ReadTool {
    pub fn with_timeout(timeout_secs: u64) -> Self {
        ReadTool { timeout_secs }
    }
}

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn title(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read a file's contents. `offset`/`limit` count the unit selected by `mode`: `bytes` (default, raw and binary-safe), `lines` (split on newline), or `chars` (UTF-8 code points). `offset` is 0-based: line 1 is `offset: 0`, line 2 is `offset: 1`, and so on."
    }

    fn schema(&self) -> RootSchema {
        schema_for!(ReadFileArgs)
    }

    fn needs_approval(&self) -> bool {
        false
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

/// The `write` tool: create or overwrite a file. `needs_approval` is true, so
/// a CLI-only host with no auto-approve denies it.
#[derive(Debug, Default)]
pub struct WriteTool {
    timeout_secs: u64,
}

impl WriteTool {
    pub fn with_timeout(timeout_secs: u64) -> Self {
        WriteTool { timeout_secs }
    }
}

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn title(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file with the given content."
    }

    fn schema(&self) -> RootSchema {
        schema_for!(WriteFileArgs)
    }

    fn needs_approval(&self) -> bool {
        true
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

/// The `glob` tool: enumerate pathnames matching a pattern. `needs_approval`
/// is false — an **interim/fake** read-only marker (Q5): the real Phase 6
/// policy is path-based, and an out-of-workspace glob may then require
/// approval.
#[derive(Debug, Default)]
pub struct GlobTool {
    timeout_secs: u64,
}

impl GlobTool {
    pub fn with_timeout(timeout_secs: u64) -> Self {
        GlobTool { timeout_secs }
    }
}

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn title(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        "List files matching a glob-wildcard pattern (e.g. `src/**/*.rs`, `*.txt`). Only the glob dialect is accepted (`*`, `?`, `**`, `[...]`) — never regular expressions. Patterns resolve relative to the current working directory; absolute path patterns are supported."
    }

    fn schema(&self) -> RootSchema {
        schema_for!(GlobArgs)
    }

    fn needs_approval(&self) -> bool {
        false
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

async fn glob_files(pattern: &str) -> Result<GlobResult, ToolError> {
    tracing::debug!(pattern = %pattern, "glob searching");
    let paths = glob::glob(pattern)
        .map_err(|error| {
            ToolError::failed("glob", format!("invalid pattern '{pattern}': {error}"))
        })?
        .filter_map(Result::ok)
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let mut sorted = paths;
    sorted.sort();
    tracing::debug!(matches = sorted.len(), "glob searched");
    Ok(GlobResult::new(sorted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::capture_logs;
    use tempfile::TempDir;

    #[test]
    fn read_tool_needs_no_approval_and_has_default_timeout() {
        let tool = ReadTool::default();
        assert!(!tool.needs_approval());
        assert_eq!(tool.timeout_secs(), DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn write_tool_requires_approval() {
        let tool = WriteTool::default();
        assert!(tool.needs_approval());
    }

    #[test]
    fn undeclared_tool_timeout_uses_default() {
        assert_eq!(ReadTool::default().timeout_secs(), DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn read_tool_schema_includes_mode_selector() {
        let schema = ReadTool::default().schema();
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
        let schema = ReadTool::default().schema();
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
    fn glob_matches_pattern_in_scratch_dir() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let sub = dir.path().join("sub");
            std::fs::create_dir_all(&sub).expect("mkdir");
            for name in ["a.txt", "b.txt"] {
                std::fs::write(dir.path().join(name), name).expect("write");
            }
            std::fs::write(sub.join("c.txt"), "c").expect("write");

            let matches = glob_files(&format!("{}/*.txt", dir.path().display()))
                .await
                .expect("glob ok");
            let lines: Vec<&str> = matches.text().lines().collect();
            assert!(lines.iter().any(|l: &&str| l.ends_with("a.txt")));
            assert!(lines.iter().any(|l: &&str| l.ends_with("b.txt")));
            assert!(!lines.iter().any(|l: &&str| l.ends_with("c.txt")));
        });
    }

    #[test]
    fn glob_invalid_pattern_is_error() {
        crate::test_support::run_locked(async {
            let result = glob_files("[").await;
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
        assert_eq!(GlobTool::default().argument_order("pattern"), 0);
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
        let pattern = format!("{}/*.txt", dir.path().display());
        let log = capture_logs(async move {
            let result = glob_files(&pattern).await;
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
            assert_eq!(
                section.line_number_offset,
                Some(0),
                "bytes mode must carry line_number_offset Some(0)"
            );
            assert_eq!(section.file_extension.as_deref(), Some("rs"));
        });
    }
}
