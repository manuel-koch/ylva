# Agents popup (`/agents`, `ctrl-a`) plan

## Context

**Component:** Ylva's Rust TUI (`src/slash_command.rs`, `src/events.rs`,
`src/tui_reducer_state/*`, `src/presenter/*`, `src/tui_widgets/*`,
`src/tui_driver/*`).

**Goal:** add a modal **Agents popup** listing the root conversation and all
subagent runs of the current session with their status, so the user can jump to
any agent's conversation without scrolling to its `spawn_subagent` bubble. It is
opened by the slash command `/agents` and by the keybinding `ctrl-a`.

**Why:** today a subagent conversation can only be opened by double-clicking its
`spawn_subagent` bubble in the root transcript. There is no aggregate run list.
A keybinding is needed because the popup must be reachable while drafting a
message and while an approval/clarify is shown (where `esc` would abort it).

**Decisions (settled with the user):**

- The popup is a **modal focus like the help popup** (`Focus::HelpPopup`), not a
  `SlashOverlay` variant. It captures all keyboard and mouse input, never
  disturbs the draft or a pending approval/clarify, and stays open on background
  events (including `note_interactive_pause` auto-switches; the current-marker
  simply follows the view).
- Opened by `/agents` (canonical name; no alias — typing `/agent` already
  auto-completes via prefix suggestions) and by `ctrl-a`, intercepted globally.
  `ctrl-a` replaces the textarea's emacs "cursor to line start" binding (`home` /
  `fn-left` remain). Document that tmux/screen users must press the prefix
  twice.
- Mutually exclusive with the help popup: `F1` while the agents popup is open
  switches to help; `ctrl-a` while help is open switches to the agents popup.
  Each trigger toggles its own popup closed; `esc` closes either.
- The list contains, in this order:
  1. **Root** row ("Root (main conversation)"), pinned first regardless of
     priority.
  2. **Running** runs, spawn order (oldest first).
  3. **Queued** runs, spawn order (oldest first); shown dimmed, skipped by
     `↑`/`↓` navigation, not activatable (no conversation exists yet).
  4. **Finished / error / cancelled** runs, newest first.
- Each row shows: current-marker (the conversation being viewed), label
  (elided with an ellipsis so the other columns stay visible), result (status
  word only: success / error / cancelled, when available), elapsed duration
  (counting up live while running, frozen once finished; `–` for runs replayed
  from a resumed session, which have no live timer), and a small badge when the
  run awaits user input (pending approval or clarify).
- `enter` on a selectable row closes the popup and switches the view to that
  agent, scrolled to the last item; on the root row it returns to root. A single
  click only highlights; double-click activates (as in the session picker).
- The popup is available while busy and in either conversation view.
- Out of scope: result excerpts, persisted timestamps for resumed runs, popup
  parking logic.

**Acceptance criteria:**

1. `/agents` (visible in slash suggestions) and `ctrl-a` open the popup in
   either conversation view, including while an approval/clarify is pending or a
   draft exists; the draft is left untouched.
2. Rows are ordered root → running → queued → finished, with marker, label,
   result, live elapsed duration and awaiting-input badge as specified.
3. `enter`/double-click on a selectable row switches view (bottom-pinned) and
   closes the popup; queued rows cannot be activated.
4. Running durations tick without user input; finished durations are frozen.
5. `esc` or the opening trigger closes the popup without changing the view;
   `F1`/`ctrl-a` switch between help and agents popups.
6. Docs updated; `make format lint` and `make test-coverage` pass (≥80 %).

**Constraints:**

- Slash parsing is a closed allowlist (`slash_command.rs`);
  `accepts_argument: false`.
- Do not break drill-in invariants (`docs/features.md` → *Drill-in TUI
  conversation view*): manual switches are never auto-returned; typing while
  watching a running subagent stays a no-op.
- Full descriptive names, no abbreviations; Apache-2.0 header on new source
  files; file-per-module layout (`foo.rs`, never `foo/mod.rs`); `main.rs` thin.
- Use the term **subagent** in docs/UI (see `CONTEXT.md`), never "child agent".
- Docs are part of the change set (`AGENTS.md`); TDD.

## Implementation Plan

Implementing agent: flip `[ ]` to `[x]` immediately after completing each step
and rewrite this file in place so progress is resumable across sessions.

1. [ ] Read `CONTEXT.md`, `docs/development-workflow.md`,
   `docs/known-test-issues.md`, and the help-popup implementation as the pattern
   to mirror (`Focus::HelpPopup` in `src/tui_reducer_state/events.rs`,
   `keyboard_help` in `src/presenter/keyboard.rs`, help branches in
   `src/presenter/mouse.rs`, `HelpPopupLayout` in
   `src/tui_reducer_state/layout.rs`, `draw_help_popup` in
   `src/tui_widgets/`).
2. [ ] Store run outcome data. Extend `SubagentConversation`
   (`src/tui_reducer_state/conversation.rs`) with a frozen elapsed duration (or
   `finished_at: Option<Instant>`) and `outcome: Option<SubagentOutcome>`
   (success / error / cancelled, derived from `DomainEvent::SubagentDone
   { status, .. }`). Set them in the `SubagentDone` arm of
   `src/presenter/domain.rs`. Write the failing tests first
   (`src/presenter/tests/domain.rs`).
3. [ ] Add the row model: `AgentRow`/`AgentRowStatus` and a builder on `AppState`
   producing the ordered list (root; running; queued; finished) with
   current-marker, elapsed duration source, and awaiting-input flag (from
   `pending_approval_order` / `queued_clarifies`, mapped to the owning run).
   Queued rows come from root `spawn_subagent` bubbles whose
   `progress_arguments` has `status=queued`. Unit-test ordering, grouping,
   current-marker and badge.
4. [ ] Add `Focus::AgentsPopup` (`src/tui_reducer_state/events.rs`; include in
   `Focus::is_modal`) and an `AgentsPopupState` (selection tracked by run id so
   reordering never moves the highlight; scroll offset) plus
   `AgentsPopupLayout` (rect, row hit-boxes) in the reducer/layout modules,
   re-exported via `src/tui_reducer_state.rs` and `src/lib.rs`. Review every
   `Focus::HelpPopup` use (see hints) and decide per site whether
   `AgentsPopup` needs the same handling (e.g. `agent_mode`, slash-overlay
   hiding, `is_typing`, input routing).
5. [ ] Add `SlashCommand::Agents`, `AGENTS_COMMAND_NAME = "/agents"` and an
   allowlist entry in `src/slash_command.rs` (description e.g. "List subagents
   and switch to one", `accepts_argument: false`); update the tests there that
   enumerate commands/suggestions. Handle submit where `/session` is handled
   (`src/tui_driver/input.rs`, presenter): clear the draft, set
   `Focus::AgentsPopup`.
6. [ ] Add the `ctrl-a` mapping in `src/events.rs` (`map_event`, near
   `KeyCode::F(1)`) to a new `AppAction::ShowAgents` → `InputEvent::ShowAgents`;
   route it from every focus handled in `src/presenter/keyboard.rs` (main,
   approval, clarify, session picker, help) so it works everywhere `F1` does,
   with the toggle/switch semantics above. Ensure it is not forwarded to the
   textarea.
7. [ ] Add `keyboard_agents_popup` in `src/presenter/keyboard.rs`:
   `↑`/`↓` (skipping queued rows), page keys, `enter` → activate, `esc` and
   `ctrl-a` → close, `F1` → switch to help, `ctrl-c` as in the help popup. Add
   mouse handling in `src/presenter/mouse.rs`: wheel scrolls, click highlights,
   double-click activates, click outside closes (as help).
8. [ ] Implement activation: close the popup, then reuse
   `switch_to_subagent_view(run_id)` / `switch_to_root_view()`
   (`src/tui_reducer_state/conversation.rs`), triggering the lazy transcript
   load for replayed runs (`needs_lazy_load`) and keeping the chat pinned to the
   last item. Switching to the already-current agent just closes the popup.
9. [ ] Render the popup in `src/tui_widgets/` (new `agents_popup.rs` or alongside
   the help popup): centered modal; columns `[marker] label … result duration
   [badge]`; label elided so marker, result, duration and badge always fit;
   group headers or separators per priority; `–` instead of a duration for
   replayed runs without `started_at`; styles from `src/theme.rs` (running /
   dimmed queued / error); write layout (rect, row hit-boxes) back each frame
   like `draw_help_popup` (`render.rs`, `tui_render_state.rs`,
   `tui_driver/updates.rs` bridge).
10. [ ] Live elapsed time: confirm the redraw tick already runs while subagents
    run (bubble durations use `subagent_started_at`); if the tick is gated on
    mode/focus, extend it so an open agents popup with a running row keeps
    redrawing. Test with the existing time-injection patterns.
11. [ ] Footer: when the popup is open the footer shows popup controls (as for
    help; `src/tui_widgets/footer.rs`). Add `ctrl-a` to the footer idle hints
    only if space allows per the existing dynamic layout rules, and to the
    help-popup keybinding table source.
12. [ ] Update docs in the same commit: `docs/features.md` (Slash commands table
    row for `/agents`; an *Agents popup* subsection near the drill-in section
    and a mention in the Help popup bullet), `docs/keybindings.md` (`ctrl-a`
    rows for all contexts, popup keys, note that `home` replaces the emacs
    line-start binding and the tmux/screen prefix caveat),
    `docs/implementation-notes.md` (new focus variant, row ordering, live
    timers, relation to the help popup), `CONTEXT.md` only if a domain term is
    added. Remove the `/agent` section from `docs/ideas/slash-commands.md`
    (including the intro sentence that refers to it). Run `rumdl check` /
    `rumdl fmt` on touched markdown.
13. [ ] Verify: `make format lint` and `make test-coverage`; manually run
    `make build-debug-and-run` with several subagents (ordering, live
    durations, badge during an approval, drill-in scroll position, `esc`,
    `F1` ↔ `ctrl-a` switching, draft preserved).

## Proposed Improvements

- Show the subagent `agent_type` as a dim secondary column when width allows.
- Persist/derive durations for resumed runs (`SubagentRunRecord` in
  `src/store/event.rs`) instead of `–`.
- Optional short result excerpt for finished rows when the terminal is wide.

## Implementation Hints

- Slash allowlist and parser: `src/slash_command.rs` (`SLASH_COMMANDS`,
  `SlashCommand`, `parse_slash_command_invocation`,
  `slash_command_suggestions`); prefix suggestions already make `/agent` offer
  `/agents`. Tests list commands explicitly (e.g. `("/session",
  SlashCommand::Session)`).
- Help popup is the model to follow: `Focus::HelpPopup` and `Focus::is_modal`
  (`src/tui_reducer_state/events.rs`), dispatch at `src/presenter/keyboard.rs`
  (`Focus::HelpPopup => keyboard_help`; `ShowHelp` handled in several handlers
  around lines 46, 165, 186, 221, 526), mouse at `src/presenter/mouse.rs`
  (~238, ~282), `show_help()` and `agent_mode` handling in
  `src/tui_reducer_state/app_state/interaction.rs` (~114, ~151), slash-overlay
  suppression in `refresh_slash_overlay` (`model_picker.rs` ~854), navigation in
  `src/tui_reducer_state/conversation/navigation.rs` (~109), input gating in
  `src/tui_driver/input.rs` (~169), layout `HelpPopupLayout` in
  `src/tui_reducer_state/layout.rs`, render state in `src/render.rs` (~390,
  ~496) and `src/tui_render_state.rs` (`HelpPopupState`).
- Key mapping: `src/events.rs` `map_event` (`KeyCode::F(1)` →
  `AppAction::ShowHelp`). The textarea is `ratatui-textarea` 0.9, whose default
  `ctrl-a` is "cursor to line start". `ctrl-p` is already repurposed
  (`ParentView`, drill-in back). `docs/keybindings.md` has the key tables.
- Subagent state (`src/tui_reducer_state/conversation.rs`, `app_state.rs`):
  `subagent_conversations: HashMap<Uuid, SubagentConversation>` (every started
  run, kept for the session); `running_subagents: HashMap<Uuid,
  SubagentRunInfo>` (removed on `SubagentDone`); `SubagentRunInfo { run_id,
  bubble_id, agent_type, task, label }`; `SubagentConversation.started_at:
  Option<Instant>` (`None` for replayed runs, which set `needs_lazy_load`).
  Neither finish time nor `status`/`result` of `DomainEvent::SubagentDone
  { run_id, status, result }` is stored yet (`presenter/domain.rs` only removes
  the run from `running_subagents`). Status strings include `"cancelled"`.
- `subagent_started_at()` maps bubble id → `Instant` for live bubble duration
  counters; reuse the same ticking mechanism.
- Queued runs exist only as root `spawn_subagent` `ToolBubble`s in
  `ToolCallState::Running` with `progress_arguments` containing `("status",
  "queued")` (see `ActiveSubagentCounts::from_entries`); no
  `SubagentConversation` exists until `SubagentStarted`. Their label comes from
  the bubble's `title_arguments`.
- Pending prompts: `pending_approval_order: Vec<Uuid>` (FIFO of approval/clarify
  ids across root and subagents) and `queued_clarifies`; tool bubbles identify
  the owning run, used for the awaiting-input badge.
- View switching: `ConversationView::{Root, Subagent { run_id, agent_type, task,
  label }}`; `switch_to_subagent_view(run_id)` (manual; sets
  `auto_switched_to_subagent = false`, re-pins scroll to bottom, remembers the
  root bubble to scroll back to) and `switch_to_root_view()` /
  `switch_to_parent_view()` (`ctrl-p`, `esc` with empty draft).
- Tests: presenter tests in `src/presenter/tests/{keyboard,mouse,domain}.rs`,
  reducer tests in `src/tui_reducer_state/tests/`, driver tests in
  `src/tui_driver/tests/`, key mapping tests in `src/events.rs`. Read
  `docs/known-test-issues.md` before tests involving `tracing` or cwd.
- Verification gate: `make format lint`, then `make test-coverage`. No
  dependency changes expected, so `make audit` is not required.
- No ADR covers slash commands, popups or keybindings
  (`docs/architecture-design-records/` has 0001–0003: git conventions, release
  automation, workspace trust); no new ADR is needed.
