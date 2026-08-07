# Spike 1: TUI Responsiveness Under Load

## Context

**System being changed:** The Ylva agent-harness repo currently contains only
documentation (no `Cargo.toml`, no `src/`). This task adds the first piece of
executable code: Spike 1 from `docs/plan.md` §0 / Phase 0 — a **standalone
proof-of-concept** proving that a `ratatui` + `tokio` TUI can stay responsive
while an artificial LLM stream injects tokens at very high volume and while the
user steers the stream mid-flight.

**Goal & motivation:** Before building the full agent architecture, `docs/plan.md`
mandates validating the two riskiest assumptions via isolated spikes. Spike 1
validates the core responsiveness claim that underpins the entire design:
the TUI thread never blocks on I/O, tokens arrive via an `mpsc` channel from a
tokio task, input is never disabled, and interruption/steering is cooperative.
If the TUI cannot hold ~60 FPS under a 1000+ messages/sec stream without input
latency or unbounded memory growth, the architecture in `docs/plan.md` §4B needs
to change before production work begins.

**Scope discipline:** This is a spike, not production code. It lives in a
dedicated `spikes/` directory, is a separate crate, and is **not** wired into any
future `src/` tree. It is explicitly called out as "standalone experiments, not
part of the production code path" in `docs/plan.md` §Phase 0.

**Acceptance criteria (verbatim from `docs/plan.md` §Spike 1 success criteria):**
- Frame time consistently ≤ 17 ms.
- Keyboard input (Enter, Ctrl+C, typing) responds instantly regardless of stream volume.
- No memory growth after 10 minutes of continuous token injection.
- Rapid steering works: a mid-stream `AgentCommand::Prompt` cancels the old
  stream and starts a new one without UI glitches.

**Key constraints:**
- Single-writer design: the generator task sends text over `mpsc`; the TUI task
  is the only consumer/renders. No shared mutable state, no `Arc<Mutex<_>>`.
- Crossterm input is read on a `spawn_blocking` thread and bridged to the async
  TUI loop via a channel + `tokio::select!` (pattern from `docs/plan.md` §4B).
- Redraw is event-driven: `terminal.draw()` runs only when a channel fires
  (token updates or input events), not on a fixed timer.
- Bounded `mpsc` capacity so a producer faster than the consumer is throttled
  instead of growing memory without limit.

## Implementation Plan

1. **Scaffold the spike crate.** Create `spikes/spike-tui-responsiveness/` as a standalone binary
   Cargo crate. Write `spikes/spike-tui-responsiveness/Cargo.toml` with package name `spike1`
   (edition matching the installed toolchain) and dependencies from
   `docs/plan.md` §2: `ratatui`, `crossterm`, `tokio` (features
   `rt-multi-thread`, `macros`, `time`), `tokio-util` (for
   `CancellationToken`), and `futures`. Pin to latest stable versions at
   implementation time (the repo has no `Cargo.lock` yet). Verify with
   `cargo build` inside `spikes/spike-tui-responsiveness`.

2. **Implement the token generator task** (`src/stream.rs`). A tokio task that
   loops sending artificial text chunks over an `mpsc::Sender<String>` at a
   configurable rate (default ≥ 1000 msgs/sec) and stops when a passed
   `CancellationToken` fires. Use a **bounded** channel (e.g. capacity 1024);
   the `send().await` throttles the generator only if the UI ever falls behind,
   which guarantees memory stays bounded under overload (never grows without
   limit). Expose the channel's live **queue depth** to the draw loop (via a
   `try_send` probe or a shared counter/semaphore) so the steady-state queue-depth
   metric can be measured.

3. **Implement the crossterm event bridge** (`src/events.rs`). Push
   `crossterm::event::read()` onto a `spawn_blocking` thread and forward events
   through an `mpsc` channel to the async loop. Map: `Ctrl+C` →
   `AgentCommand::Stop` (cancel the current generator), `Enter` →
   `AgentCommand::Prompt` (steer/restart), printable keys → append to the input
   buffer and echo into the scrolling region to prove input remains live.

4. **Implement app state** (`src/app.rs`). A scrolling buffer (a bounded
   `VecDeque<String>` ring, dropping oldest entries at a configurable cap so it
   cannot grow unbounded during the soak run), a streaming-state flag, a pending
   input string, and a metric accumulator covering frame time (record an
   `Instant` between draws, rolling window, min/avg/max, count of frames
   exceeding 17 ms), steady-state channel queue depth, and peak buffered memory
   (max ring-buffer byte length observed).

5. **Implement rendering** (`src/ui.rs`). A `ratatui` layout that draws the
   scrolling token buffer plus a status line showing: current frame time,
   frames > 17 ms, streaming state, and the last few key events received. This
   gives the operator a real-time view of the success criteria.

6. **Assemble `main.rs`.** Build a tokio `Runtime`, call `ratatui::init()`,
   spawn the generator, and run the event loop with
   `tokio::select! { ui_rx.recv(), event_rx.recv() }` per the pattern in
   `docs/plan.md` §4B (lines ~675–711). **Batch-drain per frame (Resolution 2):**
   when the token channel fires, drain *all* available tokens via `try_recv()`
   in a loop (bounded by a per-frame cap) and append them to the buffer in one
   batch, instead of `recv().await`-ing one token at a time — this amortizes
   draw/loop overhead and sustains ~60 FPS at 1000 msg/s. After applying channel
   updates, `terminal.draw()`; sample the queue depth and record the frame time
   each draw. Implement mid-stream steering triggered from `main()` (a
   hardcoded `tokio::time::sleep` then a sent `Prompt`, mirroring Phase 1):
   cancel the old generator's `CancellationToken`, await its `Cancelled`/
   completion signal, flush/replace the buffer marker, and start a fresh
   generator. On exit call `ratatui::restore()` and print a frame-time and
   soak summary to stdout/stderr.

7. **Add run controls**    for measuring success criteria. Accept a `--duration`
   (default **60 s** — the short default run; use `--duration 600` for the
   mandated 10-minute soak) and `--rate` (default 1000) via simple CLI flags (no
   `clap` needed for a spike) or env vars. The short default run is the one
   performed as part of this task's verification; the full 10-minute soak is a
   manual/CI verification step that uses the same `--duration` flag. On exit
   print the metric summary so every success criterion is objectively checkable:
   frame-time stats (min/avg/max, count > 17 ms), **steady-state channel queue
   depth** (target ≤ 1 frame's worth, ~20 tokens, at 1000 msg/s — proves no
   consumer starvation), and **peak buffered memory** (flat over the run — proves
   no growth).

8. **Add a unit test** for the pure, terminal-free logic (the buffer ring-bounds
   behavior and the frame-time rolling-window math) in `app.rs`, so the spike
   has a verifiable signal under `cargo test` even though the core FPS/input
   criteria are manual performance checks requiring a live terminal. Run
   `cargo test` and `cargo clippy`/`cargo fmt` in `spikes/spike-tui-responsiveness` to confirm
   clean.

9. **Update affected documentation.** Append a brief "Spike 1 outcome" note to
   `docs/plan.md` under the Spike 1 heading (§0) recording which success
   criteria were met/not met (frame time, input latency/queue depth, memory over
   10 min, steering behavior) and any architectural implications. Per
   `AGENTS.md` (*Docs are part of the change set*), the doc change ships with
   the spike. Do **not** create any new doc files without asking the user.

## Proposed Improvements

- Add a lightweight frame-time **histogram** (buckets: <8 ms, 8–16 ms, >16 ms, >33 ms)
  so the "consistently ≤ 17 ms" criterion is graded rather than anecdotal.
- Pre-allocate the scrolling ring buffer up front to avoid allocation churn
  during the soak run, directly supporting the no-memory-growth check.
- Optionally run the event loop on a `current_thread` runtime to confirm the
  spike needs no multi-threaded scheduler (cheaper, and closer to the final
  design).

## Implementation Hints

- The repo currently has **no** `Cargo.toml` or `src/` — the project is
  documentation-only. `spikes/spike-tui-responsiveness/` will be the first crate; there is no
  workspace root `Cargo.toml` to register it in (that is fine for a standalone
  experiment).
- Rust/Cargo toolchain installed: `cargo 1.94.1` / `rustc 1.94.1`.
- `.gitignore` already ignores `target` and `debug` (patterns match at any
  depth), so spike build artifacts will not be tracked.
- Reuse the exact channel/event-loop patterns already specified in
  `docs/plan.md`:
  - `spawn_blocking` crossterm bridge + `select!` over `ui_rx` + `event_rx`,
    redrawing only when a channel fires — `docs/plan.md` §4B lines ~675–697.
  - `ratatui::init()` / runtime / `ratatui::restore()` scaffolding — §4B lines
    ~699–711.
  - Command enum naming: `AgentCommand::Prompt`, `AgentCommand::Stop`,
    `AgentCommand::ApproveTool` — §4B lines ~357–365. Use the same names for
    spike fidelity.
- Deps to use (from `docs/plan.md` §2): `ratatui`, `crossterm`, `tokio`,
  `tokio-util`, `futures`. No `reqwest`, `serde`, or LLM crates are needed for
  Spark 1; the stream is artificial.
- `docs/plan.md` §Spike 1 says "Log frame times to verify no dropped frames":
  the exit summary (step 7) is where this durability lives.
- `AGENTS.md` notes that `ui.rs`/`main.rs` are excluded from the ≥80% coverage
  gate (they need a live terminal); the spike follows the same reasoning — only
  test the pure logic in `app.rs`.
- **Confirmed:** the spike **shall** record a short outcome note into
  `docs/plan.md` under the Spike 1 heading (step 9) — the doc edit is part of
  the change set and is authorized.
- **Confirmed:** the verification run for this task uses the default **short**
  run (60 s). The 10-minute soak is a separate, later manual/CI verification
  step using `--duration 600`, not part of this task's default run.
