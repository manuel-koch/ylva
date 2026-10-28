# TUI enhancements

## Configurable keybindings

Profiles already accept a `keybindings` table, but the TUI uses the fixed map in
[keybindings](../keybindings.md). Apply configured bindings rather than only
validating and storing them.

Before implementation, decide the action names and key syntax, how overrides
combine with built-in defaults, and how conflicting or unsupported bindings
are reported. Keep help, footer hints, and actual routing consistent. Preserve
usable interrupt, quit, approval, and clarify controls in every input mode.
