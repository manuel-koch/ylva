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

//! Incremental UTF-8 validation and bounded file I/O for `read`.

use super::ToolError;
use tokio::io::AsyncReadExt;

/// Build the tool error for a failed file I/O operation, naming the path and
/// the underlying OS error.
pub(super) fn read_io_error(path: &str, error: impl std::fmt::Display) -> ToolError {
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

/// The first byte of a streamed file that is not valid UTF-8, located by
/// absolute offset within the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Utf8StreamError {
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
pub(super) enum StreamError {
    /// I/O failure while reading the file.
    Io(std::io::Error),
    /// The stream is not valid UTF-8 at the reported absolute offset.
    Utf8(Utf8StreamError),
}

pub(super) fn stream_error_to_tool_error(path: &str, mode: &str, error: StreamError) -> ToolError {
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
pub(super) async fn stream_validated_utf8<R, O, C>(
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

/// Chunk size used to stream a file for `lines`/`chars`/autodetect reads.
/// Parameterized so chunk-boundary tests stay deterministic.
pub(super) const STREAM_CHUNK_BYTES: usize = 8 * 1024;

/// Read size used while materializing a selected byte range.
const RANGE_READ_CHUNK_BYTES: usize = 64 * 1024;

/// Read at most `max_bytes` from the current position of `file`.
pub(super) async fn read_up_to(
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

#[cfg(test)]
mod tests {
    use super::*;

    struct ShortReader<'a> {
        remaining: &'a [u8],
        fail_at_eof: bool,
    }

    impl tokio::io::AsyncRead for ShortReader<'_> {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.remaining.is_empty() && self.fail_at_eof {
                return std::task::Poll::Ready(Err(std::io::Error::other("read failure")));
            }
            if buffer.remaining() > 0 && !self.remaining.is_empty() {
                buffer.put_slice(&self.remaining[..1]);
                self.remaining = &self.remaining[1..];
            }
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn short_reads_fill_chunks_and_reassemble_utf8() {
        let text = "a\u{e9}\u{1f600}z";
        for chunk_size in 1..=text.len() + 1 {
            let mut reader = ShortReader {
                remaining: text.as_bytes(),
                fail_at_eof: false,
            };
            let mut observed = Vec::new();
            let mut decoded = String::new();
            let outcome = stream_validated_utf8(
                &mut reader,
                chunk_size,
                |chunk| observed.push(chunk.to_vec()),
                |validated| decoded.push_str(validated),
            )
            .await;
            assert!(outcome.is_ok());
            assert_eq!(decoded, text);
            assert_eq!(observed.concat(), text.as_bytes());
            assert!(observed[..observed.len() - 1]
                .iter()
                .all(|chunk| chunk.len() == chunk_size));
        }
    }

    #[tokio::test]
    async fn validation_reports_trailing_utf8_and_io_errors_after_consumed_chunks() {
        let mut reader = ShortReader {
            remaining: &[b'a', b'\n', 0xc3],
            fail_at_eof: false,
        };
        let mut decoded = String::new();
        let outcome =
            stream_validated_utf8(&mut reader, 1, |_| {}, |text| decoded.push_str(text)).await;
        assert_eq!(decoded, "a\n");
        assert!(matches!(
            outcome,
            Err(StreamError::Utf8(Utf8StreamError::Incomplete { offset: 2 }))
        ));

        let mut reader = ShortReader {
            remaining: b"abcd",
            fail_at_eof: true,
        };
        let outcome = stream_validated_utf8(&mut reader, 2, |_| {}, |_| {}).await;
        match outcome {
            Err(StreamError::Io(error)) => assert_eq!(error.to_string(), "read failure"),
            _ => panic!("stream must preserve the I/O error"),
        }
    }

    #[test]
    fn bounded_reads_leave_unselected_bytes_in_the_file() {
        crate::test_support::run_locked(async {
            let directory = tempfile::tempdir().expect("tempdir");
            let path = directory.path().join("range.txt");
            std::fs::write(&path, b"abcdef").expect("write");
            let mut file = tokio::fs::File::open(&path).await.expect("open");
            let path = path.display().to_string();
            assert!(read_up_to(&mut file, 0, &path)
                .await
                .expect("read")
                .is_empty());
            assert_eq!(read_up_to(&mut file, 2, &path).await.expect("read"), b"ab");
            assert_eq!(read_up_to(&mut file, 3, &path).await.expect("read"), b"cde");
            assert_eq!(read_up_to(&mut file, 10, &path).await.expect("read"), b"f");
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
}
