# Ylva agent-harness keybindings

Reference for Ylva's TUI keybindings.

## TUI keybindings (Phase 4)

| Key | Context | Action |
|---|---|---|
| `enter` | Input focused, normal mode | Send message; queue steer if busy |
| `shift-enter` / `alt-enter` | Input focused | Insert newline |
| `home` / `fn-left` | Input focused | Move to start of line; if already at column 0, move to end of previous line |
| `end` / `fn-right` | Input focused | Move to end of line; if already at line end, move to start of next line |
| `ctrl-c` or `esc` (×1) | Streaming / ToolPending | Show `"Press again to cancel"` hint |
| `ctrl-c` or `esc` (×2 ≤ 2 s) | Streaming / ToolPending | Interrupt conversation |
| `ctrl-c` (×1) | Idle | Show `"Press again to quit"` hint |
| `ctrl-c` (×2 ≤ 2 s) | Idle | Quit TUI |
| `esc` | Idle | Clear input field (no quit) |
| `esc` | Help popup open | Close popup |
| `enter` | Approval sub-mode | Approve tool |
| `esc` | Approval sub-mode | Deny tool |
| `ctrl-c` (×1) | Approval sub-mode | Show cancel hint |
| `ctrl-c` (×2 ≤ 2 s) | Approval sub-mode | Interrupt conversation |
| `F1` | Any | Toggle Help popup |
| `↑` (cursor on first line) | Input focused | Load previous user message; save draft |
| `↓` (cursor on last line) | Input focused | Load next user message; restore draft at newest |
| Any edit key while browsing | Input focused | Copy browsed entry to draft; exit browsing mode |
| `page-up` / `fn-up` | Any | Scroll up; disable auto-scroll |
| `page-down` / `fn-down` | Any | Scroll down; re-enable auto-scroll at bottom |
| Mouse wheel | Any | Scroll chat (no focus needed) |
| Left-click | Input focused | Move the cursor to the clicked character (word-wrapping honored) |
| Left-drag | Input focused | Select text; click (no drag) just places the cursor |

`shift-enter` / `alt-enter` rely on the kitty keyboard protocol: the TUI
enables `DISAMBIGUATE_ESCAPE_CODES` on startup (pushed with the alternate
screen and popped on exit) so terminals that support it (ZED, iTerm2, kitty,
WezTerm, …) send distinguishable CSI-u sequences. Unsupported terminals simply
ignore the flag; there `shift-enter`/`alt-enter` may fall back to a plain
`enter` and send the message.

## Build info

The Help popup (`F1`) renders the `BuildInfo` values: version, git commit,
build timestamp, and build profile.
