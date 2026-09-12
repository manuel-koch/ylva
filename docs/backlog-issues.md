# Backlog of issues discovered while testing

- [x] Viewing a subagent conversation that is done.
      Footer shows "Idle", but the parent agent is still busy ( running
      yet another subagent ).

- [x] Assistent message bubble renders a markdown table.
      Renders one char beyond the right edge of the bubble, right up to the
      conversation-list vertical scrollbar. Other bubbles keep 1 char right margin
      to the vertical scrollbar.

- [x] Assistant Markdown headings containing wide Unicode glyphs render one
      character beyond the right edge of the bubble.

- [x] LLM keep prefixing `bash` tool scripts with `cd <workspace-root>` although
      CWD==workspace-root is the effective default.
      The `bash` tool description and `command` schema now state that commands
      start in the workspace root, should use relative paths directly, and
      should only use `cd` when intentionally changing directories.

- [x] Resumed session: `clarify` bubble in a live session has content-sections for
      the result. Resumed session clarify bubble doesn't have those sections, nor
      does it show the answer the user gave:
      ```text
      ▼ Clarify → 3.163s
      question=The poem uses a "little lamp" as its main subject. I would like to
      replace this lamp with a wild animal. Which of the following animals would
      you like to use to replace the lamp? (You can
      also suggest a different animal)

      1. Elephant
      2. Lion
      3. Dolphin
      4. Eagle
      5. Rabbit
      ```

- [x] Resumed session: `write` bubble in a live session has content-sections for
      written data. Resumed session write bubble doesn't have those sections but
      shows the written content multiple times:
      ```text
      ▼ Write → bytes_written=145 0.003s
      A mighty lion, with golden mane,↵Roars softly through the night's deep game.↵He paints the shadows,
      near and far,↵A king of jungle, shining star. path=logs/poem.md

      A mighty lion, with golden mane, Roars softly through the night's deep game. He paints the shadows,
      near and far, A king of jungle, shining star.

      content:
      A mighty lion, with golden mane,
      Roars softly through the night's deep game.
      He paints the shadows, near and far,
      A king of jungle, shining star.
      ```

- [x] Resumed session: `write` bubble with error rendered odd:
      No content section, just partly markdown styled title line
      and the error as red line.
      ```text
      ▼ Write

      ## Conversation Summary↵↵**Topic 1: Atmospheric & Oceanic Color Science**↵*   **Sky Blue:**

      Attributed to **Rayleigh scattering**, where atmospheric gas molecules scatter short-wavelength
      blue and violet light more effectively than longer wavelengths.↵*   **Green Oceans:** Primarily due
      to the presence of **phytoplankton** (microscopic marine plants), whose chlorophyll reflects the
      green part of the light spectrum.↵*   **Cloud Colors (White vs. Dark):** The color is determined
      by the cloud's density and the path length of sunlight.↵    *   **White:** Caused by uniform
      scattering of light by small water droplets, diffusing all colors.↵    *   **Dark/Gray/Black:**
      Occurs in dense, mature clouds where light is heavily absorbed by the vast amount of water content
      over a long path.↵↵**Topic 2: Project File Scan**↵*   The request was to count all files in the
      project, overriding both hidden and Git-ignored file filters.↵*   The `glob` tool was executed with
      `dot_ignore:false` and `git_ignore:false`.↵*   **Result:** 566 net file matches were found.↵↵***
      path=../logs/summary.md
      Access denied by security policy: path is outside the workspace root; only paths within the
      workspace are accessible
      ```

- [ ] Drill-in a subagent conversation in TUI should be refactored, one might
      misread the conversation and assume it is from the main conversation,
      scrolled so that the subagent bubble appears at top of view.
      Maybe re-using the subagent bubble is not a good idea ?
      What else could we change to render it differently, to highlight
      that this is the subagent's conversation ?

- [x] Define the asynchronous `spawn_subagent` admission/completion contract.
      The immediate tool result is explicitly an admission event, not a child
      outcome. It includes `run_id`, `tool_call_id`, `label`, `status`,
      `outstanding_count`, and `all_done`. Each later terminal outcome is a
      structured hidden harness event with the same correlation fields plus
      `status`, `result`/`error`, `sources`, `outstanding_count`, and
      `all_done`. Intermediate completions are recorded without forcing the
      model to emit an impossible empty turn; the parent receives one
      continuation after the final outstanding result so it can aggregate.
      The current one-running-child/FIFO scheduler remains an implementation
      constraint and is not encoded as a permanent ordering guarantee.
      Regression coverage verifies correlation, intermediate-completion
      suppression, and final aggregation behavior.

- [ ] Refactor rendering of keybinding hints in footer.
      Render the keybinding in bold ("enter", "esc", "ctrl-c", ...),
      and the hint in normal font ("send", "cancel").
      Use separator glyph " · " between multiple keybinding hints.
      E.g. like "enter send · esc cancel · ctrl-c cancel & quit · F1 help"

- [ ] Main conversation footer should indicate number of running
      or queued subagents.
      But not the total number of (finished) subagents !

- [ ] Don't render the "run_id" within the title of a subagent bubble,
      neither in main conversation, nor in subagent conversation.
      This seems to be noise for user and only relevant for the
      agent harness or the LLM we are tatlking to.

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
