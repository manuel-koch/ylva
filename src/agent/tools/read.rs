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

//! `read` tool: read file content with optional byte, line, or character slicing.
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

use base64::Engine as _;
use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;

use super::{
    utf8_text_preview, validate_args_with_schema, ContentSection, FileEncoding, Section, Tool,
    ToolAttachment, ToolCall, ToolCallResult, ToolError, DEFAULT_TOOL_TIMEOUT_SECS,
    STANDARD_BASE64,
};
use crate::sandbox::{ApprovalCheck, SandboxContext};

const MEDIA_TYPE_IMAGE_PNG: &str = "image/png";
const MEDIA_TYPE_IMAGE_JPEG: &str = "image/jpeg";
const MEDIA_TYPE_IMAGE_WEBP: &str = "image/webp";
const MEDIA_TYPE_IMAGE_GIF: &str = "image/gif";
const MEDIA_TYPE_APPLICATION_PDF: &str = "application/pdf";
const SUPPORTED_IMAGE_MEDIA_TYPES: &[&str] = &[
    MEDIA_TYPE_IMAGE_PNG,
    MEDIA_TYPE_IMAGE_JPEG,
    MEDIA_TYPE_IMAGE_WEBP,
    MEDIA_TYPE_IMAGE_GIF,
];
const SUPPORTED_MEDIA_TYPES: &[&str] = &[
    MEDIA_TYPE_IMAGE_PNG,
    MEDIA_TYPE_IMAGE_JPEG,
    MEDIA_TYPE_IMAGE_WEBP,
    MEDIA_TYPE_IMAGE_GIF,
    MEDIA_TYPE_APPLICATION_PDF,
];

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

fn decode_text(path: &str, bytes: &[u8], mode: &str) -> Result<String, ToolError> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|error| {
            tracing::error!(
                path,
                mode,
                error = %error,
                "failed to decode file as UTF-8"
            );
            ToolError::failed(
                "read",
                format!(
                    "failed to read '{path}' in {mode} mode: invalid UTF-8 ({error}) — \
                     the file is not UTF-8 text; retry with mode: \"bytes\" \
                     (use encoding: \"base64\" for binary data)"
                ),
            )
        })
}

/// The unit that `ReadFileArgs::offset`/`limit` count when `mode` is set
/// explicitly.
///
/// Omit `mode` to autodetect instead: valid UTF-8 text is read with `Lines`
/// semantics, a whole binary file recognized as a supported media type
/// (PNG/JPEG/WebP/GIF/PDF) is returned as Base64 with a typed attachment,
/// and any other binary content fails with a retry hint. Use `Chars` for
/// character counts and `Bytes` explicitly for byte ranges or raw binary
/// data. `Lines`/`Chars` require valid UTF-8. Exact arbitrary bytes are
/// available with `mode: bytes` and `encoding: base64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadMode {
    Lines,
    Bytes,
    Chars,
}

/// `read` arguments: a path plus an optional `mode` (unit selector) and
/// `offset`/`limit` that count that unit. When `mode` is omitted, the read
/// autodetects text versus binary content.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct ReadFileArgs {
    /// Absolute or workspace-relative path of the file to read.
    pub path: String,
    /// Explicit unit for `offset`/`limit`. Omit to autodetect: UTF-8 text is
    /// read as lines, a whole recognized image/PDF file is returned as
    /// Base64 with a typed attachment, and other binary content fails with
    /// a retry hint.
    #[serde(default)]
    pub mode: Option<ReadMode>,
    /// Result encoding. Defaults to `utf8`; `base64` requires `mode: bytes`
    /// and returns the selected raw bytes without line breaks. Autodetect
    /// (omitted `mode`) may return `base64` for binary files regardless of
    /// this default.
    #[serde(default)]
    #[schemars(default)]
    pub encoding: FileEncoding,
    /// 0-based index of the first selected unit to read; in `lines` mode,
    /// line 1 is `offset: 0`.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Max selected units to read from `offset`. Omit to read to EOF.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Structured metadata for a successful `read` tool invocation.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ReadResultMetadata {
    /// Total bytes in the selected file content returned.
    pub read_bytes: usize,
    /// Total lines in the selected UTF-8 text returned. Omitted for opaque
    /// binary data that has no safe text projection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_lines: Option<usize>,
    /// Transport encoding used for the returned result, present for Base64
    /// reads and omitted for the normal UTF-8 projection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<FileEncoding>,
    /// Recognized media type for a complete Base64 file, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
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
    /// Encoding used for the returned content.
    pub encoding: FileEncoding,
    /// Number of raw bytes selected from the file.
    pub read_bytes: usize,
    /// UTF-8 text projection for user-facing previews when Base64 carries text.
    pub text_preview: Option<String>,
    /// Recognized supported media type when the selected Base64 range is the
    /// complete source file.
    pub media_type: Option<String>,
}

fn metadata_read_lines(result: &ReadResult) -> Option<usize> {
    match result.encoding {
        FileEncoding::Utf8 => Some(result.content.lines().count()),
        FileEncoding::Base64 => result
            .text_preview
            .as_deref()
            .map(|text_preview| text_preview.lines().count()),
    }
}

impl ToolCallResult for ReadResult {
    fn text(&self) -> String {
        self.content.clone()
    }

    fn display_text(&self) -> Option<String> {
        match self.encoding {
            FileEncoding::Utf8 => Some(self.text()),
            FileEncoding::Base64 => None,
        }
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        serde_json::to_value(ReadResultMetadata {
            read_bytes: self.read_bytes,
            read_lines: metadata_read_lines(self),
            encoding: (self.encoding == FileEncoding::Base64).then_some(self.encoding),
            media_type: self.media_type.clone(),
        })
        .ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        let mut schema = serde_json::to_value(schemars::schema_for!(ReadResultMetadata)).ok()?;
        let properties = schema.get_mut("properties")?.as_object_mut()?;
        if metadata_read_lines(self).is_none() {
            properties.remove("read_lines");
        }
        if self.encoding != FileEncoding::Base64 {
            properties.remove("encoding");
        }
        if self.media_type.is_none() {
            properties.remove("media_type");
        }
        serde_json::from_value(schema).ok()
    }

    fn attachments(&self) -> Vec<ToolAttachment> {
        let Some(media_type) = self.media_type.clone() else {
            return vec![];
        };
        if SUPPORTED_IMAGE_MEDIA_TYPES.contains(&media_type.as_str()) {
            vec![ToolAttachment::Image { media_type }]
        } else if media_type == MEDIA_TYPE_APPLICATION_PDF {
            vec![ToolAttachment::File {
                media_type,
                filename: safe_source_filename(&self.path, "attachment.pdf"),
            }]
        } else {
            vec![]
        }
    }

    fn content_sections(&self) -> Vec<Section> {
        let preview = match self.encoding {
            FileEncoding::Utf8 => Some(self.content.as_str()),
            FileEncoding::Base64 => self.text_preview.as_deref(),
        };
        let Some(preview) = preview.filter(|content| !content.is_empty()) else {
            return vec![];
        };
        let file_extension = std::path::Path::new(&self.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        vec![Section::Content(ContentSection {
            label: None,
            content: preview.to_string(),
            file_extension,
            line_number_offset: self.starting_line,
        })]
    }
}

// ── ToolCall ──────────────────────────────────────────────────────────────────

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
        tool.non_default_redacted_arguments(&self.raw_arguments)
    }
    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        let raw_arguments = self.raw_arguments;
        let serialized = serde_json::to_string(&raw_arguments).unwrap_or_default();
        let span = tracing::info_span!("tool_args", args = %serialized);
        Box::pin(
            async move {
                ReadTool::default()
                    .validate_args(&raw_arguments)
                    .map_err(|error| {
                        ToolError::failed("read", format!("invalid arguments: {error}"))
                    })?;
                let args =
                    serde_json::from_value::<ReadFileArgs>(raw_arguments).map_err(|error| {
                        ToolError::failed("read", format!("invalid arguments: {error}"))
                    })?;
                read_file(args)
                    .await
                    .map(|o| -> Box<dyn ToolCallResult> { Box::new(o) })
            }
            .instrument(span),
        )
    }
}

// ── Tool definition ───────────────────────────────────────────────────────────

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
        "Read a file's contents. Omit `mode` to autodetect: UTF-8 text is read \
        by lines, a whole binary file recognized as PNG/JPEG/WebP/GIF/PDF is \
        returned as Base64 (attached for models that support it), and other \
        binary files fail with a retry hint. Set `mode` explicitly to force a \
        unit: `lines` (0-based `offset` index, `limit` line count; requires \
        valid UTF-8), `chars` for character counts, or `bytes` for byte ranges \
        and raw binary data. `encoding` defaults to `utf8`; use `base64` with \
        explicit `mode: bytes` for exact arbitrary bytes. `offset`/`limit` \
        count the selected unit; omit both to read the whole file."
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(ReadFileArgs)
    }

    fn path_like_arguments(&self) -> &'static [&'static str] {
        &["path"]
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
            ("mode", Value::Null) => true,
            ("encoding", Value::String(encoding)) => encoding == "utf8",
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

    fn validate_args(&self, arguments: &Value) -> Result<(), String> {
        validate_args_with_schema(&self.input_schema(), arguments)?;
        if arguments.get("encoding").and_then(Value::as_str) == Some("base64")
            && arguments.get("mode").and_then(Value::as_str) != Some("bytes")
        {
            return Err("`encoding: \"base64\"` requires explicit `mode: \"bytes\"`".to_string());
        }
        Ok(())
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_TOOL_TIMEOUT_SECS
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

async fn read_file(args: ReadFileArgs) -> Result<ReadResult, ToolError> {
    if args.encoding == FileEncoding::Base64 && args.mode != Some(ReadMode::Bytes) {
        return Err(ToolError::failed(
            "read",
            "`encoding: \"base64\"` requires explicit `mode: \"bytes\"`",
        ));
    }
    tracing::debug!("reading file");
    // TODO: should we really read the whole file into memory
    //       and get the chunks afterwards ?
    //       Could eat much memory on really big files !?
    let bytes = tokio::fs::read(&args.path).await.map_err(|error| {
        tracing::error!(error = %error,"failed to read");
        ToolError::failed("read", format!("failed to read '{}': {error}", args.path))
    })?;
    match args.mode {
        None => autodetect_read(&bytes, &args.path, args.offset, args.limit),
        Some(ReadMode::Bytes) => match args.encoding {
            FileEncoding::Utf8 => {
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
                    read_bytes: content.len(),
                    content,
                    path: args.path,
                    starting_line: Some(0),
                    encoding: FileEncoding::Utf8,
                    text_preview: None,
                    media_type: None,
                })
            }
            FileEncoding::Base64 => Ok(base64_read_result(
                &bytes,
                args.offset.unwrap_or(0),
                args.limit,
                args.path,
            )),
        },
        Some(ReadMode::Lines) => {
            let text = decode_text(&args.path, &bytes, "lines")?;
            Ok(lines_read_result(&text, args.path, args.offset, args.limit))
        }
        Some(ReadMode::Chars) => {
            let text = decode_text(&args.path, &bytes, "chars")?;
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
                read_bytes: content.len(),
                content,
                path: args.path,
                starting_line: Some(0),
                encoding: FileEncoding::Utf8,
                text_preview: None,
                media_type: None,
            })
        }
    }
}

/// Resolve a read that was called without an explicit `mode`.
///
/// Valid UTF-8 text keeps the `lines` unit, so `offset`/`limit` retain their
/// line-index meaning of the previous default. A whole file recognized as a
/// supported media type goes through the Base64 path so its typed attachment
/// (and the later modality-aware projection) is produced without the caller
/// naming `mode`/`encoding`. Every other binary file — including a ranged
/// read of a recognized one — fails with the explicit-mode retry hint.
fn autodetect_read(
    bytes: &[u8],
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadResult, ToolError> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(lines_read_result(text, path.to_string(), offset, limit));
    }
    if offset.is_none() && limit.is_none() && supported_media_type(bytes).is_some() {
        return Ok(base64_read_result(bytes, 0, None, path.to_string()));
    }
    Err(ToolError::failed(
        "read",
        format!(
            "failed to read '{path}': autodetect supports UTF-8 text and whole \
             image/PDF files only; retry with explicit mode: \"bytes\" \
             (use encoding: \"base64\" for binary data)"
        ),
    ))
}

/// Build the result of a `lines`-unit read over already-decoded UTF-8 text.
fn lines_read_result(
    text: &str,
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> ReadResult {
    let lines: Vec<&str> = text.split_terminator('\n').collect();
    let start = offset.unwrap_or(0).min(lines.len());
    let end = limit
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
    ReadResult {
        read_bytes: content.len(),
        content,
        path,
        starting_line: Some(start),
        encoding: FileEncoding::Utf8,
        text_preview: None,
        media_type: None,
    }
}

/// Build the result of a Base64 read of `full_bytes[start..]`, optionally
/// limited to `limit` bytes. The media type is recognized only when the
/// selected range is the complete file.
fn base64_read_result(
    full_bytes: &[u8],
    start: usize,
    limit: Option<usize>,
    path: String,
) -> ReadResult {
    let start = start.min(full_bytes.len());
    let mut slice = &full_bytes[start..];
    if let Some(limit) = limit {
        slice = &slice[..limit.min(slice.len())];
    }
    let read_bytes = slice.len();
    let content = STANDARD_BASE64.encode(slice);
    let text_preview = utf8_text_preview(slice);
    let media_type = (start == 0 && slice.len() == full_bytes.len())
        .then(|| supported_media_type(slice))
        .flatten();
    tracing::debug!(read_bytes, "read file");
    ReadResult {
        read_bytes,
        content,
        path,
        starting_line: text_preview.as_ref().map(|_| 0),
        encoding: FileEncoding::Base64,
        text_preview,
        media_type,
    }
}

fn supported_media_type(bytes: &[u8]) -> Option<String> {
    let mime_type = infer::get(bytes)?.mime_type();
    SUPPORTED_MEDIA_TYPES
        .contains(&mime_type)
        .then(|| mime_type.to_string())
}

fn safe_source_filename(path: &str, fallback_filename: &str) -> String {
    let basename = std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..");
    basename
        .map(str::to_string)
        .unwrap_or_else(|| fallback_filename.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::capture_logs;
    use serde_json::Value;
    use tempfile::TempDir;

    #[test]
    fn read_tool_with_timeout_constructor() {
        let tool = ReadTool::with_timeout(
            10,
            SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
                crate::sandbox::Sandbox::new(),
            ))),
        );
        assert_eq!(tool.timeout_secs(), 10);
    }

    #[test]
    fn read_tool_auto_approves_workspace_path_and_has_default_timeout() {
        let tool = ReadTool::default();
        let args = serde_json::json!({ "path": "src/main.rs" });
        // Phase 6.0: reads inside the workspace are auto-approved by the sandbox;
        // the cwd is the workspace root in tests, so "src/main.rs" is in-workspace.
        assert!(matches!(
            tool.needs_approval(&args),
            crate::sandbox::ApprovalCheck::AutoApproved
        ));
        assert_eq!(tool.timeout_secs(), DEFAULT_TOOL_TIMEOUT_SECS);
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

    #[cfg(unix)]
    #[test]
    fn read_tool_cross_filesystem_rule_requires_user_approval_for_cross_device_path() {
        use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule, SandboxRuleConfig};

        let workspace = TempDir::new().expect("tempdir");
        let context = SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::sandbox::Sandbox::with_root(workspace.path().to_path_buf()),
        )))
        .with_configured_rules(vec![SandboxRuleConfig::Named {
            rule: NamedSandboxRule::CrossFilesystem,
            verdict: ApprovalVerdictConfig::UserApprovalRequired,
            reason: None,
        }]);
        let tool = ReadTool::new(context);

        assert_eq!(
            tool.needs_approval(&serde_json::json!({ "path": "/dev/null" })),
            ApprovalCheck::UserApprovalRequired,
            "a cross-device path under a configured cross_filesystem rule must \
             require user approval"
        );

        let same_device_path = workspace.path().join("notes.txt");
        assert_eq!(
            tool.needs_approval(&serde_json::json!({
                "path": same_device_path.to_string_lossy()
            })),
            ApprovalCheck::AutoApproved,
            "a same-device path does not match, so read auto-approves its \
             sandbox abstention"
        );
    }

    #[test]
    fn undeclared_tool_timeout_uses_default() {
        assert_eq!(
            ReadTool::default().timeout_secs(),
            DEFAULT_TOOL_TIMEOUT_SECS
        );
    }

    #[test]
    fn read_tool_schema_includes_mode_selector() {
        let schema = ReadTool::default().input_schema();
        let value = serde_json::to_value(&schema).expect("schema serializes");
        let required = value["required"].as_array().expect("required is an array");
        assert_eq!(required.len(), 1, "only `path` is required");
        assert_eq!(required[0], serde_json::json!("path"));
        let mode_any_of = value["properties"]["mode"]["anyOf"]
            .as_array()
            .expect("optional mode is a nullable anyOf");
        assert!(
            mode_any_of
                .iter()
                .any(|branch| branch["$ref"] == serde_json::json!("#/definitions/ReadMode")),
            "mode must reference the ReadMode definition: {mode_any_of:?}"
        );
        assert!(
            mode_any_of
                .iter()
                .any(|branch| branch["type"] == serde_json::json!("null")),
            "mode must accept null for autodetect: {mode_any_of:?}"
        );
        let variants = value["definitions"]["ReadMode"]["enum"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .expect("mode enum present");
        assert_eq!(variants, vec!["lines", "bytes", "chars"]);
    }

    #[test]
    fn read_schema_accepts_explicit_null_mode_as_autodetect() {
        let tool = ReadTool::default();
        let arguments = serde_json::json!({ "path": "source.bin", "mode": null });
        assert!(tool.validate_args(&arguments).is_ok());
        let parsed: ReadFileArgs =
            serde_json::from_value(arguments).expect("arguments deserialize");
        assert_eq!(parsed.mode, None);
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
    fn read_schema_explains_autodetect_and_explicit_modes() {
        let tool = ReadTool::default();
        assert!(tool.description().contains("Omit `mode` to autodetect"));
        assert!(tool.description().contains("`bytes` for byte ranges"));
        assert!(tool.description().contains("`encoding` defaults to `utf8`"));

        let schema = serde_json::to_value(tool.input_schema()).expect("schema serializes");
        let mode_description = schema["definitions"]["ReadMode"]["description"]
            .as_str()
            .expect("mode schema description");
        assert!(mode_description.contains("Omit `mode` to autodetect"));
        let encoding_description = schema["properties"]["encoding"]["description"]
            .as_str()
            .expect("encoding schema description");
        assert!(encoding_description.contains("`base64` requires `mode: bytes`"));
        assert!(encoding_description.contains("Autodetect"));
        assert_eq!(schema["properties"]["encoding"]["default"], "utf8");
        let variants = schema["definitions"]["FileEncoding"]["oneOf"]
            .as_array()
            .expect("encoding variants")
            .iter()
            .filter_map(|variant| variant["enum"][0].as_str())
            .collect::<Vec<_>>();
        assert_eq!(variants, vec!["utf8", "base64"]);
    }

    #[test]
    fn read_args_omit_mode_to_select_autodetect() {
        let tool = ReadTool::default();
        let arguments = serde_json::json!({
            "path": "source.txt",
            "offset": 32,
            "limit": 2,
        });
        assert!(tool.validate_args(&arguments).is_ok());

        let parsed: ReadFileArgs =
            serde_json::from_value(arguments).expect("arguments deserialize");
        assert_eq!(parsed.mode, None, "omitted mode must select autodetect");
        assert_eq!(parsed.encoding, FileEncoding::Utf8);
    }

    #[test]
    fn read_rejects_base64_without_explicit_bytes_mode() {
        let tool = ReadTool::default();
        let missing_mode = serde_json::json!({
            "path": "source.bin",
            "encoding": "base64",
        });
        let lines_mode = serde_json::json!({
            "path": "source.bin",
            "mode": "lines",
            "encoding": "base64",
        });
        let bytes_mode = serde_json::json!({
            "path": "source.bin",
            "mode": "bytes",
            "encoding": "base64",
        });

        let missing_mode_error = tool.validate_args(&missing_mode).expect_err("must reject");
        assert!(missing_mode_error.contains("requires explicit `mode: \"bytes\"`"));
        let lines_mode_error = tool.validate_args(&lines_mode).expect_err("must reject");
        assert!(lines_mode_error.contains("requires explicit `mode: \"bytes\"`"));
        assert!(tool.validate_args(&bytes_mode).is_ok());
    }

    #[test]
    fn read_omitted_mode_autodetects_text_as_lines_for_ranges() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, "one\ntwo\nthree\nfour").expect("write");
            let arguments = serde_json::json!({
                "path": path.display().to_string(),
                "offset": 1,
                "limit": 2,
            });
            let result =
                read_file(serde_json::from_value(arguments).expect("arguments deserialize"))
                    .await
                    .expect("read ok");
            assert_eq!(result.text(), "two\nthree");
            assert_eq!(result.encoding, FileEncoding::Utf8);
        });
    }

    #[test]
    fn read_omitted_mode_returns_base64_attachment_for_whole_media_file() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("image.png");
            let bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0];
            std::fs::write(&path, &bytes).expect("write fixture");
            let arguments = serde_json::json!({ "path": path.display().to_string() });
            let result =
                read_file(serde_json::from_value(arguments).expect("arguments deserialize"))
                    .await
                    .expect("autodetect media read succeeds");
            assert_eq!(result.encoding, FileEncoding::Base64);
            assert_eq!(result.media_type.as_deref(), Some(MEDIA_TYPE_IMAGE_PNG));
            assert_eq!(result.text(), STANDARD_BASE64.encode(&bytes));
            assert_eq!(result.attachments().len(), 1);
            assert_eq!(
                result.metadata(),
                Some(serde_json::json!({
                    "read_bytes": bytes.len(),
                    "encoding": "base64",
                    "media_type": MEDIA_TYPE_IMAGE_PNG,
                }))
            );
        });
    }

    #[test]
    fn read_omitted_mode_rejects_binary_without_recognized_media_type() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("payload.bin");
            std::fs::write(&path, [0x00, 0xff, 0x80, 0x01, 0xfe, b'Y']).expect("write fixture");
            let arguments = serde_json::json!({ "path": path.display().to_string() });
            let error =
                read_file(serde_json::from_value(arguments).expect("arguments deserialize"))
                    .await
                    .expect_err("unrecognized binary must fail autodetect");
            let message = error.to_string();
            assert!(
                message.contains(r#"retry with explicit mode: "bytes""#),
                "error must carry the bytes-mode remedy: {message}"
            );
            assert!(
                message.contains(r#"encoding: "base64""#),
                "error must mention base64 for binary data: {message}"
            );
        });
    }

    #[test]
    fn read_omitted_mode_rejects_ranged_reads_of_binary_files() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("image.png");
            std::fs::write(&path, [137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0])
                .expect("write fixture");
            let arguments = serde_json::json!({
                "path": path.display().to_string(),
                "limit": 4,
            });
            let error =
                read_file(serde_json::from_value(arguments).expect("arguments deserialize"))
                    .await
                    .expect_err("ranged binary read must fail autodetect");
            let message = error.to_string();
            assert!(
                message.contains(r#"retry with explicit mode: "bytes""#),
                "error must carry the bytes-mode remedy: {message}"
            );
        });
    }

    #[test]
    fn read_text_modes_reject_invalid_utf8() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("image.png");
            std::fs::write(&path, [137, 80, 78, 71, 0, 255]).expect("write");

            for mode in [Some(ReadMode::Lines), Some(ReadMode::Chars)] {
                let error = read_file(ReadFileArgs {
                    path: path.display().to_string(),
                    mode,
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                })
                .await
                .expect_err("invalid UTF-8 text reads must fail");
                assert!(
                    error.to_string().contains("invalid UTF-8"),
                    "unexpected error: {error}"
                );
                assert!(
                    error.to_string().contains(r#"retry with mode: "bytes""#),
                    "error must point at the bytes-mode remedy: {error}"
                );
                assert!(
                    error.to_string().contains(r#"encoding: "base64""#),
                    "error must mention base64 for binary data: {error}"
                );
            }
        });
    }

    #[test]
    fn read_returns_whole_file_content() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, "hello world").expect("write");
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Utf8,
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
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Utf8,
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
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Utf8,
                offset: Some(2),
                limit: Some(4),
            })
            .await
            .expect("read ok");
            assert_eq!(result.text(), "2345");
        });
    }

    #[test]
    fn read_base64_preserves_raw_bytes_and_range_metadata() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("binary.dat");
            let bytes = [0x00, 0xff, 0x80, b'A', 0x00, 0xfe];
            std::fs::write(&path, bytes).expect("write");

            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Base64,
                offset: Some(1),
                limit: Some(4),
            })
            .await
            .expect("read ok");

            assert_eq!(result.text(), STANDARD_BASE64.encode(&bytes[1..5]));
            assert_eq!(
                result.metadata(),
                Some(serde_json::json!({
                    "read_bytes": 4,
                    "encoding": "base64",
                }))
            );
            let metadata_schema =
                serde_json::to_value(result.metadata_schema().expect("metadata schema"))
                    .expect("schema serializes");
            assert!(metadata_schema["properties"].get("read_bytes").is_some());
            assert!(metadata_schema["properties"].get("read_lines").is_none());
            assert!(metadata_schema["properties"].get("encoding").is_some());
            assert!(metadata_schema["properties"].get("media_type").is_none());
            assert!(result.content_sections().is_empty());
        });
    }

    #[test]
    fn read_base64_text_uses_text_metadata_and_preview() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("source.rs");
            let text = "fn main() {}\nfn helper() {}\n";
            std::fs::write(&path, text).expect("write");

            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Base64,
                offset: None,
                limit: None,
            })
            .await
            .expect("read ok");

            assert_eq!(result.text(), STANDARD_BASE64.encode(text));
            assert_eq!(
                result.metadata(),
                Some(serde_json::json!({
                    "read_bytes": text.len(),
                    "read_lines": 2,
                    "encoding": "base64",
                }))
            );
            let metadata_schema =
                serde_json::to_value(result.metadata_schema().expect("metadata schema"))
                    .expect("schema serializes");
            assert!(metadata_schema["properties"].get("read_lines").is_some());
            assert!(metadata_schema["properties"].get("encoding").is_some());
            let sections = result.content_sections();
            assert_eq!(sections.len(), 1);
            let Section::Content(section) = &sections[0] else {
                panic!("expected a text content section");
            };
            assert_eq!(section.content, text);
            assert_eq!(section.file_extension.as_deref(), Some("rs"));
            assert_eq!(section.line_number_offset, Some(0));
        });
    }

    #[test]
    fn read_base64_supports_invalid_utf8_without_logging_payload() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("binary.dat");
        let bytes = [
            0x00, 0xff, 0x80, b'R', b'E', b'A', b'D', b'-', b'B', b'I', b'N', b'A', b'R', b'Y',
            b'-', b'S', b'E', b'N', b'T', b'I', b'N', b'E', b'L',
        ];
        std::fs::write(&path, bytes).expect("write");
        let encoded = STANDARD_BASE64.encode(bytes);
        let log = capture_logs(async {
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Base64,
                offset: None,
                limit: None,
            })
            .await
            .expect("read ok");
            assert_eq!(result.text(), encoded);
        });
        assert!(
            log.contains("read_bytes=23"),
            "log must contain safe size: {log}"
        );
        assert!(
            !log.contains(&encoded),
            "Base64 content leaked into log: {log}"
        );
        assert!(
            !log.contains("READ-BINARY-SENTINEL"),
            "decoded content leaked into log: {log}"
        );
    }

    #[test]
    fn read_base64_recognizes_supported_complete_media_signatures() {
        crate::test_support::run_locked(async {
            let directory = TempDir::new().expect("temporary directory");
            let cases = [
                (
                    "image.png",
                    vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0],
                    MEDIA_TYPE_IMAGE_PNG,
                ),
                (
                    "image.jpg",
                    vec![255, 216, 255, 224, 0, 16],
                    MEDIA_TYPE_IMAGE_JPEG,
                ),
                (
                    "image.webp",
                    vec![b'R', b'I', b'F', b'F', 0, 0, 0, 0, b'W', b'E', b'B', b'P'],
                    MEDIA_TYPE_IMAGE_WEBP,
                ),
                (
                    "image.gif",
                    b"GIF89a\x00\x00".to_vec(),
                    MEDIA_TYPE_IMAGE_GIF,
                ),
                (
                    "report.pdf",
                    b"%PDF-1.7\n".to_vec(),
                    MEDIA_TYPE_APPLICATION_PDF,
                ),
            ];

            for (filename, bytes, media_type) in cases {
                let path = directory.path().join(filename);
                std::fs::write(&path, &bytes).expect("write fixture");
                let result = read_file(ReadFileArgs {
                    path: path.display().to_string(),
                    mode: Some(ReadMode::Bytes),
                    encoding: FileEncoding::Base64,
                    offset: None,
                    limit: None,
                })
                .await
                .expect("read fixture");

                assert_eq!(result.media_type.as_deref(), Some(media_type));
                assert_eq!(
                    result.metadata().expect("metadata")["media_type"],
                    media_type
                );
                assert_eq!(result.attachments().len(), 1);
                match result.attachments().first().expect("attachment") {
                    ToolAttachment::Image { media_type: actual } => {
                        assert_eq!(actual, media_type)
                    }
                    ToolAttachment::File {
                        media_type: actual,
                        filename: actual_filename,
                    } => {
                        assert_eq!(actual, media_type);
                        assert_eq!(actual_filename, filename);
                    }
                }
                if result.text_preview.is_none() {
                    assert!(result.content_sections().is_empty());
                }
            }
        });
    }

    #[test]
    fn read_base64_does_not_attach_partial_or_false_positive_signatures() {
        crate::test_support::run_locked(async {
            let directory = TempDir::new().expect("temporary directory");
            let path = directory.path().join("looks-like.png");
            let bytes = [137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4];
            std::fs::write(&path, bytes).expect("write fixture");

            let partial = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Base64,
                offset: Some(0),
                limit: Some(8),
            })
            .await
            .expect("partial read");
            assert_eq!(partial.media_type, None);
            assert!(partial.attachments().is_empty());
            assert_eq!(partial.metadata().expect("metadata")["encoding"], "base64");

            let false_positive_path = directory.path().join("not-an-image.png");
            std::fs::write(&false_positive_path, b"not an image").expect("write fixture");
            let false_positive = read_file(ReadFileArgs {
                path: false_positive_path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Base64,
                offset: None,
                limit: None,
            })
            .await
            .expect("false-positive read");
            assert_eq!(false_positive.media_type, None);
            assert!(false_positive.attachments().is_empty());
        });
    }

    #[test]
    fn read_pdf_attachment_uses_a_safe_fallback_filename() {
        let result = ReadResult {
            content: STANDARD_BASE64.encode(b"%PDF-1.7"),
            path: "/".to_string(),
            starting_line: None,
            encoding: FileEncoding::Base64,
            read_bytes: 8,
            text_preview: None,
            media_type: Some(MEDIA_TYPE_APPLICATION_PDF.to_string()),
        };
        assert_eq!(
            result.attachments(),
            vec![ToolAttachment::File {
                media_type: MEDIA_TYPE_APPLICATION_PDF.to_string(),
                filename: "attachment.pdf".to_string(),
            }]
        );
    }

    #[test]
    fn read_call_rejects_incompatible_encoding_before_file_io() {
        crate::test_support::run_locked(async {
            let call = ReadTool::default().create_call(
                uuid::Uuid::nil(),
                serde_json::json!({
                    "path": "does-not-exist.bin",
                    "encoding": "base64",
                }),
            );
            let error = call.run().await.expect_err("incompatible call must fail");
            assert!(
                error
                    .to_string()
                    .contains("requires explicit `mode: \"bytes\"`"),
                "unexpected error: {error}"
            );
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
                mode: Some(ReadMode::Lines),
                encoding: FileEncoding::Utf8,
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
                mode: Some(ReadMode::Chars),
                encoding: FileEncoding::Utf8,
                offset: Some(1),
                limit: Some(3),
            })
            .await
            .expect("read ok");
            assert_eq!(result.text(), "éll");
        });
    }

    #[test]
    fn read_tool_orders_path_first_and_marks_its_defaults() {
        let tool = ReadTool::default();
        assert_eq!(tool.argument_order("path"), 0);
        assert!(tool.argument_order("mode") < tool.argument_order("offset"));
        assert!(tool.is_default_argument("mode", &Value::Null));
        assert!(!tool.is_default_argument("mode", &Value::from("lines")));
        assert!(!tool.is_default_argument("mode", &Value::from("bytes")));
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
            "mode": null,
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
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Utf8,
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
    fn read_non_default_redacted_arguments_drop_defaults_and_keep_path() {
        let tool = ReadTool::default();
        let arguments = serde_json::json!({
            "path": "a.txt",
            "mode": null,
            "offset": 0,
            "limit": serde_json::Value::Null,
        });
        let redacted = tool.non_default_redacted_arguments(&arguments);
        let keys: Vec<&str> = redacted.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, vec!["path"]);
    }

    #[test]
    fn read_tool_done_content_sections_have_line_numbers() {
        crate::test_support::run_locked(async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test.rs");
            std::fs::write(&path, "fn main() {}\nfn foo() {}\n").unwrap();
            let result = read_file(ReadFileArgs {
                path: path.to_str().unwrap().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Utf8,
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
            encoding: FileEncoding::Utf8,
            read_bytes: 0,
            text_preview: None,
            media_type: None,
        };
        assert!(result.content_sections().is_empty());
    }

    #[test]
    fn read_call_tool_call_methods() {
        let id = uuid::Uuid::new_v4();
        let args = serde_json::json!({ "path": "src/main.rs" });
        let call = ReadTool::default().create_call(id, args.clone());
        assert_eq!(call.id(), id);
        assert_eq!(call.name(), "read");
        assert_eq!(call.arguments(), &args);
        assert_eq!(call.timeout_secs(), DEFAULT_TOOL_TIMEOUT_SECS);
        assert_eq!(call.argument_order("path"), 0);
        let redacted = call.non_default_redacted_arguments();
        assert!(redacted.iter().any(|(key, _)| key == "path"));
    }

    #[test]
    fn read_input_args_all_properties_have_descriptions() {
        let schema = ReadTool::default().input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "ReadFileArgs");
    }
}
