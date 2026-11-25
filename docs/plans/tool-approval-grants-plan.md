# Reusable tool-approval grants

## Context

Ylva (Rust TUI agent harness, `[lib]` + `[[bin]]` crate `ylva`) asks the user to approve every tool
call whose sandbox verdict is `ApprovalCheck::UserApprovalRequired`. This change adds **grants**:
user-owned, reusable approvals for *similar* future calls, so repeated prompts shrink without
touching sandbox rules or acting as an OS isolation boundary. The feature is standalone (no
dependency on self-improvement, skills, `clarify`, or the approval-verdict-trail idea).

Goal and behavior summary:

- Approval dialog keeps approve-once and deny (default behavior unchanged) and adds explicit
  reusable choices: **temporary grant** (in-memory, global to the running root conversation and
  all its agents/subagents, no per-agent scoping) and **persistent grant** (profile + workspace
  scoped, applies to all agent types/roles, also in headless runs).
- Grants only ever satisfy an *ordinary* `UserApprovalRequired` verdict. They never override:
  schema errors, effective `AutoDenied`, disabled/unregistered tools, private-address checks,
  other execution checks, or mandatory-interaction tools (`clarify`, a future repeated-call
  loop safeguard).
- Evaluation order: validate args → existing tool policy → (`AutoApproved` proceeds) →
  `UserApprovalRequired` → grant match → otherwise prompt (headless: deny as today).
- Persistent grants live in `<workspace-root>/.ylva/profiles/<profile>/grants.toml`; the file is
  covered by workspace trust (ADR 0003), loaded only from the workspace, never from the home
  profile. A grants-only profile directory is valid.
- New flag `--no-workspace-grants` (TUI + headless) disables persistent grants for the whole
  invocation (including subagents); it does not bypass workspace trust.
- No grant-management TUI; temporary grants expire on root session switch, `/clear`, exit and are
  not restored on resume/clone; persistent grants are hand-editable and reloaded next launch.
- Observability: structured logs record decision source (temporary/persistent grant) and a
  non-sensitive grant reference (no raw payloads or sensitive literals), keeping the preceding
  verdict as its parent. No tool-bubble/transcript indicator: it could not be replayed on a
  resumed conversation (deliberate deviation from the idea doc).

Per-tool matching (grant "subject"):

| Tool | Stored matcher | Matches |
| --- | --- | --- |
| `read`/`write`/`edit` | `rule` path glob | resolved file path only (not offsets, content, patches); each tool separate unless user adds it |
| `search` | `rule` path glob | search root only (not expression/filters) |
| `glob` | `rule` path glob | traversal root derived by existing tool, not pattern text |
| `load_skill` | `name` + `rule` (source path glob/literal) | skill name AND resolved instruction-source path |
| `bash` | `rule` string: literal program + literal args, optional trailing `*` (= one or more further args) | each *simple command* separately |
| `web_fetch` | `rule` = scheme+host+effective port | initial URL origin only |
| `web_search` | none | any search terms |
| `spawn_subagent` | none | any target agent type/task/briefing |

Path-rule semantics: reuse the sandbox custom-glob matcher (`path_glob_matches`: component-aware
`*`/`?`/`[...]`, whole-component `**`, dot-component rules). Bare `**` is an *invalid* grant rule
(`**/*` is valid). Generated exact rules escape glob metacharacters. Workspace-relative rules
(`./...`) for in-workspace targets, absolute for outside; relative rules resolve against the
workspace root (not launch dir) and match only while the resolved target stays inside the
workspace. No `~`/env expansion in stored rules; no inode/symlink identity system.

Bash semantics (full detail, since the idea doc is not available to the implementer):

- Grant table: `foo` matches only `foo`; `foo bar` only `foo bar`; `foo bar *` matches
  `foo bar -a`, `foo bar -a -b` but not `foo bar` or `foo baz`; `foo -a *` matches `foo -a file`
  but not `foo -a`. Whole-argument boundaries (`foo bar *` ≠ `foo barcode`). Trailing `*` is the
  only wildcard; `**` token invalid; quoting preserved so a quoted literal `*` differs from the
  suffix. Differs deliberately from configured Bash rules (there trailing `*` = exactly one arg,
  `**` = one or more) — preview and docs must say so.
- A grant never satisfies `unquoted_glob`/`unquoted_brace_expansion` verdicts (`git log *` does
  not approve `git log *.md`; `git log -- '*.md'` is covered). Outside the trailing suffix a
  matcher may not contain unquoted glob/brace characters.
- Split every call into simple commands with the existing extractor; judge each separately; a
  grant never covers a pipeline/redirect/list as a whole.
- Compound-call dialog: one row per analysable simple command not already covered (policy
  `AutoApproved` or existing grant → read-only/collapsed "N already allowed"). Each row: editable
  single-line matcher + selector `off | temporary | persistent` (default `off`). Every command must
  be covered by policy, grant, or selected row; one effective `auto_denied` command denies the
  whole call before any dialog. Unanalysable constructs (subshells, heredocs, substitutions,
  control flow, non-harmless redirects) get no row; show footnote "Other parts of this call can
  only be approved once."; if nothing is analysable show no checklist/footnote. Confirm = "run
  this call and save selected grants"; if every uncovered command has a selected row the call runs
  without a separate approve-once, else it runs once for the remainder. Deny saves nothing. A
  failed persistent save leaves the approval pending.
- Matcher proposal: leading tokens verbatim, flags included, capped at 4 tokens; stop at first
  path-like, quoted, or glob/expansion token; append trailing `*` only when tokens were dropped;
  short all-literal commands propose an exact matcher. One line, truncated with `…`; newline
  rejected.
- Program token matched literally (`make foo` ≠ `/usr/bin/make foo`); no PATH resolution;
  environment and working directory not matched (preview warns; `cd`, `pushd`, `export`,
  `source` non-grantable).
- Static in-code non-grantable table: interpreters with inline-code flags (`python`/`python3 -c`,
  `node -e|-p`, `perl`/`ruby -e`, `php -r`, `sh`/`bash`/`zsh -c`); interpreter+file rows are
  grantable but marked wider; wrappers (`env`, `xargs`, `sudo`, `nohup`, `time`, `timeout`,
  `find -exec`, `watch`) not unwrapped; destructive programs (`rm`, `dd`, `mkfs`, `chmod`,
  `chown`, `mv`) with trailing `*` (exact-argument grants allowed); rows with env-assignment
  prefixes or `$VAR`/`~` in matched tokens.
- Editing rules: edited matcher must parse as literal program + literal args + optional trailing
  `*`; must match the row's actual command; program token unchanged (no bare `*`); non-grantable
  table re-applied; narrowing/widening within same program allowed (preview marks wider);
  `make foo` → `make foo *` rejected (needs ≥1 extra arg), widen as `make *`. Edit changes only
  the stored grant, never the call now running.

Web/subagent semantics: `web_fetch` grant for `https://docs.example:443` also matches
`https://docs.example/a`, not `http://…`, another host, or port `8443`; redirects can reach other
public origins (preview must say so; deny/private-address checks still apply to hops).
`web_search` grant survives backend change (subject to effective policy). `spawn_subagent` grant
does not change availability, concurrency, depth, agent selection, nor grant the spawned agent
tool permissions.

Scope and runtime:

- Temporary: one shared runtime state across root and subagent loops, global (no requester or
  agent scoping; also covers later subagents); cleared on session switch, `/clear`, exit; not
  restored on resume/clone.
- Persistent: active profile + workspace; each agent's own effective policy still evaluated first;
  an agent-specific approval-required rule can be satisfied by a grant; an effective denial
  cannot. Grants are separate from configured rule lists (not replaced by agent-specific lists).
- Grant changes re-evaluate not-yet-executed calls, including calls waiting in another agent's
  approval queue; recheck effective policy before dispatch; never revive a
  cancelled call or interrupt a running one; no stale prompts; no leakage across root conversations.
- Preview before confirmation: exact matcher, affected tools, scope (stating it applies to all
  agents), consequences;
  persistent also shows file to be written, resolved base/scope for path matchers, any workspace
  marker to be created; warns that matching calls already waiting may proceed; file-mutation
  grants warn they permit **any** later content change at that path; Bash wildcards shown as a
  wider command scope; literal paths/hosts/prefixes may be sensitive — show and warn.
- Defaults/UX: default to the requesting tool and its specific resource; user may widen the
  resource, add compatible tools (one `[[grants]]` entry per tool); user may edit the
  rule, invalid or non-matching rule rejected with an explanation; scope chosen per grant (per
  row for Bash, default temporary). Offer reusable grants only when the call has a supported
  matcher; otherwise only existing one-time decisions.
- Apply a confirmed temporary grant immediately; apply a persistent grant only after successful
  save; on failed/conflicting save leave the approval pending (do not silently fall back to
  once/temporary); if publication may have happened before an error, reconcile and report
  uncertainty.

Persistence/trust rules:

- `grants.toml` can be the only workspace profile file; it must not create/shadow `config.toml`;
  explicit profile selection still requires a real config in a workspace or home layer; reuse
  profile-name validation; creating a grant store is not profile creation. Missing file = no
  persistent grants (no home fallback).
- `ylva profile list` labels a workspace profile dir with grants but no `config.toml`/`.env` as
  grants-only (no "shadows home" claim); do not create a workspace shim for it; explicit workspace
  profile deletion removes the directory incl. `grants.toml` — output (incl. dry-run) and docs say
  so.
- Creating `.ylva/` creates a workspace marker (changes discovery/layering in unmarked dirs).
  If no marker was discovered, offer to create `.ylva/` in the launch directory as part of saving;
  show proposed root and warn. Create no profile config. Apply the saved grant in the current run.
  If the proposed `.ylva/` is the Ylva home (including via canonical alias), refuse persistent
  save, explain, keep once/temporary available. If save fails after marker creation, report
  whether the marker remains and leave approval pending.
- Trust: include the selected profile's `grants.toml` in the covered content before loading, in
  TUI and headless startup; works when it is the only config file; fingerprinting works for
  malformed contents; untrusted/changed file cannot authorize calls; a confirmed grant takes
  effect in the current run after save but must not silently re-trust unrelated workspace
  configuration — next launch requires `ylva trust` (incl. `--profile`). A profile-specific
  grants change invalidates only that profile's trust.
- `--no-workspace-grants`: after the normal trust check, skip loading/validating grant entries
  (even if malformed); trust still covers the file (untrusted/changed blocks startup; file access
  errors remain errors); persistent-grant creation unavailable in the dialog with explanation;
  one-time and temporary remain; not propagated to child Ylva processes.
- Saving: show exact entry + target file; warn the file may show up in Git status and contains
  sensitive literals (do not touch ignore rules/Git config/index); preserve unrelated
  entries/comments/permissions; create new files owner-only (keep existing permissions); serialize
  Ylva's saves; compare current file with loaded/last-saved snapshot and report external edits;
  publish atomically via existing safe-write practice (`private_fs::replace_file`).
- Format: TOML, `[[grants]]` entries (`tool`, `rule`, and `name` for `load_skill`); no version
  field; reject malformed files, unknown fields, unknown tool names, invalid matchers with a clear
  error (when grants enabled); missing/invalid fields must never broaden scope; store only
  matching attributes.

Acceptance criteria:

1. One-time approve/deny behave unchanged; effective denial, invalid args, disabled tools,
   private-address checks, CLI auto-approve flags, and rule-list precedence unchanged.
2. Temporary grants cover root and subagent calls, incl. already-waiting calls; session switch,
   `/clear`, exit clear them.
3. Persistent grants match workspace+profile+tool+resource for any agent type/role incl. headless;
   manual edits take effect after restart only.
4. `--no-workspace-grants` semantics above hold; trust still covers the file.
5. Tests cover: file vs directory scope; read ranges/write contents irrelevance; search/glob roots;
   skill name+source; `*` vs `**`; hidden paths; root-relative paths from nested launch dir; moved
   workspaces; literal glob/expansion chars; absolute paths; escaping relative targets incl.
   symlinks; bare `**` rejection; Bash exact/suffix/one-or-more/argument boundary/quoted
   wildcard/unquoted_glob carve-out; compound-call row semantics, footnote, confirm behavior,
   `auto_denied` whole-call denial; non-grantable table; literal program matching; edited-matcher
   validation; unrepresentable commands stay approval-required.
6. `make format lint` and `make test-coverage` pass (coverage threshold maintained); `make audit`
   only if dependencies change; `rumdl` clean on touched Markdown.

## Implementation Plan

Implementing agent: flip `[ ]` to `[x]` immediately after completing each step and rewrite this
file in place, so progress is resumable. Work TDD, in order; every committed change bundles
implementation, tests and its documentation. Consent rules in `AGENTS.md` apply (no commits
without user consent).

1. [x] Re-read the grilling decisions in Implementation Hints and confirm none are stale.
2. [x] Check ADRs: confirm the trust extension is consistent with
   `docs/architecture-design-records/0003-trust-workspace-configuration-by-content-hash.md`;
   write a new ADR (use the `architecture-decision-record` skill) for "grants are separate from
   sandbox rules and never override effective denials / mandatory interaction", recording the
   grants-only profile directory and trust coverage decision.
3. [x] Add a `grants` module (`src/grants.rs` + `src/grants/*.rs`, file-per-module, license header,
   re-export through `src/lib.rs`) with the domain types: `GrantTool`, `GrantRule` (per-tool
   matcher), `Grant` (+ scope), `GrantReference` (non-sensitive id for logs).
4. [x] TDD path matcher: grant-rule validation (reject bare `**`, newline, `~`/env use as
   expansion), exact-rule generation with metacharacter escaping, workspace-relative vs absolute
   form, matching via `sandbox::rules::path_glob_matches` (expose as `pub(crate)` if needed),
   root-relative resolution, escape rejection incl. symlinks. Cover `read`/`write`/`edit`/
   `search`/`glob`/`load_skill`.
5. [x] TDD Bash matcher: parser for grant strings (literal program/args, trailing `*`, quoting
   preserved, `**` rejected), matching against extractor output, `unquoted_glob`/
   `unquoted_brace_expansion` carve-out, non-grantable static table, matcher proposal (4-token cap,
   stop tokens), edited-matcher validation (program unchanged, matches row, no newline,
   re-apply table, suffix-must-match rule), "wider" classification.
6. [x] TDD web/spawn matchers: `web_fetch` origin (scheme, host, effective port), `web_search` and
   `spawn_subagent` tool-only grants.
7. [x] Add a tool-facing hook (e.g. `Tool::grant_subject(arguments) -> GrantSubjects` in
   `src/agent/tools.rs`, default "none") and implement it per tool in `src/agent/tools/{read,
   write,edit,search,glob,bash,web_fetch,web_search,spawn_subagent}.rs` and the skill tool, reusing
   each tool's existing path resolution so grants judge the same value as policy. For Bash return
   one subject per simple command with per-command verdicts (reuse `sandbox/command.rs`
   `check_command` and `bash_command_extractor`); mark `clarify` as mandatory-interaction.
8. [x] Introduce a distinction between ordinary approval-required and mandatory interaction
   (`clarify` today) via a `Tool::requires_human_interaction()` hook so the evaluator never treats
   the latter as grantable; no new `ApprovalCheck` variant.
9. [x] Persistent store: `grants.toml` serde model (`deny_unknown_fields`), load + validate with
   clear errors, serialize preserving unrelated entries/comments (use `toml_edit`-style
   `toml_edit`, already a dependency), snapshot compare
   for external edits, serialized saves, atomic owner-only publish via
   `private_fs::replace_file`/`create_private_dir_all`, marker-creation + Ylva-home refusal logic,
   failure reporting incl. uncertain publication.
10. [x] Extend workspace trust (`src/config/workspace_trust.rs::covered_files`) to include the
        selected profile's `grants.toml`; verify malformed content still fingerprints
        (`normalized_contents` fallback); verify grants-only workspace works; update
        `workspace_trust_notice` text if needed. Ensure a save that adds a grant does not re-record
        trust.
11. [x] Profile layering: allow a workspace profile dir containing only `grants.toml` without
        shadowing the home profile or creating a shim; `ylva profile list` labels it grants-only;
        workspace profile delete (incl. dry-run) reports `grants.toml` removal
        (`src/profile_command.rs`, `src/profile_shim.rs`, `src/config/layered.rs`).
12. [x] CLI: add `--no-workspace-grants` in `src/cli.rs`; thread it through startup to grant
    loading (skip load/validate after trust check) and to the dialog (disable persistent option
    with explanation).
13. [x] Runtime state: shared grant state (`Arc`) created with the root and handed to subagent
    loops (`src/agent_factory.rs`, `src/agent/subagent_manager.rs`), holding temporary grants
    plus a generation counter bumped on every grant change; clear on session switch, `/clear`,
    exit; not persisted/restored on resume/clone.
14. [x] Evaluator: in `src/agent/tool_queue.rs` (after `needs_approval`, before emitting
        `ToolPending`) and in the headless path (`src/cli_driver/run.rs`) consult grants for
        `UserApprovalRequired` only; all simple commands of a Bash call must be covered by
        policy/grants; auto-approved-by-grant calls take the same path as `AutoApproved` (including
        the `spawn_subagent` branch); no UI marker; structured log with decision source, grant
        reference, parent verdict (no raw payloads).
15. [x] Re-evaluation: when grants change, each agent loop in `ToolPending` observes the generation
    counter change and re-runs its queue check (no new command routing), re-verifies policy before
    dispatch, remove stale approval prompts, never revive cancelled or interrupt running calls.
16. [x] TUI approval dialog (`src/presenter/keyboard.rs`, `mouse.rs`, `domain.rs`,
        `src/tui_reducer_state/*`, `src/tui_render_state.rs`, `src/tui_widgets.rs`,
        `src/events.rs`): keep Approve/Deny and add a "Grant…" button opening a second modal (rule
        editor, tool checklist, scope, preview with all required warnings, Confirm/Back); Bash
        shows per-row checklist with selector and footnote, "N already allowed"; validation errors,
        save failure/conflict behavior (refuse overwrite on external edit, leave approval pending,
        offer once/temporary/retry-with-reload), marker-creation offer. New `Effect`s for save +
        apply grants. Update keybindings. *Delivered as a minimal modal:* one always-visible
        *Grant…* button opens a second page with the read-only proposed rules, the scope choice,
        notes (the warnings above, marker creation included) and Grant/Back. Not delivered: rule
        editing, tool checklist, per-row selector and the once/temporary/retry offer after a save
        failure (the prompt just stays pending with a notice).
17. [x] Integration tests in `tests/` (grant-file trust, headless with persistent grants, flag
    behavior, subagent coverage) following `docs/development-workflow.md` and
    `docs/known-test-issues.md` (tracing/cwd caveats); keep the coverage threshold.
18. [x] Documentation (same commit as the code): `docs/features.md` (feature),
        `docs/configuration.md` (grants section next to *Sandbox (tool-approval rules)*,
        *Workspace trust*, *Managing profiles and shims*; Bash `*` semantic difference),
        `docs/cli-options.md` (`--no-workspace-grants`, profile list/delete output, trust note),
        `docs/persistence.md` (`grants.toml` layout, saving contract), `docs/keybindings.md` (dialog
        keys), `docs/implementation-notes.md` (evaluator placement, shared state, re-evaluation;
        re-read the guideline at its top first), `CONTEXT.md` (terms: Grant, Temporary grant,
        Persistent grant), new ADR from step 2, and mark `docs/ideas/tool-approval-grants.md` as
        implemented/point to the feature docs (ask before removing). Run `rumdl fmt` + `rumdl check`
        on touched Markdown.
19. [x] Verify: `make format lint`, `make test-coverage`; `make audit` if `Cargo.toml`/`Cargo.lock`
    changed.
20. [x] Grant dialog: tool checklist (grant for a subset of the proposed tools) and, for Bash,
    a per-row selector with footnote and "N already allowed" (extends the minimal modal of step 16;
    update `docs/features.md`, `docs/keybindings.md`). *Delivered:* a per-row selector
    (`off | temporary | persistent`) for every tool; adding grants for other tools than the
    requesting one is not offered.
21. [x] Grant dialog: rule editing (edit a proposed rule in place, validate with the rule parsers,
    show validation errors, keep Grant disabled while invalid); apply the edited rules in the agent
    (`GrantAndApprove` carries them, the agent re-validates, never trusts UI text blindly);
    update `docs/features.md`, `docs/keybindings.md`, `docs/implementation-notes.md`.
22. [x] Save failure/conflict recovery: after a refused or failed persistent save, offer once /
    temporary / retry-with-reload instead of only leaving the prompt pending with a notice.
    *Delivered:* the dialog stays open with the reason (`UiUpdate::GrantFailed`); retry, `t`
    temporary, `r` retry-with-reload, Back for once.
23. [x] Tests for steps 20–22 (presenter, widget, agent e2e) and verify:
    `make format lint`, `make test-coverage`, `rumdl check .`.

## Proposed Improvements

- Provide a read-only `ylva grants list` (or `/grants` slash command) later; out of scope now, but
  keep the store API ready for it.
- Provide `.ylva/.gitignore` guidance in docs rather than code (ignore rules must not be altered).

## Consistency Issues

- [low] The idea says headless mode "continues to deny calls that still require approval" yet
  persistent grants "by default apply in headless runs". Consistent only if the headless path
  consults grants strictly between verdict and denial; step 14 implements that and tests it.
- [low] `ApprovalCheck` has only three variants and the same `UserApprovalRequired` value is used
  for `clarify` and ordinary prompts (`src/agent/tool_queue.rs` special-cases by tool name).
  Step 8 must distinguish them without changing existing verdict semantics.

## Implementation Hints

- Verdicts: `ApprovalCheck { AutoApproved, AutoDenied{reason}, UserApprovalRequired }` in
  `src/sandbox/rules.rs`; `Tool::needs_approval` default is `UserApprovalRequired`
  (`src/agent/tools.rs`, ~line 505); `validate_args` runs first.
- TUI/agent approval flow: `src/agent/tool_queue.rs` (~lines 370–530) computes the verdict, then
  `AutoApproved` → execute (`spawn_subagent` is special-cased), `AutoDenied` → error result,
  `clarify` → `ClarifyPending`, else `UiUpdate::ToolPending` and `AgentMode::ToolPending`;
  `AgentCommand::ApproveTool/DenyTool` handled in `src/agent/runtime/commands.rs` (calls
  `advance_tool_queue`). Subagents pause the same way; `run_id` routes updates/commands via
  `AgentCommand::ForSubagent`.
- Headless: `src/cli_driver/run.rs` (~lines 360–415) calls `tool.needs_approval` itself and denies
  `UserApprovalRequired` with "tool requires approval, not supported in CLI mode" — grants must be
  consulted there too.
- TUI key handling: `src/presenter/keyboard.rs` (~line 210, `Focus::ToolApproval`, `PromptButton::
  Approve/Deny`), `mouse.rs` (~570), `Effect::ApproveTool/DenyTool` in
  `src/tui_reducer_state/events.rs`.
- Path matching: `path_glob_matches` in `src/sandbox/rules.rs` (private to `sandbox`, `pub(super)`);
  `docs/implementation-notes.md` → *Sandbox policy…* explains `**` special-casing of the bare
  pattern and that `evaluate_path` judges `canonicalize_best_effort(absolute(path))`. Bash:
  `src/sandbox/bash_command_extractor.rs`, `src/sandbox/command.rs::check_command`,
  `src/sandbox/command_arguments.rs`; notes in *Argument-conditioned command rules*.
- Trust: `src/config/workspace_trust.rs` — `covered_files` currently covers the profile's
  `config.toml`/`.env` plus `.ylva/agents/**`; trust store `<ylva_home>/trusted-workspaces.toml`
  keyed by workspace path + profile; `normalized_contents`/`normalize_toml` return `Option`
  (verify fallback hashing of malformed TOML). Docs: `docs/configuration.md` → *Workspace trust*.
- Safe writes: `src/private_fs.rs` (`replace_file`, `create_private_dir_all`,
  `private_open_options`).
- Constants for layout: `YLVA_DIR_NAME`, `PROFILES_DIRECTORY_NAME`, `PROFILE_CONFIG_FILE_NAME`,
  `PROFILE_ENV_FILE_NAME` (`src/config.rs`, `src/ylva_home.rs`).
- CLI auto-approve flags: `src/cli.rs` (`auto_approve_*`), mapped to `AutoApproveTool` in
  `src/agent/tools.rs` and applied as appended catch-all rules in `SandboxContext`.
- Decisions: `toml_edit` (already in `Cargo.toml`) edits `grants.toml` to preserve comments; one
  `[[grants]]` entry per tool; `load_skill` source stored workspace-relative literal inside the
  workspace, absolute otherwise; mandatory interaction via a `Tool::requires_human_interaction()`
  hook (true for `clarify` only), no new `ApprovalCheck` variant; external edit on save refuses
  overwrite and leaves the approval pending; grants are global (no agent scoping), a shared
  generation counter triggers re-evaluation in waiting loops.
- Project rules (`AGENTS.md`): descriptive names (no abbreviations), license headers on new files,
  file-per-module (`foo.rs`, no `mod.rs`), re-export via `src/lib.rs` for integration tests, keep
  `main.rs` thin, docs updated in the same change, ask before creating a new doc file.
- Plan execution policy: `docs/development-workflow.md` → *Plan execution*; verification:
  `make format lint`, `make test-coverage` (single mandatory test gate).
- Related idea (not a prerequisite): `docs/ideas/approval-verdict-trail.md` — decision maker =
  grant evaluator, rule origin = temporary/persistent grant, parent = original verdict.
