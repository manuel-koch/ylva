# Prevent accidental approval, denial and navigation from keystrokes meant for something else

## Context

Ylva is a Rust terminal UI agent harness (binary + library crate `ylva`). Two UX hazards exist
where a prompt appears at the instant the user is pressing a key that was meant for another purpose:

1. **Accidental deny/abort when leaving a subagent drill-in.** In the subagent drill-in view, `Esc`
   (empty draft) navigates back to the root conversation. If a tool approval or clarify prompt for
   the root appears just as `Esc` is pressed, the same keypress (or a second one) denies the tool
   (approval: `Esc` denies immediately) or aborts the clarify (empty-input `Esc`).
2. **Accidental approval/denial while typing a draft.** While the user types a draft and presses
   `Enter` to send it, a tool approval can arrive first. Focus switches to approval, remaining text
   lands in the denial-reason textarea, and the final `Enter` denies the tool (non-empty input) or
   approves it (empty input) instead of sending the draft.

Decided design (agreed with the user):

- **Prompt deferral:** a tool approval or clarify prompt is not shown while the user is typing.
  It stays queued until the input has been idle for 1.5 s (hard cap 10 s of continuous typing, after
  which it is shown anyway). While deferred, the footer shows a passive indicator such as
  `⏳ 1 tool approval waiting (finish typing)`.
- **Button focus model:** approve/deny (and clarify OK/Cancel) are focusable buttons. Initially
  no button has focus. `Tab` cycles focus between the buttons; `Enter` activates only the focused
  button and does nothing when no button has focus. Mouse clicks on the buttons still resolve the
  prompt directly. `Enter` and `Esc` therefore can never resolve a prompt by accident, even when
  unnoticed text was typed into the input field.
- **Single input field:** no draft snapshot and no hand-over buffer. Text typed before or after
  the prompt appears stays in the same input field; it is used as the denial reason (Deny
  focused) or clarify answer (OK focused) only on explicit button activation. Approve ignores the
  text and leaves it in the field; while Approve is focused and the field is non-empty, the text
  is rendered dimmed (plus a "text will be discarded" hint) to show it will be ignored. Deny
  sends the text as reason and clears it.
- **`Esc`:** approval: removes button focus, otherwise does nothing. Clarify: non-empty input is
  cleared; empty input does nothing unless Cancel has focus, whose activation aborts.
- **`Ctrl+C`** keeps denying immediately during approval.
- **Timings** are constants (no configuration).
- **Subagent navigation:** new `ctrl+p` binding steps out of the current subagent view to its
  parent conversation (generalized so it works for nested subagents), plus a clickable "back"
  button in the subagent drill-in header bubble. `Esc` no longer navigates back.

Goal: a prompt that appears mid-keystroke must never be resolved by a keypress the user issued
before they could perceive it, and approve/deny must never be confused with ordinary editing/sending/
navigation keys.

Acceptance criteria (definition of done):

- Leaving a subagent drill-in cannot resolve (deny/abort) a prompt that appeared concurrently.
- Typing/sending a draft cannot approve or deny a tool that appeared concurrently; an `Enter`
  or `Esc` pressed before the user could see the prompt does not resolve it.
- Draft text typed before the prompt appeared is preserved (not leaked into the denial reason) or
  clearly separated from it.
- Footer hints, `docs/keybindings.md`, `docs/features.md` and `docs/implementation-notes.md` match
  the new behavior; tests cover the new behavior; `make format lint` and `make test-coverage` pass.

Constraints: follow `AGENTS.md` (docs updated in same commit, full descriptive names, license
headers, `main.rs` thin, no file-per-module `mod.rs`); check ADRs in
`docs/architecture-design-records/` (0001-0003, none cover keybindings currently); the coverage
threshold from `docs/development-workflow.md` must be kept; consult `docs/known-test-issues.md`
before writing tests that touch `tracing`/cwd.

## Implementation Plan

An implementing agent must flip `[ ]` to `[x]` immediately after completing each step and rewrite
this file in place, so progress is resumable across sessions.

1. [ ] Read `docs/development-workflow.md`, `docs/known-test-issues.md` and the ADRs; create an ADR
   (skill `architecture-decision-record`) recording the new prompt interaction model
   (deferral, button focus, single input field, `ctrl+p`).
2. [ ] Write failing tests first (TDD), in `src/presenter/tests/keyboard.rs`,
   `src/presenter/tests.rs`, `src/tui_driver/tests/*`, `src/tui_reducer_state/tests/conversation.rs`:
   - prompt is deferred while keystrokes arrive and shown after 1.5 s idle; shown after the 10 s cap;
   - `Enter`/`Esc` with no focused button do nothing; `Tab` cycles focus (none → button → next);
     `Enter` activates only the focused button; button clicks resolve directly; same for clarify
     OK/Cancel;
   - text typed before/after the prompt appears stays in the field; Approve ignores it and leaves
     it; Deny uses it as reason and clears it; the field is dimmed while Approve is focused and
     non-empty; `Esc` clears button focus (approval) / clears text (clarify);
   - `ctrl+p` in a subagent view returns to the parent view; no-op in root view; `Esc` no longer navigates;
   - back button click returns to the parent view.
3. [ ] Add time-based state (inject a clock/tick; never read wall time in the presenter): last
   keystroke instant, deferred prompt marker, prompt-focused button, in
   `src/tui_reducer_state/` and the focus handling for `Focus::ToolApproval` and
   `TuiMode::ClarifyPending`; drive the idle timer from the existing tick in
   `src/tui_driver/event_loop.rs`.
4. [ ] Implement deferral in the approval/clarify arrival path (presenter `domain.rs`) and remove the
   draft save/restore (`pending_input_snapshot`, `Effect::RestoreInputState`, see Hints) because
   the draft now stays in the field; focus is only switched when the idle
   condition is met; keep strict-FIFO ordering of `pending_approval_order`.
5. [ ] Rework `keyboard_approval` and `keyboard_clarify` in `src/presenter/keyboard.rs`: add
   button focus state (none/approve/deny; none/ok/cancel) with `Tab` cycling, make `Enter` act
   only on the focused button, remove `Esc` resolution of
   approvals, keep `Ctrl+C`/stop-signal semantics.
6. [ ] Add `ctrl+p` mapping in `src/events.rs` (new `InputEvent` variant) and handle it in
   `keyboard_main` via a parent-navigation method next to `switch_to_root_view()` in
   `src/tui_reducer_state/conversation/navigation.rs`; remove the subagent `Esc` branch.
7. [ ] Render focus highlighting for the approval and clarify buttons and add a clarify
   OK/Cancel button pair (rects in `src/tui_reducer_state/layout.rs`, click handling in
   `src/presenter/mouse.rs`) and dim the textarea text (textarea style) while Approve is focused and
   the field is non-empty. Add the "back" button: `Rect` in
   `src/tui_reducer_state/layout.rs`, render in the subagent header bubble in
   `src/tui_widgets/`, handle the click in `src/presenter/mouse.rs`.
8. [ ] Update the footer in `src/tui_widgets/footer.rs` (~line 769-776 labels, replace
   `(enter)`/`(esc)` hints with focus/`Tab` hints, waiting indicator, subagent-view hint
   `ctrl+p back`) and the stale comment at ~line 757.
9. [ ] Update docs in the same commit: `docs/keybindings.md` (rows ~47-54, ~71-73),
   `docs/features.md` (~928, ~961, ~1059-1065), `docs/implementation-notes.md` (~568-603, ~623),
   and the new ADR in `docs/architecture-design-records/`; run `rumdl fmt` and `rumdl check`.
10. [ ] Run `make format lint`, then `make test-coverage` (mandatory single gate).

## Proposed Improvements

- Terminal bell or title notification when a deferred prompt is waiting for more than a few seconds.
- Make timing values configurable only if users request it (YAGNI otherwise).

## Consistency Issues

- [low] `docs/keybindings.md` line 49 states `Esc`/`Ctrl-C` deny "immediately ... no
  confirmation"; this contradicts the new design and must be rewritten.
- [low] Deferring prompts delays tool execution and subagent progress while the user types;
  bounded by the 10 s cap.

## Implementation Hints

- `src/presenter/keyboard.rs`: `keyboard_approval` (line ~86) denies on `CtrlC | Escape`, approves
  on `Enter` with empty textarea, denies with textarea text as reason on `Enter` otherwise; other
  keys go to `Effect::TextareaKey`. `keyboard_clarify` (~185): `Esc` with empty input aborts via
  `Effect::AbortClarify`; `Enter` answers. `keyboard_main` (~138): `Esc` with empty draft in
  `ConversationView::Subagent` calls `state.switch_to_root_view()`.
- `src/presenter/mouse.rs` ~392-414 already handles Approve/Deny button clicks using
  `state.layout.approve_button` / deny button rects (`src/tui_reducer_state/layout.rs` ~185).
- `src/tui_widgets/footer.rs` ~769-776 builds the labels; the Approve label is always
  `" [ ✓ Approve (enter) ] "`.
- Approval/clarify use a strict-FIFO `AppState::pending_approval_order`; the draft is currently
  saved in `pending_input_snapshot` and restored after all approvals resolve
  (`docs/implementation-notes.md` ~568-603); this mechanism is removed by the planned change.
- The repository is a binary + lib split: anything integration tests need is re-exported via
  `src/lib.rs`. Use full descriptive names; no abbreviations.
- Tests live in `src/presenter/tests/keyboard.rs`, `src/presenter/tests.rs`,
  `src/tui_driver/tests/{effects,input,sessions}.rs`.
- Existing docs describing behavior: `docs/keybindings.md`, `docs/features.md`,
  `docs/implementation-notes.md`.
- Delegation is currently one level deep (`docs/features.md` ~862: subagents cannot spawn further
  subagents), so `ctrl+p` currently always returns to root; implement it as "go to parent" so
  nesting would work. `ConversationView` (`src/tui_reducer_state/conversation.rs` ~417) has only
  `Root` and `Subagent {..}` without a parent link.
- `ctrl+p` is unused in the codebase, but `ratatui-textarea` binds it to "cursor up" in the root
  draft; only intercept it in the subagent view, which accepts no input.
- `src/events.rs` already maps `Ctrl+C` (~line 78) and `Ctrl+J` (~line 88) specially.
- `Tab` always moves focus between the prompt buttons while a prompt is visible (slash-command
  completion is unavailable during prompts).
