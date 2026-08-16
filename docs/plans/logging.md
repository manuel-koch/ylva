# Logging Infrastructure for Ylva

## Context

Ylva is a Rust-based terminal AI agent application (`[[bin]]` + `[lib]` crate `ylva`).
Currently there is no dedicated logging infrastructure. Diagnostic output is produced ad-hoc:
`println!` drives all console output in `main.rs`, and `eprintln!` is used for a single
verbose startup message gated on the `YLVA_VERBOSE` environment variable
(`src/main.rs:report_provider`). There are no structured log calls anywhere in the
library (`src/agent/`, `src/llm/`).

**Goal:** Replace the ad-hoc diagnostic approach with a proper logging infrastructure that:

- lets library code (`src/agent/`, `src/llm/`) emit structured, severity-tagged log events
  without coupling to any output mechanism;
- routes those events to a caller-configured, runtime-selectable log file;
- supports both human-readable text and JSONL output formats, each including timestamp,
  severity, target and line number, and message;
- exposes three new CLI flags (`--log-file`, `--log-level`, `--log-format`) so users can
  control where, at what verbosity, and in what format logs are written;
- removes the bespoke `YLVA_VERBOSE` / `report_provider` mechanism entirely, replacing it
  with a `debug!` log event visible when the user activates file logging at debug level;
- never writes secrets or API keys to the log — callers are responsible for redacting
  sensitive values before passing them to any `tracing` macro.

**Motivation:** As the codebase grows toward a TUI phase and more complex agent loops,
ad-hoc prints become insufficient: they cannot be silenced, filtered, or redirected
independently of console output, and they leave library internals unobservable.

**Acceptance criteria / definition of done:**

1. `tracing` macros (`trace!`, `debug!`, `info!`, `warn!`, `error!`) are used throughout
   `src/` library code wherever diagnostic information is produced.
2. `main.rs` initialises a `tracing-subscriber` layer that writes to the file specified by
   `--log-file`; when `--log-file` is absent, no output is produced and no file is created.
   Logging is entirely opt-in.
3. The file is opened in truncate/overwrite mode on each start (no log accumulation across
   runs).
4. The minimum severity written is controlled by `--log-level`
   (values: `trace`, `debug`, `info`, `warn`, `error`; default: `info`). Selecting a level
   includes that level and all higher-severity levels.
5. The output format is controlled by `--log-format` (values: `text`, `json`; default:
   `text`). `text` is human-readable; `json` produces JSONL (one JSON object per line).
6. The `YLVA_VERBOSE` / `report_provider` mechanism is removed entirely. The startup
   provider diagnostic becomes a `debug!` log event; the API key field logs only
   `"set"` or `"unset"` — never the key value itself (matching the existing `report_provider`
   behaviour).
7. No `tracing` call site in `src/` ever logs a secret or API key value. Callers redact
   sensitive data before passing it to any `tracing` macro; the log contains at most a
   presence indicator (e.g. `api_key = "set"` / `"unset"`).
8. Console output (`println!`) remains untouched — it is user-facing, not diagnostic.
9. All existing tests pass; new unit/integration tests verify log initialisation, level
   filtering, format selection, file overwrite behaviour, and that the API key is not
   logged in plain text.
10. Documentation (`docs/features.md`, `docs/configuration.md`) is updated.

**Key constraints:**

- No secret or API key value may appear in any log record. Every call site that touches
  sensitive configuration (provider credentials, API keys) must reduce the value to a
  presence indicator (e.g. `"set"` / `"unset"`) before passing it to a `tracing` macro.
  This is a caller-side responsibility enforced by code review, not by a runtime filter.
- Keep `main.rs` thin (per coding conventions). Logging initialisation logic must go in a
  dedicated `src/logging.rs` module exported through `src/lib.rs`.
- The `[lib]` crate must not depend on a specific subscriber; it uses only the `tracing`
  facade crate. The subscriber (writer, formatter, filter) is composed and installed by
  `main.rs` / the binary.
- No `Arc<Mutex>` or shared mutable state introduced by this change.
- Coverage threshold ≥ 80% must be maintained (`make test-coverage`).

## Implementation Plan

1. **Add `tracing` facade to `[dependencies]` in `Cargo.toml`.**
   Add `tracing = "0.1"` to `[dependencies]`. This is the only dependency the library code
   needs. Verify the crate resolves without version conflicts (it is already present
   transitively in `Cargo.lock`).

2. **Add subscriber dependencies to `[dependencies]` in `Cargo.toml`.**
   Add `tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }` and
   `tracing-appender = "0.2"` for non-blocking file I/O. These are binary-only concerns but
   belong in `[dependencies]` (not `[dev-dependencies]`) because `src/logging.rs` is compiled
   into the library for use by the binary.

3. **Create `src/logging.rs` — the logging initialisation module.**
   - Define a public `LogFormat` enum with variants `Text` and `Json` (default: `Text`).
   - Define a public `LogConfig` struct with fields:
     - `log_file: Option<PathBuf>` — path to the log file; `None` means no output.
     - `log_level: tracing::Level` — minimum severity to emit (default: `Level::INFO`).
     - `log_format: LogFormat` — output format (default: `LogFormat::Text`).
   - Define a public `init_logging(config: LogConfig)
     -> Result<Option<WorkerGuard>, String>` function that:
     - When `config.log_file` is `None`: does not install any subscriber; returns `Ok(None)`.
     - When `config.log_file` is `Some(path)`: first checks the containing directory
       exists, then opens the file in truncate/write mode (overwrite on each start), wraps
       it with `tracing_appender::non_blocking`, and installs a `tracing_subscriber::fmt`
       layer with the configured level filter. Text output is plain (no ANSI) and the
       source location is condensed to the file basename and line number; the format is
       `json()` when `LogFormat::Json`, or the custom `CondensedText` event formatter when
       `LogFormat::Text`. Any failure to set up the file (missing directory, unwritable
       path) is reported as an `Err` naming the attempted path rather than a panic.
   - Export `LogConfig`, `LogFormat`, and `init_logging` from `src/lib.rs`.

4. **Replace `YLVA_VERBOSE` / `report_provider` with `tracing` calls in `src/main.rs`.**
   - Remove `verbose_enabled()` and `report_provider()` functions.
   - Replace the `eprintln!("[ylva] provider=...")` call in `select_provider()` with a
     `tracing::debug!` event. The `api_key` field must log only `"set"` or `"unset"` —
     never the actual key value:
     `tracing::debug!(provider = kind, base_url = base, model, api_key = auth, "provider selected")`
     where `auth` is already the string `"set"` or `"unset"` (as computed in the current
     `report_provider` implementation).

5. **Instrument library code with `tracing` macros (broad pass).**
   Add diagnostic `tracing` calls at every significant call site:
   - `src/llm/openai_provider.rs`: `debug!` on request start, retry attempts, and stream
     events; `warn!` on retryable HTTP errors; `error!` on terminal errors. The `Authorization`
     header and any credential field must never appear in a log event — log only non-sensitive
     fields (URL, model, status code, retry count).
   - `src/agent.rs` / `src/agent/`: `debug!` on `AgentCommand` dispatch, tool approval /
     denial, iteration count; `warn!` when `MAX_ITERATIONS` is reached; `error!` on
     unrecoverable agent errors.
   - Keep `tracing` calls in library code only; no subscriber calls outside
     `src/logging.rs`.

6. **Parse `--log-file`, `--log-level`, and `--log-format` in `src/main.rs`.**
   - Extend the existing manual `std::env::args()` parsing (no `clap` in this phase) to
     extract `--log-file <path>`, `--log-level <level>`, and `--log-format <format>`
     arguments.
   - Construct a `LogConfig` from the parsed values and call `init_logging(config)`; on
     `Err`, print the reported path/error to stderr and exit with a non-zero code.
- Hold the returned `Option<WorkerGuard>` in a binding that lives until `main` returns
      (so the async writer is flushed on exit).
   - Invalid `--log-level` or `--log-format` values must print a usage error to stderr and
     exit with a non-zero code.

7. **Write tests for `src/logging.rs`.**
   - Unit test: `LogConfig` defaults to `Level::INFO` and `LogFormat::Text`.
   - Unit test: `init_logging` with `log_file = None` returns `None` without installing
     any subscriber.
   - Integration test with a `tempfile::tempdir()`: `init_logging` with `Some(path)` and
     `LogFormat::Text` creates the file; emitting one `info!` event and dropping the guard
     produces a non-empty file with a line containing the message.
   - Integration test with a `tempfile::tempdir()`: same as above for `LogFormat::Json` —
     verifies the file contains a valid JSON object per line.
   - Integration test: a second call to `init_logging` with `Some(path)` on the same file
     overwrites (truncates) rather than appends.
   - Integration test: emit a `debug!` event with a simulated `api_key = "set"` field,
     capture the log file, and assert the raw key value does not appear anywhere in the
     output (guards against accidental future regressions where a real key is passed).
   - Note: `tracing` subscriber initialisation is process-global; tests that install a
     subscriber must use `std::sync::Once` or tolerate the `Err` from
     `tracing_subscriber::set_global_default` to avoid double-init panics in parallel tests.

8. **Update `docs/features.md`.**
   Add a "Logging" subsection under "General" describing: opt-in file-based logging, the
   three CLI flags (`--log-file`, `--log-level`, `--log-format`), defaults (`info`, `text`),
the overwrite-on-start behaviour, and the record shape: timestamp, severity, target,
   message, and (in `text` format) the condensed `file.rs:line` source location.

9. **Update `docs/configuration.md`.**
   Add a "Logging" section documenting `--log-file`, `--log-level`, and `--log-format` with
   their types, accepted values, defaults, and a short example invocation for both formats.

10. **Run verification.**
    Execute `make format && make lint && make test-coverage` and fix all findings.
    Confirm coverage stays ≥ 80%.

## Consistency Issues

- **[low] `tracing-subscriber` global initialisation in tests.** `set_global_default` panics
  if called twice in the same process. Tests that exercise `init_logging` must guard against
  this (e.g. `std::sync::Once`, catch the `Err`). This is resolvable during implementation
  (Step 7) but must not be overlooked.

## Implementation Hints

- **Sensitive data redaction is a caller-side responsibility, not a runtime filter.**
  `tracing` fields are emitted as-is; there is no post-hoc scrubbing layer. Every call
  site that references provider credentials must reduce the value to a presence indicator
  before the `tracing` macro is invoked. The only approved pattern for API keys is
  `if has_key { "set" } else { "unset" }`. Any other representation (partial key,
  masked key, hex digest) is also prohibited.
- `tracing` is already a transitive dependency visible in `Cargo.lock`; adding it to
  `[dependencies]` simply promotes it to a direct, version-pinned dependency — no
  resolution conflict expected.
- `tracing_appender::non_blocking` returns `(NonBlockingWriter, WorkerGuard)`; the guard
  must stay alive (held in `main`) or buffered log events will be lost on exit. When
  `log_file` is `None`, no guard is needed — the function returns `None`.
- File truncation is achieved by opening with
  `std::fs::OpenOptions::new().write(true).create(true).truncate(true)`
  before passing the handle to `tracing_appender::non_blocking`.
- `tracing_subscriber::fmt().json()` emits JSONL records with fields `timestamp`, `level`,
  `target`, `line_number`, and `message` when `with_line_number(true)` and
  `with_target(true)` are set on the layer (no `filename`, to keep lines short).
- For `text` output a custom `FormatEvent` (`CondensedText`) emits
  `timestamp level target: file.rs:line: message`, where `file.rs` is the basename of the
  source file (never the absolute build path). It writes plain text (no ANSI) by setting
  `with_ansi(false)` on the layer.
- Level hierarchy (lowest → highest): `TRACE < DEBUG < INFO < WARN < ERROR`.
  `EnvFilter::new("info")` passes `info`, `warn`, and `error` and blocks `trace` and
  `debug` — this is the correct primitive for implementing the "level or greater"
  filtering requirement.
- `YLVA_VERBOSE` is currently checked only in `verbose_enabled()` (called by
  `report_provider()`); removing both functions eliminates the entire mechanism cleanly.
- Existing `println!` console output in `main.rs` is user-facing (conversation turns,
  tool status, JSONL history dump) and must not be replaced with `tracing` calls.
- Relevant source files: `src/main.rs`, `src/lib.rs`, `src/logging.rs` (new),
  `src/llm/openai_provider.rs`, `src/agent.rs`, `src/agent/tools.rs`,
  `src/agent/tools/fs.rs`.
- Docs to update: `docs/features.md`, `docs/configuration.md`.
