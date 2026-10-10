# Backlog of issues discovered while testing

- [ ] Diagrams are likely out-of-date:
  - [ ] docs/class-diagram.mmd
  - [ ] docs/tui-reducer-state-diagram.mmd

- [x] Why do we have a `tests` folder and many tests in `src` folders ?
      What is the best-practise Rust testing strategy wrt module layout ?

- [x] Resuming a session in TUI.
      The initial state of TUI doesn't show the current context-usage.

- [ ] Noticed that when many subagents have been spawned and the last subagent
      finishes, TUI shows idle status and it seems to take several seconds
      before TUI eventually shows streaming/thinking status afterwards.
      We send the finished subagent trigger to LLM, why does agent go to idle
      in the first place ( or is this just a TUI glitch ) ?

- [ ] Narrow terminal width: The /models popup renders the available provider/models
      but depending on the terminal width only the start of each row is actually visible.
      Can we use the elide_balanced_tokens() method and use maybe two rows
      per provider/model on narrow terminals ( same width as the footer uses
      to toggle layout ) ?
