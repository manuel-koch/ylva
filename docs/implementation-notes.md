# Implementation Notes

Internal mechanics, module/struct/field references, and design rationale that
back the user-facing behavior described in [features](./features.md). This
document is for contributors working on Ylva's internals, not end users.

## Tool-result wire protocol

Every `role:"tool"` message carries its `content` as a JSON struct derived
from `ToolEnvelope`. Success shape:

```json
{
  "ok": true,
  "result": "...",
  "result_metadata": { "field": value, ... },
  "result_metadata_schema": { "field": { "type": "...", "description": "..." }, ... }
}
```

Failure shape:

```json
{ "ok": false, "error": "..." }
```

`result_metadata` and `result_metadata_schema` are omitted when a tool
produces no structured metadata. When present, `result_metadata_schema` is
derived automatically from the tool's typed metadata struct (via `schemars`)
and covers only the fields actually populated for that result — the schema
can never drift from the data. Each tool description sent in the `tools`
array includes its argument schema.

## Path-like argument normalization

Path-like arguments (`read`/`write`/`edit`/`search`'s `path`, `glob`'s
`pattern`) are normalized before dispatch: an absolute value that falls
inside the process's current working directory is rewritten to its
cwd-relative form — this is what actually executes and what the UI renders
for the call, not merely cosmetic. Values that are already relative, or
absolute but outside the cwd, pass through unchanged. This is a
display/consistency nicety, not a security boundary — sandbox approval
checks run independently on whichever path form ends up being used. Each
tool declares its own path-like argument keys via
`Tool::path_like_arguments()`.

## `bash` tool process-group cleanup

On timeout or cancellation the entire process group is killed via `SIGKILL`
(Unix) using `nix::sys::signal::killpg` — grandchildren spawned by the script
are also cleaned up. Default timeout is `DEFAULT_BASH_TIMEOUT_SECS` (30s).

## CLI driver

Implemented in `src/cli_driver.rs`; selected automatically by `main.rs` when
stdout is not a terminal or forced via `--no-tui`.

When `--auto-approve-spawn` is set and the assistant calls `spawn_subagent`,
the CLI keeps the process alive past the assistant's own final `StreamDone`
until every spawned/queued child call has reported a terminal outcome
(`ToolDone` on success/failure/cancellation, or `ToolError` in the
structurally-unreachable no-`ChildAgentManager` fail-safe) — otherwise the
one-shot process would exit as soon as the parent goes idle, silently
discarding the child's result since `spawn_subagent` resolves immediately
with `{"status": "running"/"queued"}` while the child keeps running
concurrently. Without `--auto-approve-spawn`, `spawn_subagent` is denied
outright in CLI mode (`UserApprovalRequired` has no approval path headless),
so this only matters when that flag is used.

## Child-agent delegation internals

- `AgentFactory::create` wires a `ChildAgentManager` onto any `Role::Root`
  `AgentLoop` whose resolved `AgentConfig::can_spawn` is `true`; children are
  constructed via the same factory (`Role::Child(ChildRoleConfig { .. })`)
  and never receive `spawn_subagent` themselves.
- At most one child runs at a time; further `spawn_subagent` calls are
  queued in an unbounded FIFO and started automatically as each preceding
  child finishes.
- A child's outcome — success, failure, or an "unstructured" fallback — is
  injected into the parent's conversation as a new trailing `system` message
  referencing the original `spawn_subagent` call's `tool_call_id`, never by
  rewriting or duplicating that call's provisional tool result; the
  tool-calling wire protocol only permits one tool-result message per
  `tool_call_id`. That system message also tells the root that the result is
  already shown to the user verbatim (the tool bubble's Result/Error/Sources
  sections), so it should not repeat/quote it back — only add new analysis,
  a decision, or a next step. If the root agent is otherwise idle at that
  moment (no stream or tool in flight), this immediately re-prompts the root
  LLM with the updated context so it reacts to the result without waiting for
  a new user message (essential for one-shot headless sessions, which never
  get a "next turn" otherwise); if the root is still busy with unrelated work
  (mid-stream or running its own tool round), the result is picked up once
  that work's own queue-drain/`StreamDone` re-prompt happens next.
- `AgentCommand::Stop` on the parent cancels the running child (and drains
  the queue) via `ChildAgentManager::cancel_all`; each cancelled/queued call
  is resolved to a terminal `ToolDone` (`status: "cancelled"`) and logged
  (`tracing::info!`) — durable persistence of child outcomes is deferred to
  a later phase. Unlike a natural finish, this never re-prompts the root
  (the user explicitly asked to stop).
- A child's own tool calls go through the exact same `ToolPending` →
  approve/deny pause as the root's — a child is never auto-denied just for
  being a child. Each `AgentLoop` stamps its own `run_id` (`None` for the
  root, `Some(child_run_id)` for a `Role::Child`) onto every
  `UiUpdate::ToolPending`/`ClarifyPending` it emits. Drivers (`cli_driver`,
  `tui_driver`) track that `run_id` alongside the pending call and, when
  answering, wrap the verdict in `AgentCommand::ForChild { run_id, command }`
  whenever it's `Some`. The root's command loop resolves `run_id` to the
  still-running child's own command channel via
  `ChildAgentManager::running_command_sender` and forwards the inner command
  verbatim; a `run_id` with no matching running child (already finished, or
  unknown) is logged (`tracing::warn!`) and dropped rather than panicking or
  stalling the root.

### Drill-in TUI conversation view internals

- `ChildAgentManager::start` emits `UiUpdate::ChildAgentStarted { run_id,
  call_id, agent_type, task, label }` immediately before its usual
  `ToolProgress`; `call_id` is the `spawn_subagent` tool call's own id (the
  root `ToolBubble`'s `id`), threaded through purely to correlate a `run_id`
  with its bubble (`ChildRunInfo::bubble_id`). The child's own streamed
  text/thinking tokens are forwarded as `UiUpdate::ChildAgentToken { run_id,
  text }` (merged text and thinking into one buffer — no separate thinking
  bubble for the child), and `UiUpdate::ChildAgentDone { run_id, status,
  result }` marks that child's conversation as finished. The child's own
  `StreamStarted`/`StreamDone`/`StreamCancelled` lifecycle events are
  swallowed, not forwarded — they would otherwise corrupt the parent's own
  `TuiMode`.
- `AppState` tracks `active_view: ConversationView` (`Root` or `Child {
  run_id, agent_type, task, label }`, default `Root`), `child_conversations:
  HashMap<Uuid, ChildConversation>` keyed by `run_id` (each entry holds
  `info: ChildRunInfo { run_id, bubble_id, agent_type, task, label }`,
  `messages: Vec<ChatEntry>`, and `partial_text: String`), `running_child:
  Option<ChildRunInfo>` (the child currently *executing* — cleared on
  `ChildAgentDone`, but its `child_conversations` entry is never removed, so
  a finished child's transcript stays drillable), and `auto_switched_to_child:
  bool`. A `run_id`'s entry is created once, on `ChildAgentStarted`, and
  persists for the lifetime of the TUI session — this is the fix for a bug
  where the entire drill-in model tracked only a single global child
  conversation, which got silently overwritten by whichever child started
  most recently while the user was still viewing an older one's transcript
  (including the "queued child starts while an active one's view is open"
  case). `running_child` (*which child is executing*), `active_view` (*which
  child the user is viewing*), and `child_conversations` (*every child ever
  seen*) are deliberately three independent concerns — conflating
  "executing" with "viewable" was the root cause. The model tracks each
  child by its own map entry rather than a single shared slot specifically
  so it stays correct if child execution ever becomes truly parallel
  (`ChildAgentManager` today still runs one child at a time, but nothing in
  the TUI's viewing/storage layer assumes that).
- Spawning a child never changes `active_view` — the user stays on whatever
  they were looking at. Only an *interactive pause belonging to the
  currently-executing child* (`ToolPending`/`ClarifyPending` tagged with
  that child's `run_id`) auto-switches `active_view` to `Child` and arms
  `auto_switched_to_child` — unless the user is already viewing that exact
  running child's view. This auto-switch fires regardless of what
  `active_view` was beforehand: `Root`, or a different (e.g.
  already-finished) child's view the user had manually drilled into. The
  view active immediately before the auto-switch is snapshotted into
  `AppState.pre_pause_view` (`Root` or the other child's `Child { .. }`) so
  it can be restored precisely. When that pause resolves, the view
  auto-returns to `pre_pause_view` (falling back to `Root` if none was
  recorded) **only if** the switch that got there was automatic — a user who
  navigated to `Child` manually (double-clicking the spawn bubble) is never
  yanked back automatically.
  - Concurrent root-pending-approval and child-interactive-pause precedence
    is not implemented — `TuiMode`/`Focus` remain a single, global pause
    state (matching the "one active child at a time" delegation-depth-1
    constraint), so this is a known, deferred edge case.
- Double-clicking a `spawn_subagent` tool bubble's title resolves the
  clicked bubble's id to its owning `run_id` via
  `AppState::run_id_for_bubble` (a reverse lookup over `child_conversations`
  matching `info.bubble_id`) and switches to that specific child's view
  (`switch_to_child_view(run_id)`) — never "whichever child is currently
  running" or "whichever child started most recently". This is what makes
  drilling into an *older*, already-finished subagent's bubble correct even
  while a newer one is running or queued. This is deliberately distinct from
  the single-click collapse/expand toggle every bubble (including spawn
  bubbles) always supports, so the two gestures never collide. A first click
  never toggles immediately: it only arms `last_bubble_click` `(Uuid,
  Instant)`; the actual collapse/expand toggle is deferred until
  `DomainEvent::BubbleClickWindowExpired` fires (a `tokio::time::sleep`
  future polled in `tui_driver`'s `select!`, mirroring the existing
  `confirm_timeout`/`ConfirmExpired` double-press-abort pattern) with no
  confirming second click having arrived within `DOUBLE_CLICK_WINDOW`
  (250 ms). This keeps the bubble's rendered height — and therefore any
  bottom-pinned auto-scroll — unchanged between the two clicks of a
  double-click, so the second click's row reliably resolves back to the same
  bubble; toggling immediately on the first click was found to break this
  correlation in practice. Because a child's `child_conversations` entry
  isn't removed on completion, drilling in still works after the subagent
  has finished — a lone single click on a finished bubble only
  expands/collapses it once the window expires.
- While viewing `Child { run_id, .. }`, the main chat area renders that
  specific `child_conversations[run_id].messages` instead of `chat_entries`.
  A non-scrolling header above it re-renders the *real* root `spawn_subagent`
  `ToolBubble` (same chrome, same collapsed/expanded state, same
  `agent_type`/`label`/`task` content the user saw before drilling in)
  rather than a separately-maintained header string — the bubble is found
  via `ChildRunInfo::bubble_id`, an exact, permanent correlation captured at
  `ChildAgentStarted` time (the spawn_subagent tool call's own id), not by
  guessing "the most recently started spawn bubble". The header's title is
  shortened to `"Subagent"` instead of the plain title-cased `"Spawn
  Subagent"` shown for the same bubble in the root conversation (via
  `ToolBubbleArgs::display_title_override` in `tui_widgets.rs`). A child's
  transcript is **not** seeded with a synthetic task message — the header
  bubble already shows the task. If the child is paused for its own
  approval/clarify, the input box behaves normally (`Enter` routes to the
  child's flow via `Focus::ToolApproval`/`ClarifyPending`, unaffected by
  `active_view`); if the child is merely running, `Enter` with a non-empty
  draft is a no-op instead of silently steering the *root* agent — the
  previous behavior queued a steer for `Root` while the user was looking at
  the child's transcript, with no visual indication of which conversation
  the message actually went to. This is called out in the footer (`enter:
  send` is omitted entirely while viewing a running child), not via a
  separate overlay on the input box, since keystrokes are still buffered
  normally either way.
- The spawn-bubble uses the same `Color::Magenta` border as every other tool
  bubble — no separate color styling was introduced for it.
- The footer/header spinner glyph is driven by `AppState.spinner_frame`,
  incremented once per tick in `tui_driver`'s event loop — gated by
  `spinner_should_tick()`, which checks `agent_mode() != AgentMode::Idle`
  rather than raw `TuiMode`. This matters specifically for a spawned
  subagent: the root's own `TuiMode` settles to `Idle` almost immediately
  after `spawn_subagent` is called (the root's own turn is done; it's just
  waiting on the tool), so gating on raw `TuiMode` alone would freeze the
  spinner glyph on both the footer and the child-view header bubble for the
  subagent's entire run. `agent_mode()` already reports `ToolRunning`
  whenever a tool bubble (including `spawn_subagent`) is still `Running`,
  keeping the spinner animating — including while the subagent itself is
  paused for its own inner tool approval, since that only sets
  `Focus::ToolApproval` and never touches the root `spawn_subagent` bubble's
  `Running` state.
- A thin separator line (the same dim `─` rule used between the chat area
  and the input/footer panel) is rendered directly below the child-view
  header bubble, so it never reads as part of the scrollable
  conversation-list beneath it.
- The header bubble scrolls independently of the chat area when its
  rendered content exceeds the clamped max height (40% of the terminal):
  `AppState.child_view.header_bubble_scroll_offset` drives a
  `Paragraph::scroll` offset, clamped each frame in `tui_widgets::draw` to
  the header's actual overflow, and a scrollbar (reusing
  `render_chat_scrollbar`) appears in a 1-column gutter to its right only
  when the content overflows. Mouse-wheel events over the header's rect
  (`AppState.layout.header_bubble`, hit-tested in `presenter::route_mouse`
  ahead of the clarify-banner/chat-area checks) scroll the header instead of
  the chat area. The offset resets to `0` whenever a fresh child view is
  entered (`switch_to_child_view` / `ChildAgentStarted`).
- `switch_to_child_view` (manual drill-in via double-click) also re-pins the
  main chat area's scroll to the bottom (`conversation.scroll_offset = 0`,
  `conversation.scroll_locked = true`) — these fields are shared between the
  root and child views (not per-view state), so a non-bottom position left
  over from scrolling up while viewing `Root` would otherwise carry straight
  into the child transcript. Before doing so, it saves the root's own
  `(conversation.scroll_offset, conversation.scroll_locked)` into
  `AppState.saved_root_scroll` (an immediate best-effort fallback), and also
  captures the drilled-into `spawn_subagent` bubble's `Uuid` into
  `AppState.saved_root_scroll_bubble`. Manual "back to parent" navigation
  (`switch_to_root_view`, triggered by `Esc` with an empty draft) restores
  `saved_root_scroll` for the current frame, and moves
  `saved_root_scroll_bubble` into `AppState.pending_scroll_to_bubble` for
  precise resolution: once `active_view` has flipped back to `Root` and the
  next frame's `AppState.conversation.bubble_title_rows` reflects root's
  transcript again, `render::sync_from_draw` looks up that bubble's
  *current* row and scrolls there, unlocking scroll — this stays correct
  even if the root conversation grew while the child view was open. The net
  effect: the user lands back exactly at the root `spawn_subagent` bubble
  they drilled in from, not just near it. `note_child_interactive_pause`'s
  auto-switch into `Child` (an interactive pause, not manual navigation)
  never populates `saved_root_scroll`/`saved_root_scroll_bubble`, so its
  later auto-return via `switch_to_root_view` is a no-op on scroll.
- When a tool-approval is pending for the tracked subagent and the user is
  looking at `Root` (having manually navigated back via `Esc` while the
  subagent's own tool call is still awaiting approval), the approval
  footer's title is prefixed with `"Subagent tool '<name>' requests your
  approval:"` instead of the plain `"Tool '<name>' requests your approval:"`
  — otherwise the pending approval is invisible from the root transcript.
  The distinction is derived from whether `Focus::ToolApproval`'s `tool_id`
  lives in any `child_conversations` entry's `messages` (subagent) or
  `chat_entries` (root), independent of `active_view`.
- Clicking anywhere on the child-view header bubble toggles its collapsed
  state — unlike a normal chat bubble, this needs no click-vs-double-click
  debounce, since the drill-in gesture only applies from the root
  transcript. `AppState::header_bubble_id()` is the single source of truth
  for which real `chat_entries` bubble the header re-renders (used by both
  `render::project_for_draw` and `presenter::route_mouse`), and
  `AppState::toggle_header_bubble_collapsed()` flips that bubble's
  `collapsed` flag directly — deliberately not reusing
  `toggle_tool_bubble_collapsed`, which also clamps
  `conversation.scroll_offset`/`conversation.scroll_locked` against
  `conversation.bubble_title_rows`; those fields describe the
  currently-visible chat area (the child's own transcript), not the header,
  so touching them here would be a side effect on the wrong view's scroll
  state.
- The `collapsed` flag toggled above lives on the same `chat_entries`
  `ToolBubble` entry that represents the `spawn_subagent` call itself, so it
  survives the subagent finishing: neither `DomainEvent::ToolDone` (which
  only patches the entry's `state`) nor `ChildAgentDone` (which only flushes
  pending partial text and may switch `active_view` back to `Root`) touch
  `collapsed`. A manual expand is therefore preserved when the subagent
  completes, and re-entering the drilled-in view later (e.g. by
  double-clicking the now-finished bubble from `Root`) still shows it in
  whichever state it was last left. Like any other tool bubble, a
  `spawn_subagent` bubble still *starts* `collapsed` by default until the
  user toggles it once.

## TUI module layout

The TUI follows a **reducer/MVP** split across source modules. It is selected
automatically in `main.rs` via `std::io::stdout().is_terminal()`; the TUI is
a rendering layer on top of the headless `AgentLoop` — the agent architecture
itself is unchanged.

### Model

`src/tui_reducer_state.rs` — pure view-model with no I/O. Defines `AppState`,
`TuiMode`, `Focus`, `AppLayout`, `Effect`, `InputEvent`, and `DomainEvent`.

### Presenter

`src/presenter.rs` — pure `update(state, event) → Vec<Effect>` reducer. No
ratatui imports, no channel sends; fully unit-testable without a live
terminal.

`route_domain` (the `DomainEvent` dispatch entry point) logs a `DEBUG`
`"domain event handled"` event for every `DomainEvent`, carrying only the
event's variant tag (`event`), its tool/clarify `id` when it has one
(`correlation_id`), and the `TuiMode` transition (`mode_before`/
`mode_after`) — never chunk text, tool arguments, or tool output. This is
enough to follow the TUI's state-transition flow and match up a call's
`ToolStart`/`ToolProgress`/`ToolDone` (or `ClarifyPending`) sequence across
log lines, without risking sensitive payloads in the log file.

### View

`src/render.rs` + `src/tui_widgets.rs` — `render(frame, state)` entry point.
Writes computed geometry back to `state.layout`; delegates widget rendering
to helpers in `tui_widgets.rs`.

### Driver

`src/tui_driver.rs` and `src/cli_driver.rs` — async event loops, one per
runtime mode, selected by `main.rs` based on whether stdout is a terminal.

`tui_driver.rs` owns `TextArea` and channel handles, converts `AppAction →
InputEvent` and `UiUpdate → DomainEvent`, calls `update()`, then executes the
returned `Vec<Effect>` via `execute_effects()`.

`cli_driver.rs` consumes `UiUpdate` events from the agent, streams the
assistant response to stdout, and prints the final conversation as JSONL on
exit.

## TUI behavior internals

- **Steer bubble lifecycle:** a steer (a message typed while the agent is
  busy) is rendered `Queued` immediately, near the bottom of the chat, but is
  only *staged*, not yet committed to the conversation log — this avoids
  fixing its position ahead of thinking/tool-call content for the in-flight
  round that hasn't been flushed yet. It becomes `Steered` and is committed
  to the log — after everything already flushed for that round — once
  `SteerApplied` arrives, at which point it renders identically to a normal
  `Sent` message: once injected, the model has seen it like any other user
  message, so the UI only distinguishes "queued" from "seen", not *why* a
  message was seen.
- **Double-press `Ctrl+C`/`Esc`:** each key has its own 2-second confirm
  window (`CONFIRM_TIMEOUT`); only a second press of the *same* key confirms
  — pressing the other key starts its own fresh window instead, so e.g.
  `Ctrl+C` then `Esc` never quits on that `Esc`.
- **Smart auto-scroll:** `DomainEvent::StreamDone`/`StreamCancelled` do
  **not** force auto-scroll back on — a completed response respects
  whatever scroll-lock state the user left it in, the same way in-flight
  `StreamChunk`/`StreamThinkingChunk` updates already did; otherwise
  scrolling up to view an earlier subagent bubble (or a plain scroll-back in
  a long conversation) would get yanked back to the bottom the moment the
  current response finished.
- **Footer mode-word disambiguation:** the plain mode word
  (`"Streaming…"`/`"Running tool…"`/etc.) always reflects the *root* loop's
  own `TuiMode`, never a drilled-into child's. Once the child's
  `spawn_subagent` bubble reaches a terminal state while the root is still
  busy producing its final reply, the footer overrides the word with
  `"Subagent done, parent working…"` instead of the misleading root word.
  While drilled into a live child, the root's `agent_mode()` would report
  `"Running tool…"` for as long as the `spawn_subagent` bubble is `Running`
  — correct for the root footer but misleading for the child's own
  transcript — so the footer instead inspects the child's own `TurnBuffer`
  (projected per-child via `render::project_for_draw`) and shows `"Subagent
  thinking…"`/`"Subagent streaming…"`/`"Subagent working…"` as appropriate.
- **Queued subagent bubbles:** show a static `≡` glyph instead of the
  animated spinner while `status=queued` (waiting for the running slot to
  free up); the spinner only appears once `status=running`.
- **Shift+Enter / Alt+Enter:** insert a newline via the kitty keyboard
  protocol (`DISAMBIGUATE_ESCAPE_CODES`), so terminals that support it (ZED,
  iTerm2, kitty, WezTerm) report the modifier and the newline is not
  mistaken for send.
- **Message dedenting:** submitted messages are automatically dedented —
  common leading whitespace is stripped (via `textwrap::dedent`) and
  leading/trailing blank-only lines are removed.
