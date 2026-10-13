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

- [x] Narrow terminal width: The /models popup renders the available provider/models
      but depending on the terminal width only the start of each row is actually visible.
      Can we use the elide_balanced_tokens() method and use maybe two rows
      per provider/model on narrow terminals ( same width as the footer uses
      to toggle layout ) ?

- [x] A configured engine that uses context-length less then the provider
      advertises for the model.
      Open the /models picker popup, browse to the named engines provider/model
      select it with enter.
      The context-length that can be toggled with left/right keys is
      capped at the configured value, not at the provider advertised max
      context-size.
      The /models picker popup should allow for selecting the whole range
      of context-length as advertised by the provider.

- [x] Why is the advertised default reasoning-effort a separate entry in
      the list of available efforts ?
      E.g. for one LLM the order I can switch thru is no ( left to right ):
      - default (medium)
      - none
      - low
      - medium
      - high
      - xhigh
      - max
      Why does "default (medium)" appear before "none"?
      Why doesn't "medium (default)" be at the "medium" entry ?
      Resolved: an advertised provider default is marked on its own effort's
      entry (`medium (default)`) instead of a separate leading slot.

- [x] When no compaction engine is selected ( i.e. it falls back to the default
      engine ), then editing the context-length of a selected model should
      update the picker-footer too ( because the compaction model will use the
      very same model/parameters ), shouldn't it ?
