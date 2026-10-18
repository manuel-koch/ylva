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
//! [`crate::agent::DEFAULT_TOOL_TIMEOUT_SECS`]. Typed argument structs
//! derive both `JsonSchema` (for the wire schema) and `Deserialize` (for
//! parsing incoming arguments).
//!
//! **`timeout_secs` sentinel (Q6):** the tool carries a `timeout_secs: u64`
//! override where `0` selects [`DEFAULT_TOOL_TIMEOUT_SECS`]. A timeout is always
//! enforced — there is no "no-timeout" value; `0` merely selects the module
//! default.

use std::path::Path;

use base64::Engine as _;
use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;

use super::approved_target::ApprovedTarget;
use super::{
    utf8_text_preview, ContentSection, FileEncoding, Section, Tool, ToolCall, ToolCallResult,
    ToolError, DEFAULT_TOOL_TIMEOUT_SECS, STANDARD_BASE64,
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
    /// Content to write. With `utf8` this is text; with `base64` it is standard
    /// padded Base64 representing the exact bytes to write.
    pub content: String,
    /// Input encoding. Defaults to `utf8`.
    #[serde(default)]
    #[schemars(default)]
    pub encoding: FileEncoding,
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
    /// Encoding used for the input content.
    pub encoding: FileEncoding,
    /// UTF-8 text projection for user-facing previews when Base64 carries text.
    text_preview: Option<String>,
    /// Pre-formatted LLM-facing confirmation text.
    text: String,
}

impl WriteResult {
    fn new(
        path: impl Into<String>,
        content: impl Into<String>,
        bytes_written: usize,
        encoding: FileEncoding,
        text_preview: Option<String>,
    ) -> Self {
        let path = path.into();
        let content = content.into();
        WriteResult {
            text: format!("wrote {bytes_written} bytes to '{path}'"),
            path,
            bytes_written,
            content,
            encoding,
            text_preview,
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
        let content = match self.encoding {
            FileEncoding::Utf8 => Some(self.content.as_str()),
            FileEncoding::Base64 => self.text_preview.as_deref(),
        };
        let Some(content) = content.filter(|content| !content.is_empty()) else {
            return vec![];
        };
        let file_extension = std::path::Path::new(&self.path)
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
}

// ── ToolCall ──────────────────────────────────────────────────────────────────

/// Invocation object for a single `write` tool call.
pub struct WriteCall {
    id: uuid::Uuid,
    raw_arguments: serde_json::Value,
    timeout_secs: u64,
    approved_target: ApprovedTarget,
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
        let content = match self.raw_arguments.get("encoding").and_then(Value::as_str) {
            Some("base64") => self
                .raw_arguments
                .get("content")
                .and_then(Value::as_str)
                .and_then(|content| STANDARD_BASE64.decode(content).ok())
                .and_then(|bytes| utf8_text_preview(&bytes)),
            _ => self
                .raw_arguments
                .get("content")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        let Some(content) = content.filter(|content| !content.is_empty()) else {
            return vec![];
        };
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
            content,
            file_extension,
            line_number_offset: Some(0),
        })]
    }
    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
        WriteTool::default().non_default_redacted_arguments(&self.raw_arguments)
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
                write_file(args, &self.approved_target)
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
        "Create or overwrite a file with the given content. `encoding` defaults to `utf8`; \
        use `base64` to write exact arbitrary bytes from standard padded Base64. The `path` \
        argument accepts `~` and `$ENVVAR`/`${ENVVAR}` tokens; they are expanded before use."
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
                "content" => (key.to_string(), redact_content(arguments, value)),
                _ => (key.to_string(), value.clone()),
            })
            .collect()
    }

    fn is_default_argument(&self, key: &str, value: &Value) -> bool {
        matches!((key, value), ("encoding", Value::String(encoding)) if encoding == "utf8")
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_TOOL_TIMEOUT_SECS
        } else {
            self.timeout_secs
        }
    }

    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
        let approved_target = ApprovedTarget::capture(
            arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        Box::new(WriteCall {
            id,
            raw_arguments: arguments,
            timeout_secs: self.timeout_secs(),
            approved_target,
        })
    }
}

async fn write_file(
    args: WriteFileArgs,
    approved_target: &ApprovedTarget,
) -> Result<WriteResult, ToolError> {
    let encoding = args.encoding;
    let bytes = match encoding {
        FileEncoding::Utf8 => args.content.as_bytes().to_vec(),
        FileEncoding::Base64 => {
            STANDARD_BASE64
                .decode(args.content.as_bytes())
                .map_err(|error| {
                    ToolError::failed(
                        "write",
                        format!(
                            "failed to decode Base64 content for '{}': {error}",
                            args.path
                        ),
                    )
                })?
        }
    };
    let content = match encoding {
        FileEncoding::Utf8 => args.content,
        FileEncoding::Base64 => String::new(),
    };
    let text_preview = match encoding {
        FileEncoding::Utf8 => None,
        FileEncoding::Base64 => utf8_text_preview(&bytes),
    };
    let path = args.path;
    tracing::debug!("writing file");
    if let Some(parent) = Path::new(&path).parent() {
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
    let bytes_written = bytes.len();
    approved_target.write("write", &path, bytes).await?;
    tracing::debug!("wrote file");
    Ok(WriteResult::new(
        path,
        content,
        bytes_written,
        encoding,
        text_preview,
    ))
}

fn redact_content(arguments: &Value, value: &Value) -> Value {
    let content = value.as_str().unwrap_or_default();
    let summary = if arguments.get("encoding").and_then(Value::as_str) == Some("base64") {
        match STANDARD_BASE64.decode(content.as_bytes()) {
            Ok(decoded) => format!("REDACTED(bytes={})", decoded.len()),
            Err(_) => format!("REDACTED(base64_chars={})", content.len()),
        }
    } else {
        format!(
            "REDACTED(size={},lines={})",
            content.len(),
            content.lines().count()
        )
    };
    Value::String(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::capture_logs;
    use tempfile::TempDir;

    async fn write_file(args: WriteFileArgs) -> Result<WriteResult, ToolError> {
        let approved_target = ApprovedTarget::capture(&args.path);
        super::write_file(args, &approved_target).await
    }

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
    fn write_tool_outside_workspace_requires_user_approval() {
        let tool = WriteTool::default();
        // /tmp is outside the workspace root (cwd), so the sandbox floor asks
        // the user instead of approving it outright.
        let args = serde_json::json!({ "path": "/tmp/evil.sh", "content": "rm -rf /" });
        assert!(matches!(
            tool.needs_approval(&args),
            crate::sandbox::ApprovalCheck::UserApprovalRequired
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_refuses_symlink_swapped_after_approval() {
        let dir = TempDir::new().expect("tempdir");
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "original").expect("seed victim");
        let path = dir.path().join("out.txt");
        let call = WriteTool::default().create_call(
            uuid::Uuid::new_v4(),
            serde_json::json!({ "path": path.display().to_string(), "content": "evil" }),
        );

        std::os::unix::fs::symlink(&victim, &path).expect("plant symlink");

        let Err(error) = call.run().await else {
            panic!("swap must be refused");
        };
        assert!(error.to_string().contains("now resolves"), "{error}");
        assert_eq!(std::fs::read_to_string(&victim).expect("read"), "original");
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
                encoding: FileEncoding::Utf8,
            })
            .await
            .expect("write ok");
            assert!(result.text().contains("wrote 3 bytes"));
            assert_eq!(result.content_sections().len(), 1);
            let content = std::fs::read_to_string(&path).expect("read back");
            assert_eq!(content, "abc");
        });
    }

    #[test]
    fn write_base64_round_trip_reports_raw_byte_count_without_preview() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("sub").join("binary.dat");
            let bytes = [0x00, 0xff, 0x80, b'A', 0x00, 0xfe];
            let encoded = STANDARD_BASE64.encode(bytes);
            let result = write_file(WriteFileArgs {
                path: path.display().to_string(),
                content: encoded,
                encoding: FileEncoding::Base64,
            })
            .await
            .expect("write ok");

            assert_eq!(std::fs::read(&path).expect("read back"), bytes);
            assert_eq!(result.bytes_written, bytes.len());
            assert_eq!(
                result.metadata(),
                Some(serde_json::json!({ "bytes_written": 6 }))
            );
            assert!(result.content_sections().is_empty());
            assert!(result.text().contains("wrote 6 bytes"));
        });
    }

    #[test]
    fn invalid_base64_fails_before_directory_creation() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("new").join("binary.dat");
            let parent = path.parent().expect("parent").to_path_buf();
            let error = write_file(WriteFileArgs {
                path: path.display().to_string(),
                content: "not-valid!".to_string(),
                encoding: FileEncoding::Base64,
            })
            .await
            .expect_err("invalid Base64 must fail");

            let message = error.to_string();
            assert!(message.contains("failed to decode Base64 content"));
            assert!(message.contains(&path.display().to_string()));
            assert!(!path.exists());
            assert!(!parent.exists());
        });
    }

    #[test]
    fn unpadded_base64_is_rejected() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("binary.dat");
            let error = write_file(WriteFileArgs {
                path: path.display().to_string(),
                content: "YQ".to_string(),
                encoding: FileEncoding::Base64,
            })
            .await
            .expect_err("unpadded Base64 must fail");
            assert!(error
                .to_string()
                .contains("failed to decode Base64 content"));
            assert!(!path.exists());
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
                encoding: FileEncoding::Utf8,
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
    fn write_base64_redaction_hides_encoded_and_decoded_payloads() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("binary.dat");
        let bytes = b"PRIVATE-BINARY-SENTINEL";
        let encoded = STANDARD_BASE64.encode(bytes);
        let arguments = serde_json::json!({
            "path": path.display().to_string(),
            "content": encoded,
            "encoding": "base64",
        });
        let tool = WriteTool::default();
        let redacted = tool.non_default_redacted_arguments(&arguments);
        let content = redacted
            .iter()
            .find(|(key, _)| key == "content")
            .map(|(_, value)| value.as_str().expect("redaction string"));
        assert_eq!(content, Some("REDACTED(bytes=23)"));

        let invalid_arguments = serde_json::json!({
            "path": "binary.dat",
            "content": "INVALID-BASE64-SENTINEL!",
            "encoding": "base64",
        });
        let invalid_redacted = tool.non_default_redacted_arguments(&invalid_arguments);
        let invalid_content = invalid_redacted
            .iter()
            .find(|(key, _)| key == "content")
            .map(|(_, value)| value.as_str().expect("redaction string"));
        assert_eq!(invalid_content, Some("REDACTED(base64_chars=24)"));

        let log = capture_logs(async {
            let call = tool.create_call(uuid::Uuid::nil(), arguments);
            let result = call.run().await;
            assert!(result.is_ok());
        });
        assert!(
            log.contains("REDACTED(bytes=23)"),
            "missing safe summary: {log}"
        );
        assert!(
            !log.contains(&encoded),
            "encoded payload leaked into log: {log}"
        );
        assert!(
            !log.contains("PRIVATE-BINARY-SENTINEL"),
            "decoded payload leaked into log: {log}"
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
        let result = WriteResult::new("empty.txt", "", 0, FileEncoding::Utf8, None);
        assert!(result.content_sections().is_empty());
    }

    #[test]
    fn write_base64_pending_preview_is_suppressed_for_non_utf8() {
        let call = WriteTool::default().create_call(
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "path": "binary.dat",
                "content": STANDARD_BASE64.encode([0x00, 0xff, 0x80]),
                "encoding": "base64",
            }),
        );
        assert!(call.pending_content_sections().is_empty());
    }

    #[test]
    fn write_base64_text_uses_decoded_preview() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("source.rs");
            let text = "fn main() {}\n";
            let encoded = STANDARD_BASE64.encode(text);
            let call = WriteTool::default().create_call(
                uuid::Uuid::new_v4(),
                serde_json::json!({
                    "path": path.display().to_string(),
                    "content": encoded,
                    "encoding": "base64",
                }),
            );
            let pending_sections = call.pending_content_sections();
            assert_eq!(pending_sections.len(), 1);
            let Section::Content(section) = &pending_sections[0] else {
                panic!("expected a pending text content section");
            };
            assert_eq!(section.content, text);
            assert_eq!(section.file_extension.as_deref(), Some("rs"));

            let result = write_file(WriteFileArgs {
                path: path.display().to_string(),
                content: STANDARD_BASE64.encode(text),
                encoding: FileEncoding::Base64,
            })
            .await
            .expect("write ok");
            let completed_sections = result.content_sections();
            assert_eq!(completed_sections.len(), 1);
            let Section::Content(section) = &completed_sections[0] else {
                panic!("expected a completed text content section");
            };
            assert_eq!(section.content, text);
            assert_eq!(section.file_extension.as_deref(), Some("rs"));
            assert_eq!(section.line_number_offset, Some(0));
            assert_eq!(result.bytes_written, text.len());
            assert_eq!(std::fs::read_to_string(&path).expect("read back"), text);
        });
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
    fn write_base64_empty_content_is_auto_denied() {
        let tool = WriteTool::default();
        let args = serde_json::json!({
            "path": "binary.dat",
            "content": "",
            "encoding": "base64",
        });
        assert!(matches!(
            tool.needs_approval(&args),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
    }

    #[test]
    fn write_input_args_all_properties_have_descriptions() {
        let schema = WriteTool::default().input_schema();
        crate::test_support::assert_all_properties_have_descriptions(&schema, "WriteFileArgs");
        let schema = serde_json::to_value(schema).expect("schema serializes");
        assert_eq!(schema["properties"]["encoding"]["default"], "utf8");
        let description = schema["properties"]["encoding"]["description"]
            .as_str()
            .expect("encoding description");
        assert!(description.contains("Defaults to `utf8`"));
        let parsed: WriteFileArgs = serde_json::from_value(serde_json::json!({
            "path": "out.txt",
            "content": "hello",
        }))
        .expect("default encoding");
        assert_eq!(parsed.encoding, FileEncoding::Utf8);
    }
}
