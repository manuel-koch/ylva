# Known Test Issues

Workarounds and explanations for persistent test-environment quirks.

## Tracing subscriber isolation in tests

### Issue

Concurrent `tracing` subscriber registration corrupts per-callsite interest
caches, causing log events to be silently dropped in test runs.

`with_default(subscriber, ...)` calls `Dispatch::new(subscriber)` internally,
which adds the dispatcher to a process-global `LOCKED_DISPATCHERS` list and
then rebuilds every callsite's interest by ANDing together all live dispatchers
(`Interest::and`). When two tests run `with_default` concurrently, an
INFO-only dispatcher from one test can AND with the DEBUG dispatcher from
`capture_logs`, yielding `Interest::never()` for DEBUG callsites — causing
those events to be silently dropped even though the capturing subscriber is
interested. The same applies to `init_logging`, which calls
`set_global_default` → `rebuild_interest_cache` and can reset callsite
interests globally.

A subtler variant: a callsite is only registered on its **first** access.
`capture_logs` calls `rebuild_interest_cache()` before running the body, which
updates all *already-registered* callsites. But if a concurrent test (without
the lock) first accesses a callsite from a thread with no subscriber, the
callsite is registered with `Dispatch::none()` → `Interest::never()`. Later,
when `capture_logs`'s body hits that callsite, the cached "never" causes the
event to be dropped — `rebuild_interest_cache()` never saw it because it
wasn't registered yet when that was called.

### Workaround

Any unit test that calls `tracing::subscriber::with_default(...)` or
`init_logging(...)` **must** acquire `SUBSCRIBER_LOCK` for the entire duration:

```rust
let _lock = crate::test_support::SUBSCRIBER_LOCK
    .lock()
    .expect("subscriber mutex poisoned");
```

Any test that calls a function exercised by `capture_logs` tests (such as
`read_file`, `write_file`, or `glob_files`) **must** use `run_locked` instead
of `#[tokio::test]`:

```rust
#[test]
fn my_test() {
    crate::test_support::run_locked(async {
        // async test body
    });
}
```

Holding `SUBSCRIBER_LOCK` for the full duration of the `with_default` closure
(and the surrounding `Dispatch` lifetime) serialises subscriber registration
across tests and prevents this class of flaky failure.

## Process-global cwd mutation in tests

### Issue

`init_logging_relative_filename_uses_current_directory` temporarily changes the
process-global current working directory (`std::env::set_current_dir`) while
holding `SUBSCRIBER_LOCK`. Any test that reads `std::env::current_dir()` without
holding the lock can race with this mutation and observe an unexpected cwd,
causing spurious assertion failures.

### Workaround

Any test that reads or depends on the current working directory **must** also
acquire `SUBSCRIBER_LOCK` for the duration of the read:

```rust
let _lock = crate::test_support::SUBSCRIBER_LOCK
    .lock()
    .expect("subscriber mutex poisoned");
let cwd = std::env::current_dir().expect("cwd");
```

## `cfg(test)` does not reach library code from integration tests

### Issue

The Rust compiler sets `cfg(test)` only for the crate currently being compiled
as a test harness. For integration tests (`tests/*.rs`) the `ylva` library is
compiled as a normal dependency — **`cfg(test)` is NOT set** for library source
files when the caller is an integration test.

Practical consequence: `#[cfg(not(test))]` guards inside `src/` do **not**
prevent execution when called from `tests/*.rs`. Any side-effect that must be
suppressed in tests (OS clipboard writes, network calls, file I/O, …) must be
abstracted via a trait or closure injected by the caller, so integration tests
can pass a no-op implementation while unit tests continue using `#[cfg(test)]`
guards for their own call sites.

### Workaround

Abstract the side-effectful operation behind a trait or callback parameter and
inject a no-op implementation from integration test call sites. Do not rely on
`#[cfg(not(test))]` in library code to suppress behaviour from integration
tests.

## Undrained `AgentLoop` ui channel stalls `run()`

### Issue

`AgentLoop::new` gives the loop a **bounded** `ui_tx` (capacity 100), and each
tool round emits several `UiUpdate`s (`StreamStarted`, `StreamDone`,
`ToolDone`, …). The TUI and CLI drivers drain `ui_rx` while the agent runs, so
production never fills the channel. A test that instead drains `ui_rx` only
**after** `run()` returns does: after roughly 34 tool rounds
`ui_tx.send().await` parks the whole agent loop, the iteration cap never trips,
and `run()` never returns — the test shows up as a timeout hang.

Because such a test usually runs inside `run_locked` (it panics while holding
`SUBSCRIBER_LOCK`), the hang's timeout panic poisons the mutex and cascades
into unrelated tests failing with `subscriber mutex poisoned`.

Raising `MAX_TOOL_USE_ITERATIONS` moves the stall point: a test that fit under
the limit at 30 iterations (31 rounds ≈ 93 of 100 slots) can stall at 50.

### Workaround

Drain `ui_rx` **concurrently** with `run()`, mirroring the drivers: a
`tokio::select!` loop over the run handle and `ui_rx.recv()`, followed by a
non-blocking `try_recv` drain to pick up updates `run()` posted just before it
returned. See `child_role_iteration_cap_exceeded_returns_err_not_unstructured_ok`
in `src/agent.rs` for the pattern.
