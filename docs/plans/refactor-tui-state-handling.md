# Refactor TUI state handling

As of now the state handling in TUI is implemented in some global functions
that have lots of if-then-else code.

Research whether we can refactor to code to use a FSM ( finite state machine )
implementation that abtracts the state with on-enter, on-exit, transition_to,
draw etc functionality.

- Check for crates that implement FSM
- Refactor the various methods that handle/change "state" to be abstracted
  by such FSM state instances
