# Backlog of issues discovered while testing

- [ ] Drill-in a subagent conversation in TUI should be refactored, one might
      misread the conversation and assume it is from the main conversation,
      scrolled so that the subagent bubble appears at top of view.
      Maybe re-using the subagent bubble is not a good idea ?
      What else could we change to render it differently, to highlight
      that this is the subagent's conversation ?

- [x] Refactor rendering of keybinding hints in footer.
      Render the keybinding in bold ("enter", "esc", "ctrl-c", ...),
      and the hint in normal font ("send", "cancel").
      Use separator glyph " · " between multiple keybinding hints.
      E.g. like "enter send · esc cancel · ctrl-c cancel & quit · F1 help"

- [x] Main conversation footer should indicate number of running
      or queued subagents.
      But not the total number of (finished) subagents !

- [x] In footer use separator " → " between provider-connection and selected
      model-id instead of "/".

- [x] Don't render the "run_id" within the title of a subagent bubble,
      neither in main conversation, nor in subagent conversation.
      This seems to be noise for user and only relevant for the
      agent harness internally or the LLM we are talking to.

- [ ] Automatic column width sizing while rendering markdown tables seems
      too simple, resulting in very uneven distribution of column widths
      based on the content within the columns. E.g. here the 1st column
      is very small while the 2nd column is wide.
      ```text
      Area     │ Files
      ─────────────────────────────────────────────────────────────────────────────────────────────────────
      Core too │ src/agent/tools.rs , src/agent.rs , src/agent_factory.rs , src/agent/child_agent_manager.r
      l layer  │ s
      Built-in │ bash.rs , read.rs , write.rs , edit.rs , glob.rs , search.rs , clarify.rs , spawn_subagent
      tools    │ .rs , agent_finish.rs
      Security │ src/sandbox.rs , src/llm.rs , src/config.rs , src/config/profile.rs , src/config/agent.rs
      / LLM /  │
      config   │
      ```

- [ ] Some LLM seem to muse past the initial task/briefing when generating the
      final result, causing additional internal "thinking" to be fed back to
      parent agent.
      Can we improve the subagent tool schema description to guid LLM on NOT
      doing such paraphrasing ?
      Here is an example of such additional appendum from LLM:
      ```text
      ─────────────────────────────────────────────────────────────────────────────────────────────────────
      Self-Correction/Review: The request asked for a brief markdown description list, grounded in the
      code, covering all specified points (parameters, validation, error handling, encoding/binary, limits,
      limits, integration/tests).
      
      The analysis is complete and the information is sufficiently dense to form the final output. I will
      use agent_finish .
      ```

- [ ] Switched LLM model in main agent conversation. This should change the
      "default" engine (provider/model) too, if main agent is configured to use
      the "default" engine.
      Currently a later spawned subagent is still using the original
      provider/model that was active after startup ( before the manual switch ).
      Any agent spawned after a model switch should use its engine (provider/model)
      active at that moment ( if it is referring to a named engine like
      "default" ).
