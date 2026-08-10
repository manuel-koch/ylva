# Phase 1: Minimal Headless Build (The Agent Kernel)

## Context

This plan implements **Phase 1** of the Ylva agent-harness build plan, as
specified in [`docs/blueprint.md`](../blueprint.md) → §7 Phase 1 (lines 1940–1995)
and its supporting patterns in §4A (unified provider trait), §4B (non-blocking
ReAct loop, steering/interrupt), and §4C (tiered context, Phase 1 subset).

**System being changed:** The repository currently ships **no Rust code at all**
— root contains only `docs/`, `spikes/`, `Makefile`, `AGENTS.md`, `.gitignore`,
`LICENSE`, `README.md`. There is no `Cargo.toml`, no `src/`, no tests, and no
`docs/architecture-design-records/` entries. Phase 1 therefore **bootstraps the
crate** (`Cargo.toml`, `src/lib.rs`, `src/main.rs`, module files, integration
tests) for the first time.

**Goal and motivation:** Build the durable **agent kernel** headless-first — a
working ReAct agent loop in roughly 300 lines that proves the riskiest
architecture claims before any TUI exists (which is deferred to Phase 4):

- The `Provider` trait plus a deterministic `MockProvider`.
- The **dual channel topology** (`ui_tx`/`ui_rx`, `agent_tx`/`agent_rx`,
  `cmd_tx`/`cmd_rx`) with two tasks: an outer `tokio::select!` loop that owns
  `Context` and `AgentMode`, and an inner streaming task per request.
- **Cooperative steering** (a new `Prompt` while streaming is queued and
  injected at the next natural point — never aborts the live stream) and
  **interruption** (`Stop` fires a `CancellationToken`, aborts, preserves the
  partial, clears pending steers).
- **Context ownership (HARD rule):** the outer agent loop is the sole writer to
  `Context`; no `Arc<Mutex<Context>>`, no shared mutable state.

**Accepted scope for Phase 1:** No CLI surface, no config parsing, no TOML, no
tools, no `ToolRegistry`, no `schemars`, no TUI, no persistence, no token
counting, no tiered context, no `TieredMessage`. The crate set is the blueprint
§9 "Phase 1 minimal (Q23)" set — `tokio`, `tokio-util`, `futures`,
`thiserror`, `anyhow` — **plus `uuid` (`v4`) and `serde_json`**, which are
added so the full §4B `Message`/`ToolCallRequest` skeleton is real typed API
(Q-C, no placeholders / dead-code / `todo!()`).

**Definition of done / acceptance criteria** (blueprint §7 Phase 1 "Success
criteria", lines 1987–1993, restated and made testable):

1. Steering never aborts a live stream and does not panic or deadlock.
2. Final context (no interrupt) contains exactly: the initial system message
   (`Message::System`, "You are Ylva, a helpful agent with tool capabilities."),
   the first assistant response (Completed), and the queued steering user
   message(s).
3. Interrupt path: the initial system message, the first partial assistant
   (Interrupted), and **no** leftover pending steers.
4. No `Arc<Mutex<Context>>`, no shared mutable state — provable by code reading.
5. All tests pass and `make test-coverage` stays at/above the configured **80%
   line coverage** threshold for testable files (`main.rs` is excluded by the
   Makefile; `ui.rs` does not exist yet).

**Documentation coupling (AGENTS.md "Docs are part of the change set"):** every
commit in this plan bundles implementation + tests + the relevant
`docs/features.md` update named in the Documentation step below. Also, because
this is the first source-bearing commit, the `docs/blueprint.md` Phase 1
checklist checkboxes that this plan completes must be ticked in the same work.

---

## Implementation Plan

Ordered, independently-verifiable steps. Each step ends with "Definition of
done = <verification>". Work in order; do not start a step until the previous
one's verification passes.

### Step 0 — Bootstrap the crate

Create the minimal Cargo package so the Makefile targets run.

- Create `Cargo.toml` with exactly:
  - `[package] name = "ylva"`, `version = "0.1.0"`, `edition = "2021"`.
- `[dependencies]`: `tokio = { version = "1", features = ["full"] }`,
  `tokio-util = "0.7"`, `futures = "0.3"`, `thiserror = "1"`, `anyhow = "1"`,
  `uuid = { version = "1", features = ["v4"] }` and `serde_json = "1"` (added
  per decision Q-C: the full §4B `Message` skeleton carries real `uuid::Uuid`
  tool-call ids and a `serde_json::Value` arguments field — no placeholders, no
  `#[allow(dead_code)]`, no `todo!()`).
  - `[dev-dependencies]`: `tokio` with the `test-util` feature so
    `tokio::time::pause`/`advance` work in tests (add `test-util` to the tokio
    dev-dependency features; the `full` feature on the normal dependency does
    not enable `test-util`).
- The crate auto-discovers `src/main.rs` as the `[[bin]]` and `src/lib.rs` as
  the `[lib]`, both named `ylva` (per AGENTS.md "Binary + library split"). No
  explicit `[[bin]]`/`[lib]` sections are required because the paths match the
  defaults.
- Add empty `src/lib.rs`, `src/main.rs`, `src/agent.rs`, `src/llm.rs` and a
  `tests/` dir (with `.gitkeep` or an initial test) so `cargo` has valid targets.

**Definition of done:** `make build-debug` succeeds (no warnings).

### Step 1 — `Message`, `AssistantStatus` domain types

In `src/agent.rs`, define the in-memory domain model from blueprint §4B (Q1).
**Decision (Q-C, confirmed): declare the full §4B skeleton with the `uuid`
crate — no placeholders, no `#[allow(dead_code)]`.** The variants and the
`ToolCallRequest` struct are real, typed public API (re-exported via `lib.rs`),
so no dead-code warnings arise even though Phase 1 only *constructs* `System`,
`User`, and `Assistant`.

```rust
use uuid::Uuid; // v4 feature enabled

pub enum AssistantStatus { Complete, Interrupted }

pub struct ToolCallRequest {
    pub id: Uuid,
    pub name: String,
    pub arguments: serde_json::Value,
}

pub enum Message {
    System   { content: String },
    User     { content: String },
    Assistant { content: String, status: AssistantStatus, tool_calls: Vec<ToolCallRequest> },
    ToolResult { id: Uuid, name: String, result: String },
    ToolError  { id: Uuid, name: String, error: String },
}
```

`Todo!()`/`unimplemented!()` are **not** allowed in the shipped Phase 1 code.

**Definition of done:** `make lint` passes (no warnings); a unit test asserts
the enum variants construct and the tool-call struct fields are typed with
`uuid::Uuid`.

### Step 2 — `Context` (Phase 1 subset)

In `src/agent.rs`, define `Context` as the outer loop's owned history (Q2, Q13):

```rust
pub struct Context { messages: Vec<Message> }

impl Context {
    pub fn new() -> Self;   // seeds Message::System { content: "You are Ylva, a helpful agent with tool capabilities." }
    pub fn push_user(&mut self, text: String);
    pub fn push_assistant(&mut self, content: String, status: AssistantStatus);
    pub fn messages(&self) -> &[Message];
}
```

**Decision (Q-A, confirmed):** `Context::new()` seeds a single
`Message::System` with the fixed content
`"You are Ylva, a helpful agent with tool capabilities."` so the Phase 1
success criterion "final context contains a system message" holds with a real
value. This seed is the **single source of truth** for the default system
prompt in Phase 1 (defined once here, referenced by the step tests).

**Forward-compatibility:** the initial system message of a chat
must eventually stem from a **SOUL.md** file, customizable per global
(`~/.ylva/`) and per project (`.ylva/`) config. That retrieval belongs to the
config phase (Phase 6 §4G layering), **not** Phase 1. Phase 1 therefore defines
the fixed string once as a named constant (e.g. a module-level
`const DEFAULT_SYSTEM_PROMPT`) so later phases can replace the constant's
source with the SOUL.md loader without changing call sites. Keep the constant
definition in one place (SSOT).

No `buffer`, no `TieredMessage`, no token counting, no compaction. Per §4C the
Phase 1 insertion surface is exactly `push_user` and `push_assistant` (plus the
seeded system message).

**Definition of done:** unit tests for all three methods (append ordering,
immutable read-back).

### Step 3 — `StreamEvent`, `UiUpdate`, `AgentCommand`, `AgentEvent`, `AgentMode`

In `src/agent.rs` (shared event types used by both `agent` and `llm`), define
the protocol enums that both the outer loop and the provider agree on
(`StreamEvent`):

- `StreamEvent` (provider → inner task, from §4A): `Text(String)`, `Done`,
  `Cancelled`, `Error(anyhow::Error)` — plus keep the `Delta` variant out of
  Phase 1 (no tools). Note: the blueprint §7 mock text mentions the mock emits
  `StreamStarted`, but `StreamEvent` per §4A has no `StreamStarted` variant;
  this plan resolves the conflict by having the *inner streaming task* emit
  `UiUpdate::StreamStarted` at the start of each request and the mock emit only
  stream-level events (see Step 6).
- `UiUpdate` (inner → TUI/CLI consumer): `StreamStarted`, `AppendText(String)`,
  `StreamDone`, `StreamCancelled`, `Error(String)`. (Tool/steer variants are
  Phase 3+.)
- `AgentCommand` (external → agent): `Prompt(String)`, `Stop`.
- `AgentEvent` (inner task → outer loop): `StreamDone { partial: String }`,
  `StreamCancelled { partial: String }`, `StreamError(String)`.
- `AgentMode` (outer loop state): `Idle`, `Streaming`.

**Definition of done:** `make lint` + a unit test constructing/pattern-matching
each enum.

### Step 4 — `Provider` trait + `MockProvider`

In `src/llm.rs`, define the runtime abstraction. Per Q12 it is a **native async
trait** (Rust 1.75+, no `async-trait` crate). It creates its own internal
`mpsc::channel` and returns the receiver:

```rust
pub trait Provider: Send + Sync {
    fn chat(
        &self,
        messages: Vec<Message>,
        cancel: CancellationToken,
    ) -> impl std::future::Future<Output = anyhow::Result<mpsc::Receiver<StreamEvent>>> + Send;
}
```

Phase 1 has **no `tools` parameter**; Phase 2 adds it together with
`ToolCallAccumulator`.

Implement `MockProvider` in `src/llm.rs` (Q8/Q21) and re-export it for tests:

```rust
pub struct MockProvider {
    tokens: Vec<String>,
    interval: Duration,
}
impl MockProvider {
    pub fn new(tokens: Vec<String>, interval: Duration) -> Self;
}
```

`chat()` behavior:

- **Decision (Q-D, confirmed):** `chat` returns a fresh `mpsc::Receiver`
  produced by an **internal spawned task** that it owns — the same contract the
  Phase 2 real provider must implement, so switching providers later is a drop-in
  (retry/SSE parsing stay hidden inside the provider, per §4A).
- The internal task emits `StreamEvent::Text` for each token, sleeping
  `interval` between them, then `StreamEvent::Done`.
- Polls the `CancellationToken`; if it fires mid-stream, emit
  `StreamEvent::Cancelled` and stop. The `sleep`/`interval` waits must be
  cancellation-aware (e.g. `select!` the token on `cancel.cancelled()`) so a
  `Stop` mid-sleep is noticed promptly, not only between tokens.
- Using `tokio::time::interval`/`sleep` makes the mock deterministic under
  `tokio::time::pause` in tests.

**Definition of done:** unit tests on the mock confirm it emits the expected
`Text…Done` sequence with `interval` control, and emits `Cancelled` when the
token is fired mid-stream (using `tokio::time::pause`).

### Step 5 — `AgentLoop` struct + channel topology

In `src/agent.rs`, implement the outer-loop container (Q9/Q18/Q22), exactly the
shape from §4B:

```rust
pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    cmd_tx: mpsc::Sender<AgentCommand>,
    ui_rx: mpsc::Receiver<UiUpdate>,
}

impl AgentLoop {
    pub fn new(provider: Arc<dyn Provider>) -> (Self, mpsc::Receiver<UiUpdate>);
    pub async fn send_command(&self, cmd: AgentCommand);
    pub fn ui_updates(&self) -> mpsc::Receiver<UiUpdate>;
    pub fn run(self) -> tokio::task::JoinHandle<anyhow::Result<Context>>;
}
```

- `new` creates the three internal channels: `cmd_tx`/`cmd_rx` (capacity 10),
  `ui_tx`/`ui_rx` (capacity 100), and the inner `agent_tx`/`agent_rx`
  (capacity 10, created inside the spawned loop or passed in).
  Returns `(Self, ui_rx)` per the documented API.
- `run(self)` spawns the **outer task** (the AgentLoop `loop { select! { cmd_rx,
  agent_rx } }` from §4B, minus tool arms), owning `Context`, `AgentMode`,
  `current_cancel: Option<CancellationToken>`, and
  `pending_steers: VecDeque<String>`.
- **Read-only final-context access (Q-B, confirmed):** `run()` returns a
  `JoinHandle<anyhow::Result<Context>>`; the loop **returns its `Context`** as
  the task's output when it shuts down. Shutdown is triggered by dropping the
  `AgentLoop` (Q15): dropping it closes `cmd_tx`, so `cmd_rx.recv()` yields
  `None` and the `select!` exits the loop, after which the task returns the
  final committed `Context`. `main.rs` and the integration tests then read the
  final history via `handle.await?` — no `Arc<Mutex<Context>>`, no shared
  mutable state. The caller must keep the returned `ui_rx` alive/drain it so
  the inner tasks can flush their sends.

**Definition of done:** the loop skeleton compiles and runs to a clean shutdown;
`main` can construct it, send a `Prompt`, drop the `AgentLoop`, and `await` the
handle to obtain the final `Context`.

### Step 6 — Outer loop: Prompt/Stop handling, steering, interrupt

Implement the `select!` logic from §4B lines 740–827, restricted to Phase 1:

- `AgentCommand::Prompt(text)`:
  - If `text.is_empty()` → no-op (Q26).
  - If `Idle`: `context.push_user(text)`, set `Streaming`,
    `current_cancel = start_request(...)`.
  - Else (`Streaming`): **steer** → `pending_steers.push_back(text)`; do **not**
    cancel. (No `SteerQueued` `UiUpdate` in Phase 1.)
- `AgentCommand::Stop`: clear `pending_steers`; `current_cancel.take()?.cancel()`.
- `AgentEvent::StreamDone { partial }`: `context.push_assistant(partial,
  Complete)`; then if `inject_pending_steers` returns true, start a fresh
  request (stay `Streaming`); else go `Idle`.
- `AgentEvent::StreamCancelled { partial }`: only append as
  `push_assistant(partial, Interrupted)` and go `Idle` when `current_cancel` is
  `None` (ignores stale cancellation from a replaced stream, per §8 Pitfall #6).
- `AgentEvent::StreamError(e)`: clear `pending_steers`, go `Idle`,
  `ui_tx.send(UiUpdate::Error(e))`.

Implement `start_request` as a free function (from §4B lines 656–697, tool
notations removed). It takes an immutable snapshot clone
(`context.messages().clone()`), spawns the **inner streaming task**, which
first sends `UiUpdate::StreamStarted`, then drains the provider's
`Receiver<StreamEvent>`:

- `Text(s)` → accumulate `partial`, `ui_tx.send(UiUpdate::AppendText(s))`.
- `Done` → `ui_tx.send(UiUpdate::StreamDone)`,
  `agent_tx.send(AgentEvent::StreamDone { partial })`.
- `Cancelled` → `ui_tx.send(UiUpdate::StreamCancelled)`,
  `agent_tx.send(AgentEvent::StreamCancelled { partial })`.
- `Error(e)` → `ui_tx.send(UiUpdate::Error)`,
  `agent_tx.send(AgentEvent::StreamError(e))`.

Implement `inject_pending_steers` (from §4B lines 701–707): pop each queued
steer and `context.push_user`; return true if any were injected.

**Shutdown & final context (Q-B):** the loop body returns `Ok(context)` at the
top of the iteration once `cmd_rx.recv()` yields `None` (i.e. `AgentLoop` was
dropped). No infinite block after shutdown; the returned `Context` is the
read-only final history. Replace the `loop { select! }`'s implicit tail with an
explicit `break`/`return` on the `None` arm of the `cmd_rx.recv()` branch.

**Definition of done:** the full loop compiles with `make lint` clean; behavior
verified by the integration tests in Step 8.

### Step 7 — `src/lib.rs` + thin `src/main.rs`

- `src/lib.rs`: `pub mod agent; pub mod llm;` and re-export the public items
  the tests need (`AgentLoop`, `Context`, `Message`, `AssistantStatus`,
  `AgentCommand`, `UiUpdate`, `Provider`, `MockProvider`, `StreamEvent`,
  `AgentMode`) so `tests/*.rs` can reach them (AGENTS.md "Binary + library
  split": tests cannot `use` from `main.rs`).
- `src/main.rs` (thin, per the blueprint): build a `MockProvider`, construct
  `AgentLoop::new`, `rt.block_on(...)`, send a representative default demo
  prompt (e.g. `"What makes the sky look blue?"`), walk `ui_rx` printing
  appends to stdout, and on exit **print the final conversation history** to
  verify state transitions (blueprint §7 Phase 1 "Print final conversation
  history" bullet). `main.rs` reads the final history from the `JoinHandle`
  that `run()` returns (Q-B): drop the `AgentLoop`, then `handle.await?` and
  print each `Message` in the returned `Context`.
- `main.rs` is excluded from the coverage threshold by the Makefile.

**Definition of done:** `make build-debug-and-run` prints a streamed mock
response and the final history, then exits with code 0.

### Step 8 — Integration tests (`tests/`)

Add `tests/common/mod.rs` (Q25) containing the shared `MockProvider` helper
(actually import the lib's `MockProvider`) and integration tests in `tests/`:

- **Steering test:** `new(mock)`, `send_command(Prompt "first")`; under
  `tokio::time::pause`, consume `ui_rx` until `StreamStarted` … while
  `Streaming`, `send_command(Prompt "steer")`; `advance` the mock interval;
  assert sequence:   first response `Done`, then a new `StreamStarted` for the
  steer — evidence the live stream was **not** cancelled and the steer was
  injected at the natural point. Assert the final context via
  `handle.await?` (Q-B) == `[System, User(first), Assistant(first, Completed),
  User(steer)]`.
- **Interrupt test:** `new(mock)` with several tokens; send `Prompt`; consume a
  partial; `send_command(Stop)`; `advance`; assert a `StreamCancelled`
  eventually arrives and final context (via `handle.await?`) ==
  `[System, User(first), Assistant(partial, Interrupted)]` with **no** leftover
  pending steer, i.e. a subsequent context query shows the steer queue drained.
- **Empty-prompt test:** `send_command(Prompt "")` is a no-op (no `StreamStarted`
  emitted); final context stays `[System]`.
- **Concurrent-steer-during-stream no-deadlock test:** send multiple `Prompt`s
  mid-stream, assert the run completes without hanging (bound by the test time).

**Definition of done:** `make test` green; `make test-coverage` ≥ 80% line
coverage on `agent.rs` and `llm.rs` (the only testable files; `main.rs`
excluded, `ui.rs` absent).

### Documentation step (part of the change set)

Update, in the same commits as the code:

- `docs/features.md`: add the Phase 1 headless kernel as an implemented feature
  (headless agent loop, steering, interrupt, mock provider). Keep it minimal and
  factual.
- `docs/blueprint.md`: tick the Phase 1 checklist checkboxes (lines 1945–1990)
  that this plan completes.
- Per AGENTS.md development-workflow, before executing check the change against
  `docs/architecture-design-records/`; since no ADR exists yet, evaluate whether
  the first source change warrants a new ADR (e.g. "binary + library split",
  "agent loop channel topology"). **Ask before creating a new ADR file** (docs
  that don't exist should not be created without consent).

**Definition of done:** `docs/` files are consistent with the implemented code;
`rumdl` on the changed markdown passes (see `.rumdl.toml`); `make lint`,
`make test`, `make test-coverage` all green.

---

## Execution Notes

Recorded while executing this plan (the implemented code is the source of
truth; these notes keep the plan from being the stale doc):

- **`Provider::chat` returns a boxed future.** The planned
  `-> impl Future + Send` return position (RPITIT) makes the trait **not**
  `dyn`-compatible, so `Arc<dyn Provider>` (required by `AgentLoop`/`main`)
  cannot be built. Implemented as `BoxFuture<'a, …>` (via the `futures`
  crate) — still a native async trait, no `async-trait` crate.
- **`AgentLoop` run/shutdown shape.** The planned struct field `ui_rx` cannot
  coexist with `new() -> (Self, Receiver<UiUpdate>)` (a single mpsc receiver
  cannot be moved twice), and `run(self)` would move `cmd_tx` into the task,
  making "drop the `AgentLoop` to shut down" impossible. Implemented:
  `run(&mut self)` takes `cmd_rx`/`agent_rx` out of the struct via
  `Option::take`; the caller keeps `AgentLoop` (and thus `cmd_tx`) and shuts
  the loop down by dropping it. `ui_updates()` was omitted since `new()`
  already hands the receiver to the caller.
- **`tokio::select!` is `biased` with the `agent_rx` branch first** so a
  queued lifecycle event (e.g. the final `StreamDone`) is applied to the
  context before the loop returns it on shutdown — makes final-history
  assertions deterministic.
- **Coverage at execution time:** `agent.rs` 94.96% lines, `llm.rs` 98.85%
  lines, total 91.69% (≥ 80% threshold).

---

## Proposed Improvements

- **Deterministic clock in all tests:** use `tokio::time::pause` + `advance`
  everywhere so tests are instant and not flaky, matching the blueprint's Q21
  intent.
- **Add a lightweight "session finished" signal:** instead of relying on an
  empty `ui_rx` / `select!` timeout to know a turn ended, consider a
  `UiUpdate::StreamDone` already serves this in Phase 1; keep it as-is now.
- **Document the cancel-vs-steer state machine once** in a short doc comment on
  `AgentLoop` so future phases (tool approval) extend it without re-deriving the
  rules from validation/tests.
- **SOUL.md-driven system prompt (future, NOT Phase 1):** the initial system
  message of a chat should eventually be loaded from a **SOUL.md** file,
  customizable per global (`~/.ylva/`) and per project (`.ylva/`) config. Phase
  1 seeds a fixed `DEFAULT_SYSTEM_PROMPT` constant as SSOT; a later config phase
  (see Phase 6 §4G layering) replaces the constant's source with a SOUL.md
  loader without changing call sites. Out of scope for this plan.

---

## Implementation Hints

- **Repo state:** no Rust code exists yet; this plan creates the crate from
  scratch. `Makefile` targets (`build-*`, `test`, `test-coverage`, `lint`,
  `format`) are already present and assume a `Cargo.toml` with package `ylva`
  and the `src/main.rs`/`src/lib.rs` split.
- **Crate split (AGENTS.md):** `src/main.rs` = `[[bin]]`, `src/lib.rs` =
  `[lib]`, both named `ylva` via Cargo auto-discovery. Integration tests in
  `tests/` must import public items **from the lib** (`ylva::…`), never from
  `main.rs`.
- **Coverage:** Makefile `COVERAGE_THRESHOLD := 80`; `make test-coverage` uses
  `cargo llvm-cov` with `--ignore-filename-regex "(ui|main)\.rs"`. Phase 1
  testable files are `src/agent.rs` and `src/llm.rs`, so those must reach ≥ 80%
  line coverage (drive them from integration tests in `tests/` plus unit tests).
- **Allowed crates (Q23, amended by Q-C):** `tokio` (`full`), `tokio-util`
  (`CancellationToken` from `tokio_util::sync`), `futures`, `thiserror`,
  `anyhow`, plus `uuid` (`v4`) and `serde_json` added to carry the full §4B
  `Message` skeleton. The Phase 1 `Message` enum thus includes
  `System`/`User`/`Assistant{content,status,tool_calls}` plus `ToolResult`/
  `ToolError` and the `ToolCallRequest` struct — all real, typed public API
  (re-exported via `lib.rs`), so no `#[allow(dead_code)]` and no `todo!()` is
  needed.
- **Editions/Rust:** `edition = "2021"`; native async trait in the trait
  definition (no `async-trait`).
- **Channel capacities:** from §4B — `cmd` 10, `ui` 100, `agent` 10.
- **Cancellation pattern (Q20/§8 Pitfall #6):** outer loop owns the token,
  passes a clone into the provider; `current_cancel.take()?.cancel()` on `Stop`;
  the provider task emits `Cancelled`; the outer loop ignores a stale
  `StreamCancelled` when `current_cancel` is still `Some`.
- **Steering (Q/§1, §4B):** a `Prompt` while `Streaming` is never an abort —
  it is queued and injected at the next natural point (`StreamDone` in Phase 1).
  Only `Stop` aborts.
- **`tokio::time::pause` availability:** requires the `test-util` feature on the
  `tokio` **dev-dependency**; the `full` feature does not enable it.
- **Mock determinism (Q-D):** `MockProvider::new(tokens: Vec<String>, interval:
  Duration)`; `chat` returns a receiver produced by an **internal spawned task**
  that also owns the cancel polling. Sleeps between tokens must be cancel-aware
  (select on `cancel.cancelled()`).
- **Final-context access (Q-B):** `run()` returns
  `JoinHandle<anyhow::Result<Context>>`; the loop returns the `Context` on
  shutdown, which is triggered by dropping the `AgentLoop` (closes `cmd_tx` →
  `cmd_rx.recv()` yields `None`). `main.rs`/tests print or assert the final
  history via `handle.await?`.
- **Default system message (Q-A):** `Context::new()` seeds
  `Message::System` with the fixed constant
  `"You are Ylva, a helpful agent with tool capabilities."` (a named module
  constant, SSOT; future SOUL.md sourcing is Phase 6 scope).
