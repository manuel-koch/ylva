# Bash Command Rules: Static Argv-Shape Matching for `bash` Sandbox Checks

Implementing agent: flip each `[ ]` to `[x]` immediately after completing that
step and rewrite this file in place, so progress is resumable across
sessions. Do not batch checkbox updates until the end.

## Context

Ylva (a Rust TUI agent-harness) enforces tool-approval policy via
`Sandbox`/`SandboxContext` (`src/sandbox.rs`). Path-based tools (`read`,
`write`, `glob`, `search`, `edit`) already have configurable per-tool rule
lists (`[tool.<name>]`/`[agents.<type>.tools.<name>]` TOML tables, see
`docs/configuration.md`'s "Sandbox" section and
`docs/plans/phase10.3-sandbox-tool-rules.md`, already implemented). The
`bash` tool is the one exception: `SandboxContext::check_command`
(`src/sandbox.rs`) is a stub that always returns `None` (abstain), so every
`bash` call unconditionally falls back to `BashTool::needs_approval`'s
default, `ApprovalCheck::UserApprovalRequired` (`src/agent/tools/bash.rs`).

This plan implements the idea recorded in `docs/ideas/bash-rules.md` (read
that file in full before implementing — it contains the detailed rationale
this plan only summarizes): give `check_command` real, config-driven
argv-shape matching rules, so simple, safe shell invocations can be
auto-approved (or auto-denied) without recompiling, while anything the
static analysis can't confidently resolve still falls through to
`UserApprovalRequired`.

### Goal & motivation

Reduce needless approval prompts for simple, safe `bash` invocations (e.g.
`git log`, `git status`, `cat notes.txt`) by statically parsing a bash
script's top-level commands and evaluating each one against a new
`CommandGlob` rule kind, while never silently widening what gets
auto-approved: any command (or sub-part of a command) the parser can't
confidently classify must still require human approval.

### Acceptance criteria

- A new build-dependency-bearing crate, `tree-sitter-bash`, parses the
  `command` string passed to the `bash` tool into a real AST (not a
  hand-rolled tokenizer/word-splitter).
- **Grammar terminology (confirmed against `tree-sitter-bash`'s actual
  `grammar.js`/`node-types.json`, not just the idea doc's informal
  wording):** there is no `simple_command` node type — the real node name
  is `command` (assignment-prefix children plus `name`/`argument` fields).
  Only `&&`/`||` produce a `list` node; `;`, `&`, and newline are plain
  lexical *terminators* between sibling statements and do not wrap
  anything in a node of their own — two `;`-or-`&`-separated top-level
  commands are simply sibling nodes (e.g. under `program`, or under
  whichever enclosing block), not children of a `list` node. The extractor
  walks `list`, `pipeline`, and `command` node types, and must also walk a
  parent node's direct children in source order to discover
  `;`/`&`-separated sibling statements (since those aren't grouped under
  any node). Any other node type encountered anywhere in the tree
  (redirections, subshells, command substitution, parameter/variable
  expansion inside a word, `if`/`for`/`while`/`case`, function
  definitions, heredocs) makes *that* `command` (and only that one — see
  aggregation rule below) un-analysable, contributing at least
  `UserApprovalRequired` to the aggregate verdict.
- Each extracted `command` is evaluated independently against the
  tool's resolved rule list (same resolution/precedence mechanism
  `SandboxContext` already uses for path rules:
  `[agents.<type>.tools.bash]` > `[tool.bash]` > built-in floor). The
  overall verdict for the whole `bash` call is the worst case across all
  extracted commands, ranked `AutoDenied` > `UserApprovalRequired` >
  `AutoApproved`.
- A trailing `&` (backgrounding) anywhere in the script is a new named
  structural rule (`backgrounded`, alongside the existing
  `outside_workspace`/`dot_path` path rules), default-floor verdict
  `AutoDenied`, overridable per-tool via config exactly like the other two
  named rules. Because `&` produces no dedicated AST node (see above),
  detecting it requires inspecting the tree's anonymous/unnamed tokens (or
  the raw source text immediately following a statement's byte range) —
  see step 2's spike, which must empirically settle the exact mechanism
  before the extractor is built.
- A `command` node containing one or more leading `variable_assignment`
  children (e.g. `FOO=bar git status`) is governed by a fourth new named
  structural rule, `variable_assignment_prefix`, default-floor verdict
  `UserApprovalRequired`: the assignment mutates that command's
  environment in a way a pure argv-shape `CommandGlob` rule cannot see, so
  it is treated as its own explicit, independently overridable signal
  (not silently stripped, and not folded into the generic
  "un-analysable" bucket) — a user who wants to auto-approve assignment-
  prefixed invocations of a specific command can do so by overriding this
  rule's verdict per-tool, same mechanism as the other three named rules.
  The command's own `command_name`/`argument` tokens (after the
  assignment prefix) are still extracted and still separately evaluated
  against `CommandGlob` rules — `variable_assignment_prefix` firing does
  not suppress that evaluation, it only adds its own contribution to the
  per-command worst-case aggregation (see below).
- A new `CommandGlob` variant is added to `SandboxRuleConfig`
  (`src/sandbox.rs`), usable only in `[tool.bash]`/
  `[agents.<type>.tools.bash]` rule lists: a sequence of literal argv
  tokens, optionally ending in a bare `*` (matches exactly one more argv
  token) or bare `**` (matches one or more more argv tokens). No embedded/
  mid-pattern globbing (YAGNI, v1). Config loading fails fast, naming the
  tool, the rule's list index, and the expected kind, if a `CommandGlob`
  rule appears under a non-`bash` tool, or a `Named`/`Glob` path rule
  appears under `[tool.bash]`.
- Two more new named structural rules, `unquoted_glob` and
  `unquoted_brace_expansion` (same mechanism/per-tool overridability as
  `outside_workspace`/`dot_path`/`backgrounded`/`variable_assignment_prefix`),
  each with a default-floor verdict of `UserApprovalRequired`: an argv
  word containing an unquoted `*`, `?`, or `[...]` (`unquoted_glob`) or an
  unquoted `{...,...}` (`unquoted_brace_expansion`) triggers the
  respective rule. Quoted occurrences (`"*.txt"`/`'*.txt'`) do not trigger
  either rule. `~` (tilde expansion) is intentionally not treated
  specially.
- A configured `[tool.bash]`/`[agents.<type>.tools.bash]` rule list
  **fully replaces all four** new default-floor named rules
  (`backgrounded`/`variable_assignment_prefix`/`unquoted_glob`/
  `unquoted_brace_expansion`) for that tool — same full-replace-no-merge
  semantics as the existing path-tool precedent
  (`docs/plans/phase10.3-sandbox-tool-rules.md`). A user who configures
  any `bash` rule list must re-list any of these four they still want, by
  name, themselves.
- **Within a single extracted command**, the unconfigured default floor's
  four named rules are combined by worst-case ranking (not last-match-
  wins), so a stronger structural signal (e.g. `backgrounded` →
  `AutoDenied`) is never silently shadowed by a weaker one that happens to
  also match the same command (e.g. `unquoted_glob` →
  `UserApprovalRequired`) merely due to declaration order. An explicit
  user-configured `[tool.bash]`/`[agents.<type>.tools.bash]` rule list
  keeps ordinary last-match-wins semantics (list order is a deliberate
  user choice there, unlike the auto-generated default floor).
- A new `--auto-approve-bash` CLI flag (matching the existing
  `--auto-approve-write`/`--auto-approve-edit` per-tool-name convention)
  appends a catch-all `AutoApproved` `SandboxRule` (`pattern = "**"`) to
  `BashTool`'s `SandboxContext`, exactly like the two existing flags.
  `SandboxContext::check_command` must evaluate `self.rules` (today
  ignored by the `check_command` stub) **once per call, matched against
  the raw `command` string**, after the per-command structural/
  `CommandGlob` aggregation — last-match-wins between the two — so the
  flag always wins regardless of any `[tool.bash]` config or named-rule
  floor, mirroring `check_path`'s existing precedence and the "the flag
  always wins" guarantee `--auto-approve-write`/`--auto-approve-edit`
  already provide. This also makes any *future* code-appended `bash`
  catch-all (e.g. a later headless-mode flag) work correctly by
  construction, since `check_command` will no longer silently ignore
  `self.rules`.
- A `CommandGlob` rule whose trailing wildcard is a bare `*` abstains
  (never matches) when the one matched tail argv token contains an
  unquoted glob/brace character (deferring to `unquoted_glob`/
  `unquoted_brace_expansion` instead); a rule whose trailing wildcard is
  `**` is unaffected and matches normally regardless of unquoted glob/
  brace characters in its tail tokens. Literal-prefix tokens (for both
  `*` and `**` patterns) are always matched by exact text equality and are
  never subject to this abstention. See "`CommandGlob` rules and unquoted
  glob/brace tokens (resolved)" in `docs/ideas/bash-rules.md` for full
  rationale — this abstention is inherent to `CommandGlob` matching, not a
  separately configurable rule.
- `SandboxContext::check_command` returns `None` only when the script
  fails to parse at all, or when every extracted command is fully
  analysable and legal but no rule/floor fires an explicit verdict for it
  (i.e. today's "no opinion" abstention semantics are preserved as the
  safe fallback that lets `BashTool::needs_approval`'s own
  `UserApprovalRequired` default apply); otherwise it returns
  `Some(worst_case_verdict)`, per the aggregation rule above, including
  the `UserApprovalRequired` contributed by un-analysable commands/nodes.
- `docs/configuration.md`'s "Sandbox" section documents the new
  `CommandGlob` rule shape, the four new named rules
  (`backgrounded`/`variable_assignment_prefix`/`unquoted_glob`/
  `unquoted_brace_expansion`) and their default-floor verdicts, and the
  aggregation/fallback semantics above.
  `docs/features.md`/`docs/implementation-notes.md` are updated wherever
  they currently describe `bash` tool approval behavior as "always
  requires approval" or equivalent.
- `make format lint` and `make test-coverage` (≥ existing coverage
  threshold) both pass.
- Existing `src/sandbox.rs` tests for path-based rules
  (`check_path_outside_workspace_is_denied`, `check_path_dot_path_is_denied`,
  etc.) and existing `check_command_always_abstains` test are updated only
  where their premise (unconditional abstention) is deliberately changed
  by this plan — no incidental regressions to path-rule behavior.

### Key constraints

- **License compatibility.** Ylva is Apache-2.0. `tree-sitter-bash` is
  MIT-licensed — compatible. Do **not** add `conch-parser` (unmaintained
  since 2019) or `yash-syntax` (GPL-3.0-or-later, incompatible) as
  alternatives — `tree-sitter-bash` was deliberately chosen over both; see
  `docs/ideas/bash-rules.md`'s "Extraction backend" section for the full
  rationale.
- **New native build dependency.** `tree-sitter-bash` requires a C
  compiler at build time (a `cc` build-dependency compiling generated
  `parser.c`). Ylva currently has no native build dependency
  (`Cargo.toml`'s `[build-dependencies]` only has `time`) — this is a
  deliberate, accepted trade-off per the idea doc, not an oversight to
  avoid.
- **`None` is never auto-approval nor auto-denial.** It is strictly "fall
  through to the tool's own default" — for `bash` that is
  `UserApprovalRequired`. Any ambiguity (variable references, command/
  parameter substitution, unsupported syntax, parse errors) must resolve
  toward `None`/`UserApprovalRequired`, never silently toward
  `AutoApproved`.
- **Structural rules stay a Rust-code floor.** Per
  `docs/plans/phase10.3-sandbox-tool-rules.md`'s precedent (already
  implemented for `outside_workspace`/`dot_path`), the four new named
  rules (`backgrounded`, `variable_assignment_prefix`, `unquoted_glob`,
  `unquoted_brace_expansion`) must be added to the existing
  `NamedSandboxRule` enum (`src/sandbox.rs`) and evaluated the same way as
  the two existing named rules — reuse the existing `ResolvedRule`/
  `SandboxRuleConfig`/`with_configured_rules` machinery; do not build a
  parallel mechanism.
- **Scanning scope is fixed and documented, not organically expanded.**
  Only `list`/`pipeline`/`command` nodes (the real `tree-sitter-bash` node
  names — there is no `simple_command` type) are walked in v1, plus the
  sibling-statement walk needed to discover `;`/`&`-separated top-level
  statements (see Acceptance Criteria); subshells, redirections, command/
  parameter substitution, control-flow constructs, function definitions,
  and heredocs are explicitly out of scope and must degrade to
  un-analysable (`UserApprovalRequired`), not be partially supported.
- **`CommandGlob`'s `*`/`**` are only valid as the final token** in a
  pattern; there is no embedded/mid-pattern globbing (e.g. `feature/*` as
  one literal token) in v1 — YAGNI.
- **`--auto-approve-bash` naming vs. `docs/blueprint.md`'s planned
  `--auto-approve-shell`.** `docs/blueprint.md`'s Phase 11 checklist
  (§7 CLI Surface, not yet implemented) already reserves the name
  `--auto-approve-shell` for bash approval in headless mode. Per explicit
  user direction, this plan uses `--auto-approve-bash` instead (matching
  today's `--auto-approve-write`/`--auto-approve-edit` per-tool-name
  convention). `docs/blueprint.md`'s headless-mode mapping and Phase 11
  checklist item must be updated in this plan (see Implementation Plan)
  to reflect that bash's flag is `--auto-approve-bash`, not
  `--auto-approve-shell`, so the roadmap doesn't contradict the
  implementation or prompt a duplicate flag later.
- **Coverage threshold** (see `docs/development-workflow.md`) applies to
  all new/modified non-UI files this plan touches — the new AST-walking
  and rule-matching code is a natural candidate for a dedicated test
  module (e.g. `src/sandbox/bash_rules.rs` or inline in `src/sandbox.rs`,
  implementer's choice) with thorough unit coverage given its
  safety-critical nature.
- **No ADR for this plan.** Per explicit user direction, this plan does
  not record an architecture decision record, despite
  `docs/ideas/bash-rules.md` mentioning one as a possible follow-up.
  `docs/architecture-design-records/` remains untouched by this plan.
- **`docs/ideas/bash-rules.md`'s "Later addition: `tirith`" section is out
  of scope for this plan.** It documents a compatible, complementary
  follow-up idea (external subprocess content/threat scanner) that is
  *not* part of this implementation — do not add a `tirith` integration as
  part of this plan; see `docs/ideas/shell-hardening-with-tirith.md` for
  that separate, not-yet-planned idea.
- **Variable-length/mid-pattern wildcards and relaxing the v1 scanning
  scope** (subshells, redirections, substitutions) are explicitly out of
  scope for this plan (see `docs/ideas/bash-rules.md`'s "Out of scope for
  this idea" section) — do not implement them speculatively.

### Concrete starting point

First file to open: `src/sandbox.rs` — read `SandboxContext::check_command`
(currently a stub returning `None` unconditionally, around line 622) and
the existing `NamedSandboxRule`/`SandboxRuleConfig`/`ResolvedRule`/
`with_configured_rules` machinery used for path rules, since the new
command-rule mechanism must reuse it rather than duplicate it. First test
to run: `cargo test --lib sandbox::` to confirm the current baseline
(including `check_command_always_abstains`) passes before changing
anything.

## Implementation Plan

1. [x] Add `tree-sitter` and `tree-sitter-bash` as dependencies in
       `Cargo.toml` (`[dependencies]`), confirming their licenses (MIT) are
       compatible with Apache-2.0, and confirm the build succeeds with the
       new `cc`-based build step (`cargo build`) on the current toolchain.
2. [x] **Spike (throwaway probe, not the real extractor):** write a small
       standalone test/example against `tree-sitter-bash` that parses
       sample scripts and prints the *full* tree — including anonymous/
       unnamed nodes, not just named ones (`tree-sitter`'s cursor API
       visits both) — for: `cmd1 ; cmd2`, `cmd1 & cmd2`, a trailing
       `cmd &`, `cmd1 && cmd2 || cmd3`, `a | b | c`, and
       `FOO=bar git status`. Confirm empirically: (a) how `&` becomes
       observable in the tree (an anonymous token node, or only inferable
       from raw source byte ranges between sibling statements — research
       going into this plan suggests `&`/`;` are lexical terminators
       consumed between statements, not wrapped in any node, named or
       anonymous, but this must be verified against the actual installed
       crate version before the extractor is designed around it); (b) that
       `;`- and `&`-separated top-level commands are indeed direct
       siblings (not grouped under a `list` node, which the grammar
       reserves for `&&`/`||` only); (c) that a `command` node's
       `variable_assignment` children are enumerable ahead of its
       `command_name` field. Record findings as doc comments in the
       eventual extractor module (step 3) so the empirical basis for its
       design is preserved in the codebase, not just this plan. Delete the
       throwaway probe/example once step 3's real extractor module exists
       and covers the same ground via its own tests.
3. [x] Write a bash-script extractor module (new file, e.g.
       `src/sandbox/bash_command_extractor.rs`, wired into `src/sandbox.rs`
       via `mod bash_command_extractor;` or as a submodule per the
       project's file-per-module convention) that:
       - Parses a `command` string into a `tree-sitter-bash` AST.
       - Walks `list`/`pipeline`/`command` nodes (the real grammar node
       names confirmed in step 2 — no `simple_command` type exists), and
       additionally walks a parent node's direct children in source
       order to discover `;`/`&`-separated sibling statements (per step
       2's findings, these are not grouped under any node).
       - For each extracted `command` node, extracts its argv as a
       `Vec<ArgvToken>` (skipping/recording any leading
       `variable_assignment` children separately — see the
       `variable_assignment_prefix` rule below), where each token tracks
       its literal text and whether it was quoted (needed for the
       unquoted-glob/brace checks in step 5).
       - Returns a result type distinguishing: fully-parsed commands (with
       their argv lists and any variable-assignment-prefix flag), commands
       that are un-analysable (contain a disallowed node type nested
       within them), and a top-level parse failure for the whole script.
       - Detects a trailing `&` (backgrounding) per extracted command,
       exposed as a boolean flag alongside each extracted command or as
       its own structural signal for step 6's `backgrounded` rule, using
       the mechanism step 2 empirically confirmed.
4. [x] Add unit tests for the extractor covering: the three "in-scope"
       examples from `docs/ideas/bash-rules.md`'s "Scanning scope (v1)"
       section (`find . -type f ; echo "Foo" ; cat foo.txt`,
       `find . -type f | sort | cut -d "_" -f2`,
       `git fetch && git rebase origin/main || echo failed`), plus at
       least one example each of: a subshell, command substitution
       (`$(...)`/backticks), a bare `$FOO`/`${FOO}` reference, an `if`/
       `for`/`while`/`case` construct, a function definition, a heredoc, a
       trailing `&`, and a leading variable assignment
       (`FOO=bar git status`) — confirming each un-analysable construct is
       flagged as such (not silently ignored or partially parsed), the
       trailing-`&` case is flagged as backgrounded, and the
       variable-assignment case is flagged with its own signal (not
       folded into "un-analysable" and not silently ignored). Additionally
       add a small, explicitly-labeled "grammar assumption" test module
       (separate from the extractor's own behavioral tests) that directly
       asserts the raw `tree-sitter-bash` node shapes step 2's spike
       confirmed (no `simple_command` node type; `list` produced only for
       `&&`/`||`; `;`/`&` as non-node terminators; `variable_assignment`
       as a `command` sibling child) — so a future `tree-sitter-bash`
       version bump that silently changes these shapes fails CI loudly
       instead of silently misclassifying commands.
5. [x] Add unquoted-glob/brace detection: given an `ArgvToken`, inspect its
       literal text for an unquoted `*`, `?`, or `[...]` (glob) or an
       unquoted `{...,...}` (brace expansion), using the token's quoted-
       ness (tracked in step 3) to skip quoted occurrences. Add unit tests
       for: `cat *.txt` (unquoted glob flagged), `cat "*.txt"`/
       `cat '*.txt'` (not flagged), `echo {a,b}` (unquoted brace flagged),
       `echo "{a,b}"` (not flagged), and a token with no glob/brace
       characters (not flagged).
6. [x] Extend `NamedSandboxRule` (`src/sandbox.rs`) with four new
       variants: `Backgrounded`, `VariableAssignmentPrefix`,
       `UnquotedGlob`, `UnquotedBraceExpansion` (serde string values
       `"backgrounded"`, `"variable_assignment_prefix"`, `"unquoted_glob"`,
       `"unquoted_brace_expansion"`), each with its own evaluator function
       (mirroring the existing `evaluate_outside_workspace_rule`/
       `evaluate_dot_path_rule` pattern from
       `docs/plans/phase10.3-sandbox-tool-rules.md`), used both by the new
       unconditional default floor for `bash` (`backgrounded` →
       `AutoDenied`, `variable_assignment_prefix` → `UserApprovalRequired`,
       `unquoted_glob` → `UserApprovalRequired`,
       `unquoted_brace_expansion` → `UserApprovalRequired`) and by the
       config-driven named-rule path with a configured verdict.
7. [x] Add the `CommandGlob` variant to `SandboxRuleConfig`
       (`src/sandbox.rs`): `{ command = ["git", "reflog", "*"], verdict =
       "...", reason = "..." }`, with `command` as a `Vec<String>` field
       (fixed field name and shape — an array of literal tokens, not a
       single space-separated string, to avoid re-splitting/quoting
       ambiguity), optionally ending in a bare `"*"` or `"**"` string.
       Implement matching per `docs/ideas/bash-rules.md`'s "Rule matching
       (`CommandGlob`)" section: exact-length match with no trailing
       wildcard; exactly-one-extra-token for a trailing `*` (abstaining if
       that one extra token contains an unquoted glob/brace, per step 5);
       one-or-more-extra-tokens for a trailing `**` (never abstains on
       unquoted glob/brace in its tail). Literal-prefix tokens always
       match by exact text equality.
8. [x] Add config validation: extend the existing tool-name/rule-kind
       validation (`docs/plans/phase10.3-sandbox-tool-rules.md`'s
       precedent for path-only tools) so `CommandGlob` rules are only
       accepted under `[tool.bash]`/`[agents.<type>.tools.bash]`, and
       `Named`/`Glob` path rules are rejected there — a hard startup error
       naming the tool, the rule's list index in its `rules` array, and
       the expected kind for that tool.
9. [x] Implement `SandboxContext::check_command` (`src/sandbox.rs`,
       replacing the current stub): parse the command via the step-3
       extractor; for each extracted `command`, evaluate the resolved
       named-rule/`CommandGlob` list for `bash` (config-driven or default
       floor — full-replace, not merged, per the Acceptance Criteria).
       For the **unconfigured default floor** specifically, combine all
       matching named rules for that one command by worst-case ranking
       (not last-match-wins — see Acceptance Criteria); for an **explicit
       user-configured** `[tool.bash]`/`[agents.<type>.tools.bash]` list,
       keep ordinary last-match-wins across its entries (list order is a
       deliberate user choice there). For each un-analysable command/node,
       treat it as contributing `UserApprovalRequired`. Aggregate all
       per-command contributions across the whole script via worst-case
       ranking (`AutoDenied` > `UserApprovalRequired` > `AutoApproved`)
       into a single "structural verdict". Then evaluate `self.rules`
       (today ignored by the stub) **once per call, matched against the
       raw `command` string** — last-match-wins between the structural
       verdict and any `self.rules` match, evaluated *after* the
       structural verdict, so a code-appended catch-all (e.g.
       `--auto-approve-bash`'s rule, step 11) always wins regardless of
       config, mirroring `check_path`'s existing precedence. Return `None`
       only when the script parses cleanly, no rule/floor/un-analysable
       -node contributes any structural verdict, and `self.rules` has no
       match either (pure abstention, unchanged from today's
       total-abstention semantics as the safe fallback).
10. [x] Update/replace the existing `check_command_always_abstains` test
        (`src/sandbox.rs`) — it encodes the pre-this-plan stub behavior
        (total abstention for every command) and must be revised to
        reflect the new default floor (e.g. confirm a background command
        is now denied, confirm a plain safe command like `echo hello`
        still abstains under the default floor since no rule matches it
        and it's fully analysable). Add new tests for: worst-case
        aggregation across a multi-command script (e.g. one segment
        matches an `AutoApproved` `CommandGlob` rule, another is
        un-analysable — overall verdict must be `UserApprovalRequired`),
        an `AutoDenied` verdict from any single segment winning over
        `AutoApproved`/`UserApprovalRequired` elsewhere in the same script,
        a full parse failure returning `None`, a `[tool.bash]` config that
        omits the four named rules losing their default-floor protection
        (confirming the full-replace, not merge, semantics), a single
        command that simultaneously matches two of the default floor's
        named rules with different verdicts resolving to the worse of the
        two regardless of the rules' declaration order (confirming
        worst-case combination, not last-match-wins, for the unconfigured
        floor), and a `self.rules` catch-all (`with_rule` appended
        directly, simulating a future code-level override) winning over a
        structural `AutoDenied` verdict for the same command.
11. [x] Add `--auto-approve-bash`, following the exact plumbing pattern of
        `--auto-approve-write`/`--auto-approve-edit`: parse the flag in
        `src/main.rs` (alongside the existing `auto_approve_edit`/
        `auto_approve_write` parsing), add `auto_approve_bash: bool` to
        `AgentFactory` (`src/agent_factory.rs`) with a
        `with_auto_approve_bash()` builder method threaded the same way as
        `with_auto_approve_write`/`with_auto_approve_edit` through to
        `ToolRegistryConfig`, add `auto_approve_bash: bool` to
        `ToolRegistryConfig` (`src/agent/tools.rs`, defaulting to `false`),
        and in `default_registry`'s `bash` branch, append a catch-all
        `SandboxRule { pattern: "**", verdict: AutoApproved }` via
        `with_rule` when `config.auto_approve_bash` is set — mirroring the
        `write`/`edit` branches' existing `if config.auto_approve_write`/
        `auto_approve_edit` blocks. Add a test mirroring
        `auto_approve_write_wins_over_configured_deny_rule`
        (`src/agent/tools.rs`) for `auto_approve_bash` winning over a
        configured `[tool.bash]` deny rule.
12. [x] Wire `[tool.bash]`/`[agents.<type>.tools.bash]` config resolution
        for `CommandGlob`/named command rules into
        `src/agent/tools.rs`'s `default_registry` (the same wiring point
        used for path-based tools per
        `docs/plans/phase10.3-sandbox-tool-rules.md`'s step 6) — no new
        wiring mechanism, just extending the existing per-tool resolution
        to cover `bash`'s new rule kinds.
13. [x] Update `docs/configuration.md`'s "Sandbox" section: document the
        `CommandGlob` rule shape and matching semantics, the four new
        named rules and their default-floor verdicts, the worst-case
        aggregation rule (both cross-command and, for the unconfigured
        floor, within-command), the `*`-tail unquoted-glob/brace
        abstention behavior, the full-replace (not merge) semantics for
        `[tool.bash]`, the config-validation error (wrong rule kind under
        `[tool.bash]` vs. other tools), and `self.rules`'/
        `--auto-approve-bash`'s precedence over the structural verdict.
        Add a `--auto-approve-bash` row to `docs/cli-options.md`'s flags
        table, matching the existing `--auto-approve-write`/
        `--auto-approve-edit` rows' phrasing.
14. [x] Update `docs/features.md`/`docs/implementation-notes.md` wherever
        they describe `bash` tool approval as unconditionally requiring
        user approval, to reflect the new rule-driven behavior and default
        floor.
15. [x] Update `docs/blueprint.md` for consistency with the now-implemented
        `--auto-approve-bash` flag: the "Tool Approval in Headless Mode"
        mapping (`--auto-approve-shell` → `bash`, around line 2331) and
        the Phase 11 checklist item listing `--auto-approve-shell`
        (around line 3032) both currently reserve that name for bash —
        annotate or amend them (implementer's judgment on exact wording)
        so the roadmap reflects that bash's flag is `--auto-approve-bash`
        (already implemented, this plan) rather than a still-to-be-built
        `--auto-approve-shell`, avoiding a future duplicate flag for the
        same purpose. Do not otherwise renumber or re-scope Phase 11.
16. [x] Run `make format lint` and `make test-coverage`; fix any finding
        before reporting this plan done.
17. [x] Run `rumdl fmt` on every `docs/*.md` file edited in steps 13-15.

## Proposed Improvements

- Consider adding a small `ylva bash-rules check "<command>"` (or similar)
  diagnostic CLI/debug hook that prints the extracted argv/verdict
  breakdown for a given command string, to help users debug why a
  particular `[tool.bash]` rule did or didn't match — useful given how
  much subtler the aggregation/abstention semantics are here compared to
  the simple path-glob rules. Not required by the idea doc; flagging as
  optional polish.

## Consistency Issues

- [low] `docs/ideas/bash-rules.md`'s "Out of scope for this idea" section
  mentions an ADR alongside the implementation plan as a possible
  follow-up; per explicit user direction this plan intentionally does not
  record one. Flagging so a future contributor doesn't assume its absence
  is an oversight.
- [low] `docs/blueprint.md` reserves `--auto-approve-shell` (not
  `--auto-approve-bash`) as bash's headless-mode approval flag (Phase 11
  checklist, and the "Tool Approval in Headless Mode" mapping). Per
  explicit user direction this plan implements `--auto-approve-bash`
  instead, matching today's `-write`/`-edit` naming convention; step 15
  updates `docs/blueprint.md` to reconcile this so the roadmap doesn't
  contradict the shipped flag name or prompt a duplicate flag later.

## Implementation Hints

- `src/sandbox.rs`: `SandboxContext::check_command` (currently a stub at
  around line 622, always returns `None`); `NamedSandboxRule`
  (`OutsideWorkspace`/`DotPath` today); `SandboxRuleConfig`
  (`Named`/`Glob` variants today, untagged serde distinguishing by
  `rule` vs. `pattern` key presence); `ResolvedRule`;
  `SandboxContext::with_configured_rules`. All of this plan's new rule
  kinds should extend these existing types rather than introduce parallel
  ones.
- `src/agent/tools/bash.rs`: `BashTool::needs_approval` already calls
  `self.sandbox_context.check_command(command)` and falls back to
  `ApprovalCheck::UserApprovalRequired` on `None` — no change needed to
  this call site; all new behavior is internal to `check_command`.
- `--auto-approve-write`/`--auto-approve-edit` plumbing to mirror for
  `--auto-approve-bash`: CLI parsing (`src/main.rs`, around lines
  216-217, 254-258); `AgentFactory` fields/builder methods
  (`src/agent_factory.rs`, fields around lines 48-51/69-70, builders
  around lines 207-214, threaded through the constructor call chain at
  lines 259-260/280-281/367-368/391-392/436-437/488-489);
  `ToolRegistryConfig` field and `default_registry`'s per-tool
  `with_rule` block (`src/agent/tools.rs`, fields around lines 577-580/
  612-613, `write`/`edit` branches around lines 657/684); the existing
  `auto_approve_write_wins_over_configured_deny_rule` test
  (`src/agent/tools.rs`, around line 990) is the pattern for this plan's
  new `auto_approve_bash` equivalent test.
- `docs/plans/phase10.3-sandbox-tool-rules.md` is the precedent
  implementation for path-based `[tool.<name>]` config (already fully
  implemented, all steps checked) — its `NamedSandboxRule`/
  `SandboxRuleConfig`/resolution-order/validation design is the pattern
  this plan's `bash`-specific rules must follow, not reinvent.
- `docs/configuration.md`'s "Sandbox" section (around line 305 onward)
  documents the current path-rule config shape, resolution order
  (`[agents.<type>.tools.<name>]` > `[tool.<name>]` > default floor), and
  tool-name/rule-kind validation error — this plan's doc updates extend
  that same section rather than adding a separate one.
- `Cargo.toml`'s `[build-dependencies]` currently only has `time`; adding
  `tree-sitter-bash` introduces the repository's first native
  (C-compiler-requiring) build dependency.
- Grammar facts confirmed against `tree-sitter-bash`'s `grammar.js`/
  `node-types.json` (v0.25.1, MIT-licensed, actively maintained as of this
  writing): no `simple_command` node type exists (the real name is
  `command`); `list` is produced only for `&&`/`||`; `;`/`&`/newline are
  lexical terminators consumed between sibling statements without
  producing any node of their own; a `command` node's `variable_assignment`
  children (if any) precede its `command_name` field. Step 2's spike
  re-verifies this empirically against the actual installed crate version
  before the extractor (step 3) is built around it.
- `docs/architecture-design-records/` does not currently exist in this
  repository (confirmed via directory listing); this plan does not create
  it (no ADR, per explicit user direction).
- Registered tool names validated against today: `read`, `write`, `glob`,
  `bash`, `search`, `edit`, `clarify` (per `docs/configuration.md`) — the
  new `CommandGlob`-under-`bash`-only validation in step 8 extends this
  same validation surface.
- `docs/ideas/bash-rules.md`'s "Later addition: `tirith`" section
  describes a complementary, not-yet-planned follow-up
  (`docs/ideas/shell-hardening-with-tirith.md`) that integrates an
  external AGPL-3.0 subprocess as a content/threat scanner — explicitly
  out of scope here; do not implement any part of it as part of this plan.
