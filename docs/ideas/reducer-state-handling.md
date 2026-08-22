# Idea: Reducer-based TUI state handling

## Problem

The `statig` FSM approach (see `docs/plans/refactor-tui-state-handling-plan.md`) adds
significant complexity: proc-macro-generated code, `unsafe inner_mut()` in tests,
side-effect drain queues, and `#[superstate]` indirection. The result is harder to
read and trace than the original imperative code.

## Approach

Replace the `statig` FSM with a plain **reducer function**:

```text
fn update(state: &mut AppState, event: Event) -> Vec<Effect>
```

- `AppState` holds all view-model data plus `mode: TuiMode` and `focus: Focus` fields.
- `TuiMode` carries only agent-side control-flow data (is the agent idle, streaming, aborting?).
- `Focus` carries only input-routing data (where does keyboard input go right now?).
- `update()` routes events based on their type: input events by `Focus`, domain events
  by `TuiMode`. Hierarchy is expressed as nested function calls, not macros.
- The return value is a `Vec<Effect>` — driver-side actions to execute after the update.

## Key types

```rust
enum TuiMode {
    Idle { quit_confirm: Option<Instant> },
    Pending,
    Streaming,
    Aborting,
    DoublePressAbort { pressed_at: Instant, return_to: Box<TuiMode> },
}

enum Focus {
    MainInput,      // normal typing, history navigation
    HelpPopup,      // scroll keys go to help viewer; Escape closes it
    ToolApproval { tool_id: Uuid, tool_name: String },  // Enter approves, Esc denies
}

enum Effect {
    SubmitPrompt { steer: bool },
    ClearTextarea,
    SaveTextareaAndClear,
    RestoreTextarea,
    TextareaKey(KeyEvent),
    Paste(String),
    ApproveTool { id: Uuid },
    DenyTool { id: Uuid },
    StopAgent,
    Quit,
}
```

`TuiMode` and `Focus` are orthogonal: you can be `Streaming` (agent running) with
`Focus::HelpPopup` (user opened help mid-stream). Conflating them leads to the
state-explosion problem the original `statig` plan encountered.

## Driver loop

After each external event:

```rust
let effects = update(&mut state, event);
execute_effects(&mut state, &mut textarea, &agent_tx, effects).await?;
```

`execute_effects` owns the textarea and agent channel — the reducer itself is pure
and dependency-free.

## Event flow

One external event enters, state mutates, effects are executed once — no feedback loop,
no recursion, no settling:

```text
external event (keypress / agent message)
       │
       ▼
  update(state, event)  →  Vec<Effect>
       │                        │
  mutates state           driver executes each effect
                          (I/O: textarea, agent channel, quit)
```

The next cycle only starts when the next external event arrives. An effect such as
`SubmitPrompt` sends a message to the agent; the agent will eventually reply with a
fresh `TuiAgentEvent`, which enters the loop as a new event — not a recursive call.

## Three event kinds require different routing

Not all events are alike.

| Kind | Examples | Routed by |
|---|---|---|
| Keyboard / paste | key press, paste, resize | `Focus` — which widget owns keyboard right now |
| Mouse | click, scroll, drag | screen position (hit-test) — modified by modal focus |
| Domain | agent message, timer tick, signal | `TuiMode` — what the agent is doing |

Keyboard and mouse both feel like "input" but they need different dispatch strategies.
Domain events are entirely independent of both.

```rust
fn update(state: &mut AppState, event: Event) -> Vec<Effect> {
    match event {
        Event::Key(key)         => route_keyboard(state, key),
        Event::Mouse(mouse)     => route_mouse(state, mouse),
        Event::Paste(text)      => route_keyboard(state, InputEvent::Paste(text)),
        Event::Domain(domain)   => route_domain(state, domain),
    }
}
```

### Keyboard routing — exclusive focus

One entity owns keyboard input at a time. Global bindings are checked first; everything
else goes to the focused handler:

```rust
fn route_keyboard(state: &mut AppState, event: InputEvent) -> Vec<Effect> {
    // Global bindings always win regardless of focus
    match &event {
        InputEvent::ShowHelp => { … return vec![]; }
        InputEvent::SigInt   => { … return vec![Effect::StopAgent]; }
        _ => {}
    }
    // Focus-based dispatch for all other keys
    match &state.focus {
        Focus::MainInput                     => keyboard_main(state, event),
        Focus::HelpPopup                     => keyboard_help(state, event),
        Focus::ToolApproval { tool_id, .. }  => keyboard_approval(state, event, *tool_id),
    }
}
```

### Mouse routing — position-based with modal override

Mouse events carry a screen position. Routing is hit-test-based, but modal overlays
can capture all mouse events regardless of position.

**Modal vs non-modal overlays:**

| Overlay | Keyboard | Mouse scroll / click |
|---|---|---|
| Help popup (modal) | captured | captured; click outside → dismiss |
| Tool approval (non-modal) | captured | hit-tested; scroll over chat → chat scrolls |

The `Focus` enum distinguishes the two:

```rust
enum Focus {
    MainInput,
    HelpPopup,                       // modal — captures all input
    ToolApproval { tool_id: Uuid },  // non-modal — keyboard only
}

impl Focus {
    fn is_modal(&self) -> bool { matches!(self, Focus::HelpPopup) }
}
```

Mouse routing then becomes:

```rust
fn route_mouse(state: &mut AppState, event: MouseEvent) -> Vec<Effect> {
    if state.focus.is_modal() {
        // Modal overlay captures all mouse events.
        // A click outside dismisses the overlay.
        if matches!(event.kind, MouseEventKind::Down(_))
            && !in_rect(event.column, event.row, state.layout.help_popup)
        {
            state.focus = Focus::MainInput;
            return vec![];
        }
        return mouse_help_popup(state, event);
    }

    // Non-modal: route by screen position regardless of keyboard focus.
    // Scroll over chat works even while tool-approval footer has keyboard focus.
    if in_rect(event.column, event.row, state.layout.chat_area) {
        return mouse_conversation(state, event);
    }
    if in_rect(event.column, event.row, state.layout.input_area) {
        return mouse_input(state, event);
    }
    vec![]
}
```

### Domain routing — mode-based

Domain events (agent messages, signals) are independent of both focus and mouse
position. They always go to the mode handler:

```rust
fn route_domain(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match &state.mode {
        TuiMode::Idle { .. }       => domain_idle(state, event),
        TuiMode::Pending           => domain_pending(state, event),
        TuiMode::Streaming         => domain_streaming(state, event),
        TuiMode::Aborting          => domain_aborting(state, event),
        TuiMode::DoublePressAbort { return_to, .. }
                                   => domain_double_press(state, event, return_to.clone()),
    }
}
```

A keybinding like `ShowHelp` changes `Focus`, not `TuiMode` — the agent keeps running
unaffected. An agent `StreamDone` event changes `TuiMode` but never touches `Focus`.

## Focus best practices

- `Focus` changes are always triggered by explicit user actions. They are never changed
  by domain events.
- When a focused overlay closes, `Focus` returns to `MainInput`.
- Each `Focus` variant owns its complete keybinding rules; no other keyboard handler
  is active at the same time.
- Modal overlays (`is_modal() == true`) capture mouse events too; non-modal overlays
  capture keyboard only and let mouse hit-testing proceed normally.
- Global keybindings (`ShowHelp`, `SigInt`) are checked before focus dispatch and
  always win.

## Layout structs — typed UI regions with hit-test methods

Each UI region is a struct that owns its `Rect` and exposes behaviour derived from its
geometry. This makes hit-testing readable and keeps spatial knowledge (e.g. "scroll by
`height - 2` rows") in one place instead of scattered across render and presenter code.

```rust
// src/app_state.rs

struct ChatArea {
    rect: Rect,
}

impl ChatArea {
    fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }

    /// Number of rows to scroll on PageUp / PageDown.
    fn page_scroll_rows(&self) -> u16 {
        self.rect.height.saturating_sub(3)
    }
}

struct InputArea {
    rect: Rect,
}

impl InputArea {
    fn contains(&self, col: u16, row: u16) -> bool { … }
}

struct HelpPopup {
    rect:    Rect,
    visible: bool,
}

impl HelpPopup {
    fn hit(&self, col: u16, row: u16) -> bool {
        self.visible && self.rect_contains(col, row)
    }
}

struct AppLayout {
    chat:     ChatArea,
    input:    InputArea,
    help:     HelpPopup,
    approval: Option<ApprovalFooter>,
}

// AppState holds the layout:
pub struct AppState {
    pub mode:   TuiMode,
    pub focus:  Focus,
    pub layout: AppLayout,
    // … other view-model fields …
}
```

`render()` populates the layout structs by writing `Rect` values back after drawing.
This is the only write path from View to Model — a deliberate, narrow exception to the
MVP immutability rule:

```rust
// src/render.rs
pub fn render(frame: &mut Frame, state: &mut AppState) {
    let [chat_rect, input_rect] = Layout::vertical([…]).areas(frame.area());
    state.layout.chat.rect  = chat_rect;
    state.layout.input.rect = input_rect;

    render_chat(frame, chat_rect, state);
    render_input(frame, input_rect, state);

    if state.focus == Focus::HelpPopup {
        let popup_rect = centered_rect(60, 80, frame.area());
        state.layout.help.rect    = popup_rect;
        state.layout.help.visible = true;
        render_help_popup(frame, popup_rect, state);
    } else {
        state.layout.help.visible = false;
    }
}
```

### Concrete mouse routing using layout structs

```rust
// src/presenter.rs
fn route_mouse(state: &mut AppState, event: MouseEvent) -> Vec<Effect> {
    // Modal overlay captures all mouse events.
    // A click anywhere outside the popup dismisses it.
    if state.focus.is_modal() {
        if matches!(event.kind, MouseEventKind::Down(_))
            && !state.layout.help.hit(event.column, event.row)
        {
            state.focus = Focus::MainInput;
            return vec![];
        }
        return mouse_help(state, event);
    }

    // Non-modal: hit-test determines the target regardless of keyboard focus.
    // Scroll over chat works even while tool-approval footer has keyboard focus.
    if state.layout.chat.contains(event.column, event.row) {
        return mouse_chat(state, event);
    }
    if state.layout.input.contains(event.column, event.row) {
        return mouse_input_area(state, event);
    }
    vec![]
}

fn mouse_chat(state: &mut AppState, event: MouseEvent) -> Vec<Effect> {
    match event.kind {
        MouseEventKind::ScrollUp => {
            state.scroll_offset = state.scroll_offset.saturating_add(3);
            state.scroll_locked = true;
        }
        MouseEventKind::ScrollDown => {
            let max = state.chat_rendered_lines.saturating_sub(
                state.layout.chat.page_scroll_rows(),
            );
            state.scroll_offset = state.scroll_offset.saturating_sub(3).min(max);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Click on a bubble — collapse/expand or select
        }
        _ => {}
    }
    vec![]
}
```

### Concrete keyboard routing

```rust
fn route_keyboard(state: &mut AppState, event: InputEvent) -> Vec<Effect> {
    // Global bindings — checked before any focus dispatch
    match &event {
        InputEvent::ShowHelp => {
            state.focus = match state.focus {
                Focus::HelpPopup => Focus::MainInput,
                _                => Focus::HelpPopup,
            };
            return vec![];
        }
        InputEvent::SigInt => {
            state.mode = TuiMode::Aborting;
            return vec![Effect::StopAgent];
        }
        _ => {}
    }

    // Focus owns remaining keyboard events
    match &state.focus.clone() {
        Focus::MainInput                     => keyboard_main(state, event),
        Focus::HelpPopup                     => keyboard_help(state, event),
        Focus::ToolApproval { tool_id }      => keyboard_approval(state, event, *tool_id),
    }
}

fn keyboard_help(state: &mut AppState, event: InputEvent) -> Vec<Effect> {
    match event {
        InputEvent::Escape => {
            state.focus = Focus::MainInput;
        }
        InputEvent::KeyInput(key) => match key.code {
            KeyCode::Up       => state.help_data.scroll_up(),
            KeyCode::Down     => state.help_data.scroll_down(),
            KeyCode::PageUp   => state.help_data.scroll_up_by(
                state.layout.help.page_scroll_rows(),
            ),
            KeyCode::PageDown => state.help_data.scroll_down_by(
                state.layout.help.page_scroll_rows(),
            ),
            _ => {}
        },
        _ => {}
    }
    vec![]
}

fn keyboard_approval(
    state: &mut AppState,
    event: InputEvent,
    tool_id: Uuid,
) -> Vec<Effect> {
    match event {
        InputEvent::KeyInput(key) if key.code == KeyCode::Enter => {
            state.focus = Focus::MainInput;
            if state.input_is_empty {
                vec![Effect::RestoreTextarea, Effect::ApproveTool { id: tool_id }]
            } else {
                vec![Effect::DenyWithTextarea { tool_id }]
            }
        }
        InputEvent::Escape | InputEvent::CtrlC => {
            state.focus = Focus::MainInput;
            vec![Effect::RestoreTextarea, Effect::DenyTool { id: tool_id }]
        }
        InputEvent::KeyInput(key) => vec![Effect::TextareaKey(key)],
        _ => vec![],
    }
}
```

The `page_scroll_rows()` method is called identically from mouse scroll (to compute
step size) and keyboard `PageUp`/`PageDown` — the layout struct is the single source
of truth for that geometry.

## MVP separation in code

| MVP role | Responsibility | Proposed module |
|---|---|---|
| **Model** | Pure data; no logic, no rendering | `src/app_state.rs` |
| **Presenter** | `update()`, `route_keyboard()`, `route_mouse()`, `route_domain()`, per-mode/focus handlers | `src/presenter.rs` |
| **View** | `render()`, widget functions — reads `AppState`, writes only `layout` | `src/render.rs` + `src/widgets/` |
| **Driver** | Event loop, `execute_effects()`, owns textarea and channels | `src/tui_runner.rs` |

The Driver is not an MVP concept but is necessary in a TUI: it owns all I/O handles
and executes the `Effect` commands the Presenter emits. It is the only layer allowed
to do I/O.

Rules enforced by module boundaries:

- `app_state.rs` has no `use ratatui`, no `use crossterm`, no channel types.
- `presenter.rs` has no `use ratatui`, no direct channel sends — only returns `Vec<Effect>`.
- `render.rs` takes `&mut AppState` but only writes to `state.layout`.
- `tui_runner.rs` owns `AppState`, `TextArea`, and channel handles; calls `update()`
  and `render()` and executes effects.

## Relationship to other patterns

### Elm Architecture / Redux

Identical to Elm's `update(model, msg) -> (Model, Cmd)` and Redux's
`reducer(state, action) -> state` with side-effect middleware. Effects correspond to
Elm's `Cmd` or Redux middleware actions.

### Chain of Responsibility (GoF)

The two-lane dispatch (`route_input` → focus chain, `route_domain` → mode chain) is
a lightweight Chain of Responsibility. Each handler in the chain decides to consume or
ignore. Adding a new overlay is adding a new `Focus` variant and a new handler
function — no existing code changes.

### State pattern (GoF) — optional extension

Each focus or mode can be a struct implementing a trait, keeping logic co-located with
the state type rather than in a large `match`:

```rust
trait InputHandler {
    fn handle(&self, state: &mut AppState, event: InputEvent) -> Vec<Effect>;
}

trait DomainHandler {
    fn handle(&self, state: &mut AppState, event: DomainEvent) -> Vec<Effect>;
}
```

This is an optional refinement if individual handlers grow large. Start with plain
functions; extract to trait impls only when a handler warrants it.

### Command pattern (GoF)

`Effect` is a Command object; `execute_effects` is the invoker. The Presenter produces
commands; the Driver executes them. The Presenter never knows how or when they run.

## Benefits over `statig` FSM

- No external crate, no proc macros, no generated code to reason about.
- `state.mode` and `state.focus` are plain fields — no `*fsm.state()` or `unsafe`.
- Effects are the function return value — no drain queue, no side-effect accumulation.
- Tests set `state.mode` / `state.focus` directly and assert on `update()` return value.
- Input routing and domain routing are explicitly separate — no superstate hierarchy
  needed to express "agent events pass through help overlay unchanged".
- Module boundaries enforce MVP roles — compiler catches violations.
