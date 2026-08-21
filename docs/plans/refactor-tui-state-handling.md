# Refactor TUI state handling

As of now the state handling in TUI is implemented in multiple global functions
that have lots of if-then-else code and share a "state" struct.

Research whether we can refactor the code to use FSM ( finite state machine )
implementation that abstracts the state and its methods with on-enter, on-exit, transition_to, draw , handle_action etc functionality.

- Check for avail crates that implement FSM and recommended a crate
- Check if TUI could use nested states to implement the help-popup,
  message-draft, approval-pending functionality.
- Draw mermaid state-diagram to visualize the states and there transisitons.
  - Likely already useful during that planning-phase of this feature.
- Refactor the various methods that handle/change "state" to be abstracted
  by such FSM state instance(s)
