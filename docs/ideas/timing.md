# Timing

Track how long conversation events take:

- request
- thinking
- tool call
- idle

This could be visualized as a header/footer row in the TUI
or used for post-mortem analysis.

## Implementation status

Partly implemented; see [features](../features.md#tui-architecture) for the
current behavior:

- Thinking bubbles show a live elapsed timer while streaming, then the total
  duration when done.
- Finished tool bubbles show their execution time in the title.
- Running `spawn_subagent` bubbles show a live elapsed timer.

Still proposed: request and idle timing, a header/footer timing row, and
post-mortem analysis.
