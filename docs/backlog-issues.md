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
