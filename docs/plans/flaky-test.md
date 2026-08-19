# Fix flaky test: `text_format_normalizes_bridged_log_events_to_real_source` — CLOSED

## Context

### System / component

`src/logging.rs` — unit test
`logging::tests::text_format_normalizes_bridged_log_events_to_real_source`
(approximately line 418).

### Goal and motivation

The test verifies that a `log`-crate event (as emitted by dependencies such as
reqwest/hyper) is bridged into `tracing` and that `CondensedText::format_event`
reconstructs the real source location via `NormalizeEvent::normalized_metadata()`
rather than emitting the stub `[[log]]` target.

The test is flaky — it fails approximately 1/3 of full `cargo test` parallel
runs — with the symptom that the log file is **empty**, meaning the event is
dropped before reaching the subscriber. It never fails when the lib-unit-test
binary is run in isolation.

### Root cause

The `log` → `tracing` bridge path is:

```text
log::debug! → log::Log::log (LogTracer)
              → LogTracer::enabled (reads LevelFilter::current())  ← global gate
              → dispatch_record(record)
                 → dispatcher::get_default (thread-local or global)
                 → dispatch.event(...)
```

`LogTracer::enabled` gates on `tracing_core::LevelFilter::current()` — a
**process-global** atomic maximum derived by `callsite::rebuild_interest` over
all currently registered dispatchers. It is **not** the level of the
thread-local subscriber set by `with_default`.

When `cargo test` runs the full suite in parallel, sibling unit tests call
`init_logging(...)` → `subscriber.try_init()`, which installs a process-global
subscriber at `INFO` and triggers `callsite::rebuild_interest` → `set_max`.
If that recomputation lowers `LevelFilter::current()` to `INFO` in the narrow
window _between_ `with_default` being entered and `log::debug!` being
dispatched, `LogTracer::enabled` discards the DEBUG record before it ever
reaches the test's thread-local subscriber. The log file is never written.

The captured diagnostic (`current()==DEBUG` at read time after the fact) is
consistent: the global level recovers to DEBUG by the time it is read, but was
momentarily below DEBUG at dispatch time. Timing-perturbing instrumentation
(adding a Mutex lock, format!, or AtomicBool store inside the closure)
suppressed the race by slightly shifting the window.

### Why the existing test design is fragile

The test uses `log::debug!` to obtain a pre-shaped bridged event. That routes
through the **global `log` logger** (`LogTracer`) and its level gate — process
state that is outside the test's control. But the assertions only verify
`CondensedText::format_event` behaviour with normalized metadata; they never
check bridge plumbing. Depending on the global bridge is unnecessary for the
stated goal.

### Fix strategy

`tracing_log::format_trace(record: &log::Record)` is a public API that calls
`dispatch_record` directly — skipping `LogTracer::enabled` and therefore
skipping the process-global level gate entirely. It constructs the same event
shape that `LogTracer` would (with `log.target`, `log.file`, `log.line` fields
attached) and dispatches via `dispatcher::get_default`, which resolves to the
thread-local subscriber set by `with_default` when one is active.

Replacing `log::debug!(...)` with a `tracing_log::format_trace(...)` call
constructed from a `log::Record::builder()` inside `with_default`:

- eliminates the race (no global level gate involved),
- keeps the exact bridged-event shape that `normalized_metadata()` reads,
- preserves all four assertions unchanged,
- removes the need for `LogTracer::init()` in this test.

### Acceptance criteria

- `cargo test` (full parallel suite) passes ≥ 20 consecutive runs with no
  failures of the flaky test.
- All four assertions remain intact and still pass:
  - contents contains `"reqwest::connect"` (real target preserved)
  - contents contains `"logging.rs"` (file basename rendered)
  - contents does NOT contain `"[[log]]"` (stub target suppressed)
  - contents does NOT contain `"log."` (bridge metadata fields not rendered)
- No other logging tests are weakened or removed.
- Coverage threshold (≥ 80% line coverage) continues to pass
  (`make test-coverage`).

## Implementation Plan

1. **Replace `log::debug!` with `tracing_log::format_trace` in the test.**

   In `src/logging.rs`, inside
   `text_format_normalizes_bridged_log_events_to_real_source`:

   Remove the `let _ = tracing_log::LogTracer::init();` line — it is no
   longer needed since the test no longer routes through the global log
   logger.

   Replace:

   ```rust
   tracing::subscriber::with_default(subscriber, || {
       log::debug!(target: "reqwest::connect", "starting a connection");
   });
   ```

   With:

   ```rust
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
   ```

   `format_trace` dispatches directly to the thread-local subscriber via
   `dispatcher::get_default`, bypassing `LogTracer::enabled` and the
   process-global `LevelFilter::current()` entirely.

2. **Remove the `log` dev-dependency from `Cargo.toml`.**

   The `log::debug!` call in the flaky test is the only `log::*` macro call
   in the entire `src/` tree. After step 1 removes it, `log = "0.4"` in the
   `[dev-dependencies]` section of `Cargo.toml` (line 36) is unused and must
   be deleted. Run `make lint` afterwards to confirm `cargo check` and
   `cargo clippy` are clean.

3. **Verify the fix by running `cargo test` in a loop.**

   Run `for i in $(seq 1 20); do cargo test --lib -- logging::tests 2>&1 | tail -3; done`
   and confirm zero failures. Then run the full `make test` at least 5 times.

4. **Confirm coverage still passes.**

   Run `make test-coverage` and confirm it exits with code 0.

5. **Update `docs/plans/flaky-test.md`.**

   Replace this plan with a brief closed-out note recording the root cause,
   the fix applied, and the commit SHA; or delete the file and record the
   closure in the commit message.

## Implementation Hints

- `tracing_log::format_trace` is declared in
  `tracing-log-0.2.0/src/lib.rs:158` as
  `pub fn format_trace(record: &log::Record<'_>) -> io::Result<()>`.
  It calls the crate-private `dispatch_record`, which calls
  `tracing_core::dispatcher::get_default` — resolving to the thread-local
  subscriber when one is active via `with_default`.
- `dispatch_record` constructs the event with these fields:
  `log.message`, `log.target`, `log.module_path`, `log.file`, `log.line` —
  exactly the shape `NormalizeEvent::normalized_metadata()` reads back.
- The `log.file` field drives the `"logging.rs"` assertion. Setting
  `.file(Some("logging.rs"))` in the `Record::builder()` is sufficient;
  using the actual macro location (`file!()`) also works and is more
  representative.
- `log::Record::builder()` is stable public API (`log` 0.4.33 in project).
- The flaky test is at `src/logging.rs` ≈ line 418.
- `LogTracer::init()` installs the global `log` logger; the test no longer
  needs it after this change.
- Development workflow and make targets: `docs/development-workflow.md`.
- Coverage threshold ≥ 80% enforced by `make test-coverage`.

## Closure

**Root cause:** `LogTracer::enabled` gates on the process-global
`tracing_core::LevelFilter::current()`. Sibling unit tests installing a
global subscriber could temporarily lower that level below DEBUG, causing
the event to be dropped before reaching the thread-local subscriber.

**Fix applied:** Replaced `log::debug!(...)` with a `tracing_log::format_trace`
call constructed from `log::Record::builder()` inside the `with_default` closure.
`format_trace` dispatches directly to the thread-local subscriber via
`dispatcher::get_default`, bypassing `LogTracer::enabled` and the global level
gate entirely. The `LogTracer::init()` call was also removed since the test no
longer routes through the global log logger. The `log` dev-dependency is retained
because `log::Record::builder()` / `log::Level` are still used in the test.

**Verified:** 20/20 consecutive isolated runs + 5 full `make test` passes,
all four assertions intact, coverage ≥ 80%.
