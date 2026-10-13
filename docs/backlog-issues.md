# Backlog of issues discovered while testing

- [ ] Diagrams are likely out-of-date:
  - [ ] docs/class-diagram.mmd
  - [ ] docs/tui-reducer-state-diagram.mmd

- [ ] Noticed that when many subagents have been spawned and the last subagent
      finishes, TUI shows idle status and it seems to take several seconds
      before TUI eventually shows streaming/thinking status afterwards.
      We send the finished subagent trigger to LLM, why does agent go to idle
      in the first place ( or is this just a TUI glitch ) ?
