# Backlog of issues discovered while testing

- [x] Drill-in a subagent conversation in TUI should be refactored, one might
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

- [x] Automatic column width sizing while rendering markdown tables seems
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

- [x] Some LLM seem to muse past the initial task/briefing when generating the
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
      Resolved: `agent_finish`'s `result` field and tool description now
      explicitly forbid meta-commentary/self-review narration (e.g.
      "Self-Correction/Review:") in the value — only the finished deliverable
      belongs there.

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

- [x] A bash tool call seems to be stuck, I had to abort it with ctrl-c.
      ```text
      ▶ Bash grep -r "def read" .
      Tool 'bash' was denied by the user. Here is their reason for the denial:

      aborted by user
      ```
      But afterwards the TUI was stuck in "Aborting..." and any further ctrl-c
      was not causing it to quit and I had to forcefully kill the running process.
      See tests/e2e/custom/logs/tui_20260912171549.log
      Resolved: stop completion now emits an explicit idle update even when a
      pending tool only produces a terminal error. The TUI keeps its Aborting
      state until that update, so Ctrl-C exits and Esc returns to an
      interactive idle TUI.

- [x] Some LLM still seem to trigger bash tool calls with "wait" statements after
      spawning multiple subagents to wait for their results.
      Shortly afterwards LLM replied (thought):
      ```text
      The harness sent an intermediate subagent completion (write.rs analysis) not visible to me as
      content, just a notice. I should not acknowledge it or produce progress
      updates. I'll wait for the
      final continuation when all outstanding children complete. I shouldn't poll or sleep. I'll just
      stop here and wait for the harness continuation. Actually, I already sent a bash sleep which was
      unnecessary — but it's done. Now I should simply not do anything until the harness provides the
      terminal continuation.
      ```
      Resolved: `spawn_subagent`'s description already forbade inventing a wait
      call, but the LLM was slipping around it via `bash`. The `bash` tool
      description now explicitly forbids using it to sleep, poll, or wait for a
      spawned subagent, reinforcing the existing `spawn_subagent` guidance.

- [x] LLM triggered strange search query assuming a simple OR/AND syntax:
      ```text
      ▼ Thought for 6.893s
      Okay, I need to analyze the source code to understand and list the implemented TUI (Text-based User
      Interface) functionalities. This requires searching the codebase for relevant files and patterns related
      to TUI, UI elements, or display logic.

      I will start by using the search tool to look for files that likely contain TUI implementation details.
      I'll search for terms like "tui", "ui", "interface", or specific TUI library names, if any seem common,
      but for a general analysis, just these terms should suffice. Given I don't know the structure, I'll use
      a broad search pattern first.
      
      Since I want to analyze the implementation , I will start with a pattern search that is likely to hit
      the source logic.
      
      My plan:
      
      1. Use search to find files containing patterns related to TUI/UI.
      2. Review the results to identify critical files.
      3. Read the relevant files and analyze the code to list features.
      4. Compile the features into a descriptive table.
      
      Let's start with step 1. I'll look for files containing "tui" or "ui".
      ```
      The result tool calls where:
      - ▶ Search TUI OR UI → matching_files=0 matching_lines=0
      - ▶ Search cli OR interface OR display OR render → matching_files=0 matching_lines=0
      Afterwards LLM mused:
      ```text
      ▼ Thought for 4.076s
      The initial search for "TUI OR UI" and "cli OR interface OR display OR render" both resulted in zero
      matches (matching_files: 0).
      
      This strongly suggests that either:
      
      1. The codebase I am searching does not contain the functionality I am looking for (TUI/CLI).
      2. The keywords I used are not accurate for the codebase's abstraction layer.
      
      I should try a broader search to see if there are any related files or functionality, such as common UI
      libraries or entry points. I will use ls -R via bash to explore the file structure and then search for
      more specific clues.
      ```
