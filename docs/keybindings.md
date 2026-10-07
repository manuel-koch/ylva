# Ylva agent-harness keybindings

Reference for Ylva's TUI keybindings.

Footer hints render each keybinding in bold and its action in normal text.
Multiple hints are separated by ` · `, for example:
`enter send · esc cancel · ctrl-c cancel & quit · F1 help`.

The context-usage readout stays right-aligned on the second footer row,
including beside tool-approval buttons. On narrow terminals it is omitted
rather than overwriting the controls.

While a bounded continuation waits for an existing context summary, normal
interrupt controls and `/clear` remain responsive. `Compacting…` is scoped to
the displayed root/child conversation; successful summaries keep it active until
safe application, while failure, timeout, stop, and clear remove it.

While the main conversation is shown, the footer also reports separate
running and queued subagent counts in a compact label such as
`Subagents: 1 running, 1 queued`; finished subagents are not included.

When root and subagent tool approvals overlap, approving or denying one
restores the controls for another outstanding approval in its owning
conversation. The original draft returns after all approvals are resolved.

## TUI keybindings (Phase 4)

| Key | Context | Action |
|---|---|---|
| `enter` | Input focused, normal mode | Send message; queue steer if busy |
| `shift-enter` / `alt-enter` | Input focused | Insert newline |
| `home` / `fn-left` | Input focused | Move to start of line; if already at column 0, move to end of previous line |
| `end` / `fn-right` | Input focused | Move to end of line; if already at line end, move to start of next line |
| `esc` (×1) | Streaming / ToolPending, input empty | Show `"Press again to cancel"` hint |
| `esc` (×2, same key, ≤ 2 s) | Streaming / ToolPending, input empty | Interrupt conversation; TUI stays open |
| `ctrl-c` (×1) | Streaming / ToolPending, input empty | Show `"Press again to cancel & quit"` hint |
| `ctrl-c` (×2, same key, ≤ 2 s) | Streaming / ToolPending, input empty | Interrupt conversation, then quit TUI |
| `ctrl-c` or `esc` | Streaming / ToolPending, input non-empty | Clear input field and exit history browsing (no interrupt) |
| `ctrl-c` (×1) | Idle, input empty | Show `"Press again to quit"` hint |
| `ctrl-c` (×2, same key, ≤ 2 s) | Idle, input empty | Quit TUI |
| `ctrl-c` (×1) | Idle, input non-empty | Show `"Press again to quit"` hint (draft is *not* cleared) |
| `ctrl-c` (×2, same key, ≤ 2 s) | Idle, input non-empty | Quit TUI, discarding the draft |
| `esc` | Idle | Clear input field (no quit); also dismisses a pending `ctrl-c` quit confirm |
| `esc` | Help popup open | Close popup |
| `ctrl-c` (×1) | Help popup open | Show the quit/cancel hint |
| `ctrl-c` (×2, same key, ≤ 2 s) | Help popup open | Quit when idle; interrupt and quit while busy |
| Left-click inside popup | Help popup open | No effect (stays open) |
| Left-click outside popup | Help popup open | Close popup |
| `enter` | Approval sub-mode | Approve tool |
| `esc` | Approval sub-mode | Deny tool |
| `ctrl-c` (×1) | Approval sub-mode | Show cancel hint |
| `ctrl-c` (×2 ≤ 2 s) | Approval sub-mode | Interrupt conversation |
| click choice | Clarify sub-mode with choices | Copy the clicked choice into the input field |
| `enter` | Clarify sub-mode | Send the current input as the clarify answer |
| `ctrl-c` | Clarify sub-mode | Abort the clarify prompt; sends a tool error to the model |
| `esc` (input empty) | Clarify sub-mode | Abort the clarify prompt; sends a tool error to the model |
| `esc` (input non-empty) | Clarify sub-mode | Clear the input field |
| `F1` | Any | Toggle Help popup |
| `↑` (cursor on first line) | Input focused | Load previous user message; save draft |
| `↓` (cursor on last line) | Input focused | Load next user message; restore draft at newest |
| Any edit key while browsing | Input focused | Copy browsed entry to draft; exit browsing mode |
| `↑` / `↓` | Help popup open | Scroll help popup up / down one line |
| `←` / `→` | Help popup open | Scroll help popup left / right by 4 columns |
| `page-up` / `fn-up` | Any | Scroll chat up; when help popup is open, scroll popup up |
| `page-down` / `fn-down` | Any | Scroll chat down; when help popup is open, scroll popup down |
| Mouse wheel | Any | Scroll chat; when help popup is open, scroll popup (no focus needed) |
| Left-click | Input focused | Move the cursor to the clicked character (word-wrapping honored) |
| Left-drag | Input focused | Select text; click (no drag) just places the cursor |
| Left-click | Child-view header bubble | Toggle the header bubble expand/collapse state |
| Left-drag | Chat or child-view header bubble | Select visible text; release copies a non-empty selection to the clipboard |
| Left-click on tool bubble title / border | Any | Toggle tool bubble expand (`▼`) / collapse (`▶`) |
| Double-click on a `spawn_subagent` tool bubble's title | Any | Drill in to the subagent's conversation view (manual navigation; does not auto-return; still works after the subagent has finished) |
| `esc` (input empty) | Subagent drill-in view | Go back to the root conversation (does not cancel/interrupt the root agent's turn); restores the scroll position from before drilling in |
| `esc` (input non-empty) | Subagent drill-in view | Clear the input field (same as elsewhere; does not navigate back) |
| `enter` | Subagent drill-in view | No-op; the subagent conversation doesn't accept input and Enter no longer steers the root agent from here (footer omits `enter send`) |

## Slash-command overlay

| Key | Context | Action |
|---|---|---|
| `/` as the first character | Input focused | Open the command overlay; it narrows as more of the command is typed |
| `↑` / `↓` | Command overlay open | Move the highlighted command |
| `tab` | Command overlay open | Complete the highlighted command |
| `enter` | Input holds a recognized command or a highlighted command suggestion | Run the selected command instead of sending the draft to the model (see [features](./features.md#slash-commands)) |
| `esc` | Command overlay open | Close the overlay and clear the input (otherwise the still-matching draft would immediately reopen it) |
| `↑` / `↓` | `/models` picker open | Move the highlighted provider-connection/model |
| `page-up` / `fn-up` | `/models` picker open | Move the highlighted provider-connection/model up by a page |
| `page-down` / `fn-down` | `/models` picker open | Move the highlighted provider-connection/model down by a page |
| `Mouse wheel` | `/models` picker open | Move the highlighted provider-connection/model up / down |
| `left-click` on an entry | `/models` picker open | Highlight the provider-connection/model; does not switch until `enter` |
| `left-drag` | Slash-command overlay open | Select visible popup text; release copies it to the clipboard |
| `←` / `→` | `/models` picker open | Cycle the highlighted model's reasoning effort when alternatives are available |
| any character / backspace | `/models` picker open | Insert into the draft (`/models <filter>`) and live-narrow the picker's already-fetched choices; no re-query |
| `enter` | `/models` picker open | Switch to the highlighted provider connection/model, apply its selected reasoning effort, and clear the input |
| `esc` | `/models` picker open | Cancel without switching and clear the input |

Only a second press of the *same* key confirms a pending double-press action.
Pressing the other key instead starts that key's own fresh confirm window
(and dismisses the one it replaced) — e.g. `esc` then `ctrl-c` never quits on
that `ctrl-c`, and `ctrl-c` then `esc` never quits on that `esc`; each key's
first press must be repeated to confirm its own action.

`shift-enter` / `alt-enter` rely on terminal-specific modified-key encodings.
The TUI enables the kitty keyboard protocol's `DISAMBIGUATE_ESCAPE_CODES` on
startup (pushed with the alternate screen and popped on exit) so terminals
that support it (iTerm2, kitty, WezTerm, …) send distinguishable CSI-u
sequences. Terminals that encode `shift-enter` as a line feed (such as Zed's
terminal) are normalized to the same newline action. Other unsupported
terminals may fall back to a plain `enter` and send the message.

## Build info

The Help popup (`F1`) renders the `BuildInfo` values: version, git commit,
build timestamp, and build profile.
