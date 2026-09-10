# Backlog of issues discovered while testing

- [ ] Viewing a subagent conversation that is done.
      Footer shows "Idle", but the parent agent is still busy ( running
      yet another subagent ).

- [ ] Assistent message bubble renders a markdown table.
      Renders one char beyond the right edge of the bubble, right up to the
      conversation-list vertical scrollbar. Other bubbles keep 1 char right margin
      to the vertical scrollbar.

- [ ] LLM keep prefixing `bash` tool scripts with `cd <workspace-root>` although
      CWD==workspace-root is the effective default.
      Do we need to refine the `bash` schema description to help LLM ?
