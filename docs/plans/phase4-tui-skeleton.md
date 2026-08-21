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
  is rendered in a visually distinct "queued" style until `SteerApplied`
  converts it to normal. Interrupting (`Ctrl+C`/`Esc`) marks all pending
  bubbles as `Cancelled` with a faded "Not sent." footer annotation.
- **`Ctrl+C` and `Esc` use a double-press confirm with a 2-second window
  (`CONFIRM_TIMEOUT`). The confirm is key-agnostic: first press of either key
  sets confirm state; second press of either key within the window triggers the
  action:**
  - While streaming or `ToolPending`: first press → confirm state set (hint in
    footer `"Press again to cancel"`); second press ≤ 2 s →
    `AgentCommand::Stop`.
  - While idle: `Ctrl+C` first press → confirm state set (hint `"Press again
    to quit"`); second press ≤ 2 s → quit TUI. `Esc` while idle → clear input
    field only (no confirm, no quit).
  - Help popup open → `Esc` closes the popup immediately; confirm state is
    unchanged.
  - A natural mode transition to `Idle` (via `StreamDone`, `StreamCancelled`,
    or `Error`) clears confirm state immediately.
  - OS SIGINT (from the `ctrlc` handler) → `AgentCommand::Stop` immediately,
    no double-press required.
- **Tool approval sub-mode:** when a tool is awaiting approval
  (`pending_approval` is `Some`), the footer switches to
  `[ ✓ Approve (Enter) ]  [ ✗ Deny (Esc) ]` with clickable mouse buttons.
  In this sub-mode `Enter` sends `ApproveTool`, `Esc` sends `DenyTool`
  (immediate, no double-press). `Ctrl+C` still follows the double-press
  confirm → `AgentCommand::Stop`. Clicking outside approval buttons is a no-op.
- **User message visual design:** user messages (sent, queued, cancelled) are
  rendered as blocks with a colored left border and a soft background shade —
  no text title prefix. Border color: Blue (sent), Yellow (queued), DarkGray
  (cancelled). Cancelled messages display a dimmed `"Not sent."` footer line
  beneath the text. Colors are named constants ready to be extracted into a
  theme structure.
- A keybinding (`F1`) opens/closes a **Help popup** displaying the full
  keybinding table and `BuildInfo` values (version, commit, build timestamp,
  profile).
- Pressing `↑` when the cursor is on the first line of the input box loads the
  previous sent user message (including pending steers); `↓` on the last line
  moves forward, restoring the live draft at the newest position. Any edit
  keypress while browsing copies the entry to a fresh draft and exits browsing
  mode. Cancelled steer messages are retained in `input_history` so the user
  can recover and resend them via `↑`.
- `cargo run` on a non-terminal stdout (piped or redirected) falls back to the
  existing headless behaviour — no ratatui initialisation, identical output to
  today's `main.rs`.
- `make lint` and `make test` pass; ≥ 80 % line coverage maintained (excluding
  `ui.rs` and `main.rs`; `headless_runner.rs` and `tui_runner.rs` are included).
- `docs/features.md`, `docs/keybindings.md`, `docs/blueprint.md` (Phase 4
  task checklist and spec), and `docs/development-workflow.md` are updated in
  the same commit.

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
- **`ui.rs` and `main.rs` are excluded from coverage** per
  `development-workflow.md`. `headless_runner.rs` and `tui_runner.rs` are
  covered.
- **No sidebar.** The sidebar is deferred entirely; no stub code is added.
- **Double-press confirm is key-agnostic.** A single `confirm_state:
  Option<Instant>` in the driver (not in `TuiState`) tracks the first-press
  timestamp. No `ConfirmKind` enum is needed: the current `agent_mode`
  determines the action when the second press fires.
- **`ChatEntry` is ephemeral TUI view state only**, never persisted. Cancelled
  steers marked in `ChatEntry` do not appear in `AgentLoop::Context` or in any
  future JSONL session store. `AgentLoop::Context` is unaffected by Phase 4.
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
- `UiUpdate` (in `src/agent.rs`) currently carries: `AppendText`,
  `StreamStarted`, `StreamDone`, `StreamCancelled`, `Error`, `ToolStart`,
  `ToolPending`, `ToolDone`, `ToolError`, `ToolKilled`. `SteerQueued` and
  `SteerApplied` are absent — they were listed as "Phase 3+" comments and are
  not yet emitted.
- `docs/keybindings.md` contains a Phase 4 placeholder that names the Help
  popup but marks the keybinding as TBD.
- `ratatui-textarea 0.9` is a rewrite of `tui-textarea` (the name referenced
  in blueprint §2 and §9) targeting the new `ratatui-core` sub-crate
  architecture. It is compatible with `ratatui 0.30` via shared
  `ratatui-core ^0.1.x`. It supports multiline input, mouse events, selection,
  and clipboard out of the box. The plan uses `ratatui 0.30` +
  `ratatui-textarea 0.9` (newer than the blueprint's listed `ratatui 0.29` /
  `tui-textarea 0.7`), which is a compatible upgrade.

## Implementation Plan

### Step 1 — Add crate dependencies

Add to `Cargo.toml` under `[dependencies]`:

```toml
ratatui          = { version = "0.30", features = ["crossterm"] }
crossterm        = "0.29"
ratatui-textarea = { version = "0.9", features = ["crossterm"] }
textwrap         = "0.16"
ctrlc            = { version = "3", features = ["termination"] }
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
5. In `headless_runner.rs` (Step 7), add no-op match arms for `SteerQueued`
   and `SteerApplied` to avoid lint warnings.

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
    /// Ctrl+C pressed: stop if busy; quit if idle (after double-press confirm).
    CtrlC,
    /// Esc pressed: mode-dependent — see run_tui dispatch table.
    Escape,
    /// F1 pressed — toggle Help popup.
    ShowHelp,
    /// All other key events forwarded to the main task.
    KeyInput(crossterm::event::KeyEvent),
    /// Mouse events forwarded to the main task.
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
| `F1`                 | `ShowHelp`           |
| Any other `KeyEvent` | `KeyInput(event)`    |
| `MouseEvent`         | `MouseInput(event)`  |
| `ResizeEvent`        | `Resize { w, h }`    |

All context-dependent logic (what `Esc` means, confirm state, approval
sub-mode) lives in the main task, not here.

**Tests:** Unit tests using synthetic `crossterm::event::KeyEvent` values.
Cover each shortcut (Ctrl+C, Esc, F1) and a regular character event that must
produce `KeyInput`. No real terminal needed.

### Step 4 — Create `src/app.rs` — TUI application state

Create `src/app.rs`.

#### `UserMessageState` enum

```rust
/// State of a user message entry in the TUI chat history.
/// `ChatEntry` is ephemeral view state only — never persisted.
pub enum UserMessageState {
    /// Committed: injected into the model context.
    Sent,
    /// Pending steer: queued, not yet injected.
    Queued,
    /// Cancelled: stream was interrupted before injection; text retained in
    /// input_history so the user can recover via ↑.
    Cancelled,
}
```

#### `ChatEntry` enum

```rust
pub enum ChatEntry {
    UserMessage { text: String, state: UserMessageState },
    AssistantMessage { text: String, interrupted: bool },
    ToolLine { label: String },
    ErrorLine { message: String },
}
```

#### `TuiState` struct

```rust
pub struct TuiState {
    pub agent_mode:             AgentMode,
    pub chat_entries:           Vec<ChatEntry>,
    pub partial_assistant_text: String,
    pub show_help:              bool,
    pub steer_indicator:        bool,   // true while ≥1 steer is queued
    pub scroll_offset:          usize,
    pub scroll_locked:          bool,   // true = auto-scroll to bottom
    pub chat_area_height:       u16,    // updated on every resize
    /// Set by UiUpdate::ToolPending; cleared by ToolStart/ToolKilled/StreamCancelled.
    /// While Some, the TUI is in approval sub-mode.
    pub pending_approval:       Option<(uuid::Uuid, String)>,  // (tool_id, tool_name)
    // Input history navigation
    pub input_history:          Vec<String>,   // all UserMessage texts, oldest→newest
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

| `UiUpdate` variant | Mutation |
|---|---|
| `StreamStarted` | `agent_mode = Streaming`; `partial_assistant_text = ""` |
| `AppendText(s)` | append to `partial_assistant_text`; auto-scroll if `scroll_locked` |
| `StreamDone` | commit partial → `AssistantMessage { interrupted: false }`; `agent_mode = Idle`; re-lock scroll |
| `StreamCancelled` | commit partial → `AssistantMessage { interrupted: true }`; mark all `Queued` entries → `Cancelled`; `steer_indicator = false`; `pending_approval = None`; `agent_mode = Idle`; re-lock scroll |
| `SteerQueued` | `steer_indicator = true` (no bubble — driver pushed it before sending the command) |
| `SteerApplied` | mark all `Queued` entries → `Sent`; `steer_indicator = false` |
| `ToolPending { name, id, .. }` | push `ToolLine { label: format!("[tool pending] {name}") }`; `pending_approval = Some((id, name))` |
| `ToolStart { name, arguments, .. }` | push `ToolLine` with rendered invocation; `pending_approval = None` |
| `ToolDone { name, .. }` | push `ToolLine { label: format!("[tool done] {name}") }` |
| `ToolError { error, .. }` | push `ErrorLine { message: error }` |
| `ToolKilled { .. }` | push `ToolLine { label: "[tool killed]".into() }`; `pending_approval = None` |
| `Error(msg)` | push `ErrorLine { message: msg }`; `agent_mode = Idle` |

#### `TuiState` driver helpers

```rust
/// Called by the driver before sending AgentCommand::Prompt.
/// state == Queued when agent_mode != Idle (steer); Sent otherwise.
/// Appends text to input_history and resets history_index to None.
pub fn push_user_bubble(&mut self, text: String, state: UserMessageState)

/// Called when the user scrolls up — disables auto-scroll.
pub fn scroll_up(&mut self, lines: usize)

/// Called on End key or scroll-to-bottom event — re-enables auto-scroll.
pub fn scroll_to_bottom(&mut self)

/// Navigate input history. Returns the text to load into the textarea,
/// or None if the boundary was already reached (no change).
///
/// direction == Up:
///   - history_index None: save current textarea content as draft,
///     set history_index = Some(input_history.len() - 1), return that entry.
///   - Some(0): already at oldest — return None.
///   - Some(i): decrement i, return input_history[i].
///
/// direction == Down:
///   - history_index None: already at live draft — return None.
///   - Some(i) where i < input_history.len() - 1: increment i, return entry.
///   - Some(i) at newest: set history_index = None, return draft.
///
/// Any non-navigation keypress while history_index is Some(_) copies the
/// current entry into the live draft and resets history_index to None before
/// processing the keypress — handled in the key dispatch function.
pub fn navigate_history(
    &mut self,
    direction: HistoryDirection,
    current_text: &str,
) -> Option<String>

pub enum HistoryDirection { Up, Down }
```

**Tests:** Unit tests for `apply_update` covering every variant;
`push_user_bubble` with `Sent`/`Queued` states and `input_history` growth;
scroll lock toggling; `StreamCancelled` marks Queued entries as Cancelled
and clears `pending_approval`; `SteerApplied` marks Queued entries as Sent;
`ToolPending` sets `pending_approval`; `ToolStart` clears it;
`navigate_history` boundary conditions (empty history, oldest, newest, draft
restore, any-key exits browsing mode).

### Step 5 — Create `src/ui.rs` — ratatui layout and widgets

Create `src/ui.rs` with:

```rust
/// Clickable approval button rects returned by draw when in approval sub-mode.
pub struct ButtonAreas {
    pub approve: ratatui::layout::Rect,
    pub deny: ratatui::layout::Rect,
}

/// Draw the full TUI frame. Returns Some(ButtonAreas) when in tool approval
/// sub-mode (pending_approval is Some), None otherwise.
pub fn draw(
    frame: &mut ratatui::Frame,
    state: &TuiState,
    textarea: &ratatui_textarea::TextArea<'_>,
) -> Option<ButtonAreas>
```

#### Color constants (top of `ui.rs`)

```rust
// User message border colors — extract into Theme struct in a later phase.
const USER_MESSAGE_BORDER_COLOR:           Color = Color::Blue;
const USER_MESSAGE_QUEUED_BORDER_COLOR:    Color = Color::Yellow;
const USER_MESSAGE_CANCELLED_BORDER_COLOR: Color = Color::DarkGray;
// Soft background shade applied to all user message blocks.
const USER_MESSAGE_BG_COLOR: Color = Color::Rgb(20, 20, 40);
```

#### Layout

```text
┌─────────────────────────────────────────────────────────────┐
│  Chat area (scrollable, fills remaining height)             │
├─────────────────────────────────────────────────────────────┤
│  Input box (ratatui-textarea, 1–⌊height/3⌋ rows)           │
├─────────────────────────────────────────────────────────────┤
│  Footer row 1: mode/hint/approval prompt                    │
│  Footer row 2: key hints                                    │
└─────────────────────────────────────────────────────────────┘
```

`input_height = textarea.lines().len().max(1).min(frame.size().height / 3)`.
The chat area receives all remaining rows.

#### Chat area — user messages

Each `ChatEntry::UserMessage` is rendered as a block with a left-only border
(`Borders::LEFT`) and a soft background shade:

- `Sent`: left border color `USER_MESSAGE_BORDER_COLOR` (Blue).
  Content: plain text, normal style.
- `Queued`: left border color `USER_MESSAGE_QUEUED_BORDER_COLOR` (Yellow).
  Content: dimmed/italic text.
- `Cancelled`: left border color `USER_MESSAGE_CANCELLED_BORDER_COLOR`
  (DarkGray). Content: dimmed/italic text; a separate dimmed line
  `"Not sent."` is rendered immediately below the text, inside the same block.

All user message blocks share `USER_MESSAGE_BG_COLOR` as background. No text
title prefix is used.

#### Chat area — other entries

- `AssistantMessage`: plain wrapped text; partial/streaming text shown inline.
  Interrupted messages: faded suffix `"…"`.
- `ToolLine`: cyan text.
- `ErrorLine`: red text.

Wrap all entries with `textwrap` at `frame.size().width - 2`.
Scroll offset driven by `state.scroll_offset`.

#### Input box

Rendered from the `ratatui-textarea` widget. Border title: `"Input"` normally;
`"Steering…"` when `state.steer_indicator` is true; `"Awaiting approval…"`
when `state.pending_approval` is Some.

#### Footer — normal mode

- **Row 1:** `" [mode] Idle  ⋮ 1 steer queued"` — mode word reflects
  `state.agent_mode`; steer count shown only when `state.steer_indicator`.
  When confirm state is active, override row 1 with the hint text (e.g.
  `"Press Ctrl+C or Esc again to cancel"` / `"Press Ctrl+C or Esc again to quit"`).
  The hint text is passed into `draw` as a parameter derived from the driver's
  `confirm_state`.
- **Row 2:** `" Enter: send  Ctrl+C/Esc: cancel/quit  F1: help"`.

#### Footer — approval sub-mode

When `state.pending_approval` is `Some((_, name))`:

- **Row 1:** Tool name label, e.g. `" Tool: read — approve or deny?"`.
- **Row 2:** Two styled `Span` buttons:
  `[ ✓ Approve (Enter) ]` and `[ ✗ Deny (Esc) ]`, rendered with distinct
  background colors (green / red). Record each button's `Rect` and return
  as `ButtonAreas`.

#### Help popup

When `state.show_help`, render `ratatui::widgets::Clear` + centered `Block`
(60 % width, 70 % height) with two sections:

1. **Keybindings** — full keybinding table from `docs/keybindings.md`.
2. **Build info** — `BuildInfo::render()` output.

Footer line: `"F1 or Esc to close"`.

**Tests:** `ui.rs` is excluded from coverage. Visual correctness verified
manually.

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
panics and errors. Use a RAII guard so restoration cannot be skipped.

#### Constants

```rust
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);
```

#### `ActionOutcome` enum

```rust
pub enum ActionOutcome {
    Continue,
    Quit,
}
```

#### Driver-local state (not in `TuiState`)

```rust
// Timestamp of the first Ctrl+C or Esc press, when confirm is pending.
// None = no confirm active.
let mut confirm_state: Option<Instant> = None;

// Last ButtonAreas returned by draw; used for mouse hit-testing.
let mut button_areas: Option<ButtonAreas> = None;
```

#### Responsibilities of `run_tui`

1. Construct `AgentLoop::new(provider, registry)`.
2. Spawn the crossterm bridge (`src/events.rs`) — bounded `mpsc` channel
   (capacity 64).
3. Install a `ctrlc` handler: bridge to tokio via `std::sync::mpsc` +
   `tokio::task::spawn_blocking` relay; send `AgentCommand::Stop` on SIGINT —
   immediate, no double-press.
4. Call `agent.run()` to get the `JoinHandle`.
5. If `initial_prompt` is `Some`, call
   `state.push_user_bubble(text, UserMessageState::Sent)` then send
   `AgentCommand::Prompt`.
6. Enter the main `tokio::select!` loop:

   ```rust
   loop {
       select! {
           action = action_rx.recv() => {
               match handle_action(
                   action, &mut state, &mut textarea,
                   &mut confirm_state, &button_areas, &cmd_tx,
               ) {
                   ActionOutcome::Quit => break,
                   ActionOutcome::Continue => {}
               }
           }
           _ = /* batch drain ui_rx */ => {
               let old_mode = state.agent_mode;
               // drain up to 64 updates
               while let Ok(update) = ui_rx.try_recv() {
                   state.apply_update(&update);
               }
               // A transition to Idle clears confirm state immediately (Q5)
               if old_mode != AgentMode::Idle
                   && state.agent_mode == AgentMode::Idle
               {
                   confirm_state = None;
               }
           }
       }
       let hint = confirm_hint(&confirm_state, &state.agent_mode);
       button_areas = terminal.draw(|f| ui::draw(f, &state, &textarea, hint.as_deref()))?;
   }
   ```

#### `handle_action` dispatch

```rust
fn handle_action(
    action: AppAction,
    state: &mut TuiState,
    textarea: &mut TextArea<'_>,
    confirm_state: &mut Option<Instant>,
    button_areas: &Option<ButtonAreas>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
) -> ActionOutcome
```

**Approval sub-mode** (`state.pending_approval` is `Some`):

| Event | Action |
|---|---|
| `Enter` | send `ApproveTool { id }`; `Continue` |
| `Escape` | send `DenyTool { id }`; `Continue` |
| `CtrlC` | double-press confirm → `AgentCommand::Stop` if second press; `Continue` |
| `MouseInput` clicking approve button | send `ApproveTool { id }`; `Continue` |
| `MouseInput` clicking deny button | send `DenyTool { id }`; `Continue` |
| Other `KeyInput` | `textarea.input(event)`; `Continue` |

**Normal mode** (`state.pending_approval` is `None`):

| Event | Popup open | Agent mode | `confirm_state` | Action |
|---|---|---|---|---|
| `ShowHelp` | any | any | any | toggle `state.show_help`; `Continue` |
| `Escape` | yes | any | any | `state.show_help = false`; `Continue` (confirm unchanged) |
| `Escape` | no | `Idle` | any | clear textarea; clear confirm; `Continue` |
| `Escape` or `CtrlC` | no | `Streaming`/`ToolPending` | None or expired | set `confirm_state = Some(Instant::now())`; `Continue` |
| `Escape` or `CtrlC` | no | `Streaming`/`ToolPending` | Some(t) ≤ timeout | send `Stop`; clear confirm; `Continue` |
| `CtrlC` | no | `Idle` | None or expired | set `confirm_state = Some(Instant::now())`; `Continue` |
| `CtrlC` | no | `Idle` | Some(t) ≤ timeout | `Quit` |
| `Enter` | no | any | any | if non-empty: `push_user_bubble`; send `Prompt`; reset textarea; `Continue` |
| `↑` on first line | no | any | any | `navigate_history(Up, …)`; `Continue` |
| `↓` on last line | no | any | any | `navigate_history(Down, …)`; `Continue` |
| Non-nav `KeyInput` while `history_index` is `Some` | no | any | any | exit browsing; `textarea.input(event)`; `Continue` |
| Other `KeyInput` | no | any | any | `textarea.input(event)`; `Continue` |
| `MouseInput` | no | any | any | `textarea.input(event)`; `Continue` |

**Confirm expiry rule:** at the top of `handle_action`, if `confirm_state`
is `Some(t)` and `t.elapsed() > CONFIRM_TIMEOUT`, clear it before dispatching
(treat the incoming press as the first press).

#### Helper

```rust
/// Returns the footer hint string when confirm is active, None otherwise.
fn confirm_hint(confirm_state: &Option<Instant>, mode: &AgentMode) -> Option<String>
```

7. On `Quit`, exit the loop.
8. Drop `agent` to close `cmd_tx`; await the `JoinHandle`.

**Tests:** `tests/tui_state.rs` integration test using `MockProvider`:

- `run_tui` called with a `TestBackend` terminal.
- Drive via `AppAction` channel (bypass crossterm bridge).
- Assert: `Enter` → `TuiState` has `UserMessage(Sent)` + eventually
  `AssistantMessage`.
- Assert: double-press `CtrlC` mid-stream → `AgentMode` transitions to `Idle`;
  pending bubbles become `Cancelled`.
- Assert: single `CtrlC` mid-stream then `Escape` within timeout → same Stop
  outcome (key-agnostic confirm).
- Assert: steer cycle (`push_user_bubble(Queued)` + `SteerQueued` +
  `SteerApplied`) leaves no `Queued` entries in `chat_entries`.
- Assert: `StreamCancelled` marks `Queued` entries as `Cancelled`.
- Assert: approval sub-mode — `ToolPending` event → `Enter` sends `ApproveTool`.
- Assert: approval sub-mode — `ToolPending` event → `Escape` sends `DenyTool`.
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

Provider selection and `initial_prompt` resolution stay in `main.rs`. The
drain loop logic is **identical** to today's headless path in `main.rs`. Add
no-op match arms for `UiUpdate::SteerQueued` and `UiUpdate::SteerApplied`.

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
        runtime.block_on(headless_runner::run_headless(
            provider, registry, prompt.unwrap_or_default(),
        ))
    }
}
```

**Tests:** Add tests in `tests/headless_runner.rs` that drive it with
`MockProvider` and verify stdout output via captured output or event-counting.
`headless_runner.rs` is included in coverage (≥ 80 %).

### Step 8 — Export new modules from `src/lib.rs`

Add to `src/lib.rs`:

```rust
pub mod app;
pub mod events;
pub mod headless_runner;
pub mod tui_runner;
pub mod ui;
```

Re-export `TuiState`, `ChatEntry`, `UserMessageState`, and `AppAction` at the
crate root for use in integration tests.

### Step 9 — Update documentation

Update the following docs **in the same commit** as the code changes:

1. **`docs/blueprint.md`** — mark all Phase 4 tasks `[x]` (done) and expand
   the Phase 4 section to document the decisions made during implementation:
   - Double-press confirm (key-agnostic, 2-second window, `CONFIRM_TIMEOUT`
     constant) for Ctrl+C/Esc interrupt and quit.
   - OS SIGINT → immediate Stop (no double-press).
   - Natural mode transition to Idle clears confirm state.
   - Tool approval sub-mode: Enter=approve, Esc=deny, mouse-clickable footer
     buttons; Ctrl+C still follows double-press confirm in this sub-mode.
   - User message visual design: left-border block with `UserMessageState`
     color coding (Blue/Yellow/DarkGray); no text prefix.
   - Cancelled steer: marked `Cancelled` in `ChatEntry` (ephemeral), retained
     in `input_history` for recovery.

2. **`docs/features.md`** — add a `## Phase 4: TUI skeleton` section covering:
   layout (chat + variable input box + 2-row footer), input-always-active
   guarantee, steer bubble lifecycle, cancelled-steer visual treatment,
   tool approval sub-mode, Help popup, double-press Ctrl+C/Esc semantics,
   mode dispatch (terminal vs non-terminal), smart auto-scroll.

3. **`docs/keybindings.md`** — replace the Phase 4 placeholder with:

   | Key | Context | Action |
   |---|---|---|
   | `Enter` | Input focused, normal mode | Send message; queue steer if busy |
   | `Shift+Enter` / `Alt+Enter` | Input focused | Insert newline |
   | `Ctrl+C` or `Esc` (×1) | Streaming / ToolPending | Show `"Press again to cancel"` hint |
   | `Ctrl+C` or `Esc` (×2 ≤ 2 s) | Streaming / ToolPending | Interrupt generation (`AgentCommand::Stop`) |
   | `Ctrl+C` (×1) | Idle | Show `"Press again to quit"` hint |
   | `Ctrl+C` (×2 ≤ 2 s) | Idle | Quit TUI |
   | `Esc` | Idle | Clear input field (no quit) |
   | `Esc` | Help popup open | Close popup |
   | `Enter` | Approval sub-mode | Approve tool (`ApproveTool`) |
   | `Esc` | Approval sub-mode | Deny tool (`DenyTool`) |
   | `Ctrl+C` (×1) | Approval sub-mode | Show cancel hint |
   | `Ctrl+C` (×2 ≤ 2 s) | Approval sub-mode | Interrupt round (`AgentCommand::Stop`) |
   | `F1` | Any | Toggle Help popup |
   | `↑` (cursor on first line) | Input focused | Load previous user message; save draft |
   | `↓` (cursor on last line) | Input focused | Load next user message; restore draft at newest |
   | Any edit key while browsing | Input focused | Copy browsed entry to draft; exit browsing mode |
   | `PageUp` / `↑` | Chat focused | Scroll up; disable auto-scroll |
   | `PageDown` / `↓` | Chat focused | Scroll down |
   | `End` | Chat focused | Scroll to bottom; re-enable auto-scroll |

4. **`docs/development-workflow.md`** — update the coverage-exclusion list:
   `ui.rs` and `main.rs` excluded; `headless_runner.rs` and `tui_runner.rs`
   included.

## Proposed Improvements

- **`Ctrl+L` clear-screen.** Map `Ctrl+L` to `AppAction::Redraw` (clears and
  redraws the terminal) so the user can recover from terminal artefacts.

- **`Ctrl+Left`/`Ctrl+Right` word-jump.** `ratatui-textarea` supports this
  natively via `CursorMove::WordForward` / `WordBack`; ensure the `KeyInput`
  passthrough does not accidentally intercept these.

- **Theme struct.** The color constants (`USER_MESSAGE_BORDER_COLOR`, etc.)
  in `ui.rs` are the foundation of a future `Theme` struct allowing
  user-selectable palettes. Extract when the config layer (Phase 7) lands.

## Consistency Issues

- **`draw` signature change vs. Step 5 description:** `draw` now returns
  `Option<ButtonAreas>` and accepts a `hint: Option<&str>` parameter. The
  `terminal.draw(|f| ...)` closure captures the return value via the
  `terminal.draw()` return. Verify that `ratatui::Terminal::draw` propagates
  the closure's return value (it does as of ratatui 0.26+).

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
  Drive `run_tui` via a `TestBackend` terminal + injected `AppAction` channel
  to reach the event-loop branches without a real terminal.
- **`BuildInfo::render()`:** in `src/build_info.rs`; already used by
  `--version`; use in the Help popup "Build info" section.
- **`ratatui-textarea 0.9` API:** `TextArea::input(event)` processes a
  `crossterm::event::KeyEvent` in-place. `textarea.lines()` returns current
  content. Reset by replacing with `TextArea::default()`.
- **`ratatui` `Paragraph` scroll:** `Paragraph::scroll((row_offset, 0))`.
  Compute offset as `total_content_lines.saturating_sub(visible_height)` for
  auto-scroll-to-bottom.
- **`ratatui::widgets::Clear`:** render before the Help popup block to erase
  underlying content.
- **Left-border block rendering:** use `Block::default().borders(Borders::LEFT)`
  with `.border_style(Style::default().fg(color))` and
  `.style(Style::default().bg(USER_MESSAGE_BG_COLOR))` on the surrounding
  `Block`. Render each user message as a separate `Paragraph` inside its block.
- **`terminal.draw` return value:** `ratatui::Terminal::draw` returns
  `io::Result<CompletedFrame<R>>` where `R` is the closure's return type.
  Use `terminal.draw(|f| ui::draw(f, &state, &textarea, hint.as_deref()))?`
  to capture `Option<ButtonAreas>`.
- **`ctrlc` handler and tokio bridging:** `ctrlc::set_handler` runs in a
  separate OS thread. Bridge to tokio via a `std::sync::mpsc` relay consumed
  by a `spawn_blocking` watcher. Never call `runtime.block_on()` inside the
  handler.
- **Terminal detection:** `use std::io::IsTerminal; std::io::stdout().is_terminal()`.
- **Module layout:** Rust 2018+ file-per-module style (`foo.rs` not
  `foo/mod.rs`) per `AGENTS.md` / `development-workflow.md`.
- **Crate versions (confirmed compatible):**
  - `ratatui = "0.30"` requires `crossterm ^0.29` and `ratatui-core ^0.1.2`.
  - `ratatui-textarea = "0.9"` requires `ratatui-core ^0.1.1` (resolves to
    `0.1.2`) — no conflict.
  - Pin `crossterm = "0.29"` explicitly to prevent version skew.
- **`tests/e2e/` contains shell scripts** (`.zsh`), not Rust integration tests.
  New Rust integration tests go in `tests/tui_state.rs` and
  `tests/headless_runner.rs` at the crate root.

## Bugs / Follow-Up Changes

- [x] in ZED terminal, shift-enter doesn't insert a newline into input field,
      alt-enter does.

- [x] in iterm2 terminal neither shift-enter nor alt-enter inserts a newline
      into input field
      (Both fixed by enabling the kitty keyboard protocol
      `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES` on terminal setup, so
      ZED/iTerm2 send distinguishable CSI-u sequences that crossterm parses as
      `Enter + SHIFT` / `Enter + ALT`; `Shift+Enter`/`Alt+Enter` then insert a
      newline as documented.)

- [x] When input field contains multiline text, cmd-left or cmd-right only jumps
      to the next line start/end on current line, it doesn't advance to prev/next
      line.
      (Cmd+Left/Cmd+Right now wrap across line starts/ends — covered by
      `cmd_right_and_left_navigate_line_starts_and_ends`.)

- [x] Can't drag'n'drop text via mouse to select it.
      (Mouse drag inside the input box starts a selection and extends it as the
      cursor moves; a plain click just places the cursor. Covered by
      `drag_in_input_selects_text` and `click_in_input_places_cursor`.)

- [x] Click inside input field doesn't move the cursor.
      (A left-button click inside the input box places the cursor at the clicked
      character, accounting for word-wrapping; `cursor_for_click` unit-tested.)

- [x] Just starting TUI immediately uses default prompt
      "What makes the sky look blue?".
      TUI should start with no inital user input though.
      (The default demo prompt is now headless-only. The TUI starts with an empty
      input box; only an explicit `--prompt` or piped stdin auto-sends.)

- [x] Scroll indicator looks odd, although I've scrolled to the end of the list,
      the indicator is not at the bottom of vertical scrollable list.
      (The chat scrollbar's `content_length` was the total line count; ratatui's
      thumb proportion never reached the bottom when the viewport spans >1 row.
      Now `content_length = max_offset + 1`, so the thumb sits at the bottom at max
      offset — covered by `vertical_scrollbar_thumb_reaches_bottom_at_max_offset`.)

- [x] Add top/bottom 1 char padding to sent user message rendering

- [x] Add 2-char margin between left border and content of sent user
      message rendering

- [x] Render input field like a sent message (but differernt left border color),
      no full border around content, no "Input" label.
      (User bubbles get 1-char top/bottom padding and a 2-char margin after the
      left bar; the input box is now a left-border bubble in Cyan with no title and
      word-wraps long lines instead of growing a horizontal scrollbar.)

- [x] Sending new input via enter, insert a new bubble, but the footer indicator
      is still showing "[mode] idle" although the new message has been sent
      to the model and we are waiting for the response. Only when streaming of
      response starts, the footer shows "[model] Streaming".
      (A new `TuiState.submitted` flag is set when a prompt is sent from idle and
      cleared on `StreamStarted`; the footer shows "Sending…" during the gap.)
      Should we introduce a dedicated `UiUpdate` event that indicates a request
      to the provider was sent and we are waiting for response/stream or are even
      retrying the request nth attempt ?

- [x] Change style of rendered buttons, so that a mouse hover changes its
      background or border.
      (Mouse position is tracked in the driver and passed to `ui::draw` /
      `draw_approval_footer`; hovering over a button changes its background from
      Green/Red to LightGreen/LightRed with bold text.)

- [x] in ZED terminal pressing cmd-left or cmd-right always only jumps to start/end
      of current row, never jumps to prev/next row.
      Using ctrl-left and ctrl-right allows my to cycle thru the word boundaries
      prev/next over all lines though.
      (`Cmd+Left` and `Cmd+Right` are intercepted by iTerm2 (tab switching) and
      ZED before reaching the terminal app — no fix is possible at that level.
      Instead the SUPER/META handlers were removed entirely. `Home` / `Fn+Left`
      and `End` / `Fn+Right` now carry the cross-line navigation: `Home` moves
      to the start of the current line, wrapping to the end of the previous
      line when already at column 0; `End` moves to the end of the current
      line, wrapping to the start of the next line when already at the line
      end. `End` no longer scrolls chat; `PageDown` reaches the bottom instead.
      Updated keybindings.md and the help popup.)

- [x] There is always an empty line in the prompt input field ( if height is 2 rows,
      the 2nd row is empty and I can't even move the cursor into it. )
      (The `+1` added to `wrapped_rows` when computing `input_height` in `ui.rs`
      created a phantom extra row; the minimum `clamp` was also raised from 2 to 1.
      Fixed by using `(wrapped_rows as u16).clamp(1, max_input)` so the box height
      exactly matches the visible content rows.)

- [x] Text in prompt input field is underlined, should not be decorated.
      (ratatui-textarea's default `cursor_line_style` is
      `Style::default().add_modifier(Modifier::UNDERLINED)`, which underlines the
      entire line the cursor is on. Added a `configure_textarea` helper that calls
      `set_cursor_line_style(Style::default())` (and `set_wrap_mode`) on every
      `TextArea` creation or reset so no underline decoration is applied.)

- [x] After sending a message, the bubble of the message uses a darker background
      color, but only where there are actual characters.
      The background should fill the whole rect of the bubble though.
      (`append_user_lines` now calls `.patch_style(Style::default().bg(USER_MESSAGE_BG_COLOR))`
      on every `Line` so the dark shade fills the entire terminal row, not just the
      cells that contain span characters.)

- [x] Use lowercase keybinding hints: "enter: send", "ctrl-c/esc: cancel/quit".
      But keep it uppercase for the special keys like "F1" keybinding.
      (Footer hint string updated to lower-case for regular keys; `F1` stays
      upper-case per the guideline.)

- [x] selecting text in input field, but I can't copy it. Neither cmd-c for
      right-click for a context-menu allows me to copy selected text.
      Can we automatically add mouse-selected text to clipboard ( with a small
      toast message that text was copied )?
      (On `MouseUp` after a drag-select, `textarea.copy()` yanks the selection and
      `arboard` copies it to the OS clipboard; `TuiState::set_toast` shows a
      "✓ Copied to clipboard" overlay for 2 s; `clear_expired_toast` dismisses
      it each frame. Clipboard failures are logged and silently skipped.)

- [x] Rethink color-scheme.
      Rendering the ylva title in TUI in blue and using blue-border for send
      user-messages will likely cause a user to think that blue stuff comes from
      agent instead of user.
      (`USER_MESSAGE_BORDER_COLOR` changed from `Color::Blue` to `Color::Green`
      so sent user-message bubbles are clearly distinct from the blue Ylva logo.
      The input-box border stays Cyan — "drafting" vs "sent" distinction.)

- [x] TUI conversion list should insert an empty row between any item in the list.
      Currently the model reply is directly below the user-message bubble.
      (The `draw_chat` loop now pushes `Line::default()` after every `ChatEntry`
      so all consecutive entries — user bubble, assistant reply, tool line, error
      line — are separated by one blank row.)

- [x] First mouse-wheel event seems to scroll the conversation-list to the top in a
      single step.
      consecutive mouse-wheel do scroll one-line at a time though.
      (While `scroll_locked` is true `draw_chat` kept `scroll_offset` at 0 even
      though the viewport was pinned to `max_offset`. The first `scroll_up(1)` call
      set `scroll_locked = false` and left `scroll_offset = 0`, jumping the view
      to the top. Fixed by syncing `state.scroll_offset = max_offset` inside the
      locked branch of `draw_chat` each frame so the offset always matches the
      rendered position before any user scroll event.)

- [x] Sending a steer-message while streaming has not yet arrived or is in progress:
      Adds the pending user-message before the currently active stream-response in
      the conversion-list.
      But that steering message is logically after the current in-progress-streaming,
      it will be send when the stream has finished, won't it ?
      (`draw_chat` now collects all `UserMessage { state: Queued }` entries into
      a `deferred_queued_messages` vec while iterating `state.chat_entries` and
      renders them _after_ the in-progress streaming bubble, so the conversation
      reads: `[previous entries] → [streaming bubble] → [queued steer bubble(s)]`.)

- [x] Ensure a blank line between the botom of the scrollable conversion list and the
      input-field
      (The `draw_chat` loop already appends `Line::default()` after every entry,
      including the last streaming bubble, so a blank line is always present at
      the bottom of the scrollable content. No extra layout spacer is needed —
      the existing trailing blank is the separator.)

- [x] Can not drag'n'drop with mouse to select text ( and copy it ) in
      response bubble ( neither in streaming nor in finished state ).
      (Added `ChatSelection`, `chat_area`, `chat_rendered_lines`, `chat_selection`,
      and `chat_drag_active` to `TuiState`. `draw_chat` saves the plain text of
      every rendered line and applies a selection-highlight background to selected
      lines. `handle_mouse_input` starts a chat selection on `MouseDown` in the
      chat area, extends it on `MouseDrag`, and on `MouseUp` copies the selected
      text to the OS clipboard via `arboard` and shows a "✓ Copied to clipboard"
      toast — matching the behaviour of the input-field drag-select.)

- [x] Footer needs some animation/progress while agent is doing something.
      It would indicate if agent is actually idle.
      Maybe an animated dot running round a circle/rect of 3x3 chars ?
      Or a little ball/dot bouncing off the ground in a 1 width x 3 height area ?
      (Done: Added `spinner_frame: u8` to `TuiState`. `tui_runner.rs` advances it
      every 150 ms via a `tokio::time::sleep` arm in the `select!` loop when the
      agent is not `Idle`. `ui.rs` cycles through a 10-frame braille spinner
      (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) in `draw_normal_footer` as a prefix to the mode word.)

- [x] Pasting a multiline text into input field immediately submits it.
      This prevents further editing a pasted text and should not happen.
      If the pasted text is multiline every line is submitted individually
      instead of capturing the whole pasted text in the input-field.
      (Done: Enabled bracketed paste (`EnableBracketedPaste`/`DisableBracketedPaste`)
      in `run_tui_on_stdout` and `TerminalGuard`. Added `AppAction::Paste(String)` to
      `events.rs` and mapped `Event::Paste`. `handle_paste` in `tui_runner.rs`
      inserts pasted text character-by-character, using `Shift+Enter` for newlines.)

- [x] render tool pending/result in the same bubble, use magenta left-border
      like in user/agent message.
      (Done: Replaced `ChatEntry::ToolLine` with `ChatEntry::ToolBubble { id, name,
      state }` and `ToolBubbleState` enum (`Pending`, `Running`, `Done`, `Error`,
      `Killed`, `Denied`). `apply_update` mutates the existing bubble in-place via
      `find_tool_bubble_mut`. `ui.rs` renders all states with a magenta `▌` border
      and dark-magenta background in `append_tool_bubble_lines`.)

- [x] Bubble for a pending tool approval should render the tool args, so
      user can see what requires approval, not just which tool.
      (Done: Added `arguments: serde_json::Value` to `UiUpdate::ToolPending` and
      both send sites in `agent.rs`. `TuiState::pending_approval` extended to a
      3-tuple `(Uuid, String, Value)`. The `Pending` bubble state stores and
      renders the arguments as a one-liner JSON string beneath the title.)

- [x] need 2-char spacer between "approve" and "deny" buttons when rendered in footer.
      Title for the approval should be like "Tool 'read' requests your approval:"
      (Done: Changed title format string to `" Tool '{tool_name}' requests your
      approval:"` and updated the layout in `draw_approval_footer` to
      `[approve_width, 2, deny_width]` constraints, ignoring the middle area.)

- [x] Use only lowercase keybinding notation: ctrl-c, fn-left etc.
      alt-shift-n ( not used, just an example ).
      Only F-Keys like F1 should use uppercase letters.
      Update all docs and text used as keybinding help in code to follow this notation.
      (All keybinding strings in `src/ui.rs` (help popup) and `docs/keybindings.md`
      updated to lowercase hyphen-separated notation; F1 remains uppercase.)

- [x] Remove the "Ylva" logo in conversation list once a message has been submitted.

- [x] Leading and trailing empty lines are preserved in the rendered bubbles of
      user messages. These strings should be trimmed on submission.
      They are trimmed before sending to provider, but the TUI shouldn't even
      see those leading/trailing spaces.
      (`trim_empty_lines` strips blank-only lines from head and tail of the
      textarea content before passing it to `push_user_bubble` and the agent
      command, so the bubble and the provider both receive the same trimmed text.)

- [x] Scrolling in conversation is not possible when a tool approval is currently
      pending.
      This is not ideal when multiple tool calls have been requested by the model.
      The first call needs approval, runs and produces result.
      The output of the tool call is currently rendered _after_ any of the other
      requested tool calls.
      Non active tool calls ( requested by model but not yet 1st entry in the
      tool call chain ) should always be rendered as last entries in the
      conversation list, like steering user message that have not been sent yet.
      This will result in a pending tool to be at the end of the conversation list
      and close to the approve/deny buttons ( no scrolling needed likely ).

- [x] Drag'n'drop in a bubble of the conversation-list is rendered as a full terminal
      width line.
      User can't distinguish where the drag starts and where it ends.
      This seems to be triggered by a click too, I see a selection-like
      line where I have clicked.
      Though after a specific drag'n'drop only the chars that were dragged are
      actually copied to clipboard.
      Thus it seems only the rendering is not correct.
      (Fixed: `apply_selection_highlight_to_line` splits spans at selection column
      boundaries so only the actually-selected characters receive the highlight BG.
      `draw_chat` now skips rendering when `sel.is_empty()` and passes per-line
      column ranges to the helper so partial start/end lines are correctly clipped.
      On `MouseUp` without a real drag the stale `chat_selection` anchor is cleared.)

- [x] When a tool approval is denied by user the error shown in the
      tool-bubble is misleading: "tool 'glob' was denied (no auto-approve configured"
      This sounds like there was some auto-approval involved or even required that
      resulted in denying the tool, while this was a deliberate action of the user in
      denying the tool.
      (Changed `AgentCommand::DenyTool` to carry a `reason: String` supplied by
      the caller. TUI sends `"denied by user"`; headless runner sends
      `"no --auto-approve-write configured"`. Agent formats the error as
      `"tool '{name}' was denied: {reason}"`.

- [x] While streaming response tokens are recieved, the progress indicator stops
      running and appears stuck.
      (`tokio::time::sleep` inside the `select!` loop was cancelled on every
      iteration when the `update` arm won, stalling the spinner during heavy
      streaming. Replaced with a `tokio::time::interval` created once before
      the loop; the interval fires at its own cadence regardless of which
      `select!` arm wins each iteration.)

- [x] Ctrl-c to abort TUI while tool pending is not possible, doesn't hint for
      second keypress required nor exits on consecutive ctrl-c.
      Must kill executable from outside, leaving terminal rendering broken afterwards.
      (Ctrl+C while a tool approval is pending now immediately sends `DenyTool`
      with `reason: "denied by user"`, identical to pressing Esc or the Deny
      button. No confirm window is opened. If the agent surfaces another pending
      tool the next Ctrl+C denies that one too. Once all pending tools are cleared
      and the agent is busy re-prompting the model, Ctrl+C falls back to the
      double-press → Stop path.)

- [x] Change the order of rendered elements with tool approval is pending:
      ```text
      <approval title>
      <approve button> <deny button>
      <input field>
      ```
      (When `pending_approval` is `Some`, `draw` now builds the layout with the
      two footer rows — title then buttons — placed above the input field, so the
      approve/deny controls sit immediately above where the user types a denial
      reason. In normal mode the original order — input above footer rows — is
      preserved.)

- [x] process of tool approval should be:
  - On start of tool approval, save the current content of input-field
    ( need it later to restore it ) and clear input-field.
  - Use the entered input-field text as "reason" why the tool approval was denied.
    If tool was approved, don't use input field.
  - Restore the saved content to input-field after approval is done

- [x] Selecting text while tool pending is not possible

- [x] When pasting text into input field, the text should be dedent first using the
      same function that is used to dedent text before submitting it to agent.
      (`handle_paste` now calls `dedent(&text)` on the raw pasted string before
      inserting characters, so common leading whitespace and surrounding blank lines
      are stripped — exactly like submit-time dedent. Covered by
      `paste_dedents_text_before_inserting`.)

- [x] Why is the input-field limited to just 4 lines ? Terminal has height 16.
      If there is a ratio defined, extract it as a constant to locate it easier in
      the code.
      (`USER_MESSAGE_INPUT_HEIGHT_TERMINAL_FRACTION = 3` constant in `src/ui.rs` caps the input
      field at one-third of the terminal height.)

- [x] Multiline text retrieved via former message from the conversation ( keybinding
      up/down in input field ) are insert into input field without the original
      linebreaks.
      Thus a former multiline prompt ends up as a single long (wrapped) string
      in the input field.

- [x] When retrieving former messages from conversation the position of
      last-picked-historic-message is maintained/saved.
      Can this index get messed up when queued user messages are removed ( e.g. by ctrl-c while pending ) ?
      I noticed that while jumping back/forth and pressing esc the next jump back/forth picks up were I "left" the history at the last press of esc.
      I guess this would be the same if multiple user messages are sent and
      I start jumping back/forth in conversation history later.
      This feels strange from UX perspective because I may not track mentally
      were my current index in the conversion-history is and moving up after
      multiple conversion-turns should immediately bring back the "last" message,
      shouldn't it?
      Should we "reset" the current index in conversation-history after an "esc"
      press ( to clear current input message ) or whenever a new user message was submitted ?
      (The index cannot be corrupted by queued-message removal because `input_history`
      is a separate `Vec` that is only ever appended to; cancelling queued entries
      only touches `chat_entries`. `push_user_bubble` already resets `history_index`
      to `None` on every submission. The remaining gap — Esc did not reset the index
      — is now fixed: `handle_escape` in `tui_runner.rs` clears both `history_index`
      and `draft` before resetting the textarea, so the next ↑ always starts from
      the most-recent entry. Covered by `idle_escape_resets_history_index`.)

- [x] Help popup should use 1 char padding and the popup should have a title "Help"
      at upper-left position of popup ( 2 char indent, not directly at the edge )
      instead of "F1 or esc to close".
      Use blue border for the help popup.
      Help popup content can't be scrolled when its content is bigger than the 
      terminal height. Should have vertical scrolling indicators too.
      Just render "▲" and "▼" ( no vertical indicator inbetween ) when there is
      more up/down content avail.
      (Popup block now uses `Color::Blue` border, title `" Help"` left-aligned with
      2-char indent, and `Padding::uniform(1)`. The "F1 or esc to close" overlay is
      removed. All content (keybindings + build info) is rendered as a single
      `Paragraph` with `scroll((help_scroll_offset, 0))`; `▲` / `▼` appear in the
      top-right / bottom-right of the inner area when content overflows. `PageUp`,
      `PageDown`, and mouse wheel route to `help_scroll_up` / `help_scroll_down`
      while the popup is open. The offset is reset to 0 when the popup is closed.)

- [x] Submitting a message that entails a tool call.
      Message is sent, streaming starts.
      Starting the help popup.
      Tool call arrives, pending approval is shown beneath the help popup.
      Now pressing "esc" denies the approval instead of dismissing the open help
      popup.
      (`handle_escape` in `tui_runner.rs` now checks `show_help` first; when the
      help popup is open Esc closes it without touching `pending_approval`.
      Covered by `escape_closes_help_popup_before_denying_pending_approval`.)

- [x] Double-Ctrl-c while streaming doesn't seem to stop it immediately.
      I had to keep pressing ctrl-c repeatly until streaming was over before the
      TUI would actually quit.
      (Second Ctrl+C while streaming now returns `ActionOutcome::Quit` immediately
      after sending `AgentCommand::Stop`. The TUI exits and cleans up; the agent
      and in-flight stream task are aborted during shutdown. Esc double-press
      (stop-only, no quit) is unchanged.)

- [x] Sent a user message that will trigger a tool call.
      While the message is pending, type yet another message, but don't submit it.
      The tool approval arrives, but the formerly typed text is still in the
      input-field.
      The input-field content should be saved and cleared before showing the
      approval and should be restored after approval was approved/denied.
      (Added `pending_approval_input_snapshot: Option<Vec<String>>` to `TuiState`.
      `save_textarea_for_approval` is called from the event loop when a `ToolPending`
      update transitions `pending_approval` from `None` to `Some`: it saves the
      current textarea lines into the snapshot and clears the textarea.
      `restore_textarea_after_approval` is called in every approve/deny path
      — Esc, Ctrl+C, Enter (approve/deny-with-reason), and mouse approve/deny
      buttons — restoring the snapshot into the textarea and clearing the field.
      Covered by `approval_enter_approve_restores_draft_in_textarea`,
      `approval_escape_deny_restores_draft_in_textarea`,
      `approval_ctrl_c_deny_restores_draft_in_textarea`,
      `approval_mouse_approve_restores_draft_in_textarea`,
      `save_textarea_for_approval_stores_content_and_clears_textarea`,
      `save_textarea_for_approval_with_empty_textarea_stores_none`, and
      `restore_textarea_after_approval_restores_snapshot_and_clears_it`.)

- [ ] Add a thin solid white line between the lower area ( input-field + footer )
      and the conversation-list above.

- [ ] In Footer change the "active" indicator:
  - drop the "[mode]" snippet
  - use just "Sending...", "Streaming...", "Running Tool", "Idle"
  - keep the spinning dots while agent is not idle

- [ ] render toast messages in lower right edge of app.

- [ ] Adjust the width of bubbles ( tool, user message, assistent message )
      to not be rendered beneath the conversation-scrollbar but have a 1 char margin between bubble-right-edge and scrollbar.
