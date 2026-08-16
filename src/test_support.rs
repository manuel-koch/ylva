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

//! Shared unit-test helpers.
//!
//! This module is compiled only into unit-test builds (it is wired from
//! `src/lib.rs` behind `#[cfg(test)]`) and hosts helpers used by more than one
//! `#[cfg(test)]` module, e.g. the `tracing` capture helpers that
//! `src/agent.rs` and `src/agent/tools/fs.rs` use to assert on log events.
//! SSOT: shared helpers live here instead of being copied per module.

use std::sync::Arc;

/// A `tracing` log target that appends to an in-memory buffer
/// synchronously in the calling thread.
///
/// A file-backed `tracing_appender::non_blocking` writer would race the
/// assertions it serves: its worker thread may not have drained the last events
/// by the time the test reads the file, and the guard's drop only waits up to
/// 100 ms for the worker — under load the tail can be lost. This shared buffer
/// has no worker thread, so every event is written before the emitting call
/// returns and the captured text is deterministic.
#[derive(Clone, Default)]
pub(crate) struct SharedLog {
    buffer: Arc<std::sync::Mutex<String>>,
}

impl SharedLog {
    pub(crate) fn read(&self) -> String {
        self.buffer.lock().expect("log lock poisoned").clone()
    }
}

impl std::io::Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = std::str::from_utf8(buf).expect("log output is utf8");
        self.buffer
            .lock()
            .map_err(|_| std::io::Error::other("log lock poisoned"))?
            .push_str(text);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedLog {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Drive an async body while capturing its `debug!`/`error!` events into a
/// shared in-memory buffer, returning the captured text.
pub(crate) fn capture_logs(body: impl std::future::Future<Output = ()>) -> String {
    let log = SharedLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime")
            .block_on(body);
    });
    log.read()
}
