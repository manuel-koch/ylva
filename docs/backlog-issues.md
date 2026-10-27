# Backlog of issues discovered while testing

- [ ] Restore the terminal (mouse capture, bracketed paste) on panic and on
      SIGTERM/SIGHUP; today only a clean exit restores it, so an abnormal exit
      leaves the terminal emitting garbage on mouse events.

- [ ] Add a hard upper bound (e.g. 50 MB) on pasted content, rejecting larger
      pastes with a toast, to protect against memory blow-ups.
