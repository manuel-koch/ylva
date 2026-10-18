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

//! A byte accumulator that never holds more than a fixed limit.
//!
//! Shared by every place that collects data from a source the process does not
//! control (subprocess output, HTTP bodies) so the cap is enforced *before*
//! bytes are stored, not after the whole input has been buffered.

#[derive(Debug, Clone)]
pub struct BoundedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    overflowed: bool,
}

impl BoundedBuffer {
    pub fn new(limit: usize) -> Self {
        BoundedBuffer {
            bytes: Vec::new(),
            limit,
            overflowed: false,
        }
    }

    /// Append as much of `chunk` as fits. Returns `false` (and records the
    /// overflow) when part of it had to be discarded.
    pub fn push(&mut self, chunk: &[u8]) -> bool {
        let room = self.limit - self.bytes.len();
        if chunk.len() > room {
            self.bytes.extend_from_slice(&chunk[..room]);
            self.overflowed = true;
            return false;
        }
        self.bytes.extend_from_slice(chunk);
        true
    }

    /// Whether any pushed byte was discarded.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Drop a trailing incomplete UTF-8 sequence, such as one left by cutting
    /// valid text at the limit. Other invalid bytes are kept.
    pub fn trim_incomplete_utf8_suffix(&mut self) {
        if let Err(utf8_error) = std::str::from_utf8(&self.bytes) {
            if utf8_error.error_len().is_none() {
                self.bytes.truncate(utf8_error.valid_up_to());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_everything_up_to_the_limit() {
        let mut buffer = BoundedBuffer::new(6);
        assert!(buffer.push(b"abc"));
        assert!(buffer.push(b"def"));
        assert!(!buffer.overflowed());
        assert_eq!(buffer.as_slice(), b"abcdef");
    }

    #[test]
    fn never_stores_more_than_the_limit_and_records_overflow() {
        let mut buffer = BoundedBuffer::new(4);
        assert!(!buffer.push(&[b'x'; 1_000_000]));
        assert!(buffer.overflowed());
        assert_eq!(buffer.len(), 4);
        assert!(!buffer.push(b"more"));
        assert_eq!(buffer.len(), 4);
    }

    #[test]
    fn trims_only_an_incomplete_trailing_character() {
        let mut buffer = BoundedBuffer::new(4);
        buffer.push("abé".as_bytes());
        assert_eq!(buffer.len(), 4);
        buffer.trim_incomplete_utf8_suffix();
        assert_eq!(buffer.as_slice(), "abé".as_bytes());

        let mut cut = BoundedBuffer::new(3);
        cut.push("abé".as_bytes());
        cut.trim_incomplete_utf8_suffix();
        assert_eq!(cut.as_slice(), b"ab");

        let mut invalid = BoundedBuffer::new(8);
        invalid.push(&[b'a', 0xff, b'b']);
        invalid.trim_incomplete_utf8_suffix();
        assert_eq!(invalid.as_slice(), &[b'a', 0xff, b'b']);
    }
}
