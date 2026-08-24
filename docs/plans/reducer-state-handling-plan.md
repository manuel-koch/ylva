# Refactor TUI State Handling to a Reducer Pattern

## Context

### System / component being changed

The TUI event loop lives in `src/tui_runner.rs`. It currently owns all
context-dependent input handling: double-press confirm timing, tool-approval
sub-mode, help-popup flag, mouse routing, domain update handling — all
interleaved in a single `handle_action` function and its callees, together
with driver state (`TextArea`, channel handles, `confirm_state`). Pure
view-model data lives in `src/app.rs` (`TuiState`).

### Goal and motivation

Replace the imperative, tightly coupled event-handling code in `tui_runner.rs`
with a **pure reducer function**:

```text
fn update(state: &mut AppState, event: Event) -> Vec<Effect>
```

The Presenter (`update`, routing helpers) becomes pure and free of I/O — no
crossterm imports, no `agent.rs` imports, no channel sends.
The Driver (`execute_effects`, the async loop) owns all I/O: `TextArea`, agent
channels, quit control. This separation makes the state machine easy to test
without a live terminal, and removes the need to thread `&mut TextArea` and
`&mpsc::Sender` deep into every key handler.

The idea also replaces `TuiState::show_help: bool` + `pending_approval: Option<…>`
with two orthogonal typed fields — `TuiMode` (agent-side control flow) and
`Focus` (keyboard routing) — plus a typed `AppLayout` carrying ratatui `Rect`
values with hit-test methods. The driver-local `confirm_state: Option<Instant>`
is folded into `TuiMode::DoublePressAbort`. A typed `InputEvent` enum (derived
from `AppAction`) and a typed `DomainEvent` enum (converted from `UiUpdate` in
the Driver) form the complete event surface of the Presenter.

### Motivation in brief

- `handle_action` and its callees mix pure state logic with `cmd_tx.try_send`
  I/O, making unit tests require a real channel.
- `confirm_state` is driver-local but conceptually belongs to the state machine.
- Flat boolean flags (`show_help`) and a mixed-concern `pending_approval` field
  lead to implicit constraints (e.g. "if both are true, which wins?").
- No module boundary enforces the MVP split; compiler can't catch violations.

### Acceptance criteria / definition of done

1. A new module `src/presenter.rs` contains `update()`, `route_keyboard()`,
   `route_mouse()`, `route_domain()`, and per-mode / per-focus handler
   functions — no `use ratatui`, no crossterm imports, no channel sends.
2. A new module `src/app_state.rs` replaces `src/app.rs` as the pure model:
   - `AppState` holds `mode: TuiMode`, `focus: Focus`, `layout: AppLayout`,
     and all current view-model fields from `TuiState`.
   - `TuiMode`, `Focus`, `Effect`, `InputEvent`, and `DomainEvent` enums are
     defined here.
   - `AppLayout` with `ChatArea`, `InputArea`, `HelpPopup` structs and
     `contains` / `page_scroll_rows` / `hit` methods are defined here.
   - No `use ratatui::backend`, no channel types, no `UiUpdate` imports — only
     `ratatui::layout::Rect` is allowed.
3. `src/tui_runner.rs` (Driver) converts `AppAction → InputEvent` and
   `UiUpdate → DomainEvent`, calls `update()` and `execute_effects()`,
   owns `AppState`, `TextArea`, and channel handles. `run_tui` becomes a thin
   event loop.
4. `src/render.rs` (View) is extracted from `src/ui.rs` for the `render()`
   entry point; it takes `&mut AppState` and may only write to `state.layout.*`.
5. All existing integration tests in `tests/tui_state.rs` pass with type-import
   updates only; test intent is unchanged.
6. New unit tests in `src/presenter.rs` cover each handler in isolation:
   set `state.mode` / `state.focus` directly, call `update()`, assert on the
   returned `Vec<Effect>` and mutated state — no channel, no terminal.
7. `make lint` and `make test-coverage` (≥80%) pass.
8. `docs/features.md`, `docs/keybindings.md`, `docs/class-diagram.mmd`,
   `docs/blueprint.md`, and `docs/development-workflow.md` are updated.
9. `docs/tui-reducer-state-diagram.mmd` (and its rendered SVG) is produced.

### Key constraints

- `AgentMode` lives in `src/agent.rs` and drives `UiUpdate`; it must not be
  changed (it is the agent-side protocol). `TuiMode` is a separate TUI-local
  enum that mirrors the subset needed by the Presenter.
- `TextArea` owns internal scroll state (line wrapping, cursor); it must remain
  in the Driver. The Presenter may emit `Effect::TextareaKey` / `Effect::Paste`
  commands; the Driver feeds them into the textarea.
- Integration tests in `tests/tui_state.rs` currently import from `ylva::`
  (the `[lib]` crate). Any type renamed or moved must be re-exported via
  `src/lib.rs` to avoid breaking those tests.
- `src/ui.rs` and `src/render.rs` are excluded from coverage thresholds
  (require a live terminal); the `Makefile` exclusion list must include
  `render.rs` alongside the existing `ui.rs` and `main.rs` entries.
- Coverage threshold: ≥80% line coverage on testable files, enforced by
  `make test-coverage`.
- Use file-per-module Rust style (`foo.rs`, not `foo/mod.rs`).
- No commit-per-step is prescribed by this plan; commits are the implementer's
  responsibility.

## Implementation Plan

### Step 1 — Define `TuiMode`, `Focus`, `InputEvent`, `DomainEvent`, `Effect`, and `AppLayout` in `src/app_state.rs`

- [x] Create `src/app_state.rs` with:
  - [x] `TuiMode` enum (Idle, Pending, Streaming, Aborting, DoublePressAbort).
  - [x] `Focus` enum:
    - [x] `MainInput` — normal typing, history navigation.
    - [x] `HelpPopup` — modal; captures all keyboard and mouse input.
    - [x] `ToolApproval { tool_id: Uuid, tool_name: String }` — non-modal; captures
      keyboard only. Tool arguments are **not** stored here; they live solely in
      the corresponding `ToolBubble` chat entry.
    - [x] `is_modal(&self) -> bool` method (true only for `HelpPopup`).
  - [x] `InputEvent` enum: `ShowHelp`, `SigInt`, `CtrlC`, `Escape`,
    `KeyInput(crossterm::event::KeyEvent)`, `Paste(String)`. This decouples the
    Presenter from `AppAction` and raw crossterm types.
  - [x] `DomainEvent` enum: variants covering all `UiUpdate` cases that the Presenter
    needs to react to (e.g. `StreamStarted`, `StreamChunk`, `StreamDone`,
    `StreamCancelled`, `ToolPending { id, name }`, `ToolStart`, `ToolDone`,
    `ToolKilled`, `ToolDenied`, `Error`, `ConfirmExpired`). The Driver converts
    `UiUpdate → DomainEvent`; the Presenter never imports from `agent.rs`.
  - [x] `Effect` enum: `SubmitPrompt { steer: bool }`, `ClearTextarea`,
    `SaveTextareaAndClear`, `RestoreTextarea`, `TextareaKey(KeyEvent)`,
    `Paste(String)`, `ApproveTool { id: Uuid }`, `DenyTool { id: Uuid }`,
    `StopAgent`, `Quit`.
  - [x] `ChatArea`, `InputArea`, `HelpPopup` layout structs each holding a `Rect`
    with `contains(col, row) -> bool`, `page_scroll_rows() -> u16`, and (for
    `HelpPopup`) `hit(col, row) -> bool` methods.
  - [x] `AppLayout` struct aggregating those structs.
  - [x] `AppState` struct containing `mode: TuiMode`, `focus: Focus`,
    `layout: AppLayout`, plus all current fields of `TuiState` that are pure
    view-model data. Fields that belong to the Driver (`mouse_drag`,
    `button_areas`) are **not** included.
- [x] Verify: no `use ratatui::backend`, no channel types, no `UiUpdate` imports.
- [x] Write unit tests (inside `src/app_state.rs` `#[cfg(test)]`):
  - [x] `Focus::is_modal()` returns true only for `HelpPopup`.
  - [x] `ChatArea::contains` / `InputArea::contains` geometry correctness.
  - [x] `TuiMode` variant construction and equality.

### Step 2 — Convert `src/app.rs` to a shim re-exporting from `src/app_state.rs`

- [x] Replace the body of `src/app.rs` with a thin re-export shim:

  ```rust
  // src/app.rs — transitional shim; remove after full migration
  pub use crate::app_state::*;
  ```

- [x] Verify `make test` passes.

### Step 3 — Implement `src/presenter.rs` with `update()` and routing functions

- [x] Create `src/presenter.rs` with:
  - [x] `pub fn update(state: &mut AppState, event: TuiEvent) -> Vec<Effect>`
    where `TuiEvent` is an enum wrapping `InputEvent`, `MouseEvent`, and
    `DomainEvent`.
  - [x] `fn route_keyboard(state, InputEvent) -> Vec<Effect>`
  - [x] `fn route_mouse(state, crossterm::event::MouseEvent) -> Vec<Effect>`
  - [x] `fn route_domain(state, DomainEvent) -> Vec<Effect>`
  - [x] Per-focus keyboard handlers: `keyboard_main`, `keyboard_help`,
    `keyboard_approval`.
  - [x] Per-mode domain handlers: `domain_idle`, `domain_pending`,
    `domain_streaming`, `domain_aborting`, `domain_double_press`.
  - [x] Mouse handlers: `mouse_chat`, `mouse_input_area`.
  - [x] Ensure `UiUpdate::ToolStart | ToolKilled | StreamCancelled` map to
    `DomainEvent` variants that reset `state.focus` back to `Focus::MainInput`;
    document this mapping explicitly to prevent focus desync from agent state.
- [x] Port all logic from `tui_runner.rs` (`handle_action`, `handle_ctrl_c`,
  `handle_escape`, `handle_key_input`, `handle_mouse_input`, `handle_paste`,
  `confirm_or_stop`). The `confirm_state: Option<Instant>` becomes
  `TuiMode::DoublePressAbort { pressed_at: Instant, return_to: Box<TuiMode> }`.
  The `DomainEvent::ConfirmExpired` variant clears this mode back to `return_to`.
- [x] The Driver fires `DomainEvent::ConfirmExpired` via the existing
  `tokio::time::sleep` arm in `select!`, converting it to a `TuiEvent::Domain`
  before calling `update()`. The Presenter's `DoublePressAbort` handler handles
  the expiry — no timing logic inside the Presenter.
- [x] Verify: no `use ratatui`, no `use ratatui_textarea`, no channel sends.
- [x] Write unit tests (inside `src/presenter.rs` `#[cfg(test)]`):
  - [x] `ctrl_c_while_idle_first_press_sets_double_press_abort_mode`
  - [x] `ctrl_c_while_idle_second_press_returns_quit_effect`
  - [x] `ctrl_c_while_busy_first_press_records_confirm`
  - [x] `ctrl_c_while_busy_second_press_returns_stop_agent_effect`
  - [x] `ctrl_c_while_tool_approval_denies_tool`
  - [x] `escape_while_help_open_closes_help`
  - [x] `escape_while_tool_approval_denies_tool`
  - [x] `escape_while_busy_empty_input_triggers_confirm`
  - [x] `show_help_toggles_focus_to_help_popup`
  - [x] `sigint_sends_stop_agent_effect_immediately`
  - [x] `enter_with_text_returns_submit_prompt_effect`
  - [x] `enter_while_approval_empty_returns_approve_tool_effect`
  - [x] `enter_while_approval_with_text_returns_deny_tool_effect`
  - [x] `keyboard_in_help_mode_scrolls_help`
  - [x] `mouse_scroll_up_in_chat_area_decrements_scroll_offset`
  - [x] `mouse_click_outside_help_popup_closes_modal`
  - [x] `domain_stream_done_transitions_mode_to_idle`
  - [x] `domain_tool_pending_while_aborting_returns_deny_effect`
  - [x] `confirm_expired_event_clears_double_press_abort_mode`

### Step 4 — Rewrite `src/tui_runner.rs` as the Driver

- [x] Add conversions:
  - [x] `fn app_action_to_tui_event(action: AppAction) -> Option<TuiEvent>` —
    maps `AppAction` to `TuiEvent::Input(InputEvent)` or `TuiEvent::Mouse(MouseEvent)`.
  - [x] `fn ui_update_to_domain_event(update: &UiUpdate) -> DomainEvent` —
    converts each `UiUpdate` variant to the corresponding `DomainEvent`.
- [x] Add `async fn execute_effects(state, textarea, cmd_tx, effects) -> ActionOutcome`:
  - [x] `SubmitPrompt { steer }` → `cmd_tx.send(AgentCommand::Prompt)`.
  - [x] `ClearTextarea` → replace textarea with a fresh configured default.
  - [x] `SaveTextareaAndClear` → snapshot textarea into
    `state.pending_approval_input_snapshot`, clear textarea.
  - [x] `RestoreTextarea` → restore snapshot.
  - [x] `TextareaKey(event)` → `textarea.input(event)`.
  - [x] `Paste(text)` → existing dedent + feed-chars logic.
  - [x] `ApproveTool { id }` → `cmd_tx.send(AgentCommand::ApproveTool { id })`.
  - [x] `DenyTool { id }` → `cmd_tx.send(AgentCommand::DenyTool { id, reason })`.
  - [x] `StopAgent` → `cmd_tx.send(AgentCommand::Stop)`.
  - [x] `Quit` → return `ActionOutcome::Quit`.
- [ ] Remove `pending_approval: Option<(Uuid, String, Value)>` from `AppState`;
  the tool ID and name are now held in `Focus::ToolApproval { tool_id, tool_name }`,
  and tool arguments remain solely in the corresponding `ToolBubble` chat entry.
  _Deferred: `pending_approval` is still needed by `ui::draw` (via a
  `TuiState` projection) until Step 5 migrates the view layer to `AppState`._
- [x] Replace `handle_action` with: convert `AppAction` → `TuiEvent`, call `update()`, call `execute_effects()`.
- [x] The confirm-timeout `select!` arm fires `update(&mut state, TuiEvent::Domain(DomainEvent::ConfirmExpired))`.
- [x] Remove `confirm_state` from driver-local variables (now `TuiMode::DoublePressAbort`).
- [x] Keep `mouse_pos` and `mouse_drag` as driver-local state.
- [x] Verify Focus transitions for `UiUpdate::ToolPending` / `ToolStart` / `ToolKilled` /
  `StreamCancelled` are handled via `DomainEvent` variants in the Presenter; the
  Driver does not manipulate `state.focus` directly.
- [x] Verify `make test` and `make lint` pass.

**Implementation notes (Step 4):**

- `ui::draw` still takes `&mut TuiState` (not `AppState`) because integration
  tests call it directly with `TuiState`. The Driver projects `AppState →
  TuiState` via `app_state_for_draw()` each frame, then syncs view-geometry
  fields back via `sync_from_draw()`. This intermediate layer is removed in
  Step 5 when `render.rs` is extracted and wired to `AppState` directly.
- `UiUpdate::ToolError` has no `DomainEvent` counterpart; it is handled
  directly by `AppState::apply_tool_error()` (added in this step).
- Helper methods `agent_mode()`, `show_help()`, `is_approval_pending()`,
  `approval_tool_name()`, and `apply_tool_error()` were added to `AppState`.
- `run_tui` returns the legacy `TuiState` (converted from `AppState` at exit)
  so integration-test assertions on `state.agent_mode` continue to compile.

### Step 5 — Extract `src/render.rs` from `src/ui.rs`

- [x] Create `src/render.rs` with `pub fn render(frame: &mut Frame, state: &mut AppState)`:
  - [x] Calls layout functions to obtain `Rect`s and writes them back to `state.layout.*`.
  - [x] Delegates to existing widget/draw functions kept in `src/ui.rs`.
- [x] Add `render.rs` to the coverage exclusion list in `Makefile` (same
  exclusion as `ui.rs` and `main.rs`) before running coverage checks, or
  coverage will fail.

### Step 6 — Wire new modules in `src/lib.rs` and `src/main.rs`

- [x] Add `pub mod app_state;`, `pub mod presenter;`, `pub mod render;` to `src/lib.rs`.
- [x] Re-export from `src/lib.rs`: `AppState`, `TuiMode`, `Focus`, `Effect`,
  `InputEvent`, `DomainEvent`, `AppLayout` and any sub-types integration tests need.
- [x] Keep the existing `pub mod app;` shim in place until Step 8.
- [x] Update `src/main.rs` and `src/tui_runner.rs` imports to use `app_state` and `presenter`.

### Step 7 — Migrate integration tests

- [ ] Update `tests/tui_state.rs` imports to use the new type names (`AppState`,
  `TuiMode`, `Focus`) where needed. Keep test intent, structure, and assertion
  logic unchanged.
- [ ] Verify all tests under `tests/` compile and pass with `make test`.

### Step 8 — Remove legacy `src/app.rs` and clean up

- [ ] Delete `src/app.rs` (the shim introduced in Step 2).
- [ ] Remove dead re-exports from `src/lib.rs`.
- [ ] Run `make lint` and fix all warnings/clippy findings.
- [ ] Run `make test-coverage` and confirm ≥80% threshold is met.

### Step 9 — Produce the TuiMode × Focus state diagram

- [ ] Create `docs/tui-reducer-state-diagram.mmd` as a Mermaid state diagram showing:
  - [ ] All `TuiMode` variants as states.
  - [ ] All `Focus` variants as orthogonal states (or a separate parallel diagram).
  - [ ] Events that trigger transitions between them (e.g. `CtrlC`, `StreamDone`,
    `ToolPending`, `ConfirmExpired`, `ShowHelp`, `Escape`).
- [ ] Render it to `docs/tui-reducer-state-diagram.mmd.svg`.

### Step 10 — Update documentation

- [ ] `docs/features.md` — update the TUI Runtime section to describe the
  reducer/MVP module layout (Model `app_state.rs`, Presenter `presenter.rs`,
  View `render.rs` + `ui.rs`, Driver `tui_runner.rs`).
- [ ] `docs/keybindings.md` — verify all keybinding descriptions remain accurate;
  update any that change due to the refactor.
- [ ] `docs/class-diagram.mmd` — update module diagram to show `app_state.rs`,
  `presenter.rs`, `render.rs`, and the Driver (`tui_runner.rs`) as separate
  nodes with directional dependencies.
- [ ] `docs/blueprint.md` — add or update a phase entry recording this refactor
  (state-handling consolidation) as completed, after Phase 4.
- [ ] `docs/development-workflow.md` — update any module-layout references to
  reflect the new file names.

## Implementation Hints

- Current module layout (all in `src/`):
  - `app.rs` — `TuiState`, `ChatEntry`, `ToolCallState`, `UserMessageState`,
    `ChatSelection`, `InputArea`
  - `tui_runner.rs` — `run_tui`, `handle_action`, all key/mouse/paste handlers,
    `confirm_state` driver-local, `ActionOutcome`
  - `events.rs` — `AppAction` enum, `map_event`, `spawn_crossterm_bridge`
  - `ui.rs` — `draw`, `draw_chat`, `draw_help_popup`, `ButtonAreas`
  - `agent.rs` — `AgentMode`, `AgentCommand`, `UiUpdate`, `AgentLoop`
- `AgentMode` (in `src/agent.rs`) has variants: `Idle`, `Pending`, `Streaming`,
  `ToolPending`, `Aborting`. It is the agent-loop's own state; the TUI mirrors
  the relevant subset in `TuiMode`.
- The `statig` FSM dependency mentioned in related idea docs was a *previous*
  plan (`docs/plans/refactor-tui-state-handling-plan.md`). The current codebase
  does **not** use `statig`; it uses the plain imperative `handle_action`
  approach. This refactor replaces that imperative approach directly.
- `src/ui.rs` is excluded from coverage (`ui.rs` and `main.rs` in the
  `Makefile` exclusion list) because it requires a live terminal. `src/render.rs`
  must be added to the same list.
- Integration tests live in `tests/tui_state.rs` and import via the `ylva` lib
  crate (`use ylva::{tui_runner, AppAction, ChatEntry, …}`). These tests must
  remain as integration tests — they exercise the Driver + Presenter + View
  composition and catch wiring bugs unit tests cannot see.
- `ratatui_textarea::TextArea` must remain in the Driver (owns cursor/scroll
  state). The Presenter returns `Effect::TextareaKey(KeyEvent)` and
  `Effect::Paste(String)` for the Driver to apply.
- Coverage threshold: ≥80% line coverage enforced by `make test-coverage`.
- `AppAction::CtrlC` and `AppAction::SigInt` are both in `events.rs`; `CtrlC`
  follows double-press confirm logic, `SigInt` is immediate. Both map to
  `InputEvent` variants in the conversion layer.
- Mouse routing currently uses raw `ratatui::layout::Rect::contains(Position)`.
  The new `ChatArea::contains`, `InputArea::contains`, `HelpPopup::hit` methods
  wrap identical logic but keep spatial knowledge co-located.
- `Focus::ToolApproval` stores `tool_id` and `tool_name` only. Tool arguments
  stay in the `ToolBubble` `ChatEntry` (already present for rendering) and are
  not duplicated in the focus variant.
- The existing `docs/tui-fsm-state-diagram.mmd.svg` in `docs/` provides the
  rendering precedent for Step 9's Mermaid SVG.
- First file to open / first test to run: open `src/app.rs` to inventory all
  `TuiState` fields, then run `make test` to establish a passing baseline before
  any changes.
