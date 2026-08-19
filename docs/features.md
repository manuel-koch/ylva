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
terminal (piped or redirected), Ylva falls back to the headless runner.

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

- **Layout:** a scrollable chat-history area, a variable-height input box
  (capped at 1/3 of the terminal height), and a 2-row footer (mode/hint row and
  key-hints row). No sidebar in Phase 4.
- **Input always active:** the input box is never disabled. `Enter` sends
  unconditionally — while the model is streaming the message is queued as a
  pending steer, and `Ctrl+C`/`Esc` interrupt the current round.
- **Steer bubble lifecycle:** a user message appears immediately on `Enter`. It
  is rendered `Queued` (yellow) while pending, then becomes `Sent` (blue) when
  `SteerApplied` arrives; `SteerQueued`/`SteerApplied` are the only new
  `UiUpdate` variants added by this phase.
- **Cancelled steers:** interrupting marks all pending bubbles `Cancelled`
  (grey) with a dimmed "Not sent." line. The text is retained in input history
  so it can be recovered with `↑`.
- **Tool approval sub-mode:** when a tool awaits approval the footer switches
  to `[ ✓ Approve (Enter) ]  [ ✗ Deny (Esc) ]` (mouse-clickable). `Enter`
  sends `ApproveTool`, `Esc` sends `DenyTool` immediately; `Ctrl+C` still uses
  the double-press confirm.
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
- **Wordmark:** the chat history always opens with a horizontally centered
  ASCII-art "Ylva" wordmark, so the initial frame is never empty while the
  first response streams in.
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
- **Scrollbar:** the chat scrollbar's thumb reaches the very bottom of the
  track at the end of the list (and the top at the start).
