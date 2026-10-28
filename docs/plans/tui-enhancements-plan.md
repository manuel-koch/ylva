# TUI enhancements: configurable auto-expand of tool bubbles

## Context

Ylva is a Rust TUI agent harness (binary + library crate `ylva`; see `AGENTS.md`).
Tool bubbles currently arrive collapsed in the TUI (the `clarify` bubble arrives
expanded) and the user expands them by clicking. Goal: let the profile choose
tools whose bubbles are expanded when they arrive, and optionally auto-expand
the bubble that is awaiting user approval, collapsing it again once approval
resolves.

The planned configuration (`clarify` is deliberately not an option value: its
bubble is already created expanded and has no approval phase):

```toml
[tui]
auto_expand_tool = ["read", "write", "edit"]
auto_expand_approval = true
```

Acceptance criteria:

- Bubbles of tools listed in `auto_expand_tool` are expanded when they arrive
  (root and subagent conversations).
- With the approval option enabled, the bubble under approval expands when
  approval starts and collapses when approval ends.
- Defaults (empty list, `false`) preserve today's behavior; manual click
  toggling keeps working.
- Tests per `docs/development-workflow.md`; docs updated in the same change;
  `make format lint` and `make test-coverage` pass (≥80% coverage); `rumdl`
  clean on edited markdown.

Constraints: no abbreviations in names; license headers on new files;
file-per-module layout (no `mod.rs`); keep `main.rs` thin; ADR 0003 (workspace
config trust by content hash) applies to workspace profiles but needs no change
because the feature adds no credential or approval-bypass surface; creating new
doc files needs user consent.

## Implementation Plan

Implementing agents must flip `[ ]` to `[x]` immediately after completing each
step and rewrite this file in place so progress is resumable across sessions.
Follow TDD: write the failing test first per step.

1. [ ] Add a `TuiConfig` struct (`[tui]` table) in `src/config/profile.rs` with
   `auto_expand_tool: Vec<String>` and `auto_expand_approval: bool`,
   `#[serde(default)]`; add a `tui` field to `ProfileConfig` and its `Default`
   (~line 577). Add `"tui"`, `"auto_expand_tool"` and `"auto_expand_approval"`
   to the known keys in `src/config/unknown_keys.rs`. Add loading tests in
   `src/config/profile/tests/loading.rs` (defaults, valid values, wrong types).
2. [ ] Report `auto_expand_tool` entries that match no registered tool name
   (case-sensitive) as a `ConfigWarning` on `ProfileConfig::config_warnings`
   when the profile loads. The existing `--strict-config` flag already turns
   every config warning into a startup error (exit code 2), so no CLI change is
   needed; update its help text in `src/cli.rs` (~line 167) and
   `docs/cli-options.md` (line 33) to say "configuration warnings (unknown keys,
   unknown tool names)". Tests: warning for an unknown tool, none for known
   tools, and a strict-mode startup test in `src/startup.rs`'s tests
   confirming the new warning aborts only with `--strict-config`.
3. [ ] Thread the settings into the reducer state: store them in `AppState`
   (`src/tui_reducer_state/app_state.rs`), populated from the profile via
   `src/tui_driver/app.rs` / `src/tui_driver/runtime.rs` (same path as
   `input_history_limit`).
4. [ ] Apply arrival expansion: in `src/presenter/tool_bubbles.rs` create bubbles
   with `collapsed: false` when the tool name is in `auto_expand_tool` (exact
   names, no wildcard), at all
   creation sites (Pending ~line 49, Running ~line 181, Done ~line 312, Killed
   ~line 334). Route through one helper so the rule lives once (SSOT). Do not
   set `pending_bubble_expansion` (no `Expanding…` feedback) and do not clear
   `scroll_locked`: a bottom-pinned view stays pinned.
5. [ ] Apply approval expansion: when `push_tool_pending_bubble` arms
   `Focus::ToolApproval` and when a queued approval is promoted
   (`src/presenter/domain.rs`, `TypingSettled`), expand the bubble if enabled
   and record that the feature expanded it. When approval resolves (Pending →
   Running ~line 184, denial, killed ~line 337) collapse it only if it was
   expanded by this feature; any manual user toggle clears that record.
6. [ ] Tests in `src/presenter/tests/` and `src/tui_reducer_state/tests/`:
   arrival expansion, non-listed tools, approval expand/collapse, queued
   approvals, subagent-owned bubbles, manual toggle during approval; add a
   render test in `tests/tui_render_state/bubbles.rs` if layout needs coverage.
7. [ ] Documentation: `docs/configuration.md` (new `[tui]` rows/section next to
   the top-level fields), `docs/features.md` (tool bubble collapse/expand bullet
   ~line 1182), `docs/implementation-notes.md` (settings flow and approval
   expansion tracking), `docs/cli-options.md` (`--strict-config` row wording);
   update `docs/ideas/tui-enhancements.md` for the implemented section per
   project convention.
8. [ ] Final verification: `make format lint`, `make test-coverage`, and `rumdl
   check` on edited markdown.

## Implementation Hints

- Config: `ProfileConfig` in `src/config/profile.rs`; unknown TOML keys are
  reported using the list in `src/config/unknown_keys.rs`, so new keys must be
  added there.
- Bubbles: `ChatEntry::ToolBubble { id, name, state, collapsed }`. Creation sites
  are in `src/presenter/tool_bubbles.rs`. `push_tool_pending_bubble` arms
  `Focus::ToolApproval`; the Pending → Running update keeps the existing
  bubble in place, so its `collapsed` value persists past approval.
- Approvals queue via `pending_approval_order` and are promoted later (e.g. on
  `DomainEvent::TypingSettled`), so expansion must happen at promotion, not only
  at bubble creation.
- Click toggle: `AppState::toggle_tool_bubble_collapsed` in
  `src/tui_reducer_state/conversation/tools.rs`; expanding sets
  `pending_bubble_expansion`, which drives the `Expanding…` feedback and clears
  `scroll_locked`. Auto-expansion (arrival or approval) must not do either, so a
  view pinned to the bottom stays pinned. Approval end of any kind (approve,
  deny, kill) follows the same collapse rule.
- Subagent bubbles live in `subagent_conversations`; use
  `find_tool_bubble_mut` / `find_tool_bubble_for_render_mutation`.
- Config warnings today: unknown TOML keys are collected as `ConfigWarning`
  (`src/config/unknown_keys.rs`) into `ProfileConfig::config_warnings`
  (`src/config/profile.rs` ~line 118), logged in `src/startup.rs` ~line 214 and
  surfaced via `config_warnings_notice()` (`src/tui_driver/app.rs` ~line 130,
  `src/cli_driver/console_app.rs` ~line 103). `--strict-config`
  (`src/cli.rs` ~line 167, `src/startup.rs` ~line 217, documented in
  `docs/cli-options.md` line 33) fails startup on any non-empty notice, so a new
  warning is escalated automatically. Registered tool names come from the tool
  registry in `src/agent/tools.rs`.
- Profile values reach the TUI via `src/tui_driver/app.rs` and `runtime.rs`
  (e.g. `input_history_limit`).
- `docs/plans/` was empty before this file; `docs/development-workflow.md`
  ("Planned implementations", "Plan execution") requires a documentation step
  and doc updates in the same commit as the code.
