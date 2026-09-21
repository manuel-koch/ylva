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

//! `write` tool: create or overwrite a file with model-provided text content.
//!
//! All file I/O uses `tokio::fs`; execution is bounded upstream by
//! [`crate::agent::DEFAULT_TOOL_TIMEOUT_SECS`] (5s). Typed argument structs
//! derive both `JsonSchema` (for the wire schema) and `Deserialize` (for
//! parsing incoming arguments).
//!
//! **`timeout_secs` sentinel (Q6):** the tool carries a `timeout_secs: u64`
//! override where `0` selects [`DEFAULT_TOOL_TIMEOUT_SECS`]. A timeout is always
//! enforced — there is no "no-timeout" value; `0` merely selects the module
//! default.

use std::path::Path;

use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;

use super::{
    ContentSection, Section, Tool, ToolCall, ToolCallResult, ToolError, DEFAULT_TOOL_TIMEOUT_SECS,
};
use crate::sandbox::{ApprovalCheck, SandboxContext};

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

/// `write` arguments: a target path and the content to create/overwrite.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct WriteFileArgs {
    /// Absolute or workspace-relative path of the file to create or overwrite.
    pub path: String,
    /// Full text content to write. Overwrites any existing content at `path`.
    pub content: String,
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

// ── ToolCall ──────────────────────────────────────────────────────────────────

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

// ── Tool definition ───────────────────────────────────────────────────────────

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

    fn path_like_arguments(&self) -> &'static [&'static str] {
        &["path"]
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
            DEFAULT_TOOL_TIMEOUT_SECS
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::capture_logs;
    use tempfile::TempDir;

    #[test]
    fn write_tool_orders_path_first_and_marks_content_as_non_default() {
        let tool = WriteTool::default();
        assert_eq!(tool.argument_order("path"), 0);
        assert!(tool.argument_order("path") < tool.argument_order("content"));
    }

    #[test]
    fn write_tool_with_timeout_constructor() {
        let tool = WriteTool::with_timeout(
            20,
            SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
                crate::sandbox::Sandbox::new(),
            ))),
        );
        assert_eq!(tool.timeout_secs(), 20);
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
        // /tmp is outside the workspace root (cwd), so the sandbox denies it.
        let args = serde_json::json!({ "path": "/tmp/evil.sh", "content": "rm -rf /" });
        assert!(matches!(
            tool.needs_approval(&args),
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
    fn write_result_empty_content_has_no_sections() {
        let result = WriteResult::new("empty.txt", "", 0);
        assert!(result.content_sections().is_empty());
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
    fn write_input_args_all_properties_have_descriptions() {
        let schema = WriteTool::default().input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "WriteFileArgs");
    }
}
