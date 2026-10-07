# TUI enhancements

## Configurable keybindings

Profiles already accept a `keybindings` table, but the TUI uses the fixed map in
[keybindings](../keybindings.md). Apply configured bindings rather than only
validating and storing them.

Before implementation, decide the action names and key syntax, how overrides
combine with built-in defaults, and how conflicting or unsupported bindings
are reported. Keep help, footer hints, and actual routing consistent. Preserve
usable interrupt, quit, approval, and clarify controls in every input mode.

## Edit a tool invocation before approval

Undecided proposal: let the user edit a pending tool invocation before
approving it. The current interaction approves the original call or denies it,
optionally with a typed reason; it does not edit the proposed arguments.

Decide which tools and arguments are editable and how the editor is presented.
Any changed invocation must be parsed, validated, and checked against sandbox
rules again before execution. Refresh its title and diff preview, preserve its
tool-call ID and single matching outcome, and record the executed arguments
without losing the original proposal. Cancelling the editor must not execute
the tool or silently approve the original invocation.

## Configurable auto-expand per tool

We could allow configuration for auto exapnd of tool bubbles.
Thus for the selected tools, every bubble will initially be expanded
when it arives at TUI.

```toml
[tui]
auto_expand_tool = ["read","write","edit"]
```
