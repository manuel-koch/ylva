# Backlog of issues discovered while testing

- [x] "init" creates an empty ".env" file in the "default" profile
      with a comment describing its use case.

- [x] "init --workspace" creates a ".gitignore" file in the workspace
      ".ylva" directory and excludes any ".env" files from git.

- [x] Unable to select text in subagent conversation header bubble.
      Trying to drag on the header just toggles expand/collapse but
      doesn't select nor copy selected text.

- [x] We don't load past sessions to provide functionality to walk thru
      recent user messages to fill draft message input.

- [x] What happens when a user uses slash-command "/clear" while a current
      conversation is in-flight ?
      Should a "/clear" automatically force an abort of the current
      running conversation ( incl. subagents and tools ).
      Root cause: the TUI clears its visible `AppState` immediately and sends
      `AgentCommand::ClearContext`, but the agent-loop handler only replaces
      the `Context` and session store. It does not terminate the active
      provider stream, running tool, pending tool/steer work, or child-agent
      runs. The existing cancellation path used by `Stop` is therefore not
      involved. Asynchronous stream, tool, and child-agent messages remain
      queued or can arrive after the reset and are still processed by the new
      session. The reproduction log shows `clear_context` received while the
      loop is `Streaming` with an active stream; the old provider continues
      emitting for roughly two seconds, and a child/provider error is rendered
      after the visible conversation has been cleared.
      Tried a "/clear" on running conversation I saw this output in TUI
      ( which seems to include output that is likely unwanted ):
      See log tests/e2e/custom/logs/tui_20260916124312.log
      ```text
      ⓘ Conversation cleared. Following turns are recorded in a new session.

      /agent/child_agent_manager.rs ` failed. The subagent encountered a provider error (HTTP
      404), indicating that the underlying language model or service was not found.

      Therefore, I am unable to provide the summary in the requested markdown table at this time.
      ```
      Fixed: `ClearContext` now interrupts the current stream/tool token and
      calls `child_agent_manager.cancel_all()` before swapping in the fresh
      `Context`, mirroring `AgentCommand::Stop`. Since cancellation is
      cooperative, a per-loop `generation` counter (bumped on every
      `ClearContext`, stamped onto every `AgentEvent` at spawn time) and a
      `cancelled_run_ids` set (checked by the `finish_rx` arm) drop any
      stream/tool/child event produced before the clear that still arrives
      afterwards, instead of letting it land in the new session. See
      `docs/implementation-notes.md`'s "Slash commands" section.
