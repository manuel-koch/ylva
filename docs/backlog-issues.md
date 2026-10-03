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

- [ ] We should warn user in CLI and TUI when the current compaction-engine has less supported
      context-length then the current engine effective size of `soft_compaction_threshold`.
      I.e. the compaction-engine would already be overloaded by the context-size it should
      compact!
