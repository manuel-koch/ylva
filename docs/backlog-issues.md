# Backlog of issues discovered while testing

- [x] Triggering a tool call in LLM that requires user approval.
      Before the approval is requested, start typing a slash-command `/mod`
      in the message-draft, but don't complete it via tab/enter.
      When the tool approval request arrives, the rendering is a
      mixture of the slash-command-list and the tool-approval.
      We should hide any current open popup before rendering the
      tool-approval.
      When tool-approval is done, we should continue with the current
      draft-message ( which could be a slash-command )
      Fixed: arming the approval hides the slash overlay, the
      per-action recompute keeps it hidden while the approval is
      pending, and resolving the approval restores the draft and
      reopens the overlay.

- [ ] Shouldn't this be removed ?
      src/tui_driver.rs(1790):
      ```rust
      /// Compatibility shim for `main.rs` until Phase 5 Step 9 migrates the binary
      /// to construct `TuiApp` directly.
      ///
      /// TODO Phase 5 Step 9: remove this function.
      pub async fn run_tui_on_stdout(
      ```

- [x] Does the current implementation distinguish between parent & child conversion
      when rendering the context usage in the footer ?
      My impression is that it always only displays the parent conversation context
      size and never the child conversation context size, even when I just drilled-in a
      subagent run.

- [ ] We should warn user in CLI and TUI when the current compaction-engine has less supported
      context-length then the current engine effective size of `soft_compaction_threshold`.
      I.e. the compaction-engine would already be overloaded by the context-size it should
      compact!
