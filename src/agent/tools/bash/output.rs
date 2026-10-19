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

//! Bounded, arrival-ordered capture of merged stdout and stderr.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, BufReader};

use super::{BASH_OUTPUT_LIMIT_BYTES, BASH_OUTPUT_LIMIT_LINES};
use crate::bounded_buffer::BoundedBuffer;

/// Accumulates up to [`BASH_OUTPUT_LIMIT_LINES`] whole lines of text, capped
/// at [`BASH_OUTPUT_LIMIT_BYTES`] total bytes, whichever limit is hit first.
/// Lines beyond a cap are discarded. A line that alone exceeds the remaining
/// byte budget is kept as a UTF-8-safe prefix up to the byte cap rather than
/// discarded wholesale (so a single oversized first line still yields
/// non-empty output).
pub(super) struct OutputCollector {
    buffer: BoundedBuffer,
    line_count: usize,
    line_limit_hit: bool,
}

impl OutputCollector {
    pub(super) fn new() -> Self {
        OutputCollector {
            buffer: BoundedBuffer::new(BASH_OUTPUT_LIMIT_BYTES),
            line_count: 0,
            line_limit_hit: false,
        }
    }

    fn truncated(&self) -> bool {
        self.line_limit_hit || self.buffer.overflowed()
    }

    fn push_line(&mut self, line: &str) {
        if self.truncated() {
            return;
        }
        if self.line_count >= BASH_OUTPUT_LIMIT_LINES {
            self.line_limit_hit = true;
            return;
        }
        if self.buffer.push(line.as_bytes()) {
            self.line_count += 1;
        } else {
            self.buffer.trim_incomplete_utf8_suffix();
        }
    }
}

pub(super) fn take_collected_output(collector: &Mutex<OutputCollector>) -> (String, bool) {
    let mut collector = collector
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let truncated = collector.truncated();
    let output = String::from_utf8_lossy(&collector.buffer.take_bytes()).into_owned();
    (output, truncated)
}

/// Longest line kept in memory: one byte beyond the output cap, so an
/// oversized line is still recognized as exceeding any remaining budget.
const MAX_BUFFERED_LINE_BYTES: usize = BASH_OUTPUT_LIMIT_BYTES + 1;

pub(super) fn spawn_line_reader<Stream>(
    stream: Stream,
    collector: Arc<Mutex<OutputCollector>>,
) -> tokio::task::JoinHandle<()>
where
    Stream: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        while let Ok(true) =
            read_bounded_line(&mut reader, &mut line, MAX_BUFFERED_LINE_BYTES).await
        {
            let text = String::from_utf8_lossy(&line);
            collector
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push_line(&text);
        }
    })
}

/// Read one line (including its newline) into `line`, keeping at most
/// `max_bytes` of it and discarding the rest of an oversized line. Returns
/// `false` at end of stream when nothing was read.
pub(super) async fn read_bounded_line<Reader>(
    reader: &mut Reader,
    line: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<bool>
where
    Reader: AsyncBufRead + Unpin,
{
    line.clear();
    let mut read_any = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(read_any);
        }
        read_any = true;
        let newline_position = available.iter().position(|&byte| byte == b'\n');
        let consumed = newline_position.map_or(available.len(), |position| position + 1);
        let room = max_bytes.saturating_sub(line.len());
        line.extend_from_slice(&available[..consumed.min(room)]);
        reader.consume(consumed);
        if newline_position.is_some() {
            return Ok(true);
        }
    }
}
