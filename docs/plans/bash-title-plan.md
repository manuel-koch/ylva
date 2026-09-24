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

1. [x] Add an optional `label` field to `BashInputArgs` in
   `src/agent/tools/bash.rs`, declared **after** `command` so it keeps
   schema rank 1 (dim prop; `command` stays rank 0 primary). Use
   `Option<String>` with `#[serde(default)]`. Doc-comment it in the style of
   `SpawnSubagentArgs.label` (e.g. short 3–6 word purpose of the shell
   command, shown on the tool-call title line).
2. [x] Suppress empty/whitespace-only labels from the title: in
   `BashTool`/`BashCall` argument-display logic, treat `label` values that
   are absent, null, or empty/whitespace like the argument being omitted
   (pattern: `is_default_argument` in `spawn_subagent.rs`). A present
   non-empty label must render as `label=…`.
3. [x] Update the bash tool description builder (`BashTool::build_description`
   in `src/agent/tools/bash.rs`) to tell the model it may supply a short
   `label` describing the command's purpose and that the label is shown on
   the tool-call title line during approval.
4. [x] Confirm title-ranking behavior with tests (no renderer code changes
   expected): with `label` present, the title arguments are ordered
   `command` (primary, bare) then `label` (prop); without `label`, ordering
   matches today. Multiline `command` keeps existing renderer behavior
   (sanitized inline + expandable content section) — verify no regression,
   do not modify `src/tui_widgets.rs` for this feature.
5. [x] Confirm CLI status lines pick up the label automatically via
   `title_arguments` (`src/cli_driver.rs` renders the primary + props); no
   CLI code change expected. Headless `UserApprovalRequired` auto-denial
   behavior for bash stays as-is.
6. [x] Confirm logging/redaction needs: `BashTool::non_default_redacted_arguments`
   and `BashCall`'s pass-through currently forward all args; the label is
   non-sensitive and should flow through to `log_tool_approval_request` and
   the `tool_args` span unchanged — adjust only if a test or policy demands
   it.
7. [x] Add/extend tests:
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
8. [x] Update documentation in the same change:
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
9. [x] Run verification: `make format lint` and `make test-coverage`; fix any
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

## Follow-up: expanded-body Label/Command sections (all states)

Second phase, decided after the title-prop work: show the `label` as the
first content section and the `command` as the second in the **expanded**
bash tool bubble, in **all** bubble states, and hide the expanded args line
when its content is already carried by those sections. The collapsed title
line, approval footer, `Focus::ToolApproval`, and `needs_approval` stay
unchanged.

Settled decisions (grilling round):

- Sections appear in Pending, Running, Denied, Error, and Done. Error
  merges the pending sections with the payload sections (agent-side, so
  persistence and replay match the live bubble); Done prepends them to the
  result's output section.
- The expanded args line is hidden when every title argument is covered by
  a content-section label (data-driven, not a `name == "bash"` check);
  collapsed titles keep `command` + `label=…`. Old persisted bubbles
  without the new sections fall back to today's args-line display.
- A small renderer guard drops a multiline-title extra section whose label
  already exists among the state sections (prevents a duplicate `command`
  body and keeps `Label` before `Command`).
- The generic Error-section merge applies to all tools (wider blast
  radius, accepted): any `ToolError` that replaces a Pending/Running
  bubble carries that call's `pending_content_sections()` ahead of the
  payload sections.

Acceptance criteria:

- Expanded bash body order: `Label` → `Command` → `Result` (command
  output) / `Partial result`, in every state; headers render when >1
  section (existing rule). See also *Follow-up 2* below, which labels the
  output section `Result`.
- Blank/absent `label` → no `Label` section; empty `command` → no
  `Command` section.
- Multiline command renders exactly one `Command` body (dedup guard).
- Expanded bash args line hidden when covered; collapsed title unchanged;
  `edit`/`write`/`spawn_subagent` expanded args lines unchanged.
- CLI picks the sections up automatically (pending + done); headless
  auto-denial and CLI error lines unchanged; no CLI code change.
- Tests cover builder order/suppression, Done wrapper, Error merge,
  renderer dedup + coverage gate (TUI buffer assertions), and the
  end-to-end `ToolPending`/`ToolDone`/`ToolError` section flow; affected
  docs updated in the same change; `make format lint` and
  `make test-coverage` pass.

## Follow-up Implementation Plan

1. [x] Add a section builder (SSOT) in `src/agent/tools/bash.rs` producing
   `[Label?, Command]` from `(command, label)` — blank label suppressed,
   empty command skipped — and implement `BashCall::pending_content_sections`
   on top of it (reads the raw arguments, no schema validation
   dependency). Unit tests: order, blank/absent label, empty command.
2. [x] Done path: store `label` + `command` on `BashResult` (populated by
   `run_bash`) and prepend the builder's sections in
   `BashResult::content_sections()` ahead of the existing output section.
   Existing direct `BashResult` test literals gain the new fields with
   empty/None values so their assertions stay valid. Unit tests for the
   prepended order.
3. [x] Error path (agent-side merge, generic): capture
   `pending_content_sections()` before `call.run()` in
   `PendingTool::execute` and emit `pending ++ payload` from both failure
   arms (inner `Err` appends the `Partial result` section after the pending
   ones; outer timeout emits the pending sections). Apply the same merge
   at `DenyTool`, `AutoDenied` (pass the already-computed sections into
   `push_tool_error` instead of `vec![]`), the Stop/abort-while-pending
   loops, and the spawn no-manager fallback. Reducer/presenter stay
   untouched. Integration test: Stop-while-pending bash →
   `ToolError.content_sections == [Label, Command]`; approve flow →
   `ToolDone.content_sections` starts `[Label, Command]`.
4. [x] Renderer dedup guard in `append_tool_bubble_lines`
   (`src/tui_widgets.rs`, state-extends-extras site): drop an extra
   multiline-title section whose label case-insensitively matches an
   existing state content-section label. Buffer test in
   `tests/tui_render_state.rs`: multiline command + label → one command
   body, `Label` before `Command`.
5. [x] Renderer coverage gate (expanded branch only): skip the expanded
   args rows when `!content_sections.is_empty()` and every title key is
   covered by a content-section label (case-insensitive). Do not change
   `has_args` (expand indicator + summary arrow). Tests: expanded bash
   hides `label=`/args row and orders `Label:` before `Command:`;
   collapsed bash keeps `command`/`label` in the title; old-shape
   sections fall back to the args line; existing `edit` multiline args
   test keeps passing (non-regression).
6. [x] Confirm CLI (read-only): `ToolDone` already renders
   `content_sections` (gains `--- Label ---`/`--- Command ---`); CLI
   `ToolError` prints only the error line (unchanged). No CLI code unless
   a test fails; headless auto-denial stays as-is.
7. [x] Audit and fix tests affected by the generic merge: assertions on
   `ToolError`/`Error` section payloads (e.g. `agent.rs` empty-sections
   expectation, `tui_reducer_state.rs` section-count), the presenter
   live-vs-replayed bash bubble test, `tests/cli_driver.rs` done/pending
   rendering, bash result-section tests.
8. [x] Update documentation in the same change:
   - `docs/features.md` — bash bullet: sections in all states, expanded
     args line hidden when covered, collapsed/CLI title prop unchanged.
   - `docs/implementation-notes.md` — bash section (builder + `BashResult`
     fields), renderer notes (dedup guard + coverage gate), agent
     `ToolError` section merge; check `docs/persistence.md` for staleness.
   - Do **not** touch `CONTEXT.md`, `docs/ideas/bash-title.md`, footer
     paragraphs, `docs/keybindings.md`.
   - Run `rumdl fmt` on touched markdown files.
9. [x] Run verification: `make format lint` and `make test-coverage`; fix
   any coverage or lint failures before reporting done.

## Follow-up Implementation Hints

- Section builder call sites: `BashCall::pending_content_sections` (from
  `raw_arguments` Value) and `BashResult::content_sections` (from stored
  fields) — one helper, no duplicated suppression logic.
- `BashResult` field literals: `bash.rs` tests construct it directly;
  add `label: None` / `command: String::new()` there so
  `bash_result_content_section_*` assertions keep passing (empty command
  → builder yields no `Command` section).
- Error-merge emission sites (all have the `PendingTool` call available):
  `PendingTool::execute` capture before `run` (`agent.rs` ~L854),
  inner `Err` ~L930 (append `Partial result` after pending sections),
  outer timeout ~L962, `DenyTool` ~L2038 (both `push_tool_error` and
  `UiUpdate::ToolError`), `AutoDenied` ~L2893 (sections already computed
  ~L2891), Stop/abort loops ~L2304/L2313, ~L2332, ~L2359/L2369, spawn
  no-manager fallback ~L2602 (sections computed ~L2543 — clone before the
  `ToolStart` send moves them).
- Renderer: `args_after_prefix` (~L1842) is shared collapsed/expanded —
  gate only the expanded args-row emission (~L1990–2058), never
  `has_args` (feeds the expand indicator ~L1737 and summary arrow
  ~L1863/L1874). Multiline extras are stashed ~L1676 and appended ~L1735
  — dedup at the append. Label headers render only when >1 section
  (`show_labels` ~L2082).
- Renderer non-regression anchors: `edit` expanded args line (existing
  `expanded_tool_bubble_with_multiline_title_argument…` test), spawn
  (`Task`/`Briefing` labels don't cover `label`/`agent_type` keys), write
  (pending section label `None` doesn't cover `path`).
- Coverage rule is data-driven (`Content`-variant labels only); `Diff`
  labels are a path and never cover title keys of `edit`.
- CLI: `render_content_sections` at `cli_driver.rs:136` (pending ~L686+
  and done ~L838); CLI `ToolError` arm ~L847 prints no sections.
- Persistence: merged sections reach `Message::ToolError` via
  `push_tool_error` and `Message::ToolResult` via the result payload, so
  replayed bubbles match live ones without reducer changes.

## Follow-up 2: `Result:` header for the bash output section

Amends the follow-up above (whose Done row called the output section
headerless): the completed bash output section now carries a `Result`
label, so the expanded Done bubble renders an underlined `Result:` header
after `Label:`/`Command:` and before the output — matching
`spawn_subagent`'s `Result`/`Error`/`Sources` header vocabulary. The
renderer's existing `show_labels` rule (>1 section) still hides the header
in the degenerate single-section case; the CLI prints `--- Result ---`
before bash done output. Error state keeps the agent-built
`partial output` label (renamed to `Partial result` in *Follow-up 3*).

Acceptance criteria:

- Expanded Done bubble renders `Label:` → `Command:` → `Result:` in that
  order; covered args line stays hidden; collapsed title unchanged.
- CLI done output gains the `--- Result ---` header automatically; no CLI
  code change.
- Tests updated/added for the `Result` label (unit, integration,
  presenter roundtrip fixture, TUI buffer); docs reworded; gates pass.

## Follow-up 2 Implementation Plan

1. [x] Label the output section `Result` in `BashResult::content_sections`
   (`src/agent/tools/bash.rs`); update the unit, integration, and
   presenter roundtrip fixtures that asserted an unlabeled output section;
   add a TUI buffer test asserting the expanded Done bubble renders
   `Label:` → `Command:` → `Result:` in order with the covered args line
   still hidden.
2. [x] Update `docs/features.md` and `docs/implementation-notes.md`
   wording (`Result` section/header); `rumdl fmt` on touched markdown.
3. [x] Run verification: `make format lint` and `make test-coverage`; fix
   any coverage or lint failures before reporting done.

## Follow-up 3: `Partial result` vocabulary

Amends *Follow-up 2*: the output section is labeled by lifecycle, not just
success. Two cases show an incomplete result and share one header:

- A failure carrying partial output (today: bash's internal timeout
  draining captured lines) — the agent-built section previously labeled
  `partial output` is renamed to `Partial result` (matching the
  capitalized `Label`/`Command`/`Result` header vocabulary).
- A successful call whose captured output hit the line/byte cap
  (`BashResult::truncated`) — its output section is labeled
  `Partial result` instead of `Result`. Non-truncated successes keep
  `Result`. The `truncated=true` title summary prop stays as-is.

The error-arm label is tool-agnostic (`PendingTool::execute` renders it
for any `ToolError` with non-empty partial output); only bash produces
that today.

Acceptance criteria:

- Timeout/failure bubbles render `Label:` → `Command:` → `Partial
  result:`; truncated Done bubbles render `… Command:` → `Partial
  result:`; non-truncated Done keeps `Result:`.
- CLI prints `--- Partial result ---` automatically; no CLI code change.
- Tests cover both label branches; docs and prior plan wording updated;
  gates pass.

## Follow-up 3 Implementation Plan

1. [x] Rename the agent error-arm section label `partial output` →
   `Partial result` (`src/agent.rs` `PendingTool::execute` inner `Err`
   arm).
2. [x] Label the bash output section by truncation: `Partial result` when
   `BashResult::truncated`, else `Result` (`src/agent/tools/bash.rs`).
   Unit tests for both branches; existing integration/Done assertions
   (non-truncated → `Result`) keep passing.
3. [x] Update documentation: `docs/features.md` bash bullet and
   `docs/implementation-notes.md` bash section (header vocabulary, both
   `Partial result` cases); amend stale `partial output`/`Result`-header
   wording in this plan; `rumdl fmt` on touched markdown files.
4. [x] Run verification: `make format lint` and `make test-coverage`; fix
   any coverage or lint failures before reporting done.

## Follow-up 4: syntax-highlight the expanded `Command` section

The renderer's content-section path already selects a syntect grammar
from `ContentSection::file_extension` (`highlighted_lines_cached` falls
back to first-line detection/plain text for `None`). The bash `Command`
section currently passes `None`, so commands render unhighlighted.

Set `file_extension: "sh"` on the `Command` section built by
`label_and_command_display_sections` (covers both the pending body and
the completed result through the shared builder). `Label` (short prose)
and `Result`/`Partial result` (arbitrary program output) stay plain. The
collapsed/inline title rendering is unchanged (region styling only); the
CLI ignores `file_extension` (plain text output).

Acceptance criteria:

- Expanded `Command` body renders with shell syntax coloring under the
  active theme; `Label`/`Result` sections stay plain.
- Unit tests assert the `Command` section carries `Some("sh")` on both
  the pending and result paths; a TUI buffer test asserts multiple
  foreground colors on the command row (comment token vs default).
- Docs note the extension hint; gates pass.

## Follow-up 4 Implementation Plan

1. [x] Set `file_extension: Some("sh")` on the `Command` section in
   `label_and_command_display_sections` (`src/agent/tools/bash.rs`);
   extend the pending/result unit assertions to pin `Some("sh")` for
   `Command` and `None` for `Label`.
2. [x] Add a TUI buffer test rendering an expanded pending bubble whose
   command contains a trailing `#` comment; assert the command content
   row carries at least two distinct foreground colors among its
   non-space content cells (syntect grammar active vs plain single-color).
3. [x] Update documentation: `docs/implementation-notes.md` bash section
   (extension hint → syntect); `rumdl fmt` on touched markdown files.
4. [x] Run verification: `make format lint` and `make test-coverage`; fix
   any coverage or lint failures before reporting done.
