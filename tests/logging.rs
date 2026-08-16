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

//! Integration tests for the logging infrastructure.
//!
//! `tracing`'s global subscriber can only be installed once per process, so
//! these tests do **not** use the global-default path (except the one that
//! exercises the real `init_logging`). Instead each test installs an isolated
//! per-thread subscriber via `tracing::subscriber::with_default`, which is
//! deterministic and parallel-safe. The subscriber is built with the same
//! configuration `init_logging` composes (truncate-mode file writer,
//! non-blocking appender, level filter, target/line fields), so the written
//! output is verified end-to-end.

use std::fs::OpenOptions;
use std::path::Path;

use tempfile::TempDir;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;
use ylva::{init_logging, LogConfig, LogFormat};

/// Open `path` in truncate/overwrite mode, mirroring `init_logging`.
fn open_truncate(path: &Path) -> std::fs::File {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .expect("open log file")
}

/// Build an isolated subscriber (and its non-blocking guard) writing to
/// `path` in `format`, with the same layer configuration `init_logging` uses.
fn subscriber(
    path: &Path,
    format: LogFormat,
    level: tracing::Level,
) -> (impl tracing::Subscriber + Send + Sync, WorkerGuard) {
    let (writer, guard) = tracing_appender::non_blocking(open_truncate(path));
    let filter = EnvFilter::new(level.as_str());
    let base = Layer::new()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .with_line_number(true);
    let layer = match format {
        LogFormat::Json => base.json().with_filter(filter).boxed(),
        LogFormat::Text => base.with_filter(filter).boxed(),
    };
    (tracing_subscriber::registry().with(layer), guard)
}

#[test]
fn text_format_writes_message_line() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("ylva.log");
    let (subscriber, guard) = subscriber(&path, LogFormat::Text, tracing::Level::INFO);
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("hello from text");
    });
    drop(guard);
    let contents = std::fs::read_to_string(&path).expect("read log");
    assert!(contents.contains("hello from text"), "log was: {contents}");
}

#[test]
fn json_format_writes_valid_json_object_per_line() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("ylva.jsonl");
    let (subscriber, guard) = subscriber(&path, LogFormat::Json, tracing::Level::INFO);
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("hello from json");
    });
    drop(guard);
    let contents = std::fs::read_to_string(&path).expect("read log");
    let line = contents.lines().next().expect("at least one line");
    let value: serde_json::Value = serde_json::from_str(line).expect("valid JSON object per line");
    assert_eq!(value["fields"]["message"], "hello from json");
}

#[test]
fn subsequent_run_truncates_instead_of_appending() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("ylva.log");
    {
        let (subscriber, guard) = subscriber(&path, LogFormat::Text, tracing::Level::INFO);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("first run");
        });
        drop(guard);
    }
    {
        let (subscriber, guard) = subscriber(&path, LogFormat::Text, tracing::Level::INFO);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("second run");
        });
        drop(guard);
    }
    let contents = std::fs::read_to_string(&path).expect("read log");
    assert!(contents.contains("second run"), "log was: {contents}");
    assert!(
        !contents.contains("first run"),
        "file must be truncated, not appended: {contents}"
    );
}

#[test]
fn api_key_is_redacted_to_presence_indicator() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("ylva.log");
    let (subscriber, guard) = subscriber(&path, LogFormat::Text, tracing::Level::DEBUG);
    tracing::subscriber::with_default(subscriber, || {
        tracing::debug!(api_key = "set", "provider selected");
    });
    drop(guard);
    let contents = std::fs::read_to_string(&path).expect("read log");
    assert!(contents.contains("api_key=\"set\""), "log was: {contents}");
    assert!(
        !contents.contains("sk-RAW-SECRET-123"),
        "raw key value must never appear in the log: {contents}"
    );
}

/// `init_logging` installs the process-global `tracing` subscriber, which can
/// only happen once per test process. This test therefore exercises the real
/// entry point end-to-end: it must be the only test in this crate that calls
/// `init_logging`, so it can rely on the global default. The other tests keep
/// to `tracing::subscriber::with_default` for their isolated subscribers.
#[test]
fn init_logging_writes_selected_format_to_log_file() {
    let dir = TempDir::new().expect("tempdir");
    let text_path = dir.path().join("ylva.log");
    let guard = init_logging(LogConfig {
        log_file: Some(text_path.clone()),
        log_level: tracing::Level::DEBUG,
        log_format: LogFormat::Text,
    })
    .expect("log file opens");
    tracing::debug!(provider = "mock", "text format to log file");
    drop(guard);

    let text = std::fs::read_to_string(&text_path).expect("read text log");
    assert!(
        text.contains("text format to log file"),
        "text log was: {text}"
    );
    assert!(text.contains("provider=\"mock\""), "text log was: {text}");
}
