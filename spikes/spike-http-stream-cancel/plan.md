# Spike 2: HTTP Stream Cancellation

## Context

**System being changed:** The Ylva repo currently contains documentation plus one
standalone spike crate (`spikes/spike-tui-responsiveness`, Spike 1). This task adds the
second proof-of-concept from `docs/blueprint.md` §0 / Phase 0 — **Spike 2: HTTP Stream
Cancellation** — as a new standalone crate at `spikes/spike-http-stream-cancel/`.

**Goal & motivation:** `docs/blueprint.md` mandates validating the two riskiest assumptions
before building the full agent. Spike 1 proved the TUI can stay responsive under load.
Spike 2 validates the cancellation claim that underpins §4A (the unified provider) and
§4B (interruption/steering): a raw `reqwest` + `eventsource-stream` HTTP stream can be
aborted **cooperatively** such that the underlying TCP connection closes immediately
(no `CLOSE_WAIT` / `ESTABLISHED` linger) and the tokio runtime leaks **zero** tasks.
This is what lets the user hit `Ctrl+C` or steer mid-stream without leaking sockets or
background tasks in the real agent. If the install pattern in `docs/blueprint.md` §4A
(dropping the `stream` + `response` in a `tokio::select!` on `CancellationToken`) does
not reliably close the connection or leaks tasks, the provider design must change
before Phase 2/3 production work.

**Scope discipline:** This is a spike, not production code. It lives in the dedicated
`spikes/` directory, is a separate crate, and is **not** wired into any future `src/`
tree (per `docs/blueprint.md` §Phase 0). Unliked Spike 1, this experiment is **headless**
(a plain binary, no TUI): cancellation and connection state are verified programmatically
and via OS socket inspection, so no live terminal is required.

**Acceptance criteria (verbatim from `docs/blueprint.md` §Spike 2):**

- Dropping the stream future aborts the underlying HTTP connection (no `CLOSE_WAIT` or
  `ESTABLISHED` linger).
- The tokio runtime shows zero leaked tasks.
- `CancellationToken` integration works reliably (no race between `cancelled()` and
  `stream.next()`).

**Key constraints:**

- Raw `reqwest` (features `json`, `stream`) + `eventsource-stream` — the exact
  provider stack chosen in `docs/blueprint.md` §2/§4A. No convenience LLM crates.
- Cooperatively cancel via a `CancellationToken` polled in a `tokio::select!`
  (`biased` so cancel is checked first), mirroring `docs/blueprint.md` §4A lines ~281-305.
- A **local in-process mock SSE server** is the default target so verification requires
  no network or API keys; real OpenRouter / Ollama endpoints are optional opt-in
  targets for end-to-end checks (per `docs/blueprint.md` §Spike 2 "Connect to an actual SSE
  endpoint (OpenRouter streaming completion or a local mock server)" ... "Repeat for
  both OpenRouter and Ollama (`/v1/chat/completions`) endpoints").
- Deterministic, repeatable reproduction: the mid-stream cancel point is
  CLI-controlled (and swept across a range), not genuinely random.

## Implementation Plan

1. **Scaffold the spike crate.** Create `spikes/spike-http-stream-cancel/` as a
   standalone binary crate, mirroring the Spike 1 template structure
   (`Cargo.toml`, `src/`, `plan.md`, `README.md`). Write `Cargo.toml` with package
   name `ylva-spike-http-stream-cancel`, edition `2024` (matching Spike 1), and
   dependencies: `tokio` (features `rt-multi-thread`, `macros`, `time`, `sync`, `io-util`,
   `net`, `rt` for runtime metrics), `tokio-util` (for `CancellationToken`),
   `futures` (for `StreamExt::next()`), `reqwest` (features `json`, `stream`),
   `eventsource-stream`, `serde`/`serde_json`. Pin to latest stable versions at
   implementation time (each spike resolves its own `Cargo.lock`). Verify with
   `cargo build` inside `spikes/spike-http-stream-cancel`.

2. **Implement the mock SSE server** (`src/mock_server.rs`). A tokio
   `TcpListener` that: reads the HTTP request head until `\r\n\r\n`, responds with
   `HTTP/1.1 200 OK`, `Content-Type: text/event-stream`, `Cache-Control: no-cache`,
   and the OpenAI-style `data: {...}\n\n` SSE framing (and a final
   `data: [DONE]\n\n`), looping frames on a small tick (e.g. 10 ms) until the client
   disconnects or a configured frame cap is reached. Track **live connection count**
   and a monotonically **cumulative connection counter** in `Arc<AtomicUsize>`(s);
   detect client abort by observing the read side return `Ok(0)`/`Err` and
   decrementing the live count. Expose both counters so the client side confirms the
   server released the connection too. Bind to an ephemeral port (`127.0.0.1:0`) and
   report the resolved port.

3. **Implement the raw client** (`src/client.rs`). Build a `reqwest::Client`, POST
   to `<base>/v1/chat/completions` with an OpenAI-style JSON body (`model`,
   `messages`, `stream: true`), obtain `Response`, wrap
   `response.bytes_stream().eventsource()`. Iterate `stream.next()` producing an
   event count / accumulated text and per-event timestamps. Reuse the exact channel
   shape (`futures` `StreamExt::next()` inside `tokio::select!`) documented in
   `docs/blueprint.md` §4A.

4. **Implement the two cancellation mechanisms** (both required to adjudicate the
   THREE distinct cases — adapter-only drop, adapter + `Response` drop, and
   `CancellationToken` abort; see §Spike 2 resolution): in `src/client.rs`, provide
   `stream_and_drop(...)` and `stream_with_token(...)`:
   - `stream_and_drop`: receive chunks and, at a CLI-chosen mid-stream point, **drop
     the stream future directly**, exposing the raw-drop paths named by
     `docs/blueprint.md` §Spike 2. This mechanism internally offers **two sub-cases**:
     (i) drop only the `bytes_stream()`/`eventsource()` adapter, and (ii) drop the
     adapter **and** the `Response` together — so the comparison can state whether
     dropping the adapter alone leaks the socket (Pitfall #8).
   - `stream_with_token`: run the §4A `tokio::select! { biased; _ = cancel.cancelled()
     => break, item = stream.next() => { ... } }` loop; fire the
     `CancellationToken` at the chosen point and observe a clean `break` (the
     cooperative-cancel path).
   For each, also capture whether the connection closed immediately.

5. **Implement verification helpers** (`src/verify.rs`). OS-level socket inspection:
   shell out to `lsof -i :<port>` and `netstat -an` (tolerating either being absent,
   printing a warning) to assert **no** lingering `ESTABLISHED`/`CLOSE_WAIT` entries
   for the mock-server port after cancellation. Runtime-task-leak checking: build the
   runtime with metrics enabled and, before/after each run, read
   `Handle::current().metrics()[: num_tasks()/active_tasks_count()]`; assert the count
   returns to the pre-run baseline (zero net leaked tasks), and also assert a
   `shutdown_timeout` completes without hang as a secondary signal. Also record a
   **close-latency trace**: the elapsed time between firing the cancel (or performing
   the drop) and both the mock server's live-connection counter decrementing and the
   socket entry disappearing from `lsof` — per iteration — so an instant abort is
   distinguishable from a slow/lingering close (Improvement: per-iteration trace).

6. **Assemble `main.rs`.** Parse simple positional/`--flag` CLI args (manual, no
   `clap` — consistent with Spike 1): `--endpoint <mock|openrouter|ollama>`
   (default `mock`), `--cancel-mode <drop|token>` (default `token`),
   `--cancel-at <fraction 0..1 or ms offset>`, `--iterations <n>`, plus
   `--base-url`, `--model` for the real endpoints. **Keys are never hard-coded**:
   read `OPENROUTER_API_KEY` from the environment for OpenRouter and a local Ollama
   `--base-url`; the default/CI run targets only the hermetic mock server
   (Improvement: env-sourced keys, no secrets in source).
   Run a **sweep**: for a range of cancel offsets across `--iterations` each, connect
   to the mock server (default) and assert (a) live connection count returns to 0 on
   both client- and server-side, (b) no lingering socket entries, (c) zero net leaked
   tasks. For the `token` mode, sweep cancel offsets aggressively (hundreds of
   iterations) to surface any `cancelled()` vs `stream.next()` race. **Race assertion
   (tolerant grace window):** for each run, after `cancelled()` is observed and the
   `select!` breaks, allow **at most one** in-flight chunk already extracted from the
   channel before declaring the connection closed; any further chunks emitted, or a
   non-prompt termination, counts as a race and fails the run. This tolerates
   OS/runtime scheduling jitter while still proving the stream stops promptly after
   cancel. **Emit a first-class direct-drop-vs-token-abort comparison** (Improvement):
   an aggregated table reporting, per cancel-mode, mean/p90 close-latency and whether
   each sub-case closed the connection — in particular whether **dropping only the
   `eventsource` adapter (not the `Response`)** leaked the socket, which adjudicates
   Pitfall #8. Print a per-criterion pass/fail summary to stderr. Exit non-zero if any
   success criterion fails so the spike is CI-assertable.

7. **Add unit tests** for the terminal-free, deterministic logic
   (`src/mock_server.rs` framing/counters, `src/client.rs` cancel-offset scheduling,
   and any SSE-chunk counting helpers) so `cargo test` gives a signal in Spikes mode.
   Add an integration-style test that boots the mock server, connects the raw client,
   cancels, and asserts the live-connection counter reaches 0 without needing real
   network. Run `cargo test`, `cargo clippy`, `cargo fmt` inside
   `spikes/spike-http-stream-cancel` to confirm clean.

8. **Update affected documentation.** Write `spikes/spike-http-stream-cancel/README.md`
   mirroring the Spike 1 README: what it proves, how to build/run each `--endpoint`,
   controls/flags, and how the success criteria are printed. Append a brief "Spike 2
   outcome" note under the Spike 2 heading of `docs/blueprint.md` (§0) recording pass/fail
   per success criterion and any architectural implications for §4A. Per `AGENTS.md`
   (*Docs are part of the change set*), both doc changes ship with the spike. Do **not**
   create any other new doc files without asking.

## Implementation Hints

- Template: mirror `spikes/spike-tui-responsiveness/` layout (`Cargo.toml` with
  `edition = "2024"`, `src/*.rs`, `plan.md`, `README.md`); its `Cargo.toml` shows the
  expected dep-recency (`reqwest 0.12` `json`+`stream`, `eventsource-stream 0.2`,
  `tokio 1`, `tokio-util 0.7`, `futures 0.3`) from `docs/blueprint.md` §9 / §Starter Cargo.toml.
- This spike is **headless**: do not use `ratatui`/`crossterm`; verification prints to
  stdout/stderr. The only false-terminal requirement in Spike 1 does not apply here, so
  `main.rs` needs no `ratatui::init()` and the count logic can be integration-tested.
- Cancellation pattern to replicate (verbatim intent): `tokio::select! { biased; _ =
  cancel.cancelled() => { drop(stream); drop(response); break; } item = stream.next()
  => { ... } }` — `docs/blueprint.md` §4A lines ~281-305.
- `netstat`/`lsof` are the mandated OS checks (`docs/blueprint.md` §Spike 2). On macOS
  (this environment) use `lsof -i :<port>`; tolerate absent tools mildly. Server-side
  release is tracked via the mock server's live-connection counter.
- Runtime task-leak assertion: `tokio::runtime::Handle::current().metrics()` requires
  a runtime built with metrics enabled (`Builder::new_multi_thread().enable_all()`);
  use it for the before/after task count and rely on a clean `shutdown_timeout` as the
  second, OS-level signal.
- `.gitignore` already ignores `target`/`debug` at any depth, so spike build artifacts
  are not tracked (same as Spike 1).
- `docs/blueprint.md` success criteria live at §0 Spike 2 (lines ~54-67); Phase 2 lists the
  same "Cancellation verified with real TCP" check (line ~1509); Pitfall #8 (line
  ~1674) is the subtle connection-leak warning. Cite these when writing the outcome note.
- `AGENTS.md`: docs are part of the change set; only append the outcome note to
  `docs/blueprint.md` §0 Spike 2 and add the spike's own `README.md`; ask before creating
  any other new doc file. The Makefile/`make test-coverage` gate at the repo root
  (`cargo llvm-cov`, `--fail-under-lines 80`, excluding `ui.rs`/`main.rs`) targets the
  root crate and does not by itself cover spike crates — run the spike's own
  `cargo test`/`clippy`/`fmt` inside its directory.
