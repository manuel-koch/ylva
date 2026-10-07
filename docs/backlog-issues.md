# Backlog of issues discovered while testing

- [ ] Shouldn't this be removed ?
      src/tui_driver.rs(1790):
      ```rust
      /// Compatibility shim for `main.rs` until Phase 5 Step 9 migrates the binary
      /// to construct `TuiApp` directly.
      ///
      /// TODO Phase 5 Step 9: remove this function.
      pub async fn run_tui_on_stdout(
      ```

- [ ] Warn in CLI and TUI when the `[compaction].engine` context window is too
      small for the summary request it receives. Estimate the summarized prefix
      against the selected summarizer's effective context length, accounting for
      its prompt and request overhead. Compare each root or agent engine's
      `context_length × soft_compaction_threshold` with the summarizer's
      effective context length; using a different conversation engine does not
      change the compaction engine's capacity. Agent types can now override
      their compaction engine independently.

- [ ] Recall a (multiline) draft message in TUI from conversation history.
      Cursor is a the beginning of the message in input-field.
      Use mouse to scroll to the end of the message.
      Drag over last line of message.
      Actually scroll jumps to the beginning of the message and selects text there.
      Similar happens when just clicking in the lower area of inuput field after
      having scrolled down.
      The scroll is reverted to something more above and the cursor is actually
      not placed at the text position I clicked.
      Strange behaviour happens even when the input field doesn't use scroll bar.
      Clicking or selecting text in last line of input-field doesn't work as expected.
      It alwas seems to affect the first line instead.

- [x] LLM keeps "thinking" after spawning many subagents. Thinks about whether the
      subagent results will come in and when, just keeps thinking about the issue !?
      See tests/e2e/custom/logs/tui_20261006163112.log
      ```text
      ....
      I’ve completed everything else, and now it looks like I just need to take care of the child results, write
      what’s necessary, and run a lint check. That seems straightforward! I want to ensure all aspects are
      polished and tidy before considering anything done. So, the final steps will be to check these little
      details to make sure everything is functioning properly. I’m on the right path! Considering task completion

      I'm thinking that the child tasks might be getting blocked because only one agent is currently running,
      while the others are queued up. It seems like the first task needs to be completed before the second can
      start, which is a serial process. Since there have already been 12 completions over time, it looks like the
      remaining 7 should be coming up soon. I’ll keep an eye on how things progress!▊

      I’m considering whether there might be a bug in the harness that's causing events not to release properly.
      It’s a bit frustrating, but all I can really do is wait and see how it unfolds. The developer has advised
      against status polling, which feels like a good thing since it can be counterproductive. I’ll just have to
      be patient and rely on the updates.▊
      ```

- [x] LLM spawned subagent and instead of using its result, root agent
      just stated:
      "The subagent completed the analysis without editing files.
      Its findings are in the Markdown table above."
      We already have a guideline for the agent to not assume that
      subagent result is visible to the user.
      See tests/e2e/custom/logs/tui_20261006202130.log

- [ ] Test run now take 2-3x times longer. Can we investigate why
      and whether that could be improved again ?

- [x] LLM spawned subagent and is constantly thinking and waiting for the
      result musing/thinking what it should do in the meantime.
      See tests/e2e/custom/logs/tui_20261006203945.log
      ```text
      I’m wondering if the automatic results are delivered only after I produce the final output.
      But from the tool API description, it states that "the harness delivers each subagent's
      completion automatically once it is ready." So, I guess I don’t need to worry about it.
      That’s good to know! It helps clarify how the process works and what I can expect.
      ```
      ```text
      I think we're waiting too long. It seems possible that each task has a 300-second command
      timeout due to agents searching through many files. But, you know, eventually, we're going to
      get through it. I just want to make sure everything runs smoothly and efficiently. I hope
      things speed up soon! It’s a bit frustrating, but I'm staying optimistic that we'll get the
      results we're looking for.▊
      ```

- [x] Looks like TUI hints from compaction like "── compaction · 15 messages
      summarized ──" end up in the root agent conversation although they
      stem from a subagent conversation.
      See tests/e2e/custom/logs/tui_20261006203945.log

- [ ] Tool spawn_subagent doesn't show the current running duration in
      parent/child conversation while the subagent is running.
