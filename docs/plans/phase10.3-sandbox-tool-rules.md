# Phase 10.3: Per-Tool Sandbox Rule Configuration

Implementing agent: flip each `[ ]` to `[x]` immediately after completing that
step and rewrite this file in place, so progress is resumable across
sessions. Do not batch checkbox updates until the end.

## Context

Ylva (a Rust TUI agent-harness) enforces a path-based tool-approval policy
via `Sandbox`/`SandboxContext` (`src/sandbox.rs`, design in
`docs/blueprint.md` §4E "Safety: Human-in-the-Loop", implementation notes
also summarized in `docs/configuration.md`'s "Sandbox" section). Today, two
rules are **hardcoded inline** in
`SandboxContext::check_path_with_dot_path_access`:

1. `outside_workspace` → `AutoDenied` (any path that normalizes outside the
   workspace root).
2. `dot_path` → `AutoDenied` (any content-tool path whose relative-to-root
   form has a component starting with `.`, except where `allow_dot_paths` is
   requested — used by `glob` enumeration).

Named per-tool glob rules can already be appended via
`SandboxContext::with_rule`/`SandboxRule` (evaluated after the two hardcoded
ones, last-match-wins), but only `default_registry`
(`src/agent/tools.rs`) constructs these today, from CLI flags
(`--auto-approve-write`/`--auto-approve-edit`) — there is no TOML config
path for them at all.

This plan is **sub-phase 10.3** of `docs/blueprint.md`'s Phase 10 ("Full
Configuration & Persona System", §7, around line 2912). It implements
blueprint Phase 10 checklist item **9**: "Per-tool sandbox rule
configuration: load configurable `[tool.*]` rule lists (including
`outside_workspace`/`dot_path` overrides, §4E) from the layered config,
replacing Phase 6's hardcoded two-rule default."

**This plan has no hard dependency on `docs/plans/phase10.1-config-layering-and-init.md`
or `docs/plans/phase10.2-agent-type-files.md`.** It only extends the
pre-existing `ProfileConfig` struct (already present since Phase 7) with a
new `[tool.*]` table, and its "built-in floor" for the two default rules is
Rust struct-literal code, not an embedded config file — so it can be
implemented independently of, or in parallel with, 10.1/10.2. (One soft
integration point: once `phase10.1`'s three-layer `config.toml` resolution
lands, a project's own `config.toml` layer will automatically be able to
override `[tool.*]` rules too, for free, with no additional work in this
plan — until then, `[tool.*]` configuration works at whatever `config.toml`
resolution Phase 7/`phase10.1` currently provides.)

Read `docs/blueprint.md` §4E and `docs/configuration.md`'s "Sandbox" section
before implementing.

### Goal & motivation

Make the sandbox's rule list — including the two rules that today only
exist as inline Rust conditionals — fully data-driven per tool, so a user
can reorder, disable, or add to them via config without recompiling,
while still guaranteeing a safe default when no config is present.

### Acceptance criteria

- A new `[tool.<name>]` TOML table (e.g. `[tool.read]`, `[tool.write]`) with
  a `rules` list is parseable from a profile's `config.toml`, deserializing
  into an ordered rule list per tool name.
- Each rule in the list is one of:
  - A **named structural rule** referencing one of the two existing
    built-in checks (`outside_workspace`, `dot_path`) with a configurable
    `verdict` (e.g. `{ rule = "outside_workspace", verdict = "auto_denied"
    }`), so a user can reorder or change the verdict of these two checks
    without reimplementing their underlying path-comparison logic in glob
    syntax.
  - A **custom glob rule** (`{ pattern = "...", verdict = "..." }`),
    exactly like today's `SandboxRule`.
  Evaluation order is list order; last match wins (unchanged semantics from
  today's `with_rule`/last-match-wins behavior).
- **Default floor when a tool has no `[tool.<name>]` table at any
  resolved layer**: the exact two rules hardcoded today
  (`outside_workspace` → `AutoDenied`, `dot_path` → `AutoDenied`, with the
  existing `allow_dot_paths` bypass for `glob` enumeration preserved),
  applied via **Rust struct-literal code**, not parsed from any embedded
  TOML file — this floor must exist unconditionally, independent of any
  repository configuration files, so it can never be broken by a
  missing/corrupted resource.
- **A tool with a `[tool.<name>]` table configured (at any layer) uses
  exactly that list — it fully replaces the default floor for that tool**,
  it does not implicitly merge with it. A user who configures `[tool.read]`
  must explicitly include `{ rule = "outside_workspace", ... }`/
  `{ rule = "dot_path", ... }` entries themselves if they still want those
  protections for that tool.
- `--auto-approve-write`/`--auto-approve-edit` CLI flags continue to work:
  each appends its own catch-all `AutoApproved` rule **after** whichever
  rule list applies for that tool (config-provided or the default floor) —
  last-match-wins, so the flag always wins regardless of config, with no
  special bypass path.
- All pre-existing `src/sandbox.rs` unit tests
  (`check_path_outside_workspace_is_denied`, `check_path_dot_path_is_denied`,
  `check_path_allowing_dot_paths_*`,
  `check_path_glob_rule_overrides_to_explicit_verdict`) continue to pass
  **unmodified** after the refactor — any test needing a change is itself a
  signal of an accidental behavior change and must be justified explicitly.
- `make lint`, `make test`, and `make test-coverage` all pass (≥80% line
  coverage) after the change.
- `docs/configuration.md`'s "Sandbox" section and the relevant
  `docs/blueprint.md` checkbox (item 9) are updated in the same commit as
  the code they describe.

### Key constraints

- **Preserve exact current behavior when unconfigured.** This refactor's
  primary risk is a silent behavior regression in the safety-critical
  path-approval logic. Add characterization tests for today's behavior
  *before* refactoring (if not already adequately covered by the existing
  `src/sandbox.rs` tests), refactor, then confirm those tests still pass
  unchanged.
- **`ApprovalCheck` has no `Deserialize` impl today** (`src/sandbox.rs`) —
  add one (or a parallel serde-friendly enum that converts into it) without
  changing its existing non-serde usage elsewhere (`AutoApproved`/
  `AutoDenied { reason: String }`/`UserApprovalRequired`).
- **The two structural checks are currently inline conditionals**, not data:
  `SandboxContext::check_path_with_dot_path_access`
  (`src/sandbox.rs`, workspace-boundary comparison and
  `SandboxContext::is_dot_path`). Lifting them into named, reusable
  evaluator functions (callable both from the unconditional default-floor
  path and from a config-driven "named rule" path) must not duplicate their
  logic in two places.
- **`allow_dot_paths` bypass must survive the refactor**: `glob`
  enumeration calls `check_path_allowing_dot_paths`, which must continue to
  skip the `dot_path` check (structural or named-rule form) while still
  enforcing `outside_workspace` and any custom rules.
- **No new dependency anticipated.** `glob::Pattern` (already a dependency)
  and `serde`/`toml` (already dependencies) are sufficient.
- **Coverage threshold ≥80%** (`make test-coverage`) applies to all new/
  modified non-UI files this plan touches.
- **Docs-are-part-of-the-change-set** and **ADR-consistency** rules apply
  (see `AGENTS.md`/`docs/development-workflow.md`); `docs/architecture
  -design-records/` is currently empty. Given this plan changes a
  safety-relevant default-policy mechanism, consider recording the
  "structural rules stay Rust-code floor, TOML fully replaces per tool"
  decision as a new ADR via the `architecture-decision-record` skill if it
  is not already adequately captured by `docs/configuration.md`.

### Concrete starting point

First file to open: `src/sandbox.rs` (current `Sandbox`/`SandboxContext`/
`SandboxRule`/`ApprovalCheck` types and the two hardcoded rules inside
`check_path_with_dot_path_access`). First test to run: `cargo test --lib
sandbox::` to confirm the current baseline passes before changing anything.

## Implementation Plan

1. [x] Add characterization tests (if any current behavior isn't already
       covered by the existing `src/sandbox.rs` test module) for today's
       exact two-rule default behavior, so the refactor in later steps has
       a safety net proving no regression. Confirm the existing tests
       (`check_path_outside_workspace_is_denied`,
       `check_path_dot_path_is_denied`,
       `check_path_allowing_dot_paths_abstains_inside_workspace`,
       `check_path_glob_rule_overrides_to_explicit_verdict`, and any others
       in that module) already exercise: outside-workspace denial,
       dot-path denial, the `allow_dot_paths` bypass, and a custom glob
       rule overriding the hardcoded defaults. Add any missing case now,
       before refactoring.
2. [x] Add a serde-deserializable representation for `ApprovalCheck`
       (`src/sandbox.rs` or a new `src/config/sandbox_rule.rs`): a tagged
       enum/struct such as
       `{ verdict: "auto_approved" | "auto_denied" | "user_approval_required",
       reason: Option<String> }`,
       converting into `crate::sandbox::ApprovalCheck` (`AutoDenied` requires
       a `reason`; validate/default it if a config omits `reason` for
       `auto_denied`).
3. [x] Add a `SandboxRuleConfig` enum with two variants, both deserializing
       from one `rules` list entry:
       - `Named { rule: NamedSandboxRule, verdict: ApprovalCheckConfig }`
       where `NamedSandboxRule` is `OutsideWorkspace | DotPath` (serde
       string values `"outside_workspace"`/`"dot_path"`).
       - `Glob { pattern: String, verdict: ApprovalCheckConfig }` (the
       existing custom-glob-rule shape, now config-parseable).
       Use an untagged or externally-tagged serde representation that
       distinguishes the two by the presence of `rule` vs. `pattern` —
       pick whichever `serde(untagged)`/manual `Deserialize` approach reads
       most naturally as TOML (test round-tripping both variants).
4. [x] Extend `ProfileConfig` (`src/config/profile.rs`) with
       `#[serde(default)] pub tools: HashMap<String, Vec<SandboxRuleConfig>>`,
       populated from `[tool.<name>]` tables' `rules` key. Document the new
       field's doc comment referencing `docs/configuration.md`'s Sandbox
       section.
5. [x] Refactor `SandboxContext::check_path_with_dot_path_access`
       (`src/sandbox.rs`) so the two structural checks
       (workspace-boundary, dot-path) become two small, named,
       independently-callable evaluator functions (e.g.
       `evaluate_outside_workspace_rule`/`evaluate_dot_path_rule`, taking
       the same inputs `check_path_with_dot_path_access` already computes:
       `workspace_root`, `effective` path, `allow_dot_paths`), returning
       `Option<ApprovalCheck>` each. Both:
       - the **unconditional default-floor path** (no `[tool.<name>]`
       config for this tool) calls both evaluators directly with the
       hardcoded `AutoDenied` verdicts (today's exact behavior), and
       - the **named-rule config path** (`SandboxRuleConfig::Named`) calls
       the same evaluator functions but with the *configured* verdict
       instead of the hardcoded one.
       must share the same evaluator implementation — no duplicated logic.
6. [x] Wire resolved per-tool rule lists from `ProfileConfig.tools` into
       `SandboxContext` construction. In `default_registry`
       (`src/agent/tools.rs`), for each tool: if `profile.tools` has an
       entry for that tool name, build its `SandboxContext` rule list
       entirely from that entry (converting each `SandboxRuleConfig` into
       either a call to a named evaluator or a `SandboxRule{pattern,verdict}`);
       otherwise fall back to the two default-floor rules from step 5,
       exactly as today.
7. [x] Reconcile `--auto-approve-write`/`--auto-approve-edit` with the new
       config-driven rule lists: each flag still appends its own catch-all
       `AutoApproved` rule (`pattern = "**"`) **after** whichever rule list
       now applies (config-provided or default-floor) for that tool —
       preserve the existing `with_rule(...)` call in `default_registry`
       for this purpose, just ensure it runs after the config-derived rules
       are appended, not before.
8. [x] Re-run the characterization tests from step 1 (and all pre-existing
       `src/sandbox.rs` tests) **unmodified** — they must still pass
       byte-for-byte after the refactor. If any requires a change, treat
       that as a signal of an accidental behavior change; investigate and
       fix the refactor rather than the test, unless the change is
       deliberate and justified (and if so, document why in this plan
       before proceeding).
9. [x] Add new tests for the config-driven path:
       - A `[tool.read]` rule list overriding the default `dot_path` rule
       (e.g. changing its verdict to `user_approval_required`) for `read`
       only, leaving `write`/`edit` unaffected (still using the default
       floor).
       - A profile with no `[tool.*]` tables at all falls back to the two
       built-in defaults for every tool (regression-confirms step 6's
       fallback branch).
       - A `[tool.write]` list mixing a `Named` rule and a custom `Glob`
       rule, confirming list-order/last-match-wins evaluation across the
       two variant kinds.
       - `--auto-approve-write` still auto-approves even when `[tool.write]`
       is configured with rules that would otherwise deny a path (catch
       -all still wins, appended last).
10. [x] Update `docs/configuration.md`'s "Sandbox" section: replace the
        "Phase 6 hardcodes two rules... **WIP:** These two rules are not yet
        configurable" language with documentation of the new `[tool.*]`
        config table (both rule-entry shapes, the default-floor-vs-full
        -replace behavior, and how CLI auto-approve flags compose with it).
11. [x] Update `docs/blueprint.md` §7 Phase 10: flip checklist item 9 from
        `- [ ]` to `- [x]` once completed and verified.
12. [x] Run `make lint`, `make test`, and `make test-coverage`; fix any
        finding before reporting this plan done.
13. [x] Run `rumdl fmt` on `docs/configuration.md` and any other `docs/*.md`
        file edited in steps 10-11.

## Implementation Hints

- `src/sandbox.rs`: `SandboxRule { verdict: ApprovalCheck, pattern:
  glob::Pattern }`, `SandboxContext::with_rule` (appends, last match wins),
  `ApprovalCheck` (`AutoApproved`/`AutoDenied { reason }`/
  `UserApprovalRequired`, currently no `Deserialize` impl). The two
  hardcoded rules live inline in `check_path_with_dot_path_access` (around
  lines 299-322 as of this writing) — this is what step 5 must lift into
  reusable evaluator functions while preserving the `allow_dot_paths` bypass
  parameter used by `glob` enumeration (`check_path_allowing_dot_paths`).
- `src/agent/tools.rs::default_registry` (around lines 617-680 as of this
  writing) is where each tool's `SandboxContext` is built today, including
  the `auto_approve_write`/`auto_approve_edit` inline `with_rule(...)`
  calls this plan's step 7 must reconcile with config-driven rule lists —
  study that function's current per-tool construction blocks (`read`,
  `write`, `glob`, `bash`, `search`, `edit`) before changing it.
- `docs/configuration.md`'s current "Sandbox" section already documents
  today's hardcoded two-rule behavior and its own stale "WIP... Phase 9 is
  expected to load per-tool `[tool.*]` rule lists" note (a stale
  phase-number reference — this work is actually Phase 10.3 per the
  current `docs/blueprint.md`) — read it fully, then replace it per step
  10.
- `docs/blueprint.md` §4E (referenced from the Sandbox section) describes
  the intended `outside_workspace`/`dot_path` override mechanism at a
  conceptual level; this plan's `NamedSandboxRule` enum
  (`OutsideWorkspace`/`DotPath`) is the concrete realization of "including
  `outside_workspace`/`dot_path` overrides" from the Phase 10 checklist
  text.
- `ProfileConfig` (`src/config/profile.rs`) already deserializes
  `HashMap<String, AgentConfig>` for `agents` — the new `tools:
  HashMap<String, Vec<SandboxRuleConfig>>` field follows the same pattern,
  just for `[tool.<name>]` tables instead of `[agents.<name>]`.
- Coverage/lint gates: `make lint` (clippy + check, `#[expect(...)]` not
  `#[allow(...)]` for any justified suppression), `make test`, `make
  test-coverage` (≥80%, excludes `tui_widgets.rs`/`render.rs`/`main.rs`).
- `Cargo.toml` already has `toml = "1.1.5"`, `serde`/`serde_json`, and
  `glob` — no new dependency is anticipated for this plan.
- This plan's `[tool.*]` config currently only benefits from whatever
  `config.toml` resolution is active (Phase 7's home-only loader, or
  `phase10.1`'s three-layer chain if that plan has already landed) — no
  code in this plan needs to special-case which resolution is in effect;
  it only ever reads from the already-resolved `ProfileConfig`.
