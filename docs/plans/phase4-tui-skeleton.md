# Phase 4: TUI Skeleton — Rendering Layer

## Context

### System being changed

Ylva is a Rust-based terminal AI agent harness. After Phases 1–3, it has a fully
proven headless `AgentLoop` that handles cooperative steering, hard interruption,
a ReAct tool loop, and a streaming provider abstraction. The current `main.rs`
drives the agent via `println!`/`eprintln!` — no terminal UI exists yet.

### Goal and motivation

Add a responsive terminal UI as a **rendering layer** on top of the existing
headless kernel — it does not change the agent's architecture. The TUI must
remain responsive at all times: the input box is never disabled, `Enter` sends
unconditionally even while the model is streaming (queued as a pending steer),
and `Ctrl+C`/`Esc` interrupts the current round.

This is blueprint §7 Phase 4 ("TUI Skeleton — Rendering Layer Added").

### Acceptance criteria (definition of done)

- `cargo run` on a terminal launches a `ratatui` TUI with: a scrollable chat
  history area, a variable-height input box (capped at 1/3 of terminal height),
  and a 2-row footer.
- No sidebar is included in Phase 4; it is deferred to a later phase.
- Typing and pressing `Enter` sends the message; the user bubble appears in the
  chat history immediately, before the model responds.
- `Enter` sent while the model is streaming queues a pending steer; the bubble
  is rendered in a visually distinct "pending" style until `SteerApplied`
  converts it to normal. Interrupting (`Ctrl+C`/`Esc`) removes all pending
  bubbles from the chat history.
- `Ctrl+C` and `Esc` use a **double-press with a 2-second window**:
  - While streaming: 1st press → hint in footer `"Press again to cancel"`; 2nd
    press ≤2 s → `AgentCommand::Stop`.
  - While idle: `Ctrl+C` 1st press → hint `"Press again to quit"`; 2nd press
    ≤2 s → quit. `Esc` while idle → clear the input field only (no quit).
  - Help popup open → `Esc` closes the popup immediately (no confirm needed).
  - OS SIGINT (from the `ctrlc` handler) → `AgentCommand::Stop` immediately,
    no double-press required.
- A keybinding (`F1`) opens/closes a **Help popup** displaying the full
  keybinding table and `BuildInfo` values (version, commit, build timestamp,
  profile).
- Pressing `↑` when the cursor is on the first line of the input box loads the
  previous sent user message (including pending steers); `↓` on the last line
  moves forward, restoring the live draft at the newest position. Any edit
  keypress while browsing copies the entry to a fresh draft and exits browsing
  mode. Pending steer messages are included in the history.
- `cargo run` on a non-terminal stdout (piped or redirected) falls back to the
  existing headless behaviour — no ratatui initialisation, identical output to
  today's `main.rs`.
- `make lint` and `make test` pass; ≥ 80 % line coverage maintained (excluding
  `ui.rs` and `main.rs`; `headless_runner.rs` and `tui_runner.rs` are included).
- `docs/features.md`, `docs/keybindings.md`, and `docs/blueprint.md` (Phase 4
  task checklist) are updated in the same commit.

### Key constraints

- **`AgentLoop` is unchanged.** The TUI is purely a consumer of `UiUpdate` and
  a sender of `AgentCommand`. No modifications to the agent state machine.
- **Single-writer `Context`.** The inner streaming task never touches `Context`
  directly; this invariant is not weakened.
- **`UiUpdate::SteerQueued` and `UiUpdate::SteerApplied`** are the only two
  new variants added to `agent.rs` (deferred from Phase 3 in the blueprint).
  They are **pure mode signals** — no payload; the TUI driver creates chat
  bubbles, not the agent.
- **No `App` trait abstraction yet.** The formal `App` trait (`TuiApp` /
  `ConsoleApp`) arrives in Phase 5. Phase 4 uses a plain `is_terminal()` check
  in `main.rs` to dispatch between `tui_runner` and `headless_runner`.
- **`ui.rs` and `main.rs` are excluded from coverage** per `development-workflow.md`.
  `headless_runner.rs` and `tui_runner.rs` are covered.
- **No sidebar.** The sidebar is deferred entirely; no stub code is added.
- Spike 1 (`spikes/spike-tui-responsiveness`) validated the core TUI pattern —
  `spawn_blocking` crossterm bridge + bounded `mpsc` + `tokio::select!` +
  batch-drain per frame. Phase 4 inherits that pattern directly.

### Background

- Blueprint §1 (architecture diagram), §4B (non-blocking ReAct loop), §4F
  (rich chat rendering), §7 Phase 4 (phased build plan) are the authoritative
  specification.
- Spike 1 validated: bounded `mpsc` + `spawn_blocking` crossterm bridge +
  `select!` + batch-drain achieves ≤ 17 ms frame times and no consumer
  starvation at 1000+ msg/s.
- `UiUpdate` (in `src/agent.rs`) currently carries: `AppendText`, `StreamStarted`,
  `StreamDone`, `StreamCancelled`, `Error`, `ToolStart`, `ToolPending`, `ToolDone`,
  `ToolError`, `ToolKilled`. `SteerQueued` and `SteerApplied` are absent —
  they were listed as "Phase 3+" comments and are not yet emitted.
- `docs/keybindings.md` already contains a Phase 4 placeholder that names the
  Help popup but marks the keybinding as TBD.
- `ratatui-textarea 0.9` is a rewrite of `tui-textarea` targeting the new
  `ratatui-core` sub-crate architecture. It is compatible with `ratatui 0.30`
  via shared `ratatui-core ^0.1.x`. It supports multiline input, mouse events,
  selection, and clipboard out of the box.

## Implementation Plan

### Step 1 — Add crate dependencies

Add to `Cargo.toml` under `[dependencies]`:

```toml
ratatui         = { version = "0.30", features = ["crossterm"] }
crossterm       = "0.29"
ratatui-textarea = { version = "0.9", features = ["crossterm"] }
textwrap        = "0.16"
ctrlc           = { version = "3", features = ["termination"] }
```

Verify `cargo check` still passes after adding crates.

**Test:** `make lint` (cargo check + clippy) passes — no API breakage.

### Step 2 — Add `SteerQueued` and `SteerApplied` to `UiUpdate`

In `src/agent.rs`:

1. Add two new variants to `pub enum UiUpdate`:
   ```rust
   /// A steer (user message while busy) was queued. No payload — the TUI
   /// driver creates the pending bubble itself before sending the command.
   SteerQueued,
   /// All previously queued steers have been injected into the LLM context.
   SteerApplied,
   ```
2. In the `AgentCommand::Prompt` handler (busy/streaming branch), after
   `pending_steers.push_back(text)`, emit:
   ```rust
   let _ = ui_tx.send(UiUpdate::SteerQueued).await;
   ```
3. In the `if inject_pending_steers(...)` branch inside the
   `AgentEvent::StreamDone` arm (around `src/agent.rs` line 425), after the
   call, emit:
   ```rust
   let _ = ui_tx.send(UiUpdate::SteerApplied).await;
   ```
4. Re-export through `src/lib.rs` — no change needed; `UiUpdate` is already
   re-exported in full.
5. In the future `headless_runner.rs` (Step 7), add no-op match arms for
   `SteerQueued` and `SteerApplied` to avoid lint warnings.

**Tests:**
- Add a unit test in `src/agent.rs` that sends a `Prompt` while a stream is
  in flight and verifies: `SteerQueued` is emitted before `StreamDone`;
  `SteerApplied` is emitted after `StreamDone` when the steer is injected.
- Extend `tests/steering.rs` integration tests to assert on these new events
  in sequence.

### Step 3 — Create `src/events.rs` — crossterm event bridge

Create `src/events.rs` with:

```rust
pub enum AppAction {
    /// Shortcut: Ctrl+C (confirm-to-stop/quit, depending on mode).
    CtrlC,
    /// Shortcut: Escape (mode-dependent — see run_tui dispatch).
    Escape,
    /// Shortcut: F1 — toggle Help popup.
    ShowHelp,
    /// Shortcut: Ctrl+Q — not used in Phase 4 (quit via Ctrl+C double-press).
    /// Reserved variant; may be wired in a later phase.
    // Quit,
    /// All other key events forwarded to the main task for textarea input.
    KeyInput(crossterm::event::KeyEvent),
    /// Mouse events forwarded to the main task for textarea input.
    MouseInput(crossterm::event::MouseEvent),
    /// Terminal resize.
    Resize { width: u16, height: u16 },
}
```

Implement `spawn_crossterm_bridge(action_tx: mpsc::Sender<AppAction>) -> JoinHandle<()>`
using `tokio::task::spawn_blocking`. It reads `crossterm::event::read()` in a
tight loop and maps events:

| Input                | `AppAction`          |
|----------------------|----------------------|
| `Ctrl+C`             | `CtrlC`              |
| `Esc`                | `Escape`             |
| `F1`                 | `ShowHelp`          |
| Any other `KeyEvent` | `KeyInput(event)`    |
| `MouseEvent`         | `MouseInput(event)`  |
| `ResizeEvent`        | `Resize { w, h }`    |

**All context-dependent logic** (what `Esc` means, whether `Ctrl+C` cancels
or quits, double-press counting) lives in the main task, not here.

**Tests:** Unit tests using synthetic `crossterm::event::KeyEvent` values.
Cover each shortcut (Ctrl+C, Esc, F1) and a regular character event
that must produce `KeyInput`. No real terminal needed.

### Step 4 — Create `src/app.rs` — TUI application state

Create `src/app.rs`.

#### `ChatEntry` enum

```rust
pub enum ChatEntry {
    UserMessage    { text: String, pending: bool },
    AssistantMessage { text: String, interrupted: bool },
    ToolLine       { label: String },
    ErrorLine      { message: String },
}
```

#### `TuiState` struct

```rust
pub struct TuiState {
    pub agent_mode:             AgentMode,
    pub chat_entries:           Vec<ChatEntry>,
    pub partial_assistant_text: String,
    pub show_help:              bool,
    pub steer_indicator:        bool,  // true while ≥1 steer is queued
    pub status_hint:            Option<String>,  // ephemeral footer hint (double-press)
    pub scroll_offset:          usize,
    pub scroll_locked:          bool,  // true = auto-scroll to bottom
    pub chat_area_height:       u16,   // updated on every resize
    // Input history navigation
    pub input_history:          Vec<String>,  // all UserMessage texts, oldest→newest
    pub history_index:          Option<usize>, // None = live draft; Some(i) = browsing
    pub draft:                  String,        // saved live draft while browsing
}
```

`scroll_locked` starts `true`. Scrolling up sets it `false`; pressing `End`
or receiving `StreamDone`/`StreamCancelled` resets it to `true`.

#### `TuiState::apply_update`

```rust
pub fn apply_update(&mut self, update: &UiUpdate)
```

| `UiUpdate` variant     | Mutation                                                                         |
|------------------------|----------------------------------------------------------------------------------|
| `StreamStarted`        | `agent_mode = Streaming`; `partial_assistant_text = ""`                          |
| `AppendText(s)`        | append to `partial_assistant_text`; auto-scroll if `scroll_locked`               |
| `StreamDone`           | commit partial → `AssistantMessage { interrupted: false }`; `agent_mode = Idle`; re-lock scroll |
| `StreamCancelled`      | commit partial → `AssistantMessage { interrupted: true }`; remove all `UserMessage { pending: true }` entries; `steer_indicator = false`; `agent_mode = Idle`; re-lock scroll |
| `SteerQueued`          | `steer_indicator = true` (no bubble — driver already pushed it)                  |
| `SteerApplied`         | mark all `pending: true` entries → `pending: false`; `steer_indicator = false`   |
| `ToolPending { name, .. }` | push `ToolLine { label: format!("[tool pending] {name}") }`                 |
| `ToolStart { name, arguments, .. }` | push `ToolLine` with rendered invocation                          |
| `ToolDone { name, .. }` | push `ToolLine { label: format!("[tool done] {name}") }`                       |
| `ToolError { error, .. }` | push `ErrorLine { message: error }`                                          |
| `ToolKilled { .. }`    | push `ToolLine { label: "[tool killed]".into() }`                                |
| `Error(msg)`           | push `ErrorLine { message: msg }`; `agent_mode = Idle`                           |

#### `TuiState` driver helpers

```rust
/// Called by the driver before sending AgentCommand::Prompt.
/// pending = true when agent_mode != Idle (steer); false otherwise.
/// Also appends text to input_history and resets history_index to None.
pub fn push_user_bubble(&mut self, text: String, pending: bool)

/// Called when the user scrolls up — disables auto-scroll.
pub fn scroll_up(&mut self, lines: usize)

/// Called on End key or scroll-to-bottom event — re-enables auto-scroll.
pub fn scroll_to_bottom(&mut self)

/// Navigate input history. Returns the text to load into the textarea,
/// or None if the boundary was already reached (no change).
///
/// `direction == Up`:
///   - If history_index is None: save current textarea content as draft,
///     set history_index = Some(input_history.len() - 1), return that entry.
///   - If Some(0): already at oldest — return None (no change).
///   - If Some(i): decrement i, return input_history[i].
///
/// `direction == Down`:
///   - If history_index is None: already at live draft — return None.
///   - If Some(i) where i < input_history.len() - 1: increment i, return entry.
///   - If Some(i) at newest: set history_index = None, return draft.
///
/// Any non-navigation keypress while history_index is Some(_) copies the
/// current entry into the live draft and exits history mode (history_index = None)
/// before processing the keypress normally — handled in the key dispatch function.
pub fn navigate_history(&mut self, direction: HistoryDirection, current_text: &str) -> Option<String>

pub enum HistoryDirection { Up, Down }
```

**Tests:** Unit tests for `apply_update` covering every variant; `push_user_bubble`
pending/non-pending and `input_history` growth; scroll lock toggling;
`StreamCancelled` removes pending bubbles; `SteerApplied` clears pending flag;
`navigate_history` boundary conditions (empty history, oldest, newest, draft
restore, any-key exits browsing mode).

### Step 5 — Create `src/ui.rs` — ratatui layout and widgets

Create `src/ui.rs` with a single public function:

```rust
pub fn draw(frame: &mut ratatui::Frame, state: &TuiState, textarea: &ratatui_textarea::TextArea<'_>)
```

#### Layout

```
┌─────────────────────────────────────────────────────────────┐
│  Chat area (scrollable, fills remaining height)             │
├─────────────────────────────────────────────────────────────┤
│  Input box (ratatui-textarea, 1–⌊height/3⌋ rows)           │
├─────────────────────────────────────────────────────────────┤
│  Footer row 1: mode indicator + steer indicator             │
│  Footer row 2: key hints                                    │
└─────────────────────────────────────────────────────────────┘
```

Compute `input_height = textarea.lines().len().max(1).min(frame.size().height / 3)`.
The chat area receives all remaining rows.

#### Chat area

`ratatui::widgets::Paragraph` with `Line`/`Span` composition:

- User messages: bold; prefix `"You: "`.
- Pending user messages: dimmed/italic; prefix `"You (queued): "`.
- Assistant messages: normal text; partial/streaming text shown inline as it
  arrives.
- Interrupted assistant messages: faded suffix `"…"`.
- Tool lines: cyan; prefix as set in `ChatEntry::ToolLine.label`.
- Error lines: red; prefix `[error]`.
- Wrap with `textwrap` at `frame.size().width - 2`.
- Scroll offset driven by `state.scroll_offset`.

#### Input box

Rendered directly from the `ratatui-textarea` widget. Border title:
`"Input"` normally; `"Steering…"` when `state.steer_indicator` is true.

#### Footer (2 rows)

- **Row 1:** `" [mode] Idle  ⋮ 1 steer queued"` — mode word reflects
  `state.agent_mode`; steer count shown only when `state.steer_indicator`.
  When `state.status_hint` is `Some`, override row 1 content with the hint
  text (e.g. `"Press Ctrl+C again to cancel"`).
- **Row 2:** `" Enter: send  Ctrl+C: cancel/quit  F1: help"`.

#### Help popup

When `state.show_help`, render a `ratatui::widgets::Clear` + centered `Block`
(60 % width, 70 % height) with two sections:

1. **Keybindings** — the full keybinding table (same content as
   `docs/keybindings.md` for Phase 4), rendered as a two-column list.
2. **Build info** — `BuildInfo::render()` output (version, commit, build
   timestamp, profile).

Footer line at the bottom of the popup: `"F1 or Esc to close"`.

**Tests:** `ui.rs` is excluded from coverage per `development-workflow.md`.
Visual correctness is verified manually.

### Step 6 — Create `src/tui_runner.rs` — TUI event loop

Create `src/tui_runner.rs`:

```rust
pub async fn run_tui<B: ratatui::backend::Backend>(
    terminal: &mut ratatui::Terminal<B>,
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    initial_prompt: Option<String>,
) -> anyhow::Result<()>
```

The production entry point `run_tui_on_stdout` (called from `main.rs`) creates
a real `CrosstermBackend` terminal, enables raw mode + alternate screen, calls
`run_tui`, then **must restore the terminal on every exit path** — including
panics and errors. Use a RAII guard or a `scopeguard`/`defer!` macro wrapping
the cleanup call so restoration cannot be skipped.

Responsibilities of `run_tui`:

1. Construct `AgentLoop::new(provider, registry)`.
2. Spawn the crossterm bridge (`src/events.rs`) — bounded `mpsc` channel
   (capacity 64).
3. Install a `ctrlc` handler: clone `cmd_tx`, bridge to tokio via a
   `std::sync::mpsc` + `tokio::task::spawn_blocking` relay, send
   `AgentCommand::Stop` on SIGINT — no double-press, immediate.
4. Call `agent.run()` to get the `JoinHandle`.
5. If `initial_prompt` is `Some`, call `state.push_user_bubble(text, false)`
   then send `AgentCommand::Prompt`.
6. Hold a `confirm_state: Option<(ConfirmKind, Instant)>` for the double-press
   confirmation window:
   ```rust
   enum ConfirmKind { CancelStreaming, Quit }
   ```
7. Enter the main `tokio::select!` loop:

   ```
   loop {
     select! {
       action = action_rx.recv() => handle_action(action, ...),
       _ = ui_rx.recv_many_until_empty() (batch drain) => { ... }
     }
     terminal.draw(|f| ui::draw(f, &state, &textarea))?;
   }
   ```

   Key dispatch via a standalone `fn handle_action(action, state, textarea,
   confirm_state, cmd_tx, ...)` — **not inline** in `select!` — so future input
   modes (e.g. `clarify` widget) can extend it without refactoring.

   `CtrlC` / `Escape` handling in `handle_action`:

   | Event    | Agent mode  | Popup open | `confirm_state` | Action |
   |----------|-------------|------------|-----------------|--------|
   | `Escape` | any         | yes        | any             | close popup (`state.show_help = false`) |
   | `Escape` | `Idle`      | no         | any             | clear textarea |
   | `Escape` | `Streaming`/`ToolPending` | no | None | set `confirm_state = Some((CancelStreaming, Instant::now()))`; `state.status_hint = Some("Press Esc again to cancel")` |
   | `Escape` | `Streaming`/`ToolPending` | no | Some((CancelStreaming, t)) if elapsed ≤2s | send `AgentCommand::Stop`; clear `confirm_state` + `status_hint` |
   | `CtrlC`  | `Streaming`/`ToolPending` | no | None | set confirm; hint `"Press Ctrl+C again to cancel"` |
   | `CtrlC`  | `Streaming`/`ToolPending` | no | Some((CancelStreaming, t)) ≤2s | send `AgentCommand::Stop` |
   | `CtrlC`  | `Idle`      | no         | None            | set `confirm_state = Some((Quit, Instant::now()))`; hint `"Press Ctrl+C again to quit"` |
   | `CtrlC`  | `Idle`      | no         | Some((Quit, t)) ≤2s | return (quit) |

   Expired `confirm_state` (elapsed > 2s): clear it and clear `status_hint`
   before processing the new press (treat as first press).

   `Enter` handling (plain, no modifier):
   - Read `textarea.lines().join("\n")`.
   - If non-empty: `state.push_user_bubble(text, agent_mode != Idle)`;
     send `AgentCommand::Prompt(text)`; reset textarea.
   - `push_user_bubble` also resets `history_index` to `None` and clears `draft`.

   `Up` / `Down` arrow handling:
   - Determine whether the cursor is on the first line (`Up`) or last line
     (`Down`) of the textarea by comparing the textarea's cursor row against
     `0` or `textarea.lines().len() - 1`.
   - If the boundary condition is met: call
     `state.navigate_history(direction, current_text)`. If `Some(text)` is
     returned, replace the textarea content with that text and move the cursor
     to the end. If `None`, do nothing (boundary already reached).
   - If the cursor is not on the boundary line: pass the event to
     `textarea.input(event)` as normal.

   Any non-navigation `KeyInput` while `state.history_index` is `Some(_)`:
   - Call `state.navigate_history` logic to copy the browsed entry into
     `draft` and reset `history_index = None` (exit browsing mode), then
     process the keypress with `textarea.input(event)` normally.

   `Shift+Enter` / `Alt+Enter` and all other `KeyInput` → `textarea.input(event)`.

   `MouseInput` → `textarea.input(event)`.

   Batch-drain `ui_rx` (up to 64 per frame) via `try_recv()` calling
   `state.apply_update()` on each.

8. On `AppAction::Quit` (or `ui_rx` closed), exit the loop.
9. Drop `agent` to close `cmd_tx`; await the `JoinHandle`.

**Tests:** `tests/tui_state.rs` integration test using `MockProvider`:
- `run_tui` called with a `TestBackend` terminal.
- Drive via `AppAction` channel (bypass crossterm bridge).
- Assert: `SendMessage` (modelled as plain-`Enter` key) → `TuiState` has
  `UserMessage` + eventually `AssistantMessage`.
- Assert: `Stop` mid-stream → `AgentMode` transitions to `Idle`.
- Assert: steer cycle (`push_user_bubble(pending=true)` + `SteerQueued` +
  `SteerApplied`) leaves no pending bubbles in `chat_entries`.
- Assert: `StreamCancelled` removes pending bubbles.
- Assert: `draw` does not panic on any of the above states.

### Step 7 — Extract headless path to `src/headless_runner.rs`

Create `src/headless_runner.rs` and move the `ui_rx` drain loop from `main.rs`
into it:

```rust
pub async fn run_headless(
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    initial_prompt: String,
) -> anyhow::Result<()>
```

This function receives a pre-built provider + registry (no env-var parsing
inside). Provider selection and `initial_prompt` resolution stay in `main.rs`.

The drain loop logic is **identical** to today's headless path in `main.rs`.
Add no-op match arms for `UiUpdate::SteerQueued` and `UiUpdate::SteerApplied`.

`main.rs` becomes:

```rust
fn main() {
    // --version early exit (unchanged)
    let provider = select_provider();
    let registry = Arc::new(default_registry());
    let prompt   = initial_prompt(args);
    let runtime  = tokio::runtime::Runtime::new()?;

    if std::io::stdout().is_terminal() {
        runtime.block_on(tui_runner::run_tui_on_stdout(provider, registry, prompt))
    } else {
        runtime.block_on(headless_runner::run_headless(provider, registry, prompt.unwrap_or_default()))
    }
}
```

**Tests:**
- Existing integration tests exercise `AgentLoop` directly — no changes needed.
- `headless_runner.rs` is included in coverage (≥ 80 %); add tests in
  `tests/headless_runner.rs` that drive it with `MockProvider` and verify
  stdout output via captured output or event-counting.

### Step 8 — Export new modules from `src/lib.rs`

Add to `src/lib.rs`:

```rust
pub mod app;
pub mod events;
pub mod headless_runner;
pub mod tui_runner;
pub mod ui;
```

Re-export `TuiState`, `ChatEntry`, and `AppAction` at the crate root for use
in integration tests (`tests/tui_state.rs`, `tests/headless_runner.rs`).

### Step 9 — Update documentation

Update the following docs **in the same commit** as the code changes:

1. **`docs/blueprint.md`** — mark all Phase 4 tasks `[x]` (done).
2. **`docs/features.md`** — add a `## Phase 4: TUI skeleton` section covering:
   layout (chat + variable input box + 2-row footer), input-always-active
   guarantee, steer bubble lifecycle, pending-bubble removal on interrupt,
   Help popup, double-press confirmation, mode dispatch
   (terminal vs non-terminal), smart auto-scroll.
3. **`docs/keybindings.md`** — replace the Phase 4 placeholder with:

   | Key                | Context          | Action                                      |
   |--------------------|------------------|---------------------------------------------|
   | `Enter`            | Input focused    | Send message; queue steer if busy           |
   | `Shift+Enter` / `Alt+Enter` | Input focused | Insert newline in input box        |
   | `Ctrl+C` (×1)      | Streaming        | Show `"Press again to cancel"` hint         |
   | `Ctrl+C` (×2 ≤2 s) | Streaming       | Interrupt generation (`AgentCommand::Stop`) |
   | `Ctrl+C` (×1)      | Idle             | Show `"Press again to quit"` hint           |
   | `Ctrl+C` (×2 ≤2 s) | Idle            | Quit                                        |
   | `Esc`              | Help popup open | Close popup                                 |
   | `Esc`              | Idle             | Clear input field                           |
   | `Esc` (×1)         | Streaming        | Show `"Press again to cancel"` hint         |
   | `Esc` (×2 ≤2 s)    | Streaming        | Interrupt generation (`AgentCommand::Stop`) |
   | `F1`               | Any              | Toggle Help popup                          |
   | `↑` (cursor on first line)  | Input focused | Load previous user message into input; save draft |
   | `↓` (cursor on last line)   | Input focused | Load next user message; restore draft at newest   |
   | Any edit key while browsing | Input focused | Copy browsed entry to draft; exit browsing mode   |
   | `PageUp` / `↑`     | Chat focused     | Scroll up; disables auto-scroll             |
   | `PageDown` / `↓`   | Chat focused     | Scroll down                                 |
   | `End`              | Chat focused     | Scroll to bottom; re-enables auto-scroll    |

4. **`docs/development-workflow.md`** — update the coverage-exclusion list to
   reflect the new module names: `ui.rs` and `main.rs` excluded;
   `headless_runner.rs` and `tui_runner.rs` included.

## Proposed Improvements

- **`Ctrl+L` clear-screen.** Map `Ctrl+L` to `AppAction::Redraw` (clears and
  redraws the terminal) so the user can recover from terminal artefacts.

- **`Ctrl+Left`/`Ctrl+Right` word-jump.** `ratatui-textarea` supports this
  natively via `CursorMove::WordForward` / `WordBack`; ensure the `KeyInput`
  passthrough does not accidentally intercept these.

## Implementation Hints

- **Existing crossterm spike:** `spikes/spike-tui-responsiveness/` is the
  reference for the `spawn_blocking` + bounded-channel + `select!` +
  batch-drain pattern. Read it before writing `tui_runner.rs`.
- **`UiUpdate` location:** `src/agent.rs` lines 79–103. Insert `SteerQueued`
  and `SteerApplied` here.
- **`inject_pending_steers` call site:** `src/agent.rs` ~line 425 — the
  `if inject_pending_steers(...)` block inside `AgentEvent::StreamDone`.
  Emit `SteerApplied` after the call.
- **`SteerQueued` emit site:** `src/agent.rs` ~line 554 — the
  `pending_steers.push_back(text)` line. Emit `SteerQueued` after it.
- **Coverage exclusions:** `development-workflow.md` lists `ui.rs` and
  `main.rs`. `tui_runner.rs` and `headless_runner.rs` must meet ≥ 80 %.
- **`BuildInfo::render()`:** in `src/build_info.rs`; already used by
  `--version`; use in the "Build info" section of the Help popup.
- **`ratatui-textarea 0.9` API:** `TextArea::input(event)` processes
  `crossterm::event::Event` or `KeyEvent` in-place. `textarea.lines()` returns
  current content. Reset by replacing with `TextArea::default()` or using
  `move_cursor(CursorMove::Top)` + `delete_str(entire content)`.
- **`ratatui` `Paragraph` scroll:** `Paragraph::scroll((row_offset, 0))`.
  Compute offset as `total_content_lines.saturating_sub(visible_height)` for
  auto-scroll-to-bottom.
- **`ratatui::widgets::Clear`:** render this before the Help popup block to
  erase the underlying content in the popup area.
- **`ctrlc` handler and tokio bridging:** `ctrlc::set_handler` runs in a
  separate OS thread outside the tokio runtime. Bridge to tokio via a
  `std::sync::mpsc` relay consumed by a `spawn_blocking` watcher, or an
  `Arc<AtomicBool>` polled in the event loop. Never call `runtime.block_on()`
  inside the handler — this deadlocks when the runtime is single-threaded.
- **Terminal detection:** `use std::io::IsTerminal; std::io::stdout().is_terminal()`.
- **Module layout:** Rust 2018+ file-per-module style (`foo.rs` not
  `foo/mod.rs`) per `AGENTS.md` / `development-workflow.md`.
- **Crate versions (confirmed compatible):**
  - `ratatui = "0.30"` requires `crossterm ^0.29` and `ratatui-core ^0.1.2`.
  - `ratatui-textarea = "0.9"` requires `ratatui-core ^0.1.1` (resolves to
    `0.1.2`) — no conflict.
  - Pin `crossterm = "0.29"` explicitly to prevent version skew.
- **`tests/e2e/` contains shell scripts** (`.zsh`), not Rust integration tests.
  The new Rust integration tests go in `tests/tui_state.rs` and
  `tests/headless_runner.rs` at the crate root.
