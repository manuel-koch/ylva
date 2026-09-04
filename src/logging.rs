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

//! Logging initialisation for the binary.
//!
//! Library code (`src/**`) emits structured, severity-tagged
//! events via the `tracing` facade only; it never depends on a subscriber.
//! This module composes and installs the subscriber — a single file writer in
//! the format the caller selected via `--log-format`, and a level filter — so
//! the binary owns all logging policy. When logging is enabled, the file the
//! caller selected via `--log-file` receives exactly the configured format.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use tracing::Event;
use tracing::Subscriber;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_log::NormalizeEvent;
use tracing_subscriber::fmt::format::{DefaultFields, FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt::{time, FmtContext, FormattedFields, Layer};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer as _};

/// Output format for log records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// Human-readable text lines.
    #[default]
    Text,
    /// JSONL (one JSON object per line).
    Json,
}

/// Keeps the non-blocking file writer alive so buffered events flush on drop.
///
/// The guard is intentionally never read: holding it alive until process exit
/// **is** the whole contract, since it flushes its writer when dropped. The
/// `allow` reflects that the field exists for its `Drop` side effect only,
/// mirroring `tracing_appender`'s own `WorkerGuard` (which keeps its handle in
/// an underscore-prefixed field for the same reason).
#[must_use]
#[derive(Debug)]
pub struct LogGuards {
    #[expect(dead_code)]
    worker: WorkerGuard,
}

/// Caller-supplied logging configuration.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Path to the log file; `None` disables logging entirely.
    pub log_file: Option<PathBuf>,
    /// Minimum severity to emit (includes all higher-severity levels).
    pub log_level: tracing::Level,
    /// Output format.
    pub log_format: LogFormat,
}

impl Default for LogConfig {
    fn default() -> Self {
        LogConfig {
            log_file: None,
            log_level: tracing::Level::INFO,
            log_format: LogFormat::Text,
        }
    }
}

/// Initialise the global `tracing` subscriber to write `config.log_file` in
/// the format the caller selected via `config.log_format`.
///
/// Returns `Ok(None)` (and installs nothing) when `config.log_file` is `None`,
/// so logging is entirely opt-in. Otherwise verifies the containing directory
/// exists, opens the file in truncate/overwrite mode, wraps it with a
/// non-blocking writer, and installs a single `fmt` layer — `text` or `json`
/// per `config.log_format` — with the configured level filter and
/// target/line-number fields. Text output is written plain, without ANSI color
/// escape sequences, since it targets a file rather than a terminal. The source
/// file path is intentionally omitted to keep lines readable. A failure to set
/// up the file is reported as an `Err` naming the path that could not be used,
/// rather than a panic. The returned [`LogGuards`] must be kept alive until
/// process exit or buffered events may be lost.
pub fn init_logging(config: LogConfig) -> Result<Option<LogGuards>, String> {
    let Some(ref path) = config.log_file else {
        return Ok(None);
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(format!(
                "cannot open log file '{}': directory '{}' does not exist",
                path.display(),
                parent.display()
            ));
        }
    }
    let file = open_truncate(path)
        .map_err(|error| format!("cannot open log file '{}': {error}", path.display()))?;
    let (subscriber, guards) = build_subscriber(file, config.log_format, config.log_level);
    // `try_init` tolerates a second install (e.g. under parallel tests, where
    // the process-global subscriber can only be set once); the first call
    // still installs the subscriber and every call returns a live guard.
    let _ = subscriber.try_init();
    Ok(Some(guards))
}

/// Open `path` in truncate/overwrite mode.
fn open_truncate(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// Build an isolated `tracing` subscriber writing `file` in the given `format`,
/// using `log_level` for the level filter. Text output is written plain (no
/// ANSI color escape sequences) since it targets a file rather than a
/// terminal, and the source file is condensed to its basename (e.g.
/// `http.rs:767`) so lines stay readable even for dependency crates whose
/// absolute build paths would otherwise dominate the line. The returned
/// [`LogGuards`] must be kept alive until buffered events are flushed.
fn build_subscriber(
    file: File,
    format: LogFormat,
    log_level: tracing::Level,
) -> (impl tracing::Subscriber + Send + Sync, LogGuards) {
    let filter = EnvFilter::new(log_level.as_str());
    let (writer, guard) = tracing_appender::non_blocking(file);
    let layer = match format {
        LogFormat::Text => Layer::new()
            .with_writer(writer)
            .with_ansi(false)
            .fmt_fields(DefaultFields::new())
            .event_format(CondensedText)
            .with_filter(filter)
            .boxed(),
        LogFormat::Json => Layer::new()
            .with_writer(writer)
            .with_ansi(false)
            .with_target(true)
            .with_line_number(true)
            .json()
            // Only the root-to-leaf `spans` array is emitted. The redundant
            // innermost `span` field (same data as the array's last element)
            // adds no information, so it is disabled to keep each JSONL line
            // free of duplicated span data. The full per-line `spans` chain is
            // what allows correlating log lines within one span scope.
            // WIP: a session/trace id is a future concern (see docs/blueprint.md).
            .with_current_span(false)
            .with_filter(filter)
            .boxed(),
    };
    let subscriber = tracing_subscriber::registry().with(layer);
    (subscriber, LogGuards { worker: guard })
}

/// A text event formatter that keeps lines short by condensing the source
/// location to the file basename (e.g. `http.rs:767`) instead of the absolute
/// build path, and emits no ANSI escape sequences. The currently-entered span
/// scope is rendered inline (`tool_execute:{tool="read" tool_call_id="call_1"}`)
/// so structured context such as the running tool and its id is visible in
/// every text log line.
struct CondensedText;

impl<S, N> FormatEvent<S, N> for CondensedText
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        // Events bridged from the `log` crate (e.g. reqwest/hyper) carry a
        // stub metadata (`target: "log"`, no file/line) and store the real
        // target/module/file/line in `log.*` fields. Normalizing them back
        // reproduces the original source location, matching what the JSONL
        // formatter already does and keeping the text line's `[[target ->
        // file.rs:line]]` location truthful for dependency crates as well.
        let normalized_meta = event.normalized_metadata();
        let meta = normalized_meta.as_ref().unwrap_or_else(|| event.metadata());

        // Timestamp.
        let mut timestamp = String::new();
        time::SystemTime.format_time(&mut Writer::new(&mut timestamp))?;

        // Event fields (message + structured fields).
        let mut fields = String::new();
        ctx.field_format()
            .format_fields(Writer::new(&mut fields), event)?;

        // Span scope: one `name:{fields}` per entered span, root first.
        let mut spans: Vec<String> = Vec::new();
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                let mut part = format!("{}:", span.metadata().name());
                let ext = span.extensions();
                if let Some(span_fields) = ext.get::<FormattedFields<N>>() {
                    if !span_fields.is_empty() {
                        part.push_str(&format!(" {{{span_fields}}}"));
                    }
                }
                spans.push(part);
            }
        }

        // Source location: `[[target -> basename:line]]`
        let mut location = String::new();
        if !meta.target().is_empty() {
            location.push_str(meta.target());
        }
        if let Some(file) = meta.file() {
            let basename = Path::new(file)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.to_string());
            if !location.is_empty() && !basename.is_empty() && meta.line().is_some() {
                location.push_str(" -> ");
            }
            location.push_str(&basename);
            if let Some(line) = meta.line() {
                location.push_str(&format!(":{line}"));
            }
        }
        if !location.is_empty() {
            location = format!("[[{location}]]");
        }

        // Concatenate only the non-empty snippets, joined by a single space.
        let snippets: Vec<String> = [
            timestamp,
            meta.level().to_string(),
            fields.trim().to_string(),
            spans.join(" "),
            location,
        ]
        .into_iter()
        .filter(|snippet| !snippet.is_empty())
        .collect();
        writeln!(writer, "{}", snippets.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_config_defaults_to_info_and_text() {
        let config = LogConfig::default();
        assert_eq!(config.log_level, tracing::Level::INFO);
        assert_eq!(config.log_format, LogFormat::Text);
        assert_eq!(config.log_file, None);
    }

    #[test]
    fn init_logging_without_file_returns_none() {
        assert!(init_logging(LogConfig::default())
            .expect("no error")
            .is_none());
    }

    #[test]
    fn init_logging_with_text_creates_file_and_returns_guard() {
        // Holds SUBSCRIBER_LOCK: try_init() calls set_global_default which
        // triggers rebuild_interest_cache, racing with capture_logs tests.
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("ylva.log");
        let guard = init_logging(LogConfig {
            log_file: Some(path.clone()),
            log_level: tracing::Level::INFO,
            log_format: LogFormat::Text,
        })
        .expect("log file opens");
        assert!(guard.is_some());
        assert!(path.exists());
    }

    #[test]
    fn init_logging_writes_plain_text_without_ansi_escape_sequences() {
        // Holds SUBSCRIBER_LOCK: `with_default` calls `Dispatch::new` which
        // registers the dispatcher in the global `LOCKED_DISPATCHERS` list.
        // Concurrent `with_default` callers from other test threads can then
        // join that list and trigger `rebuild_callsite_interest` with
        // `Interest::and()` across all live dispatchers. An INFO-only
        // dispatcher ANDs to `Interest::never()` for DEBUG callsites, silently
        // dropping events that `capture_logs` tests rely on. Holding the lock
        // prevents concurrent registration and keeps callsite interests stable.
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("ylva.log");
        let file = open_truncate(&path).expect("open log file");
        let (subscriber, guard) = build_subscriber(file, LogFormat::Text, tracing::Level::INFO);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(provider = "mock", "plain line");
        });
        drop(guard);
        let contents = std::fs::read_to_string(&path).expect("read log");
        assert!(contents.contains("plain line"), "log was: {contents}");
        assert!(
            contents.contains("provider=\"mock\""),
            "field should be rendered plain: {contents}"
        );
        assert!(
            !contents.contains('\u{1b}'),
            "file log must not contain ANSI escape sequences: {contents:?}"
        );
    }

    #[test]
    fn text_format_condenses_source_to_basename_and_line() {
        // Holds SUBSCRIBER_LOCK: see
        // init_logging_writes_plain_text_without_ansi_escape_sequences.
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("ylva.log");
        let file = open_truncate(&path).expect("open log file");
        let (subscriber, guard) = build_subscriber(file, LogFormat::Text, tracing::Level::INFO);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "some_crate::module", "condensed source");
        });
        drop(guard);
        let contents = std::fs::read_to_string(&path).expect("read log");
        assert!(
            contents.contains("some_crate::module"),
            "log was: {contents}"
        );
        assert!(
            contents.contains("logging.rs"),
            "source should be the file basename, not the absolute path: {contents}"
        );
        assert!(
            !contents.contains("workspace"),
            "absolute source path must not appear: {contents}"
        );
    }

    #[test]
    fn init_logging_with_json_creates_file_and_returns_guard() {
        // Holds SUBSCRIBER_LOCK: see init_logging_with_text_creates_file_and_returns_guard.
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("ylva.jsonl");
        let guard = init_logging(LogConfig {
            log_file: Some(path.clone()),
            log_level: tracing::Level::DEBUG,
            log_format: LogFormat::Json,
        })
        .expect("log file opens");
        assert!(guard.is_some());
        assert!(path.exists());
    }

    #[test]
    fn json_format_emits_spans_chain_but_not_single_span_field() {
        // Holds SUBSCRIBER_LOCK: see
        // init_logging_writes_plain_text_without_ansi_escape_sequences.
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("ylva.jsonl");
        let file = open_truncate(&path).expect("open log file");
        let (subscriber, guard) = build_subscriber(file, LogFormat::Json, tracing::Level::DEBUG);
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("tool_execute", tool = "read", tool_call_id = "call_1");
            let _enter = span.enter();
            tracing::debug!("read file");
        });
        drop(guard);
        let contents = std::fs::read_to_string(&path).expect("read log");
        let record: serde_json::Value =
            serde_json::from_str(contents.trim()).expect("log line is valid JSON: {contents}");
        assert!(
            record.get("span").is_none(),
            "redundant single-span field must not be emitted: {contents}"
        );
        let spans = record
            .get("spans")
            .and_then(serde_json::Value::as_array)
            .expect("spans chain must be emitted: {contents}");
        assert_eq!(spans.len(), 1, "one entered span: {contents}");
        assert_eq!(spans[0]["name"], "tool_execute");
        assert_eq!(spans[0]["tool"], "read");
        assert_eq!(spans[0]["tool_call_id"], "call_1");
    }

    #[test]
    fn text_format_renders_current_span_name_and_fields() {
        // Holds SUBSCRIBER_LOCK: see
        // init_logging_writes_plain_text_without_ansi_escape_sequences.
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("ylva.log");
        let file = open_truncate(&path).expect("open log file");
        let (subscriber, guard) = build_subscriber(file, LogFormat::Text, tracing::Level::DEBUG);
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("tool_execute", tool = "read", tool_call_id = "call_1");
            let _enter = span.enter();
            tracing::debug!("read file");
        });
        drop(guard);
        let contents = std::fs::read_to_string(&path).expect("read log");
        assert!(
            contents.contains("tool_execute:"),
            "span name must be rendered: {contents}"
        );
        assert!(
            contents.contains("tool=\"read\""),
            "span field must be rendered: {contents}"
        );
        assert!(
            contents.contains("tool_call_id=\"call_1\""),
            "span field must be rendered: {contents}"
        );
    }

    /// A `log`-crate record (as emitted by dependencies such as reqwest/hyper)
    /// is bridged into `tracing` with a stub metadata (`target: "log"`, no
    /// file/line) and the real location in `log.*` fields. The text line must
    /// reconstruct the real source location via normalization instead of
    /// rendering the stub `[[log]]`.
    #[test]
    fn text_format_normalizes_bridged_log_events_to_real_source() {
        // Holds SUBSCRIBER_LOCK: see
        // init_logging_writes_plain_text_without_ansi_escape_sequences.
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("ylva.log");
        let file = open_truncate(&path).expect("open log file");
        let (subscriber, guard) = build_subscriber(file, LogFormat::Text, tracing::Level::DEBUG);
        tracing::subscriber::with_default(subscriber, || {
            let record = log::Record::builder()
                .level(log::Level::Debug)
                .target("reqwest::connect")
                .file(Some("logging.rs"))
                .line(Some(1))
                .args(format_args!("starting a connection"))
                .build();
            tracing_log::format_trace(&record).expect("format_trace");
        });
        drop(guard);
        let contents = std::fs::read_to_string(&path).expect("read log");
        assert!(
            contents.contains("reqwest::connect"),
            "bridged log must keep its real target: {contents}"
        );
        assert!(
            contents.contains("logging.rs"),
            "bridged source must be the file basename: {contents}"
        );
        assert!(
            !contents.contains("[[log]]"),
            "stub target must be replaced by normalization: {contents}"
        );
        assert!(
            !contents.contains("log."),
            "bridging metadata fields must not be rendered: {contents}"
        );
    }

    #[test]
    fn init_logging_missing_directory_reports_path_instead_of_panicking() {
        let missing_dir = tempfile::TempDir::new()
            .expect("tempdir")
            .path()
            .join("does-not-exist");
        let path = missing_dir.join("ylva.log");
        let result = init_logging(LogConfig {
            log_file: Some(path.clone()),
            log_level: tracing::Level::INFO,
            log_format: LogFormat::Text,
        });
        let message = result.expect_err("missing directory must be an error");
        assert!(
            message.contains("does-not-exist"),
            "error should name the missing directory: {message}"
        );
        assert!(
            message.contains(&path.display().to_string()),
            "error should name the attempted file: {message}"
        );
    }

    #[test]
    fn init_logging_relative_filename_uses_current_directory() {
        // Holds SUBSCRIBER_LOCK: see init_logging_with_text_creates_file_and_returns_guard.
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(dir.path()).expect("set cwd");
        let result = init_logging(LogConfig {
            log_file: Some("ylva.log".into()),
            log_level: tracing::Level::INFO,
            log_format: LogFormat::Text,
        });
        std::env::set_current_dir(previous).expect("restore cwd");
        assert!(
            result.is_ok(),
            "a bare relative file must resolve to the current directory: {result:?}"
        );
        let guard = result.expect("log file opens");
        assert!(guard.is_some());
        assert!(dir.path().join("ylva.log").exists());
    }

    #[test]
    fn init_logging_unwritable_path_reports_path_instead_of_panicking() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("ylva.log");
        // A directory in place of the file cannot be opened for writing.
        std::fs::create_dir(&path).expect("create dir at log path");
        let result = init_logging(LogConfig {
            log_file: Some(path.clone()),
            log_level: tracing::Level::INFO,
            log_format: LogFormat::Text,
        });
        let message = result.expect_err("unwritable path must be an error");
        assert!(
            message.contains(&path.display().to_string()),
            "error should name the attempted file: {message}"
        );
    }
}
