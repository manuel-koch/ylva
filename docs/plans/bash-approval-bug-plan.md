# Fix: unmatched command in a `bash` list must not be auto-approved

## Context

**Component:** `bash` tool approval in the sandbox
(`SandboxContext::check_command` in `src/sandbox/command.rs`, called by
`BashTool::needs_approval` in `src/agent/tools/bash.rs`).

**Bug.** A `bash` script (list `a; b`, `a && b`, or pipeline `a | b`) is
split into commands; each command is evaluated against the configured
`[tools.bash]` / `[agents.<type>.tools.bash]` rules, and the per-command
verdicts are combined by worst-case ranking
(`AutoDenied` > `UserApprovalRequired` > `AutoApproved`). A command that
matches **no** rule contributes `None`, and `None` loses to any `Some`. So a
single auto-approved command makes the whole script auto-approved even if
the other commands matched nothing:

```toml
[tools.bash]
rules = [{ command = ["cat", "**"], verdict = "auto_approved" }]
```

- `rm -rf foo; cat x` → `Some(AutoApproved)` (verified 2026-10-21 with a
  throwaway unit test). Expected: user approval.
- Same for pipelines, e.g. `cat x | sh`.
- `export PATH=/tmp/evil; cat x` is already `UserApprovalRequired`
  (`export` parses as a declaration command → `Unanalysable`).

The sample profile `.ylva/profiles/other/config.toml` ships exactly such
`["<cmd>", "**"]` approving rules, so this is reachable in practice.

**Goal.** A script is auto-approved only if **every** command in it is
auto-approved by a rule. The sandbox may abstain (`None`) only when **no**
command produced a verdict.

**Motivation.** Approval is per tool call (the whole script), so the
approval decision must cover every command that will run. The idea doc
`docs/ideas/bash-approval.md` (layer 2, "Shell-state commands in a list"
and "Known bug") relies on this invariant.

**Acceptance criteria.**

- With rule `["cat", "**"] → auto_approved`:
  - `rm -rf foo; cat x`, `cat x && rm -rf foo`, `cat x | sh` →
    `Some(UserApprovalRequired)`.
  - `cat a; cat b` → `Some(AutoApproved)` (unchanged).
  - `rm -rf foo` (single unmatched command) → `None` (unchanged; the tool
    default applies).
- Existing worst-case behavior unchanged: any `AutoDenied` wins; any
  `UserApprovalRequired` / `Unanalysable` beats `AutoApproved`.
- Default floor (no `[tools.bash]` config) unchanged; it never produces
  `AutoApproved`, so it is unaffected.
- `--auto-approve-bash` (`self.rules`, evaluated last on the raw string)
  still wins over the structural verdict.
- `docs/configuration.md` describes the new aggregation rule; the idea doc
  no longer lists this as a known bug.
- `make format lint` and `make test-coverage` pass.

**Out of scope.** Path checks for `bash` arguments, argument grammars,
changes to rule matching (`CommandGlobPattern::matches`), default-floor
semantics, config validation.

## Implementation Plan

An implementing agent must flip `[ ]` to `[x]` immediately after completing
each step and rewrite this file in place, so progress is resumable across
sessions.

1. [x] Add failing regression tests in `src/sandbox/command/tests.rs`
   (TDD, run them and see them fail before step 2):
   1. [x] `check_command_unmatched_command_in_list_blocks_auto_approval`:
      configured rule `["cat", "**"] → AutoApproved`; assert
      `Some(UserApprovalRequired)` for `rm -rf foo; cat x`,
      `cat x && rm -rf foo` and `cat x | sh`.
   2. [x] `check_command_all_commands_auto_approved_list_is_auto_approved`:
      same rule; `cat a; cat b` → `Some(AutoApproved)`, and the single
      unmatched `rm -rf foo` → `None`.
2. [x] Fix the aggregation in `SandboxContext::check_command`
   (`src/sandbox/command.rs`): while iterating statements, track whether
   any `ExtractedStatement::Command` evaluated to `None`. After the loop, if
   the aggregate is `Some(AutoApproved)` and at least one command was
   unmatched, the structural verdict becomes
   `Some(UserApprovalRequired)`. Leave all other outcomes (including
   all-`None` → `None`) unchanged. Keep `worse_verdict` /
   `approval_check_rank` as they are. Do not touch the `self.rules` loop.
3. [x] Update the doc comment of `check_command` (the **Aggregation**
   paragraph and the "Returns `None` only when …" paragraph) to state: an
   unmatched command alongside an auto-approved one yields
   `UserApprovalRequired`; `None` is returned only when no command produced
   a verdict (or parsing failed) and `self.rules` has no match. Update the
   module doc line (`//! Command structure evaluation …`) only if it becomes
   inaccurate.
4. [x] Update `BashTool::needs_approval`'s doc comment in
   `src/agent/tools/bash.rs` only if it contradicts the new behavior (it
   currently describes abstention generically; likely no change).
5. [x] Update `docs/configuration.md`, section "`bash` command rules" →
   "**Per-command evaluation and aggregation.**": after "All commands in the
   script are then combined the same way (strictest verdict wins) …", add
   that a command matching no rule never lets the script be auto-approved:
   if any other command is `auto_approved`, the script requires user
   approval. Adjust the following paragraph ("When the script fails to
   parse, or every command in it is understood and legal but no rule
   fires …") so it stays accurate.
6. [x] Update `docs/ideas/bash-approval.md`, layer 2: replace the
   "*Known bug (verified 2026-10-21)*" paragraph with a short note that the
   "every command must be auto-approved" invariant is enforced by
   `SandboxContext::check_command`.
7. [x] Run `rumdl fmt` and `rumdl check` on the changed markdown files.
8. [x] Run `make format lint` and `make test-coverage`; fix any failure.

## Proposed Improvements

- Add a short "verdict reason" to the approval prompt / debug log when the
  upgrade from step 2 applies (e.g. "`rm` matches no rule"), so users see
  why a list with an approved command still asks. Fits
  `docs/ideas/approval-verdict-trail.md`.

## Implementation Hints

- `src/sandbox/command.rs`:
  - `check_command` (≈ line 57) loops over
    `bash_command_extractor::extract(command)` statements:
    `ExtractedStatement::Command(extracted)` → `self.evaluate_command(extracted)`
    (`Option<ApprovalCheck>`); `ExtractedStatement::Unanalysable` →
    `Some(UserApprovalRequired)`; combined via `Self::worse_verdict`.
    `ExtractionResult::ParseFailed` → `None`.
  - `evaluate_command` returns `None` when a configured list has no
    matching rule, or when the default floor's four named rules don't fire.
  - `worse_verdict(None, Some(x)) == Some(x)` — the root cause.
  - After the structural verdict, `self.rules` (e.g. `--auto-approve-bash`
    catch-all) is applied last-match-wins against the raw command string.
  - A `tracing::debug!` logs the final verdict (`sandbox_verdict`).
- `src/agent/tools/bash.rs` `needs_approval`: `None` →
  `ApprovalCheck::UserApprovalRequired`; `Some(v)` → `v`. Only caller of
  `check_command`.
- `bash_command_extractor` recurses into `list` and `pipeline` nodes, so
  every pipeline element is its own `ExtractedStatement`.
- Tests: `src/sandbox/command/tests.rs` uses
  `make_sandbox(temporary_dir.path())`,
  `SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(...)`
  with a `SandboxRuleConfig::CommandGlob { command, verdict:
  ApprovalVerdictConfig::AutoApproved, reason: None }` entry (see `check_command_aggregates_worst_case_across_multiple_commands`).
  Run targeted: `cargo test --lib sandbox::command`.
- Existing tests that must stay green and pin unchanged behavior:
  `check_command_default_floor_abstains_for_plain_safe_commands`
  (single commands → `None`),
  `check_command_auto_denied_in_one_segment_wins_over_others`
  (`echo hello ; sleep 10 &` → `AutoDenied`),
  `check_command_aggregates_worst_case_across_multiple_commands`,
  `check_command_parse_failure_returns_none`,
  `check_command_self_rules_catch_all_wins_over_structural_auto_denied`;
  `src/sandbox/tests.rs` asserts `check_command("echo hello") == None`.
- Docs describing the behavior: `docs/configuration.md` section
  "`bash` command rules" (≈ lines 1098–1117). `docs/features.md` (≈ line
  109) only summarizes `bash` approval generically; no change expected.
  `docs/architecture-design-records/` is empty; no ADR affected.
- Project rules (`AGENTS.md`): full descriptive names, Apache-2.0 headers
  (existing files already have them), docs updated in the same commit,
  verification gate `make format lint` + `make test-coverage` (≥ 80% line
  coverage). No dependency change, so no `make audit`.
