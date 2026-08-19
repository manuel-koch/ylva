# Ylva agent-harness keybindings

Reference for Ylva's TUI keybindings.

## TUI keybindings (Phase 4)

| Key | Context | Action |
|---|---|---|
| `Enter` | Input focused, normal mode | Send message; queue steer if busy |
| `Shift+Enter` / `Alt+Enter` | Input focused | Insert newline |
| `Home` / `Fn+Left` | Input focused | Move to start of line; if already at column 0, move to end of previous line |
| `End` / `Fn+Right` | Input focused | Move to end of line; if already at line end, move to start of next line |
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
| `PageUp` / `fn+Up` | Any | Scroll up; disable auto-scroll |
| `PageDown` / `fn+Down` | Any | Scroll down; re-enable auto-scroll at bottom |
| Mouse wheel | Any | Scroll chat (no focus needed) |
| Left-click | Input focused | Move the cursor to the clicked character (word-wrapping honored) |
| Left-drag | Input focused | Select text; click (no drag) just places the cursor |

`Shift+Enter` / `Alt+Enter` rely on the kitty keyboard protocol: the TUI
enables `DISAMBIGUATE_ESCAPE_CODES` on startup (pushed with the alternate
screen and popped on exit) so terminals that support it (ZED, iTerm2, kitty,
WezTerm, …) send distinguishable CSI-u sequences. Unsupported terminals simply
ignore the flag; there `Shift+Enter`/`Alt+Enter` may fall back to a plain
`Enter` and send the message.

## Build info

The Help popup (`F1`) renders the `BuildInfo` values: version, git commit,
build timestamp, and build profile.
