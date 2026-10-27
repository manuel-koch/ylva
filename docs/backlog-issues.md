# Backlog of issues discovered while testing

- [ ] Pasting really long text into message draft input field
      blocks ylva, no further input possible, it is just stuck
      and even double-ctrl-c is not possible to abort the TUI.
      CPU usage is 100% and after killing the executable the
      terminal renders garbage on mouse events.

- [ ] Should we rather print a marker in the message draft input field that
      is a placeholder for really long pasted content ?
      But that would likely just work for pasting really big content.
      Once user is using up/down keys to retrieve user messages from session
      history, it can still hapen that really big content is added to input field.
