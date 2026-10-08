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

//! Media recognition, Base64 results and safe attachment filenames.

use super::super::{utf8_text_preview, STANDARD_BASE64};
use super::{FileEncoding, ReadResult, ToolError, READ_MEDIA_MAX_BYTES, SUPPORTED_MEDIA_TYPES};
use base64::Engine as _;

/// Build the tool error for a complete recognized media file that exceeds
/// [`READ_MEDIA_MAX_BYTES`]. Truncation is not an option: a partial file is
/// never attachable, so the read fails with the size, the limit, and the
/// range-read remedy.
pub(super) fn read_media_too_large_error(
    path: &str,
    media_type: &str,
    file_length: usize,
) -> ToolError {
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

/// Build the result of a Base64 read of the already-selected `bytes`.
///
/// The media type is recognized only when `is_complete_file` says the
/// selection covers the complete source file — a truncated result is never a
/// complete file, so the cap and the attachment rules compose.
pub(super) fn base64_read_result(
    selected: &[u8],
    is_complete_file: bool,
    path: String,
    truncated: bool,
    source_size_bytes: usize,
    source_lines: Option<usize>,
) -> ReadResult {
    let read_bytes = selected.len();
    let content = STANDARD_BASE64.encode(selected);
    let text_preview = utf8_text_preview(selected);
    let media_type = is_complete_file
        .then(|| supported_media_type(selected))
        .flatten();
    ReadResult {
        read_bytes,
        source_size_bytes,
        source_lines,
        content,
        path,
        starting_line: text_preview.as_ref().map(|_| 0),
        encoding: FileEncoding::Base64,
        text_preview,
        media_type,
        truncated,
    }
}

pub(crate) fn supported_media_type(bytes: &[u8]) -> Option<String> {
    let mime_type = infer::get(bytes)?.mime_type();
    SUPPORTED_MEDIA_TYPES
        .contains(&mime_type)
        .then(|| mime_type.to_string())
}

pub(crate) fn safe_source_filename(path: &str, fallback_filename: &str) -> String {
    let basename = std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..");
    basename
        .map(str::to_string)
        .unwrap_or_else(|| fallback_filename.to_string())
}
