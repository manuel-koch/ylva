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

- [x] In footer show the active named engine and selected model-id, using
      separator " → " instead of displaying the provider connection.

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
      code, covering all specified points (parameters, validation, error
      handling, encoding/binary, limits, limits, integration/tests).

      The analysis is complete and the information is sufficiently dense to form the final output. I will
      use agent_finish .
      ```

- [x] Switched LLM model in main agent conversation after startup.
      This should change the "default" engine (provider/model) too, if main
      agent is configured to use the "default" engine.
      Currently a later spawned subagent is still using the original
      provider/model that was active after startup ( before the manual switch ).
      Any agent spawned after a model switch should use its engine (provider/model)
      active at that moment ( if it is referring to a named engine like
      "default" ).
      Resolved: children using the same named engine inherit the root's active
      provider/model when they are spawned; explicit child engine overrides
      retain their own named engine selection.

- [ ] A bash tool call seems to be stuck, I had to abort it with ctrl-c.
      ```text
      ▶ Bash grep -r "def read" .
      Tool 'bash' was denied by the user. Here is their reason for the denial:

      aborted by user
      ```
      But afterwards the TUI was stuck in "Aborting..." and any further ctrl-c
      was not causing it to quit and I had to forcefully kill the running process.
      See tests/e2e/custom/logs/tui_20260912171549.log

- [ ] Some LLM still seem to trigger bash tool calls with "wait" statements after
      spawning multiple subagents to wait for their results.
      Shortly afterwards LLM replied (thought):
      ```text
      The harness sent an intermediate subagent completion (write.rs analysis) not visible to me as
      content, just a notice. I should not acknowledge it or produce progress updates. I'll wait for the
      final continuation when all outstanding children complete. I shouldn't poll or sleep. I'll just
      stop here and wait for the harness continuation. Actually, I already sent a bash sleep which was
      unnecessary — but it's done. Now I should simply not do anything until the harness provides the
      terminal continuation.
      ```
