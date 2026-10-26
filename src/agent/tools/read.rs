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
//! **Seek/stream I/O.** `bytes` mode seeks to `offset` and reads at most
//! `limit` bytes, so unselected bytes are never materialized; the file's
//! metadata length decides whether the selection is the complete file (which
//! gates media-type recognition). `lines`, `chars` and the omitted-`mode`
//! autodetect path stream the file in fixed-size chunks
//! (`STREAM_CHUNK_BYTES`), validating UTF-8 incrementally and carrying only
//! the incomplete trailing sequence between chunks: the selected line/char
//! window is retained while the validation scan continues to EOF. Whole-file
//! buffering remains only where a complete result is required — a `bytes`
//! Base64 selection within the output cap, and the autodetect fallback that
//! re-reads a complete recognized media file from the same open handle.
//!
//! **Output cap** (see [`READ_OUTPUT_LIMIT_BYTES`]): Text-oriented results
//! carry at most
//! [`READ_OUTPUT_LIMIT_BYTES`] of raw selected bytes. Content past the cap is
//! cut silently (no in-text marker) and reported as `truncated: true` in the
//! result metadata plus a `Partial output` content-section label, so the model
//! can continue via `offset`/`limit`. Complete recognized media reads
//! (PNG/JPEG/WebP/GIF/PDF Base64 attachments) are exempt: a truncated image or
//! PDF is unusable, and `media_type` is only ever recognized for complete
//! files. They are bounded instead by [`READ_MEDIA_MAX_BYTES`] — a whole file
//! above that ceiling fails with an explicit size error rather than loading
//! unbounded, because neither provider input limits nor any model's transcript
//! can absorb an arbitrary Base64 payload.
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

mod media;
mod stream;
mod windows;

use media::{base64_read_result, read_media_too_large_error};
pub(crate) use media::{safe_source_filename, supported_media_type};
use stream::{
    read_io_error, read_up_to, stream_error_to_tool_error, stream_validated_utf8, StreamError,
    STREAM_CHUNK_BYTES,
};
use windows::{CharWindowScanner, LineWindowScanner, SourceLineCounter};

use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tracing::Instrument;

use super::approved_target::ApprovedTarget;
use super::{
    validate_args_with_schema, ContentSection, FileEncoding, Section, Tool, ToolAttachment,
    ToolCall, ToolCallResult, ToolError, DEFAULT_TOOL_TIMEOUT_SECS,
};
use crate::sandbox::{ApprovalCheck, SandboxContext};

pub(crate) const MEDIA_TYPE_IMAGE_PNG: &str = "image/png";
const MEDIA_TYPE_IMAGE_JPEG: &str = "image/jpeg";
const MEDIA_TYPE_IMAGE_WEBP: &str = "image/webp";
const MEDIA_TYPE_IMAGE_GIF: &str = "image/gif";
pub(crate) const MEDIA_TYPE_APPLICATION_PDF: &str = "application/pdf";
pub(crate) const SUPPORTED_IMAGE_MEDIA_TYPES: &[&str] = &[
    MEDIA_TYPE_IMAGE_PNG,
    MEDIA_TYPE_IMAGE_JPEG,
    MEDIA_TYPE_IMAGE_WEBP,
    MEDIA_TYPE_IMAGE_GIF,
];
pub(crate) const SUPPORTED_MEDIA_TYPES: &[&str] = &[
    MEDIA_TYPE_IMAGE_PNG,
    MEDIA_TYPE_IMAGE_JPEG,
    MEDIA_TYPE_IMAGE_WEBP,
    MEDIA_TYPE_IMAGE_GIF,
    MEDIA_TYPE_APPLICATION_PDF,
];

/// Hard cap on the raw selected bytes returned by a text-oriented `read`
/// result: `bytes` + `utf8`, non-media `bytes` + `base64`, `lines`, `chars`
/// and autodetect text. Content past the cap is discarded silently (no in-text
/// marker): the result metadata carries `truncated: true` and the UI content
/// section is labeled `Partial output`, the same vocabulary `bash` and
/// `search` use. The cap wins over a larger explicit `limit`.
///
/// Complete recognized media reads (PNG/JPEG/WebP/GIF/PDF Base64 attachments)
/// bypass it — a truncated image or PDF is unusable, and `media_type` is only
/// ever recognized for complete files. Precedent: `SEARCH_OUTPUT_LIMIT_BYTES`.
pub const READ_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;

/// Hard ceiling on the raw byte length of a **complete** recognized media
/// read — the one path exempt from [`READ_OUTPUT_LIMIT_BYTES`]. A whole file
/// larger than this is rejected with a tool error instead of being returned:
/// a partial file cannot be attached, and an unbounded Base64 payload would
/// blow past provider input limits, request size, and any model's transcript
/// budget. Ranged media reads are already bounded by
/// [`READ_OUTPUT_LIMIT_BYTES`]; `bytes` + `utf8` has no attachment and is
/// capped too.
pub const READ_MEDIA_MAX_BYTES: usize = 8 * 1024 * 1024;

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

/// Format a byte count as a human-readable KiB value.
fn format_kibibytes(bytes: usize) -> String {
    format!("{} KiB", bytes / 1024)
}

/// Format a byte count as a human-readable MiB value.
fn format_mebibytes(bytes: usize) -> String {
    format!("{} MiB", bytes / (1024 * 1024))
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
///
/// Text results carry at most [`READ_OUTPUT_LIMIT_BYTES`] raw bytes; a capped
/// result is flagged `truncated` in the result metadata (complete recognized
/// media files are exempt up to [`READ_MEDIA_MAX_BYTES`] — a larger whole file
/// fails instead of being truncated) and can be continued with
/// `offset`/`limit`.
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
///
/// A result carries at most [`READ_OUTPUT_LIMIT_BYTES`] of raw selected bytes;
/// a longer selection is cut silently, flagged `truncated` in the result
/// metadata (complete recognized media files are exempt), and can be
/// continued with `offset`/`limit`.
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
    /// Max selected units to read from `offset`. Omit to read to EOF. The
    /// result still carries at most [`READ_OUTPUT_LIMIT_BYTES`] raw bytes;
    /// capped results are flagged `truncated`, so resume via `offset`/`limit`.
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
    /// Present and `true` when [`READ_OUTPUT_LIMIT_BYTES`] cut the returned
    /// content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
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
    /// Complete source-file size, before range and output caps.
    source_size_bytes: usize,
    /// Complete source text's line count when the read scanned all text;
    /// unavailable for binary reads and partial byte ranges.
    source_lines: Option<usize>,
    /// UTF-8 text projection for user-facing previews when Base64 carries text.
    pub text_preview: Option<String>,
    /// Recognized supported media type when the selected Base64 range is the
    /// complete source file.
    pub media_type: Option<String>,
    /// `true` when [`READ_OUTPUT_LIMIT_BYTES`] cut `content`; reported as
    /// `truncated: true` metadata and the `Partial output` section label.
    pub truncated: bool,
}

impl ReadResult {
    fn truncate_to_token_budget(&mut self, max_output_tokens: usize) {
        if self.encoding == FileEncoding::Base64 {
            // A binary attachment cannot be meaningfully token-truncated. Keep
            // the existing raw-byte safety ceilings for these results.
            return;
        }
        let (content, truncated) =
            crate::llm::TokenEstimator::Tiktoken.truncate(&self.content, max_output_tokens);
        if truncated {
            self.content = content;
            self.read_bytes = self.content.len();
            self.truncated = true;
        }
    }
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
            truncated: self.truncated.then_some(true),
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
        if !self.truncated {
            properties.remove("truncated");
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
        // Truncated content is an incomplete display of the result — same
        // vocabulary as bash's partial-output section.
        let label = Some(
            if self.truncated {
                "Partial output"
            } else {
                "Output"
            }
            .to_string(),
        );
        vec![Section::Content(ContentSection {
            label,
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
    runtime_output_token_limit: Option<usize>,
    approved_target: ApprovedTarget,
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
    fn set_runtime_output_token_limit(&mut self, max_output_tokens: Option<usize>) {
        self.runtime_output_token_limit = max_output_tokens;
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
        let runtime_output_token_limit = self.runtime_output_token_limit;
        let approved_target = self.approved_target;
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
                approved_target.verify_unchanged("read", &args.path)?;
                let file = approved_target
                    .open_for_read()
                    .await
                    .map_err(|error| read_io_error(&args.path, error))?;
                let mut result = read_opened_file(args, file, STREAM_CHUNK_BYTES).await?;
                if let Some(max_output_tokens) = runtime_output_token_limit {
                    if result.encoding == FileEncoding::Base64
                        && crate::llm::TokenEstimator::Tiktoken
                            .estimate_tokens(&result.content)
                            > max_output_tokens
                    {
                        return Err(ToolError::failed(
                            "read",
                            "binary or media result exceeds the available context token allowance; retry with a smaller byte range or a text-oriented mode",
                        ));
                    }
                    result.truncate_to_token_budget(max_output_tokens);
                }
                tracing::debug!(
                    source_size_bytes = result.source_size_bytes,
                    source_lines = ?result.source_lines,
                    returned_bytes = result.read_bytes,
                    returned_lines = ?metadata_read_lines(&result),
                    truncated = result.truncated,
                    "read result size"
                );
                Ok(Box::new(result) as Box<dyn ToolCallResult>)
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
        static DESCRIPTION: LazyLock<String> = LazyLock::new(|| {
            format!(
                "Read a file's contents. Omit `mode` to autodetect: UTF-8 text is read \
                by lines, a whole binary file recognized as PNG/JPEG/WebP/GIF/PDF is \
                returned as Base64 (attached for models that support it), and other \
                binary files fail with a retry hint. Set `mode` explicitly to force a \
                unit: `lines` (0-based `offset` index, `limit` line count; requires \
                valid UTF-8), `chars` for character counts, or `bytes` for byte ranges \
                and raw binary data. `encoding` defaults to `utf8`; use `base64` with \
                explicit `mode: bytes` for exact arbitrary bytes. `offset`/`limit` \
                count the selected unit; omit both to read the whole file. Results \
                carry at most {} of raw selected bytes: longer selections are \
                cut silently and flagged `truncated` in the result metadata \
                (complete recognized media files are exempt, up to {} — a larger \
                whole file fails with an explicit error), so continue reading \
                with `offset`/`limit` to page through the rest. The `path` argument \
                accepts `~` and `$ENVVAR`/`${{ENVVAR}}` tokens; they are expanded \
                before use.",
                format_kibibytes(READ_OUTPUT_LIMIT_BYTES),
                format_mebibytes(READ_MEDIA_MAX_BYTES),
            )
        });
        &DESCRIPTION
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
        let approved_target = ApprovedTarget::capture(
            arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        Box::new(ReadCall {
            id,
            raw_arguments: arguments,
            timeout_secs: self.timeout_secs(),
            runtime_output_token_limit: None,
            approved_target,
        })
    }
}

/// Read from an already opened `file`. `chunk_size` is a parameter so
/// chunk-boundary tests can pin the behavior.
///
/// `bytes` mode seeks to `offset` and reads at most `limit` bytes, so
/// unselected bytes are never materialized. `lines`, `chars` and autodetect
/// read fixed-size chunks and retain only the selected window while still
/// validating UTF-8 to EOF.
async fn read_opened_file(
    args: ReadFileArgs,
    file: tokio::fs::File,
    chunk_size: usize,
) -> Result<ReadResult, ToolError> {
    tracing::debug!("reading file");
    let chunk_size = chunk_size.max(1);
    let path = args.path;
    let file_length = file
        .metadata()
        .await
        .map_err(|error| read_io_error(&path, error))?
        .len();
    let file_length = usize::try_from(file_length).unwrap_or(usize::MAX);
    let result = match args.mode {
        Some(ReadMode::Bytes) => {
            bytes_read(
                file,
                file_length,
                path,
                args.offset,
                args.limit,
                args.encoding,
            )
            .await
        }
        Some(ReadMode::Lines) => {
            lines_read(file, chunk_size, file_length, path, args.offset, args.limit).await
        }
        Some(ReadMode::Chars) => {
            chars_read(file, chunk_size, file_length, path, args.offset, args.limit).await
        }
        None => autodetect_read(file, chunk_size, file_length, path, args.offset, args.limit).await,
    }?;
    tracing::debug!(
        source_size_bytes = result.source_size_bytes,
        source_lines = ?result.source_lines,
        raw_selected_bytes = result.read_bytes,
        raw_selected_lines = ?metadata_read_lines(&result),
        truncated = result.truncated,
        "read file"
    );
    Ok(result)
}

/// Seek-and-range read backing `bytes` mode: only the selected byte range is
/// loaded (never the whole file), then decoded or encoded once.
async fn bytes_read(
    mut file: tokio::fs::File,
    file_length: usize,
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    encoding: FileEncoding,
) -> Result<ReadResult, ToolError> {
    let start = offset.unwrap_or(0).min(file_length);
    let selected_length = match limit {
        Some(limit) => limit.min(file_length - start),
        None => file_length - start,
    };
    // Media-type recognition requires the selection to be the complete file;
    // with range reads that is checked against the file's metadata length.
    let is_complete_selection = start == 0 && selected_length == file_length;
    file.seek(SeekFrom::Start(u64::try_from(start).unwrap_or(u64::MAX)))
        .await
        .map_err(|error| read_io_error(&path, error))?;
    // Read at most the output cap first so oversized selections never
    // materialize in full.
    let mut selected = read_up_to(
        &mut file,
        selected_length.min(READ_OUTPUT_LIMIT_BYTES),
        &path,
    )
    .await?;
    let mut truncated = selected.len() < selected_length;
    if truncated && encoding == FileEncoding::Base64 && is_complete_selection {
        // A complete recognized media read bypasses the output cap: a
        // truncated image or PDF is unusable. It is still bounded by
        // READ_MEDIA_MAX_BYTES so a whole file never loads unbounded.
        if let Some(media_type) = supported_media_type(&selected) {
            if selected_length > READ_MEDIA_MAX_BYTES {
                return Err(read_media_too_large_error(
                    &path,
                    &media_type,
                    selected_length,
                ));
            }
            let remainder = read_up_to(&mut file, selected_length - selected.len(), &path).await?;
            selected.extend_from_slice(&remainder);
            truncated = selected.len() < selected_length;
        }
    }
    if truncated && encoding == FileEncoding::Utf8 {
        // Cut cleanly at the cap: back off a trailing partial character so the
        // lossy decode never starts a multi-byte sequence it cannot finish.
        if let Err(error) = std::str::from_utf8(&selected) {
            if error.error_len().is_none() {
                selected.truncate(error.valid_up_to());
            }
        }
    }
    match encoding {
        FileEncoding::Utf8 => {
            let content = String::from_utf8_lossy(&selected).to_string();
            let source_lines =
                (start == 0 && selected.len() == file_length).then(|| content.lines().count());
            Ok(ReadResult {
                read_bytes: content.len(),
                source_size_bytes: file_length,
                source_lines,
                content,
                path,
                starting_line: Some(0),
                encoding: FileEncoding::Utf8,
                text_preview: None,
                media_type: None,
                truncated,
            })
        }
        FileEncoding::Base64 => Ok(base64_read_result(
            &selected,
            is_complete_selection && !truncated,
            path,
            truncated,
            file_length,
            None,
        )),
    }
}

/// Streaming `lines` read: decode complete lines from fixed-size chunks,
/// skip `offset` lines, collect up to `limit` lines, then keep scanning to EOF
/// so whole-file UTF-8 validation is preserved.
async fn lines_read(
    file: tokio::fs::File,
    chunk_size: usize,
    file_length: usize,
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadResult, ToolError> {
    let mut reader = tokio::io::BufReader::with_capacity(chunk_size, file);
    let mut scanner = LineWindowScanner::new(offset, limit);
    let mut source_lines = SourceLineCounter::default();
    stream_validated_utf8(
        &mut reader,
        chunk_size,
        |_| {},
        |text| {
            source_lines.observe(text);
            scanner.process(text);
        },
    )
    .await
    .map_err(|error| stream_error_to_tool_error(&path, "lines", error))?;
    Ok(scanner.finish(path, file_length, source_lines.count()))
}

/// Streaming `chars` read: count code points from the start of the file and
/// keep only the selected characters, validating UTF-8 to EOF.
async fn chars_read(
    file: tokio::fs::File,
    chunk_size: usize,
    file_length: usize,
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadResult, ToolError> {
    let mut reader = tokio::io::BufReader::with_capacity(chunk_size, file);
    let mut scanner = CharWindowScanner::new(offset, limit);
    let mut source_lines = SourceLineCounter::default();
    stream_validated_utf8(
        &mut reader,
        chunk_size,
        |_| {},
        |text| {
            source_lines.observe(text);
            scanner.process(text);
        },
    )
    .await
    .map_err(|error| stream_error_to_tool_error(&path, "chars", error))?;
    Ok(scanner.finish(path, file_length, source_lines.count()))
}

/// Resolve a read that was called without an explicit `mode`.
///
/// Valid UTF-8 text keeps the `lines` unit, so `offset`/`limit` retain their
/// line-index meaning of the previous default. A whole file recognized as a
/// supported media type goes through the Base64 path so its typed attachment
/// (and the later modality-aware projection) is produced without the caller
/// naming `mode`/`encoding`. Every other binary file — including a ranged
/// read of a recognized one — fails with the explicit-mode retry hint.
///
/// Classification streams: UTF-8 is validated chunk by chunk while the line
/// window is collected, and a single invalid byte settles the decision. The
/// media sniff runs against the first chunk only; the fallback then re-reads
/// the whole file from the same open handle — unless it is longer than
/// [`READ_MEDIA_MAX_BYTES`], which fails before any whole-file load.
async fn autodetect_read(
    file: tokio::fs::File,
    chunk_size: usize,
    file_length: usize,
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadResult, ToolError> {
    let mut reader = tokio::io::BufReader::with_capacity(chunk_size, file);
    let mut scanner = LineWindowScanner::new(offset, limit);
    let mut source_lines = SourceLineCounter::default();
    let mut first_chunk: Vec<u8> = Vec::new();
    let outcome = stream_validated_utf8(
        &mut reader,
        chunk_size,
        |chunk| {
            if first_chunk.is_empty() {
                first_chunk.extend_from_slice(chunk);
            }
        },
        |text| {
            source_lines.observe(text);
            scanner.process(text);
        },
    )
    .await;
    match outcome {
        Ok(()) => Ok(scanner.finish(path, file_length, source_lines.count())),
        Err(StreamError::Io(error)) => Err(read_io_error(&path, error)),
        Err(StreamError::Utf8(_)) => {
            let recognized_media_type = (offset.is_none() && limit.is_none())
                .then(|| supported_media_type(&first_chunk))
                .flatten();
            match recognized_media_type {
                Some(media_type) if file_length > READ_MEDIA_MAX_BYTES => {
                    Err(read_media_too_large_error(&path, &media_type, file_length))
                }
                Some(_) => {
                    reader
                        .seek(SeekFrom::Start(0))
                        .await
                        .map_err(|error| read_io_error(&path, error))?;
                    let mut bytes = Vec::new();
                    reader
                        .read_to_end(&mut bytes)
                        .await
                        .map_err(|error| read_io_error(&path, error))?;
                    Ok(base64_read_result(
                        &bytes,
                        true,
                        path,
                        false,
                        file_length,
                        None,
                    ))
                }
                None => Err(ToolError::failed(
                    "read",
                    format!(
                        "failed to read '{path}': autodetect supports UTF-8 text and whole \
                         image/PDF files only; retry with explicit mode: \"bytes\" \
                         (use encoding: \"base64\" for binary data)"
                    ),
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests;
