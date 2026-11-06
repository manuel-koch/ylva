# TUI enhancements

## Configurable keybindings

The TUI currently uses the fixed map in [keybindings](../keybindings.md).
A future implementation could let profiles override selected bindings.

Before implementation, decide the action names and key syntax, how overrides
combine with built-in defaults, and how conflicting or unsupported bindings
are reported. Keep help, footer hints, and actual routing consistent. Preserve
usable interrupt, quit, approval, and clarify controls in every input mode.
