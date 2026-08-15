# Timing

We should be able to track how long conversation events take:

- request
- thinking
- tool call
- idle

We could visualize this as a header/footer row in TUI
or use it for post-mortem analysis.

While model is thinking, we should have an indicator in the TUI
that model is thinking, updating the indicator with the current
running duration and finally with overall duration of thinking.
