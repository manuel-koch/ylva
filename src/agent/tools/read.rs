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
//! result metadata plus a `Partial result` content-section label, so the model
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

use base64::Engine as _;
use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
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

/// Hard cap on the raw selected bytes returned by a text-oriented `read`
/// result: `bytes` + `utf8`, non-media `bytes` + `base64`, `lines`, `chars`
/// and autodetect text. Content past the cap is discarded silently (no in-text
/// marker): the result metadata carries `truncated: true` and the UI content
/// section is labeled `Partial result`, the same vocabulary `bash` and
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

/// Build the tool error for a failed file I/O operation, naming the path and
/// the underlying OS error.
fn read_io_error(path: &str, error: impl std::fmt::Display) -> ToolError {
    tracing::error!(error = %error, "failed to read");
    ToolError::failed("read", format!("failed to read '{path}': {error}"))
}

/// Build the tool error for a byte stream that is not valid UTF-8, keeping the
/// strict-text remedy hint. `utf8_error` is rendered like
/// [`std::str::Utf8Error`]'s `Display`.
fn decode_error(path: &str, mode: &str, utf8_error: &str) -> ToolError {
    tracing::error!(
        path,
        mode,
        error = %utf8_error,
        "failed to decode file as UTF-8"
    );
    ToolError::failed(
        "read",
        format!(
            "failed to read '{path}' in {mode} mode: invalid UTF-8 ({utf8_error}) — \
             the file is not UTF-8 text; retry with mode: \"bytes\" \
             (use encoding: \"base64\" for binary data)"
        ),
    )
}

/// Format a byte count as a human-readable KiB value.
fn format_kibibytes(bytes: usize) -> String {
    format!("{} KiB", bytes / 1024)
}

/// Format a byte count as a human-readable MiB value.
fn format_mebibytes(bytes: usize) -> String {
    format!("{} MiB", bytes / (1024 * 1024))
}

/// Build the tool error for a complete recognized media file that exceeds
/// [`READ_MEDIA_MAX_BYTES`]. Truncation is not an option: a partial file is
/// never attachable, so the read fails with the size, the limit, and the
/// range-read remedy.
fn read_media_too_large_error(path: &str, media_type: &str, file_length: usize) -> ToolError {
    ToolError::failed(
        "read",
        format!(
            "failed to read '{path}': complete {media_type} file is {file_length} bytes, over \
             the {READ_MEDIA_MAX_BYTES}-byte limit for whole-file reads (a partial file cannot \
             be attached); inspect ranges with mode: \"bytes\" and offset/limit \
             (use encoding: \"base64\" for binary data)"
        ),
    )
}

/// The first byte of a streamed file that is not valid UTF-8, located by
/// absolute offset within the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Utf8StreamError {
    /// A definitively invalid byte sequence starting at `offset`.
    Invalid { offset: usize, error_len: usize },
    /// The file ended in the middle of a sequence starting at `offset`.
    Incomplete { offset: usize },
}

impl Utf8StreamError {
    /// Render exactly like [`std::str::Utf8Error`]'s `Display`, so a streamed
    /// read reports byte-identical error text to whole-file validation.
    fn description(&self) -> String {
        match self {
            Self::Invalid { offset, error_len } => {
                format!("invalid utf-8 sequence of {error_len} bytes from index {offset}")
            }
            Self::Incomplete { offset } => {
                format!("incomplete utf-8 byte sequence from index {offset}")
            }
        }
    }
}

/// Failure modes of [`stream_validated_utf8`], kept separate so callers can
/// react to invalid UTF-8 (autodetect classifies the file instead of failing).
enum StreamError {
    /// I/O failure while reading the file.
    Io(std::io::Error),
    /// The stream is not valid UTF-8 at the reported absolute offset.
    Utf8(Utf8StreamError),
}

fn stream_error_to_tool_error(path: &str, mode: &str, error: StreamError) -> ToolError {
    match error {
        StreamError::Io(error) => read_io_error(path, error),
        StreamError::Utf8(error) => decode_error(path, mode, &error.description()),
    }
}

/// Fill `chunk` completely unless the reader hits EOF; returns bytes read
/// (`0` means EOF).
async fn fill_chunk<R>(reader: &mut R, chunk: &mut [u8]) -> Result<usize, std::io::Error>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut filled = 0;
    while filled < chunk.len() {
        let read = reader.read(&mut chunk[filled..]).await?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

/// Stream a file in fixed-size chunks, validating UTF-8 incrementally instead
/// of buffering the whole file.
///
/// Only the incomplete trailing sequence of a chunk is carried over to the next
/// chunk, so memory stays O(chunk). Every byte range that is now known to be
/// valid UTF-8 is passed to `consume_validated`, which may discard it (the
/// output cap, the selection window) — the scan still runs to EOF so
/// whole-file validation semantics are preserved. `observe_chunk` sees each
/// raw chunk before validation — autodetect sniffs media types from the first
/// one, which may contain bytes that are not valid UTF-8.
async fn stream_validated_utf8<R, O, C>(
    reader: &mut R,
    chunk_size: usize,
    mut observe_chunk: O,
    mut consume_validated: C,
) -> Result<(), StreamError>
where
    R: tokio::io::AsyncRead + Unpin,
    O: FnMut(&[u8]),
    C: FnMut(&str),
{
    let mut chunk = vec![0u8; chunk_size];
    let mut scratch: Vec<u8> = Vec::with_capacity(chunk_size + 4);
    let mut carry: Vec<u8> = Vec::new();
    let mut base_offset = 0usize;
    loop {
        let filled = fill_chunk(reader, &mut chunk)
            .await
            .map_err(StreamError::Io)?;
        if filled == 0 {
            break;
        }
        observe_chunk(&chunk[..filled]);
        scratch.clear();
        scratch.extend_from_slice(&carry);
        scratch.extend_from_slice(&chunk[..filled]);
        match std::str::from_utf8(&scratch) {
            Ok(valid) => {
                consume_validated(valid);
                base_offset += scratch.len();
                carry.clear();
            }
            Err(error) => match error.error_len() {
                Some(error_len) => {
                    return Err(StreamError::Utf8(Utf8StreamError::Invalid {
                        offset: base_offset + error.valid_up_to(),
                        error_len,
                    }));
                }
                None => {
                    let valid_up_to = error.valid_up_to();
                    let valid = std::str::from_utf8(&scratch[..valid_up_to])
                        .expect("bytes before an incomplete sequence are valid UTF-8");
                    consume_validated(valid);
                    base_offset += valid_up_to;
                    carry.clear();
                    carry.extend_from_slice(&scratch[valid_up_to..]);
                }
            },
        }
    }
    if !carry.is_empty() {
        if let Err(error) = std::str::from_utf8(&carry) {
            return Err(StreamError::Utf8(Utf8StreamError::Incomplete {
                offset: base_offset + error.valid_up_to(),
            }));
        }
    }
    Ok(())
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
    /// UTF-8 text projection for user-facing previews when Base64 carries text.
    pub text_preview: Option<String>,
    /// Recognized supported media type when the selected Base64 range is the
    /// complete source file.
    pub media_type: Option<String>,
    /// `true` when [`READ_OUTPUT_LIMIT_BYTES`] cut `content`; reported as
    /// `truncated: true` metadata and the `Partial result` section label.
    pub truncated: bool,
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
        let label = self.truncated.then(|| "Partial result".to_string());
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
        Box::new(ReadCall {
            id,
            raw_arguments: arguments,
            timeout_secs: self.timeout_secs(),
        })
    }
}

/// Chunk size used to stream a file for `lines`/`chars`/autodetect reads.
/// Parameterized so chunk-boundary tests stay deterministic.
const STREAM_CHUNK_BYTES: usize = 8 * 1024;

/// Read size used while materializing a selected byte range.
const RANGE_READ_CHUNK_BYTES: usize = 64 * 1024;

async fn read_file(args: ReadFileArgs) -> Result<ReadResult, ToolError> {
    read_file_streaming(args, STREAM_CHUNK_BYTES).await
}

/// Resolve a read by streaming the file instead of loading it whole.
///
/// `bytes` mode seeks to `offset` and reads at most `limit` bytes, so
/// unselected bytes are never materialized. `lines`, `chars` and autodetect
/// read fixed-size chunks and retain only the selected window while still
/// validating UTF-8 to EOF. `chunk_size` is a parameter so chunk-boundary
/// tests can pin the behavior.
async fn read_file_streaming(
    args: ReadFileArgs,
    chunk_size: usize,
) -> Result<ReadResult, ToolError> {
    if args.encoding == FileEncoding::Base64 && args.mode != Some(ReadMode::Bytes) {
        return Err(ToolError::failed(
            "read",
            "`encoding: \"base64\"` requires explicit `mode: \"bytes\"`",
        ));
    }
    tracing::debug!("reading file");
    let chunk_size = chunk_size.max(1);
    let path = args.path;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|error| read_io_error(&path, error))?;
    let file_length = file
        .metadata()
        .await
        .map_err(|error| read_io_error(&path, error))?
        .len();
    let file_length = usize::try_from(file_length).unwrap_or(usize::MAX);
    match args.mode {
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
        Some(ReadMode::Lines) => lines_read(file, chunk_size, path, args.offset, args.limit).await,
        Some(ReadMode::Chars) => chars_read(file, chunk_size, path, args.offset, args.limit).await,
        None => autodetect_read(file, chunk_size, file_length, path, args.offset, args.limit).await,
    }
}

/// Read at most `max_bytes` from the current position of `file`.
async fn read_up_to(
    file: &mut tokio::fs::File,
    max_bytes: usize,
    path: &str,
) -> Result<Vec<u8>, ToolError> {
    let mut buffer = Vec::with_capacity(max_bytes.min(RANGE_READ_CHUNK_BYTES));
    (&mut *file)
        .take(u64::try_from(max_bytes).unwrap_or(u64::MAX))
        .read_to_end(&mut buffer)
        .await
        .map_err(|error| read_io_error(path, error))?;
    Ok(buffer)
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
            tracing::debug!(
                size = content.len(),
                lines = content.lines().count(),
                "read file"
            );
            Ok(ReadResult {
                read_bytes: content.len(),
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
        )),
    }
}

/// Assemble the selected line window of a `lines` read while the file streams
/// by.
///
/// Mirrors `text.split_terminator('\n')` + `join("\n")`: a trailing newline
/// does not create a final empty line, `"\n"` is one empty line, CRLF endings
/// keep their `'\r'`, and exactly one `'\n'` separates selected lines in the
/// joined content.
struct LineWindowScanner {
    /// 0-based index of the first selected line.
    window_start: usize,
    /// 0-based index after the last selected line (`None` = to EOF).
    window_end: Option<usize>,
    /// 0-based index of the line currently being scanned.
    line_index: usize,
    /// Number of selected lines already started.
    collected_lines: usize,
    /// `true` while no byte of the current line has been seen yet.
    at_line_start: bool,
    /// `true` while the current line belongs to the selected window.
    collecting: bool,
    /// `true` once the current line is known to exist and has been counted.
    line_confirmed: bool,
    content: String,
    /// `true` when [`READ_OUTPUT_LIMIT_BYTES`] cut off selected content.
    truncated: bool,
}

impl LineWindowScanner {
    fn new(offset: Option<usize>, limit: Option<usize>) -> Self {
        let window_start = offset.unwrap_or(0);
        Self {
            window_start,
            window_end: limit.map(|limit| window_start.saturating_add(limit)),
            line_index: 0,
            collected_lines: 0,
            at_line_start: true,
            collecting: false,
            line_confirmed: false,
            content: String::new(),
            truncated: false,
        }
    }

    /// `true` once every selected line has been collected — nothing more can
    /// be added, so callers may skip to the validation scan.
    fn window_completed(&self) -> bool {
        self.window_end
            .is_some_and(|window_end| self.line_index >= window_end)
    }

    fn in_window(&self) -> bool {
        self.line_index >= self.window_start
            && self
                .window_end
                .is_none_or(|window_end| self.line_index < window_end)
    }

    /// Count the current line and separate it from the previously collected
    /// one. A line exists once it has content or its `'\n'` terminator is
    /// seen; the empty remainder of a file ending in `'\n'` is not a line.
    ///
    /// Returns `false` when the output cap leaves no room for the separator.
    fn confirm_line(&mut self) -> bool {
        if self.line_confirmed {
            return true;
        }
        if self.collected_lines > 0 {
            if self.content.len() >= READ_OUTPUT_LIMIT_BYTES {
                self.truncated = true;
                return false;
            }
            self.content.push('\n');
        }
        self.line_confirmed = true;
        self.collected_lines += 1;
        true
    }

    /// Append as much of `segment` as fits the output cap, cutting on a
    /// UTF-8 char boundary. Returns `false` when content was left behind.
    fn push_segment(&mut self, segment: &str) -> bool {
        let remaining = READ_OUTPUT_LIMIT_BYTES.saturating_sub(self.content.len());
        let mut take = segment.len().min(remaining);
        while take > 0 && !segment.is_char_boundary(take) {
            take -= 1;
        }
        self.content.push_str(&segment[..take]);
        if take < segment.len() {
            self.truncated = true;
            return false;
        }
        true
    }

    /// Consume the next validated UTF-8 range of the file, keeping only the
    /// selected window. Once the output cap cut selected content the window
    /// stops growing, while the caller's validation scan runs to EOF.
    fn process(&mut self, text: &str) {
        if self.window_completed() || self.truncated {
            return;
        }
        let mut rest = text;
        loop {
            let (segment, tail) = match rest.find('\n') {
                Some(index) => (&rest[..index], Some(&rest[index + 1..])),
                None => (rest, None),
            };
            let starts_line = self.at_line_start;
            if starts_line {
                self.at_line_start = false;
                self.collecting = self.in_window();
                self.line_confirmed = false;
            }
            if self.collecting && (!segment.is_empty() || tail.is_some()) && !self.confirm_line() {
                return;
            }
            if self.collecting && !segment.is_empty() && !self.push_segment(segment) {
                return;
            }
            match tail {
                Some(tail) => {
                    self.line_index += 1;
                    self.at_line_start = true;
                    self.collecting = false;
                    rest = tail;
                }
                None => {
                    if starts_line && segment.is_empty() {
                        // Nothing of this line was consumed — stay at its start.
                        self.at_line_start = true;
                        self.collecting = false;
                    }
                    return;
                }
            }
        }
    }

    fn finish(self, path: String) -> ReadResult {
        // Lines seen so far: completed lines plus a pending final line that
        // was not terminated by `'\n'`. At EOF this is the file's line count;
        // after the output cap stopped collection it is a lower bound that is
        // already ≥ the window start, so the `min` below still clamps to the
        // window start.
        let lines_seen = self.line_index + usize::from(!self.at_line_start);
        let starting_line = Some(self.window_start.min(lines_seen));
        let content = self.content;
        tracing::debug!(
            size = content.len(),
            lines = content.lines().count(),
            "read file"
        );
        ReadResult {
            read_bytes: content.len(),
            content,
            path,
            starting_line,
            encoding: FileEncoding::Utf8,
            text_preview: None,
            media_type: None,
            truncated: self.truncated,
        }
    }
}

/// Assemble the selected character window of a `chars` read while the file
/// streams by, counting UTF-8 code points from the start of the file.
struct CharWindowScanner {
    /// 0-based index of the first selected character.
    window_start: usize,
    /// 0-based index after the last selected character (`None` = to EOF).
    window_end: Option<usize>,
    /// Number of code points seen so far.
    char_index: usize,
    content: String,
    /// `true` when [`READ_OUTPUT_LIMIT_BYTES`] cut off selected content.
    truncated: bool,
}

impl CharWindowScanner {
    fn new(offset: Option<usize>, limit: Option<usize>) -> Self {
        let window_start = offset.unwrap_or(0);
        Self {
            window_start,
            window_end: limit.map(|limit| window_start.saturating_add(limit)),
            char_index: 0,
            content: String::new(),
            truncated: false,
        }
    }

    /// `true` once every selected character has been collected.
    fn window_completed(&self) -> bool {
        self.window_end
            .is_some_and(|window_end| self.char_index >= window_end)
    }

    /// Consume the next validated UTF-8 range of the file, keeping only the
    /// selected characters. Once the output cap cut selected content the
    /// window stops growing, while the caller's validation scan runs to EOF.
    fn process(&mut self, text: &str) {
        if self.window_completed() || self.truncated {
            return;
        }
        for character in text.chars() {
            if self.char_index >= self.window_start
                && self
                    .window_end
                    .is_none_or(|window_end| self.char_index < window_end)
            {
                if self.content.len() + character.len_utf8() > READ_OUTPUT_LIMIT_BYTES {
                    self.truncated = true;
                    return;
                }
                self.content.push(character);
            }
            self.char_index += 1;
        }
    }

    fn finish(self, path: String) -> ReadResult {
        let content = self.content;
        tracing::debug!(
            size = content.len(),
            lines = content.lines().count(),
            "read file"
        );
        ReadResult {
            read_bytes: content.len(),
            content,
            path,
            starting_line: Some(0),
            encoding: FileEncoding::Utf8,
            text_preview: None,
            media_type: None,
            truncated: self.truncated,
        }
    }
}

/// Streaming `lines` read: decode complete lines from fixed-size chunks,
/// skip `offset` lines, collect up to `limit` lines, then keep scanning to EOF
/// so whole-file UTF-8 validation is preserved.
async fn lines_read(
    file: tokio::fs::File,
    chunk_size: usize,
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadResult, ToolError> {
    let mut reader = tokio::io::BufReader::with_capacity(chunk_size, file);
    let mut scanner = LineWindowScanner::new(offset, limit);
    stream_validated_utf8(
        &mut reader,
        chunk_size,
        |_| {},
        |text| scanner.process(text),
    )
    .await
    .map_err(|error| stream_error_to_tool_error(&path, "lines", error))?;
    Ok(scanner.finish(path))
}

/// Streaming `chars` read: count code points from the start of the file and
/// keep only the selected characters, validating UTF-8 to EOF.
async fn chars_read(
    file: tokio::fs::File,
    chunk_size: usize,
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadResult, ToolError> {
    let mut reader = tokio::io::BufReader::with_capacity(chunk_size, file);
    let mut scanner = CharWindowScanner::new(offset, limit);
    stream_validated_utf8(
        &mut reader,
        chunk_size,
        |_| {},
        |text| scanner.process(text),
    )
    .await
    .map_err(|error| stream_error_to_tool_error(&path, "chars", error))?;
    Ok(scanner.finish(path))
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
    let mut first_chunk: Vec<u8> = Vec::new();
    let outcome = stream_validated_utf8(
        &mut reader,
        chunk_size,
        |chunk| {
            if first_chunk.is_empty() {
                first_chunk.extend_from_slice(chunk);
            }
        },
        |text| scanner.process(text),
    )
    .await;
    match outcome {
        Ok(()) => Ok(scanner.finish(path)),
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
                    Ok(base64_read_result(&bytes, true, path, false))
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

/// Build the result of a Base64 read of the already-selected `bytes`.
///
/// The media type is recognized only when `is_complete_file` says the
/// selection covers the complete source file — a truncated result is never a
/// complete file, so the cap and the attachment rules compose.
fn base64_read_result(
    selected: &[u8],
    is_complete_file: bool,
    path: String,
    truncated: bool,
) -> ReadResult {
    let read_bytes = selected.len();
    let content = STANDARD_BASE64.encode(selected);
    let text_preview = utf8_text_preview(selected);
    let media_type = is_complete_file
        .then(|| supported_media_type(selected))
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
        truncated,
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
        assert!(tool
            .description()
            .contains(&format_kibibytes(READ_OUTPUT_LIMIT_BYTES)));
        assert!(tool
            .description()
            .contains(&format_mebibytes(READ_MEDIA_MAX_BYTES)));

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
            truncated: false,
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
            truncated: false,
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

    // ── Range-boundary characterization tests ─────────────────────────────────
    //
    // These pin the *current* selection semantics at the boundaries of
    // `offset`/`limit`: past EOF, `limit: 0`, and ranges that cross EOF.

    const BOUNDARY_TEXT: &str = "one\ntwo\nthree";

    #[test]
    fn read_bytes_utf8_range_boundaries() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, BOUNDARY_TEXT).expect("write");

            let read = |offset: Option<usize>, limit: Option<usize>| ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Utf8,
                offset,
                limit,
            };

            let offset_past_eof = read_file(read(Some(20), None)).await.expect("read");
            assert_eq!(offset_past_eof.text(), "");
            assert_eq!(offset_past_eof.read_bytes, 0);
            assert_eq!(offset_past_eof.starting_line, Some(0));

            let offset_past_eof_no_limit = read_file(read(Some(100), None)).await.expect("read");
            assert_eq!(offset_past_eof_no_limit.text(), "");
            assert_eq!(offset_past_eof_no_limit.read_bytes, 0);

            let limit_zero = read_file(read(None, Some(0))).await.expect("read");
            assert_eq!(limit_zero.text(), "");
            assert_eq!(limit_zero.read_bytes, 0);

            let crossing_eof = read_file(read(Some(11), Some(5))).await.expect("read");
            assert_eq!(crossing_eof.text(), "ee");
            assert_eq!(crossing_eof.read_bytes, 2);
            assert_eq!(crossing_eof.starting_line, Some(0));
        });
    }

    #[test]
    fn read_bytes_base64_range_boundaries() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, BOUNDARY_TEXT).expect("write");

            let read = |offset: Option<usize>, limit: Option<usize>| ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Base64,
                offset,
                limit,
            };

            let offset_past_eof = read_file(read(Some(20), None)).await.expect("read");
            assert_eq!(offset_past_eof.text(), "");
            assert_eq!(offset_past_eof.read_bytes, 0);
            assert_eq!(offset_past_eof.starting_line, Some(0));
            assert_eq!(
                offset_past_eof.metadata(),
                Some(serde_json::json!({
                    "read_bytes": 0,
                    "read_lines": 0,
                    "encoding": "base64",
                }))
            );

            let limit_zero = read_file(read(None, Some(0))).await.expect("read");
            assert_eq!(limit_zero.text(), "");
            assert_eq!(limit_zero.read_bytes, 0);
            assert!(limit_zero.content_sections().is_empty());

            let crossing_eof = read_file(read(Some(11), Some(5))).await.expect("read");
            assert_eq!(
                crossing_eof.text(),
                STANDARD_BASE64.encode(&BOUNDARY_TEXT.as_bytes()[11..])
            );
            assert_eq!(crossing_eof.read_bytes, 2);
            assert_eq!(crossing_eof.starting_line, Some(0));
        });
    }

    #[test]
    fn read_lines_range_boundaries() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, BOUNDARY_TEXT).expect("write");

            let read = |offset: Option<usize>, limit: Option<usize>| ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Lines),
                encoding: FileEncoding::Utf8,
                offset,
                limit,
            };

            let offset_past_eof = read_file(read(Some(20), None)).await.expect("read");
            assert_eq!(offset_past_eof.text(), "");
            assert_eq!(offset_past_eof.read_bytes, 0);
            assert_eq!(offset_past_eof.starting_line, Some(3));

            let limit_zero = read_file(read(None, Some(0))).await.expect("read");
            assert_eq!(limit_zero.text(), "");
            assert_eq!(limit_zero.read_bytes, 0);
            assert_eq!(limit_zero.starting_line, Some(0));

            let limit_zero_after_offset = read_file(read(Some(2), Some(0))).await.expect("read");
            assert_eq!(limit_zero_after_offset.text(), "");
            assert_eq!(limit_zero_after_offset.read_bytes, 0);
            assert_eq!(limit_zero_after_offset.starting_line, Some(2));

            let crossing_eof = read_file(read(Some(1), Some(10))).await.expect("read");
            assert_eq!(crossing_eof.text(), "two\nthree");
            assert_eq!(crossing_eof.read_bytes, 9);
            assert_eq!(crossing_eof.starting_line, Some(1));
        });
    }

    #[test]
    fn read_chars_range_boundaries() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, "héllo").expect("write");

            let read = |offset: Option<usize>, limit: Option<usize>| ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Chars),
                encoding: FileEncoding::Utf8,
                offset,
                limit,
            };

            let offset_past_eof = read_file(read(Some(10), None)).await.expect("read");
            assert_eq!(offset_past_eof.text(), "");
            assert_eq!(offset_past_eof.read_bytes, 0);
            assert_eq!(offset_past_eof.starting_line, Some(0));

            let limit_zero = read_file(read(None, Some(0))).await.expect("read");
            assert_eq!(limit_zero.text(), "");
            assert_eq!(limit_zero.read_bytes, 0);
            assert_eq!(limit_zero.starting_line, Some(0));

            let crossing_eof = read_file(read(Some(3), Some(10))).await.expect("read");
            assert_eq!(crossing_eof.text(), "lo");
            assert_eq!(crossing_eof.read_bytes, 2);
            assert_eq!(crossing_eof.starting_line, Some(0));
        });
    }

    #[test]
    fn read_autodetect_text_range_boundaries() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, BOUNDARY_TEXT).expect("write");

            let read = |offset: Option<usize>, limit: Option<usize>| ReadFileArgs {
                path: path.display().to_string(),
                mode: None,
                encoding: FileEncoding::Utf8,
                offset,
                limit,
            };

            let offset_past_eof = read_file(read(Some(20), None)).await.expect("read");
            assert_eq!(offset_past_eof.text(), "");
            assert_eq!(offset_past_eof.encoding, FileEncoding::Utf8);
            assert_eq!(offset_past_eof.read_bytes, 0);
            assert_eq!(offset_past_eof.starting_line, Some(3));

            let limit_zero = read_file(read(None, Some(0))).await.expect("read");
            assert_eq!(limit_zero.text(), "");
            assert_eq!(limit_zero.read_bytes, 0);
            assert_eq!(limit_zero.starting_line, Some(0));

            let crossing_eof = read_file(read(Some(1), Some(10))).await.expect("read");
            assert_eq!(crossing_eof.text(), "two\nthree");
            assert_eq!(crossing_eof.read_bytes, 9);
            assert_eq!(crossing_eof.starting_line, Some(1));
        });
    }

    // ── Chunk-boundary tests ──────────────────────────────────────────────────
    //
    // `read_file_streaming` takes the chunk size as a parameter so these stay
    // fast and deterministic: no large fixtures, no timing assertions.

    /// A line that starts in one chunk and ends in another is reassembled.
    #[test]
    fn read_lines_reassembles_lines_split_across_chunks() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, "aaa\nbbb\nccc").expect("write");
            let arguments = ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Lines),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            };

            let whole = read_file_streaming(arguments.clone(), 6)
                .await
                .expect("read");
            assert_eq!(whole.text(), "aaa\nbbb\nccc");
            assert_eq!(whole.starting_line, Some(0));

            let only_second = read_file_streaming(
                ReadFileArgs {
                    offset: Some(1),
                    limit: Some(1),
                    ..arguments.clone()
                },
                6,
            )
            .await
            .expect("read");
            assert_eq!(only_second.text(), "bbb");
            assert_eq!(only_second.starting_line, Some(1));

            let only_first = read_file_streaming(
                ReadFileArgs {
                    offset: None,
                    limit: Some(1),
                    ..arguments
                },
                6,
            )
            .await
            .expect("read");
            assert_eq!(only_first.text(), "aaa");
        });
    }

    /// A chunk size of one byte splits every multi-byte sequence.
    #[test]
    fn read_chars_counts_code_points_split_across_chunks() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            // `é` is 2 bytes, `😀` is 4 bytes, both split by chunk size 1.
            std::fs::write(&path, "aé😀z").expect("write");
            let arguments = ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Chars),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            };

            let whole = read_file_streaming(arguments.clone(), 1)
                .await
                .expect("read");
            assert_eq!(whole.text(), "aé😀z");
            assert_eq!(whole.read_bytes, "aé😀z".len());

            let window = read_file_streaming(
                ReadFileArgs {
                    offset: Some(1),
                    limit: Some(2),
                    ..arguments
                },
                1,
            )
            .await
            .expect("read");
            assert_eq!(window.text(), "é😀");
            assert_eq!(window.read_bytes, "é😀".len());
        });
    }

    /// Invalid UTF-8 is reported with its absolute file offset, whether the
    /// bad byte starts a chunk or arrives as part of a carried sequence.
    #[test]
    fn read_lines_reports_invalid_utf8_at_chunk_boundaries() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");

            let starts_chunk = dir.path().join("starts-chunk.txt");
            std::fs::write(&starts_chunk, [b'a', b'b', b'c', 0xff, b'd']).expect("write");
            let error = read_file_streaming(
                ReadFileArgs {
                    path: starts_chunk.display().to_string(),
                    mode: Some(ReadMode::Lines),
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                },
                3,
            )
            .await
            .expect_err("invalid UTF-8 must fail");
            assert!(
                error
                    .to_string()
                    .contains("invalid utf-8 sequence of 1 bytes from index 3"),
                "unexpected error: {error}"
            );

            let carried = dir.path().join("carried.txt");
            std::fs::write(&carried, [b'a', 0xc3, 0x28, b'b']).expect("write");
            let error = read_file_streaming(
                ReadFileArgs {
                    path: carried.display().to_string(),
                    mode: Some(ReadMode::Chars),
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                },
                2,
            )
            .await
            .expect_err("invalid UTF-8 must fail");
            assert!(
                error
                    .to_string()
                    .contains("invalid utf-8 sequence of 1 bytes from index 1"),
                "unexpected error: {error}"
            );

            let trailing = dir.path().join("trailing.txt");
            std::fs::write(&trailing, [b'a', 0xc3]).expect("write");
            let error = read_file_streaming(
                ReadFileArgs {
                    path: trailing.display().to_string(),
                    mode: Some(ReadMode::Lines),
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                },
                1,
            )
            .await
            .expect_err("truncated sequence must fail");
            assert!(
                error
                    .to_string()
                    .contains("incomplete utf-8 byte sequence from index 1"),
                "unexpected error: {error}"
            );
        });
    }

    #[test]
    fn utf8_stream_error_description_matches_std_display() {
        let invalid_bytes: Vec<u8> = vec![b'a', b'b', 0xff, b'c'];
        let error = std::str::from_utf8(&invalid_bytes).expect_err("invalid fixture");
        let stream_error = Utf8StreamError::Invalid {
            offset: error.valid_up_to(),
            error_len: error.error_len().expect("definitive invalid byte"),
        };
        assert_eq!(stream_error.description(), error.to_string());

        let incomplete_bytes: Vec<u8> = vec![b'a', b'b', 0xc3];
        let error = std::str::from_utf8(&incomplete_bytes).expect_err("incomplete fixture");
        let stream_error = Utf8StreamError::Incomplete {
            offset: error.valid_up_to(),
        };
        assert_eq!(stream_error.description(), error.to_string());
    }

    #[test]
    fn read_autodetect_keeps_utf8_first_ordering_and_sniffs_the_first_chunk() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");

            // ASCII `%PDF…` is valid UTF-8, so it stays a text result.
            let pdf_text = dir.path().join("ascii.pdf.txt");
            std::fs::write(&pdf_text, b"%PDF-1.7\n42\n").expect("write");
            let text_result = read_file_streaming(
                ReadFileArgs {
                    path: pdf_text.display().to_string(),
                    mode: None,
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                },
                4,
            )
            .await
            .expect("ASCII PDF header is text");
            assert_eq!(text_result.encoding, FileEncoding::Utf8);
            assert_eq!(text_result.text(), "%PDF-1.7\n42");
            assert_eq!(text_result.media_type, None);

            // The GIF signature is ASCII, so classification completes from the
            // first chunk even though a later chunk is invalid UTF-8.
            let gif = dir.path().join("late-binary.gif");
            let mut gif_bytes = b"GIF89a".to_vec();
            gif_bytes.extend(std::iter::repeat_n(b'x', 40));
            gif_bytes.push(0xff);
            gif_bytes.extend(std::iter::repeat_n(b'y', 10));
            std::fs::write(&gif, &gif_bytes).expect("write");
            let media_result = read_file_streaming(
                ReadFileArgs {
                    path: gif.display().to_string(),
                    mode: None,
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                },
                8,
            )
            .await
            .expect("media classification from the first chunk");
            assert_eq!(media_result.encoding, FileEncoding::Base64);
            assert_eq!(
                media_result.media_type.as_deref(),
                Some(MEDIA_TYPE_IMAGE_GIF)
            );
            assert_eq!(media_result.text(), STANDARD_BASE64.encode(&gif_bytes));
        });
    }

    #[test]
    fn read_autodetect_streams_only_the_selected_window() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("a.txt");
            std::fs::write(&path, "one\ntwo\nthree\nfour").expect("write");
            let result = read_file_streaming(
                ReadFileArgs {
                    path: path.display().to_string(),
                    mode: None,
                    encoding: FileEncoding::Utf8,
                    offset: Some(2),
                    limit: Some(1),
                },
                3,
            )
            .await
            .expect("read");
            assert_eq!(result.text(), "three");
            assert_eq!(result.starting_line, Some(2));
        });
    }

    // ── Output-cap tests ──────────────────────────────────────────────────────

    /// Fixture just over [`READ_OUTPUT_LIMIT_BYTES`].
    fn over_cap_text_bytes() -> Vec<u8> {
        let mut bytes = vec![b'a'; READ_OUTPUT_LIMIT_BYTES + 1024];
        bytes[READ_OUTPUT_LIMIT_BYTES / 2] = b'\n';
        bytes
    }

    /// Assert the shared truncation contract: content cut at the cap, the
    /// model-facing `truncated: true` metadata, the schema property kept only
    /// when present, and the `Partial result` section label.
    fn assert_truncated_read(result: &ReadResult, expected_bytes: usize) {
        assert!(result.truncated, "result must be flagged truncated");
        assert_eq!(result.read_bytes, expected_bytes);
        assert!(
            result.read_bytes <= READ_OUTPUT_LIMIT_BYTES,
            "raw selected bytes must not exceed the cap"
        );
        let metadata = result.metadata().expect("metadata");
        assert_eq!(
            metadata["truncated"],
            serde_json::json!(true),
            "metadata must report truncation: {metadata}"
        );
        let schema = serde_json::to_value(result.metadata_schema().expect("metadata schema"))
            .expect("schema serializes");
        assert!(
            schema["properties"].get("truncated").is_some(),
            "truncated must stay in the schema for a truncated result: {schema}"
        );
        // Base64 results without a UTF-8 projection carry no section at all.
        if let Some(Section::Content(section)) = result.content_sections().first() {
            assert_eq!(
                section.label.as_deref(),
                Some("Partial result"),
                "truncated content must be labeled Partial result"
            );
        }
    }

    #[test]
    fn read_caps_text_results_at_the_output_limit() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("large.txt");
            let bytes = over_cap_text_bytes();
            std::fs::write(&path, &bytes).expect("write");
            let expected_bytes = READ_OUTPUT_LIMIT_BYTES;

            let cases = [
                ReadFileArgs {
                    path: path.display().to_string(),
                    mode: Some(ReadMode::Lines),
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                },
                ReadFileArgs {
                    path: path.display().to_string(),
                    mode: Some(ReadMode::Chars),
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                },
                ReadFileArgs {
                    path: path.display().to_string(),
                    mode: None,
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                },
                ReadFileArgs {
                    path: path.display().to_string(),
                    mode: Some(ReadMode::Bytes),
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                },
            ];
            for arguments in cases {
                let result = read_file(arguments).await.expect("read");
                assert_eq!(
                    result.content_sections().len(),
                    1,
                    "capped text results keep one content section"
                );
                assert_truncated_read(&result, expected_bytes);
            }
        });
    }

    #[test]
    fn read_output_cap_beats_an_oversized_explicit_limit() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("large.txt");
            std::fs::write(&path, vec![b'b'; READ_OUTPUT_LIMIT_BYTES + 4096]).expect("write");
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: Some(READ_OUTPUT_LIMIT_BYTES + 4096),
            })
            .await
            .expect("read");
            assert_truncated_read(&result, READ_OUTPUT_LIMIT_BYTES);

            let lines_result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Lines),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: Some(usize::MAX),
            })
            .await
            .expect("read");
            assert_truncated_read(&lines_result, READ_OUTPUT_LIMIT_BYTES);
        });
    }

    #[test]
    fn read_base64_text_is_truncated_at_the_output_limit() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("large.txt");
            let bytes = over_cap_text_bytes();
            std::fs::write(&path, &bytes).expect("write");
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Base64,
                offset: None,
                limit: None,
            })
            .await
            .expect("read");

            assert_truncated_read(&result, READ_OUTPUT_LIMIT_BYTES);
            assert_eq!(
                result.text(),
                STANDARD_BASE64.encode(&bytes[..READ_OUTPUT_LIMIT_BYTES])
            );
            assert_eq!(result.media_type, None, "truncated is never complete");
            assert!(result.attachments().is_empty());
        });
    }

    #[test]
    fn read_recognized_media_just_over_the_output_limit_stays_complete() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let mut png = vec![137u8, 80, 78, 71, 13, 10, 26, 10];
            png.extend(std::iter::repeat_n(
                0u8,
                READ_OUTPUT_LIMIT_BYTES + 1024 - png.len(),
            ));
            let path = dir.path().join("large.png");
            std::fs::write(&path, &png).expect("write");

            let explicit = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Base64,
                offset: None,
                limit: None,
            })
            .await
            .expect("complete media read");
            assert!(!explicit.truncated, "media reads bypass the cap");
            assert_eq!(explicit.read_bytes, png.len());
            assert_eq!(
                explicit.media_type.as_deref(),
                Some(MEDIA_TYPE_IMAGE_PNG),
                "complete media stays attached past the cap"
            );
            assert_eq!(explicit.attachments().len(), 1);
            let metadata = explicit.metadata().expect("metadata");
            assert!(
                metadata.get("truncated").is_none(),
                "untruncated metadata must omit truncated: {metadata}"
            );

            let autodetected = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: None,
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            })
            .await
            .expect("autodetected media read");
            assert!(!autodetected.truncated);
            assert_eq!(
                autodetected.media_type.as_deref(),
                Some(MEDIA_TYPE_IMAGE_PNG)
            );
            assert_eq!(autodetected.read_bytes, png.len());
        });
    }

    #[test]
    fn read_complete_media_over_the_size_ceiling_is_rejected() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let mut bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10];
            bytes.resize(READ_MEDIA_MAX_BYTES + 1, 0);
            let path = dir.path().join("huge.png");
            std::fs::write(&path, &bytes).expect("write");
            let path_string = path.display().to_string();

            let cases = [
                ReadFileArgs {
                    path: path_string.clone(),
                    mode: Some(ReadMode::Bytes),
                    encoding: FileEncoding::Base64,
                    offset: None,
                    limit: None,
                },
                ReadFileArgs {
                    path: path_string.clone(),
                    mode: None,
                    encoding: FileEncoding::Utf8,
                    offset: None,
                    limit: None,
                },
            ];
            for arguments in cases {
                let error = read_file(arguments)
                    .await
                    .expect_err("an oversized complete media read must fail");
                let message = error.to_string();
                assert!(
                    message.contains(&format!("over the {}-byte limit", READ_MEDIA_MAX_BYTES)),
                    "error must state the ceiling: {message}"
                );
                assert!(
                    message.contains(&path_string),
                    "error must name the path: {message}"
                );
                assert!(
                    message.contains(r#"mode: "bytes""#),
                    "error must carry the range-read remedy: {message}"
                );
            }
        });
    }

    #[test]
    fn read_untruncated_result_omits_truncated_metadata_and_section_label() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("small.txt");
            std::fs::write(&path, "one\ntwo").expect("write");
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Lines),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            })
            .await
            .expect("read");

            assert!(!result.truncated);
            let metadata = result.metadata().expect("metadata");
            assert!(
                metadata.get("truncated").is_none(),
                "untruncated metadata must omit truncated: {metadata}"
            );
            let schema = serde_json::to_value(result.metadata_schema().expect("metadata schema"))
                .expect("schema serializes");
            assert!(
                schema["properties"].get("truncated").is_none(),
                "truncated must be pruned from the schema when absent: {schema}"
            );
            let sections = result.content_sections();
            let Section::Content(section) = &sections[0] else {
                panic!("expected a text content section");
            };
            assert_eq!(section.label, None, "uncapped reads keep no label");
        });
    }
}
