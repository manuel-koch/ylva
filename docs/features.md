# Ylva agent-harness features

## General

- Rust-based terminal AI agent application, runnable as a CLI
- Implements agent-harness functionality

## Build Info

Use `ylva --version` to print build provenance (`Version`, `Commit`,
`Built`, `Profile`).

## Logging

Logging is opt-in and controlled entirely by CLI flags (there is no logging
configuration file).
See [CLI options](./cli-options.md) for usage of `--log-file`, `--log-level`
and `--log-format` options.

## CLI Runtime

Non-interactive, one-shot session: the initial user turn is sent to the
agent, the assistant response is streamed to stdout as it arrives, and the
final committed conversation history is printed as JSONL on exit — one JSON
object per message, in the same wire shape the provider sees.

Implemented in `src/cli_driver.rs`; selected automatically by `main.rs` when
stdout is not a terminal or forced via cli flag `--no-tui`.

Initial user turn resolution: `--prompt <text>`, else piped stdin read to EOF
(only when stdin is not a terminal) — e.g. `echo '…' | ylva`.

Tool approval is host-driven: `read`/`glob` are auto-approved immediately;
**WIP:** `write` is always denied and the denial is fed back to the model as a
`ToolError` plus a follow-up prompt.

Tool status lines render as `[tool] <title> …` (e.g. `[tool] Read docs/features.md [limit=32]`)

**WIP:** Provider selection is from environment variables with Ollama-first precedence
(`OLLAMA_*`, then `OPENROUTER_*`), else `MockProvider` when no backend is configured.

## TUI Runtime

Interactive terminal UI launched automatically when stdout is a terminal
(see [Phase 4: TUI skeleton](#phase-4-tui-skeleton)). When stdout is not a
terminal (piped or redirected), Ylva falls back to the CLI driver.

## Model Provider

Currently the following AI model providers are supported:

- Ollama ( via OpenAI compatible endpoints )
- OpenRouter ( via OpenAI compatible endpoints )

## Tools to be used by models

The following tools can be used by AI models during a conversation turn.

Tools report their results via **uniform tool-result envelope**:
Every `role:"tool"` message carries its `content` as JSON struct

- `{"ok":true,"result":...}` on success
- `{"ok":false,"error":...}` on failure

**WIP:** `read` and `write` currently only support UTF-8 code points via lossy decode.

**WIP:** `write` is currenty a stub toolthat is unconditionally denied!

- **read**: Read file content from given `path` using `mode`/`offset`/`limit`.
  Mode can be `bytes`/`chars`/`lines`.
  Offset and limit are 0-based!
- **write**: Write `content` to file at given `path`.  
- **glob**: Search for paths using a glob `pattern`, including wildcards like `*`, `?`, `**`, `[...]`.

## Phase 4: TUI skeleton

The TUI is a **rendering layer** on top of the headless `AgentLoop`; the agent
architecture is unchanged. It is selected automatically in `main.rs` via
`std::io::stdout().is_terminal()`.

### TUI module layout

The TUI follows a **reducer/MVP** split across source modules:

#### Model

`src/app_state.rs` — pure view-model with no I/O. Defines `AppState`,
`TuiMode`, `Focus`, `AppLayout`, `Effect`, `InputEvent`, and `DomainEvent`.

#### Presenter

`src/presenter.rs` — pure `update(state, event) → Vec<Effect>` reducer. No
ratatui imports, no channel sends; fully unit-testable without a live terminal.

#### View

`src/render.rs` + `src/ui.rs` — `render(frame, state)` entry point. Writes
computed geometry back to `state.layout`; delegates widget rendering to helpers
in `ui.rs`.

#### Driver

`src/tui_driver.rs` and `src/cli_driver.rs` — async event loops, one per
runtime mode, selected by `main.rs` based on whether stdout is a terminal.

`tui_driver.rs` owns `TextArea` and channel handles, converts
`AppAction → InputEvent` and `UiUpdate → DomainEvent`, calls `update()`, then
executes the returned `Vec<Effect>` via `execute_effects()`.

`cli_driver.rs` consumes `UiUpdate` events from the agent, streams the
assistant response to stdout, and prints the final conversation as JSONL on
exit.

- **Layout:** a scrollable chat-history area, a variable-height input box
  (capped at 1/3 of the terminal height), a `─` separator line, and a 2-row
  footer (mode/hint row and key-hints row). No sidebar.
- **Input always active:** the input box is never disabled. `Enter` sends
  unconditionally — while the model is streaming the message is queued as a
  pending steer, and `Ctrl+C`/`Esc` interrupt the current round.
- **Steer bubble lifecycle:** a user message appears immediately on `Enter`. It
  is rendered `Queued` (yellow) while pending, then becomes `Steered` (light
  green) when `SteerApplied` arrives — visually distinct from a normal `Sent`
  message to identify mid-stream corrections.
- **Cancelled steers:** interrupting marks all pending bubbles `Cancelled`
  (grey) with a dimmed "Not sent." line. The text is retained in input history
  so it can be recovered with `↑`.
- **Tool approval sub-mode:** when a tool awaits approval the footer switches
  to `[ ✓ Approve (Enter) ]  [ ✗ Deny (Esc) ]` (mouse-clickable). `Enter`
  sends `ApproveTool`, `Esc` sends `DenyTool` immediately; `Ctrl+C` still uses
  the double-press confirm. The input field is available to type a reason for
  denial (a dimmed `"Enter reason for denial here…"` placeholder appears when
  the field is empty). The user's draft is snapshotted when approval sub-mode
  starts and restored automatically after the tool is approved or denied.
- **Double-press `Ctrl+C`/`Esc`:** a key-agnostic confirm with a 2-second
  window (`CONFIRM_TIMEOUT`). While streaming/tool-pending the first press
  shows `"Press again to cancel"` and the second stops the round; while idle
  `Ctrl+C` double-press quits and `Esc` just clears the input. A natural
  transition to `Idle` clears confirm state. OS SIGINT stops immediately (no
  double-press).
- **Help popup (`F1`):** toggles a centered popup with the full keybinding
  table and `BuildInfo` values.
- **Smart auto-scroll:** the chat follows the newest output by default; the
  mouse wheel and `PageUp`/`PageDown`/`End` scroll the history directly (no
  focus needed, so the wheel works anywhere over the TUI). Scrolling up
  disables auto-scroll; scrolling back down to the bottom (or pressing `End`)
  re-enables it. The scroll offset is clamped to the content height, so
  scrolling past either end never silently drifts.
- **Wordmark:** while the conversation is empty (no messages submitted yet), a
  horizontally centered ASCII-art "Ylva" wordmark is displayed with the version
  and git commit shown below it in DarkGray. Once the first message is sent,
  the wordmark disappears and the conversation fills the chat area.
- **Input history:** `↑` on the first line loads the previous sent message
  (draft is saved); `↓` restores it; any edit while browsing copies the entry
  to a fresh draft.
- **Line navigation:** `Fn+Left`/`Fn+Right` move the input cursor to the
  start/end of the current line, wrapping to the previous/next line.
- **Starts empty:** the TUI launches with an empty input box and no auto-sent
  turn — the hardcoded demo prompt is headless-only. An explicit `--prompt` or
  piped stdin still auto-sends on startup.
- **Sending state:** after `Enter` (or an auto-sent initial prompt) the footer
  shows `"Sending…"` until the first stream event arrives, instead of `"Idle"`.
- **Input as a message:** the input box renders like a sent user message — a
  colored left border (Cyan), no full frame and no `"Input"` title. Long text
  **word-wraps** inside the box (no horizontal scrollbar), and the box grows
  with its content up to the 1/3 height cap.
- **Mouse editing:** a left-click in the input box places the cursor at the
  clicked character (word-wrapping honored); a left-drag selects text. `read`/
  `glob` tool status lines and approval buttons remain mouse-clickable.
- **Shift+Enter / Alt+Enter:** insert a newline via the kitty keyboard protocol
  (`DISAMBIGUATE_ESCAPE_CODES`), so terminals that support it (ZED, iTerm2,
  kitty, WezTerm) report the modifier and the newline is not mistaken for send.
- **Message dedenting:** submitted messages are automatically dedented —
  common leading whitespace is stripped (via `textwrap::dedent`) and leading
  and trailing blank-only lines are removed. This makes pasting indented code
  snippets or quoted text send cleanly.
- **Multiline paste:** the TUI supports pasting multiline text via the
  terminal's bracketed-paste protocol (enabled automatically by crossterm).
  Pasted text is dedented before insertion.
- **Tool bubble collapse/expand:** clicking a tool bubble's title line or its
  left-border bar toggles whether the bubble's arguments and result are shown
  (expanded, `▼`) or hidden (collapsed, `▶`). Collapsed bubbles display only
  the title line, useful for decluttering long conversations.
- **Tool duration:** when a tool finishes, its execution time is shown in the
  bubble title.
- **Toast notifications:** transient messages appear in the bottom-right corner
  (e.g., `"Copied to clipboard"`) and auto-dismiss after ~1.5 seconds.
- **Scrollbar:** the chat scrollbar's thumb reaches the very bottom of the
  track at the end of the list (and the top at the start).

## Phase 5: Named engines and profiles

### Named engine configuration

Backends are now described by named engine entries, each specifying the provider,
model, sampling parameters, and connection details. Multiple engines can be defined
and referenced by name — for example a fast local Ollama engine for everyday chat and
a more capable OpenRouter model for heavier tasks.

See [configuration](./configuration.md) for all available fields.

### Runtime profiles

A profile groups a default engine with optional per-agent overrides. This is the
groundwork for Phase 6 child agents, which will let different agent types run against
different engines automatically.

**WIP:** Profile and engine definitions are constructed in code today; TOML file loading
is introduced in Phase 7.

### `/models` command (coming soon)

The groundwork for a `/models` slash command is in place. When implemented it will
list the models available from the active provider directly inside the conversation.
