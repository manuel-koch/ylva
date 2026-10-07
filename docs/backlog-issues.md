# Backlog of issues discovered while testing

- [x] Shouldn't this be removed ?
      src/tui_driver.rs(1790):
      ```rust
      /// Compatibility shim for `main.rs` until Phase 5 Step 9 migrates the binary
      /// to construct `TuiApp` directly.
      ///
      /// TODO Phase 5 Step 9: remove this function.
      pub async fn run_tui_on_stdout(
      ```

- [x] Recall a (multiline) draft message in TUI from conversation history.
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

- [x] In help popup the order for "Environment" entries should be adjusted to
      ```text
      Ylva home:      <ylva_home>
      Profile:        <profile>
      Profile path:   <profile_path>
      Workspace root: <workspace_root>
      ```
