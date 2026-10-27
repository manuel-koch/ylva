# Handle huge draft messages in the TUI input field

## Context

Component: Ylva's Rust TUI input box (`src/tui_driver/*`, `src/tui_widgets.rs`, backed by the
`ratatui-textarea` 0.9.x `TextArea`).

Problem (from `docs/backlog-issues.md`):

- Pasting very long text into the message draft input makes Ylva hang at 100% CPU. No input is
  processed, not even double Ctrl-C. After killing the process, the terminal keeps mouse
  capture/bracketed paste enabled and renders garbage on mouse events.
- Design question, decided: show a placeholder marker (e.g. `[Pasted text #1: 120,000 chars]`)
  for pasted content beyond a size threshold. History recall is deliberately excluded:
  `AppState.input_history` holds full texts (filled from replayed session messages, prior
  sessions and sent prompts) and recalled entries are loaded verbatim.

Root cause (confirmed by code reading):

1. `paste_text` (`src/tui_driver/input.rs`) feeds the pasted text to the textarea one
   `KeyEvent` per character via `textarea.input(...)`. Every inserted character calls
   `push_history` → `refresh_screen_map` → `screen_map_load`, which re-wraps the **entire**
   text and rebuilds all screen-line/data-pointer vectors. Paste cost is therefore
   O(n²) in pasted length. The event loop is single-threaded, so the TUI (including Ctrl-C
   handling) is blocked for the duration; this is the "stuck at 100% CPU" symptom.
2. Even after the paste completes, every frame and event-loop iteration performs several O(n)
   operations on the whole draft: `textarea.lines().join("\n")` (event loop `textarea_ctx`,
   `frame.rs`), a full `TextArea::from(lines.to_vec())` measurement clone plus wrap in
   `tui_widgets.rs` (input height), a `textarea.clone()` viewport probe, the textarea render,
   and `refresh_slash_overlay(&textarea.lines().join("\n"))`. A multi-MB draft stays sluggish.
3. History recall (`load_history_into_textarea` → `set_input_text`) builds a `TextArea` from the
   whole entry, so huge history entries hit cause 2 as well (accepted; see step 7, measure first).
4. Terminal garbage after kill is a consequence of the process being killed without running the
   terminal restore (`src/tui_driver/terminal.rs` disables bracketed paste/mouse capture on
   clean exit only). It disappears once the hang is fixed; a hard kill still cannot restore.

Goal: pasting or recalling arbitrarily large text never freezes the TUI, Ctrl-C always works,
and the full content is still sent when the user submits.

Acceptance criteria:

- Pasting e.g. 1 MB / 20 000 lines completes in well under a second (bounded by a test) and
  the UI stays responsive, Ctrl-C and double-Ctrl-C quit as usual.
- A paste above 1,000 chars is shown as a one-line placeholder in the draft; on send the
  placeholder is replaced by the pasted content, so the agent receives the exact (dedented, as
  today) full text.
- Recalling a message from input history (Up/Down, including prior sessions) loads the whole text
  as-is; no placeholder is ever substituted for part of a recalled message.
- Existing multiline paste behavior (dedent, newline handling, history-browsing exit) is
  unchanged for normal-sized pastes.
- `make format lint`, `make test-coverage` (≥80%) pass; docs updated.

## Implementation Plan

Implementing agent: flip `[ ]` to `[x]` right after completing each step and rewrite this file
in place so progress is resumable across sessions. Follow TDD: write the failing test first.

1. [x] Write a failing regression test in `src/tui_driver/tests/` (see
   `src/tui_driver/tests/sessions.rs:92` for how `AppAction::Paste` is driven) that pastes a
   very large text (e.g. 500 000 chars with newlines) through `paste_text` and asserts content
   equality and a generous time bound. Also add a unit test next to `paste_text` for
   CRLF input and multi-line input equivalence with the old behavior.
2. [x] Fix root cause 1: rewrite `paste_text` in `src/tui_driver/input.rs` to insert the
   dedented text with a single `textarea.insert_str(...)` call (one chunk insert = one screen-map
   refresh) instead of per-char `input` calls. Keep `exit_input_history_browsing` and `dedent`
   behavior. Verify tabs and `\r\n` handling stays equivalent (`insert_str` strips a trailing
   `\r` per line).
3. [x] Introduce the large-paste threshold as a single constant (e.g.
   `LARGE_PASTE_CHAR_THRESHOLD = 1_000` chars, next to the input-history code in
   `src/tui_reducer_state/`), measured on the dedented pasted text; make it overridable in tests.
4. [x] Placeholder for large pastes only (typed text and history recall never collapse): when a
   paste exceeds the threshold, insert a single-line marker such as
   `[Pasted text #N: 3,200 lines, 120,000 chars]` at the cursor and store the dedented pasted text
   in `AppState` (new field, e.g. `pasted_blobs: Vec<PastedBlob { marker, text }>`). `N` is a
   per-draft counter so two pastes get distinct markers. Keep the logic in a small, pure,
   unit-testable module, not in `main.rs`/the event loop.
5. [x] On `Effect::SubmitPrompt` (`src/tui_driver/effects.rs`), replace every marker still present
   verbatim in the draft with its stored text immediately, then `dedent`, then send. A marker the
   user edited is plain text and is not expanded. Clear the store on submit, `ClearTextarea` and
   `clear_textarea`. The approval-pending snapshot (`save_input_state_and_clear` /
   `restore_input_state`) must park and restore the store together with the draft.
6. [x] Input history is unchanged: recalling any entry (including huge ones) loads the whole text
   as-is via `set_input_text`, no placeholder. History entries stored in `input_history` are always
   the expanded text, because the expansion happens before the prompt is sent and recorded.
7. [x] Measured (~27 ms release at ~1 MB; skipped, recorded in implementation-notes):
   measure the per-frame cost of a huge draft that is in the textarea (typed, or recalled from
   history). Only if a frame at ~1 MB exceeds ~50 ms, cut the O(n) passes: avoid the repeated
   `lines().join("\n")` in `src/tui_driver/event_loop.rs` (`textarea_ctx`) and
   `src/tui_driver/frame.rs`, and avoid the per-frame `TextArea::from(lines.to_vec())` measurement
   clone in `src/tui_widgets.rs`. Otherwise record the measurement in
   `docs/implementation-notes.md` and skip (YAGNI).
8. [x] Add tests: marker created above the threshold and not at/below it; expansion on submit
   incl. dedent; edited marker is not expanded; two pastes get distinct markers; history recall of
   a huge entry loads verbatim without a marker; draft save/restore around approval keeps blobs
   consistent; history-browsing exit on paste still works.
9. [x] Documentation (same commit): `docs/features.md` ("Multiline paste"
   bullet, ~line 1123) describes the placeholder behavior and threshold; the "Input history" bullet
   (~line 1075) states that recalled messages load verbatim;
   `docs/implementation-notes.md` (input handling section near ~line 641) documents the
   single-chunk insert, root cause and blob store; `docs/keybindings.md` only if any key is
   added; replace the two resolved items in `docs/backlog-issues.md` with two new backlog
   items (terminal restore on panic/SIGTERM/SIGHUP, hard paste-size cap with toast); update
   this plan.
10. [x] Verify: `make format lint`, `make test-coverage`; run `rumdl fmt` / `rumdl check` on
    changed markdown. Manual multi-MB paste in a real terminal is left to the user.

## Proposed Improvements

- Panic/kill-safe terminal restore: install a panic hook and signal handling (SIGTERM/SIGHUP)
  that disables mouse capture and bracketed paste, so abnormal exits do not leave the terminal
  emitting garbage. Out of scope here; tracked in the backlog.
- Hard upper bound (e.g. 50 MB) on pasted content with a toast explaining rejection, to protect
  against memory blow-ups (tracked in the backlog, out of scope here).
- Run paste handling off the UI-critical path is unnecessary once insertion is O(n); do not add
  threads (KISS).

## Consistency Issues

- [low] The placeholder makes the textarea show something different from what is sent. The
  "dedent on send" contract (`dedent(&raw_text)` in `Effect::SubmitPrompt`) must be applied
  *after* marker expansion; pasted text is stored already dedented, so the result matches today's
  behavior.
- [low] Session input history stores full text; a placeholder must never be persisted or added to
  history in place of real content (guaranteed by expanding before send).

## Implementation Hints

- `src/tui_driver/input.rs`: `paste_text`, `set_input_text`, `clear_textarea`,
  `save_input_state_and_clear`, `restore_input_state`, `load_history_into_textarea`, `dedent`.
- `src/tui_driver/effects.rs`: effect dispatch; `Effect::Paste`, `Effect::SetInputText`,
  `Effect::SubmitPrompt` (~lines 108-135).
- `src/presenter/keyboard.rs`: `InputEvent::Paste` → `Effect::Paste` (lines ~134, 171, 213; other
  focus modes ignore paste).
- `src/tui_reducer_state/input_history.rs`: `navigate_input_message_history`,
  `exit_input_history_browsing`; `AppState` in `src/tui_reducer_state/app_state.rs`.
- `src/tui_driver/event_loop.rs` (~line 158-165, 301, 344, 379, 438) and
  `src/tui_driver/frame.rs` (~58-61): repeated `lines().join("\n")`.
- `src/tui_widgets.rs` (~245-265 height measurement, ~508 viewport probe clone).
- `ratatui-textarea` 0.9.2: `insert_char`/`input` → `push_history` → `refresh_screen_map` →
  `screen_map_load` (full re-wrap every call); `insert_str` → `insert_piece`/`insert_chunk`,
  which refreshes once per call. Undo history is capped (`set_max_histories`, default 50).
- `AppAction::Paste` comes from crossterm `Event::Paste` (bracketed paste enabled in
  `src/tui_driver/terminal.rs`); `src/events.rs:67` maps it.
- Conventions (AGENTS.md): no abbreviations in names, license header on new files, file-per-module
  (`foo.rs`), keep `main.rs` thin, docs updated in the same commit, no commits without consent.
  Check `docs/known-test-issues.md` before tests touching `tracing`/cwd. No ADR currently covers
  TUI input handling; add one only if the placeholder design is considered an architecture
  decision.
- Test gate: `make format lint`, then `make test-coverage` (≥80% aggregate).
