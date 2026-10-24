# TUI enhancements

## Configurable keybindings

Profiles already accept a `keybindings` table, but the TUI uses the fixed map in
[keybindings](../keybindings.md). Apply configured bindings rather than only
validating and storing them.

Before implementation, decide the action names and key syntax, how overrides
combine with built-in defaults, and how conflicting or unsupported bindings
are reported. Keep help, footer hints, and actual routing consistent. Preserve
usable interrupt, quit, approval, and clarify controls in every input mode.

## Configurable auto-expand per tool

We could allow configuration for auto expand of bubbles per tool.
Thus for the selected tools, every bubble will initially be expanded
when it arives at TUI.

```toml
[tui]
auto_expand_tool = ["read","write","edit"]
```

We could allow configuration for auto expand of the current bubble
that is under user-approval.
Thus when approval starts, the bubble gets auto-expanded and when approval
is thru, bubble gets collapsed again.

```toml
[tui]
auto_expand_tool = ["clarify"]
# or
auto_expand_approve_tool = true
```
