# Timing

Track how long conversation events take:

- request
- thinking
- tool call
- idle

This could be visualized as a header/footer row in the TUI
or used for post-mortem analysis.

While the model is thinking, show an indicator in the TUI
that updates with the current running duration and then shows
the total duration when done.
