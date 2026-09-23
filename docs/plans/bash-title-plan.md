# Bash tool label (title-line purpose string)

The implementing agent must flip `[ ]` to `[x]` on each step immediately after
completing it and rewrite this file in place, so progress is resumable across
sessions.

## Context

Ylva is a Rust TUI agent-harness. Tools expose typed argument structs derived
with `schemars`; schema field declaration order determines the display rank of
arguments used to render tool-call bubble titles and CLI status lines. The
lowest-ranked (first-declared) title argument renders as the bare, bold
primary; remaining title arguments render as dim `key=value` props.

The `spawn_subagent` tool already has a `label` argument (short purpose
string) used as its primary title text. The `bash` tool today accepts only
`command`, so during user approval the collapsed tool-call bubble shows only
the raw command and the user gets no statement of purpose.

The idea (source: `docs/ideas/bash-title.md`, left untouched by this change):

> Enhance the `bash` tool to allow llm to describe what the purpose of the
> shell execution is and to show user more info during the pending user
> approval process. Could be a "label" like for `spawn_subagent` tool.

Settled interpretation (grilling rounds 1–2): add an **optional** `label` to
`bash`, ranked **second** (after `command`), displayed as a dim `label=…`
prop on the tool-call bubble/CLI title line — including while the bubble is
collapsed and pending approval. The approval **footer stays unchanged**
(`Tool 'bash' requests your approval:`); no `render.rs` projection or
`Focus::ToolApproval` changes. "More info during pending approval" is
delivered entirely via the bubble title line.

Acceptance criteria:

- `bash` accepts an optional `label` string; schema and tool description
  document it; existing calls without `label` (or with empty/whitespace-only
  `label`) behave exactly as today.
- When present and non-empty, `label` appears as a dim `label=…` prop after
  the bare `command` primary on the bubble/CLI title line, in all bubble
  states that show `title_arguments` (pending, running, done, denied, error).
- The approval footer, `Focus::ToolApproval`, and `TuiState.pending_approval`
  projection are untouched.
- Sandbox/approval verdicts are unaffected: `needs_approval` still depends
  only on `command` and sandbox rules.
- Tests cover schema, ranking, empty-label suppression, and end-to-end
  `title_arguments` flow; `make format lint` and `make test-coverage` pass;
  affected docs are updated in the same change.

Key constraints:

- Schemas are derived from typed structs via `schemars` with
  `preserve_order` — never hand-written JSON; every property needs a
  doc-comment (enforced by a test).
- `title_arguments()` filters null, non-body arguments and sorts by
  `argument_order` (= schema declaration order); an absent optional
  argument simply drops out of the title.
- `BashCall::argument_order` delegates to `BashTool::default().argument_order`,
  so struct-decl order drives both tool and call ranking automatically.
- Focus state intentionally does not carry tool arguments
  (`Focus::ToolApproval` holds only id + name); arguments live solely in the
  corresponding `ToolBubble` chat entry. This change must not widen focus.
- Docs are part of the change set: `docs/features.md` and
  `docs/implementation-notes.md` must be updated in the same commit;
  markdown linted with `rumdl` (config `.rumdl.toml`).
- Verification gate: `make format lint` and `make test-coverage` (≥80% line
  coverage on testable files; `tui_widgets.rs`, `render.rs`, `main.rs` are
  coverage-excluded).
- Out of scope by decision: approval-footer display, `render.rs`
  third-slot plumbing, `CONTEXT.md` glossary, `docs/ideas/bash-title.md`,
  generalizing labels to other tools.
- No ADRs exist yet (`docs/architecture-design-records/` contains only
  `.gitkeep`), so no recorded decisions constrain this change.

## Implementation Plan

1. [ ] Add an optional `label` field to `BashInputArgs` in
   `src/agent/tools/bash.rs`, declared **after** `command` so it keeps
   schema rank 1 (dim prop; `command` stays rank 0 primary). Use
   `Option<String>` with `#[serde(default)]`. Doc-comment it in the style of
   `SpawnSubagentArgs.label` (e.g. short 3–6 word purpose of the shell
   command, shown on the tool-call title line).
2. [ ] Suppress empty/whitespace-only labels from the title: in
   `BashTool`/`BashCall` argument-display logic, treat `label` values that
   are absent, null, or empty/whitespace like the argument being omitted
   (pattern: `is_default_argument` in `spawn_subagent.rs`). A present
   non-empty label must render as `label=…`.
3. [ ] Update the bash tool description builder (`BashTool::build_description`
   in `src/agent/tools/bash.rs`) to tell the model it may supply a short
   `label` describing the command's purpose and that the label is shown on
   the tool-call title line during approval.
4. [ ] Confirm title-ranking behavior with tests (no renderer code changes
   expected): with `label` present, the title arguments are ordered
   `command` (primary, bare) then `label` (prop); without `label`, ordering
   matches today. Multiline `command` keeps existing renderer behavior
   (sanitized inline + expandable content section) — verify no regression,
   do not modify `src/tui_widgets.rs` for this feature.
5. [ ] Confirm CLI status lines pick up the label automatically via
   `title_arguments` (`src/cli_driver.rs` renders the primary + props); no
   CLI code change expected. Headless `UserApprovalRequired` auto-denial
   behavior for bash stays as-is.
6. [ ] Confirm logging/redaction needs: `BashTool::non_default_redacted_arguments`
   and `BashCall`'s pass-through currently forward all args; the label is
   non-sensitive and should flow through to `log_tool_approval_request` and
   the `tool_args` span unchanged — adjust only if a test or policy demands
   it.
7. [ ] Add/extend tests:
   - `src/agent/tools/bash.rs`: schema includes optional `label`;
     `command` ranks ahead of `label` as primary title argument; non-empty
     `label` appears in `title_arguments()` after `command`;
     absent/empty/whitespace-only `label` is suppressed from
     `title_arguments()`; all properties still have descriptions
     (`bash_input_args_all_properties_have_descriptions` must keep passing);
     description builder mentions the label (follow existing description
     tests at `bash.rs` L544–578).
   - `src/agent/tools.rs` or `src/agent.rs`: `title_arguments` flow
     preserves bash label ordering on `ToolPending` emission (follow
     `tool_call_request_preserves_tool_specific_title_arguments` at
     `agent.rs` ~L3466).
   - Integration (`tests/tools_agent_loop.rs` / `tests/tui_render_state.rs`
     as applicable): a bash call with a label carries it in the pending
     event's `title_arguments`.
   - Follow `docs/known-test-issues.md`: tracing/cwd tests must hold
     `SUBSCRIBER_LOCK` / use `run_locked`; no `cfg(test)` side-effect
     injection from integration tests.
8. [ ] Update documentation in the same change:
   - `docs/features.md` — the `bash` bullet under "Tools to be used by
     models" (optional `label`, displayed as a title-line prop); the CLI
     Runtime status-line paragraph if it describes title rendering; do
     **not** change the "Tool approval sub-mode" footer description (footer
     unchanged).
   - `docs/implementation-notes.md` — the `bash` tool section and any
     title-arguments note (label rides persisted `title_arguments`; rank
     follows schema field order).
   - Do **not** touch `CONTEXT.md` or `docs/ideas/bash-title.md`.
   - Run `rumdl fmt` on touched markdown files.
9. [ ] Run verification: `make format lint` and `make test-coverage`; fix any
   coverage or lint failures before reporting done.

## Implementation Hints

- Schema source of truth: `BashInputArgs` at `src/agent/tools/bash.rs:67–79`;
  only field today is `command`. Declare `label` after `command`.
  `spawn_subagent`'s label doc-comment style: `SpawnSubagentArgs` at
  `src/agent/tools/spawn_subagent.rs:49–67`; its ranking test
  `label_is_the_primary_title_argument_ranked_ahead_of_agent_type` (~L318)
  is the mirror-image of bash's required test (there label is rank 0; here
  rank 1 behind `command`).
- Title ranking: `ToolCall::title_arguments()` at `src/agent/tools.rs:298–311`
  filters non-null, non-body keys and sorts by `Tool::argument_order`
  (default impl derives rank from schemars property order; doc at
  `tools.rs:285–294`). `BashCall::argument_order` at `bash.rs:174–176`
  delegates to `BashTool::default()`, keeping tool and call in sync
  automatically. `schemars`/`serde_json` `preserve_order` features enabled
  in `Cargo.toml` (L22, L26).
- Empty-label suppression pattern: `is_default_argument` at
  `spawn_subagent.rs:154–156` (hides default `agent_type`); also see
  `write.rs:329` and `read.rs:407`. Default `Tool::is_body_argument` returns
  `false` (`tools.rs:275–277`); do not mark `label` or `command` as body
  args.
- Renderer behavior (read-only for this change): `append_tool_bubble_lines`
  at `src/tui_widgets.rs:1582–1831` — primary = first scalar title arg, bare
  bold (L1687–1700); multiline scalars sanitized inline with `↵` plus an
  expandable content section (L1662–1686); remaining args as `key=value`
  props (L1701–1714); collapsed title
  `{prefix} {primary_arg} {props} [→ {summary}]` (L1827). CLI counterpart:
  `render_invocation_from_title_arguments` at `cli_driver.rs:60–64`, status
  line ~L649–671; headless bash `UserApprovalRequired` auto-denial at
  L709–729.
- Approval flow (unchanged, for context): `agent.rs::advance_tool_queue`
  L2953–2966 emits `UiUpdate::ToolPending { …, title_arguments, … }` from
  `front.call.title_arguments()`; footer built in `draw_approval_footer` at
  `src/tui_widgets.rs:3058–3155` (title text L3069–3081 — stays
  tool-name-only); `Focus::ToolApproval` at
  `src/tui_reducer_state.rs:881–890` (comment L887–888: arguments
  intentionally not in focus); `TuiState.pending_approval` third slot
  hard-coded `Value::Null` in `src/render.rs:58–66` — leave all of these
  untouched.
- Description builder: `BashTool::build_description` at `bash.rs:230–243`
  (timeout already interpolated; description-content tests at L544–578).
  Schema exposure: `input_schema()` at `bash.rs:288–290`. Registry wiring
  `src/agent/tools.rs:724–726` needs no change for a new optional field.
- `needs_approval`: `bash.rs:298–307` reads only `command` via
  `sandbox_context.check_command`; a label must not alter this.
- Persistence: `title_arguments` captured on `ToolCallRequest`
  (`agent.rs:446–461`, capture ~L1468–1475) and preserved across reducer
  transitions (`tui_reducer_state.rs:2963–2980`) — the label survives
  resume automatically; no persistence code change needed.
- Doc-comment enforcement: test `bash_input_args_all_properties_have_descriptions`
  at `bash.rs:792` fails if the new field lacks a doc comment.
- Existing bash tests live in `bash.rs` `mod tests` (L512–809); registry
  title tests at `tools.rs:944` (`registered_tool_titles_omit_unknown_arguments`)
  and `tools.rs:1380` (`tool_call_default_pending_sections_and_argument_order`).
- Docs to update: `docs/features.md` L279–293 (`bash` bullet), L72–79 (CLI
  Runtime approval/status lines) if wording covers titles;
  `docs/implementation-notes.md` L357–376 (`bash tool process-group cleanup`
  section — add schema/label note only if natural) and the
  Store/title-arguments note at L461. Explicitly leave untouched:
  `CONTEXT.md`, `docs/ideas/bash-title.md`, footer paragraphs in
  `features.md` (L454–459), `docs/keybindings.md`.
- Coverage/verification: `docs/development-workflow.md` L3–4, L16–18 (≥80%
  threshold; `tui_widgets.rs`, `render.rs`, `main.rs` excluded);
  `docs/known-test-issues.md` for tracing (`SUBSCRIBER_LOCK`/`run_locked`)
  and cwd isolation rules.
- `docs/plans/` currently holds only `.gitkeep`; this file is the first plan
  there.
