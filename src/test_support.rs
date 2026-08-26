// Copyright © 2026 Manuel Koch
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// <http://www.apache.org/licenses/LICENSE-2.0>
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

/// Mutex shared between `capture_logs`, `run_locked`, and any unit test that
/// installs a tracing subscriber (via `with_default` or `init_logging`).
///
/// ## Why every `with_default` test must hold this lock
///
/// `tracing::subscriber::with_default(sub, ...)` calls `Dispatch::new(sub)`
/// internally, which calls `callsite::register_dispatch`. That function adds
/// the new dispatcher to a process-global `LOCKED_DISPATCHERS` list and then
/// rebuilds interest for **every registered callsite** by ANDing the results
/// from all live dispatchers together (`Interest::and`).
///
/// If two tests call `with_default` concurrently:
///
/// - Both dispatchers are alive simultaneously in `LOCKED_DISPATCHERS`.
/// - Any subsequent `register_dispatch` call (from a third concurrent
///   `with_default`) iterates both dispatchers.
/// - An INFO-only dispatcher returns `Interest::never()` for DEBUG callsites.
///   ANDing that with the DEBUG subscriber's `Interest::always()` yields
///   `Interest::never()` for those callsites — silently dropping DEBUG events
///   in `capture_logs` for the rest of the test run.
///
/// Holding `SUBSCRIBER_LOCK` for the entire lifetime of the `with_default`
/// closure (and the surrounding `Dispatch` object) prevents concurrent
/// registration and keeps callsite interests stable while `capture_logs`
/// bodies are executing.
///
/// ## Why `init_logging` tests must hold this lock
///
/// `init_logging` calls `try_init()` → `set_global_default` →
/// `rebuild_interest_cache`, which also resets DEBUG callsite interest to
/// `Interest::never()` when the global subscriber is INFO-only. Holding the
/// lock prevents that from racing with `capture_logs`'s own
/// `rebuild_interest_cache` call and the subsequent event dispatch.
///
/// ## Why tests calling tracing-instrumented functions must use `run_locked`
///
/// `capture_logs` calls `rebuild_interest_cache()` before running the test
/// body, setting every *already-registered* callsite to "always". However, a
/// callsite is only registered on its **first** access. If a concurrent test
/// (not holding `SUBSCRIBER_LOCK`) first accesses a callsite that `capture_logs`
/// later depends on, the registration uses `Dispatch::none()` (since that
/// thread has no subscriber) and caches `Interest::never()`. When
/// `capture_logs`'s body then hits that callsite, the cached "never" is
/// returned and the event is dropped — without `rebuild_interest_cache()` ever
/// seeing it (it wasn't yet registered when that was called).
///
/// The fix: any test that calls a function exercised by `capture_logs` tests
/// (e.g. `read_file`, `write_file`, `glob_files`) must run under this lock via
/// `run_locked`, so that it cannot run concurrently with `capture_logs`. This
/// ensures callsites are always first-registered from within a `capture_logs`
/// context — where `get_default` returns the thread-local DEBUG subscriber —
/// yielding `Interest::always()` for those callsites.
pub(crate) static SUBSCRIBER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Drive an async body while capturing its `debug!`/`error!` events into a
/// shared in-memory buffer, returning the captured text.
///
/// `SUBSCRIBER_LOCK` is held for the entire call so that no other test can
/// concurrently call `with_default` (which registers a dispatcher and may
/// rebuild callsite interests via `Interest::and`, silently dropping DEBUG
/// events) or `init_logging` (which calls `set_global_default` →
/// `rebuild_interest_cache`, resetting callsite interest for INFO-only
/// subscribers). See `SUBSCRIBER_LOCK`'s doc comment for the full
/// explanation.
///
/// `tracing::callsite::rebuild_interest_cache()` is called after installing
/// the subscriber. Without it, callsites that were first invoked before any
/// subscriber was active have their interest cached as `Interest::never()`,
/// causing events to be silently dropped even though the thread-local
/// subscriber installed by `with_default` would want to see them.
/// Runs an async body under `SUBSCRIBER_LOCK` on a current-thread runtime.
///
/// Use this in tests that call tracing-instrumented functions (such as
/// `read_file`, `write_file`, `glob_files`) but do **not** need to capture
/// log output. Holding `SUBSCRIBER_LOCK` prevents concurrent callsite
/// registration with `Dispatch::none()` (which sets `Interest::never()`)
/// from corrupting the per-callsite interest cache that `capture_logs` relies
/// on. See `SUBSCRIBER_LOCK`'s doc comment for the full explanation.
pub(crate) fn run_locked(body: impl std::future::Future<Output = ()>) {
    let _lock = SUBSCRIBER_LOCK.lock().expect("subscriber mutex poisoned");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime")
        .block_on(body);
}

pub(crate) fn capture_logs(body: impl std::future::Future<Output = ()>) -> String {
    let _lock = SUBSCRIBER_LOCK.lock().expect("subscriber mutex poisoned");

    let log = SharedLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        // Re-evaluate all callsite interests against this subscriber so
        // callsites first seen when no subscriber was active are not stuck
        // with Interest::never().
        tracing::callsite::rebuild_interest_cache();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime")
            .block_on(body);
    });
    log.read()
}
