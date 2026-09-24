# Add `cross_filesystem` named path rule to the read tool sandbox

## Context

Ylva's read tool asks `SandboxContext::check_path` for a verdict before reading; `None`
(abstention) is interpreted as auto-approval. The sandbox already implements path
normalization, best-effort canonicalization, configured-rule evaluation with ordered
last-match-wins semantics, a default floor (`outside_workspace`, `dot_path`), agent-scoped
rule overrides, and a CLI catch-all rule that always evaluates last.

The remaining work (per `docs/ideas/read-tool-auto-approve.md`) is a configurable
**cross-filesystem policy**: a new path-based named rule `cross_filesystem` that matches
when the candidate path lives on a different filesystem (device) than the canonical
workspace root. It must be usable in the read tool's ordered rule list:

```toml
[tool.read]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { rule = "dot_path", verdict = "auto_denied" },
  { rule = "cross_filesystem", verdict = "user_approval_required" },
]
```

Motivation: catch reads of files on mounted shares/volumes that `outside_workspace`
cannot see, because the path may still be lexically inside the workspace (e.g. a mount
point under the workspace root).

Settled design decisions (grilling round 1–2; no open questions remain):

- **Platform semantics**: ylva currently targets macOS and Linux only. The
  `CrossFilesystem` variant parses on every platform (configs must parse identically),
  but device comparison uses unix `st_dev`; on any non-unix target the comparison
  always reports "same device", so the rule never matches there.
- **Metadata-unavailable behavior**: if neither the target nor any ancestor yields
  metadata, or the workspace root's device cannot be read, the rule does **not** match
  — it fires only on a positively established device difference. `outside_workspace`
  and `dot_path` remain the boundary guards.
- **Seam shape**: pure helpers — `resolve_device(path) -> Option<u64>` and a pure
  `is_cross_device(Option<u64>, Option<u64>) -> bool`; no mutable/injectable state.
- **Resolution timing**: both the workspace root's device and the candidate anchor's
  device are resolved **per `check_path` call** (consistent with the existing
  defensive re-canonicalize; no caching on `Sandbox`, no invalidation concerns —
  a mid-session remount cannot leave stale IDs because both sides are statted fresh
  within the same call).
- **Rule scope**: generic named path rule, available to every path tool (read, glob,
  write, edit, search) — not special-cased to `[tool.read]`.
- **Default floor**: `cross_filesystem` is opt-in only; the default floor is
  unchanged (blueprint: "Not configured by default").
- **Test fixture**: both pure helper tests with injected device ids **and** one real
  unix cross-device test using `/dev` (devfs/tmpfs differs from the workspace device;
  no mounts required).
- **No ADR**: `docs/architecture-design-records/` is empty; nothing is deviated from.
  Decision trail lives in `docs/configuration.md` and this plan.
- **Stale docs fixed in this change**: `README.md` feature checkbox and the dangling
  `docs/plans/bash-rules-plan.md` pointer in `src/sandbox.rs`.

Acceptance criteria / definition of done:

- `cross_filesystem` parses as a named rule in `[tool.read]`, in agent-scoped
  `[agents.<type>.tools.read]`, and under any path tool accepted by profile validation.
- The rule compares the candidate path's device against the canonical workspace root's
  device, using the deepest existing ancestor when the target itself does not exist.
- All three verdicts (`auto_approved`, `auto_denied`, `user_approval_required`) and
  optional custom `reason` work; a default denial reason exists when `auto_denied` is
  configured without one.
- Ordered last-match-wins evaluation is preserved: a configured rule list still fully
  replaces the default floor; the CLI catch-all still wins last; no-match still abstains
  (read tool → auto-approve).
- The rule is **not** part of the default floor.
- Unknown metadata ⇒ rule does not match; non-unix ⇒ rule never matches; both
  behaviors documented.
- Tests cover: pure device helpers, same-device paths, cross-device paths, missing
  targets, each verdict, rule ordering, and agent-scoped configuration.
- Docs updated in the same change set, including the two stale-doc fixes (see step 7).
- `make format lint` and `make test-coverage` pass (≥80% line coverage threshold).

Key constraints:

- Binary + library split: anything `tests/` needs must be re-exported via `src/lib.rs`
  (existing `sandbox` items already are).
- `SandboxContext::canonicalize_best_effort` is shared with `src/store.rs`
  (`project_key`) and `docs/persistence.md` documents it — its public behavior must not
  change; extend via new sibling helpers instead.
- The `CrossFilesystem` serde variant must exist on all platforms; only the comparison
  behavior may be platform-gated (never the enum variant).
- Test quirks per `docs/known-test-issues.md`: tracing subscriber lock
  (`SUBSCRIBER_LOCK` / `run_locked`), cwd-reading tests must hold the lock,
  integration tests cannot see `cfg(test)`.

Starting point: open `src/sandbox.rs`, read
`check_path_with_dot_path_access` (rule walk around lines 773–876) and
`evaluate_named_rule` (lines 753–771); first test to run:
`cargo test --lib sandbox::tests::check_path_outside_workspace_is_denied`.

An implementing agent must flip `[ ]` to `[x]` on each step immediately after completing
it and rewrite this file in place, so progress is resumable across sessions.

## Implementation Plan

1. [ ] Add `CrossFilesystem` variant to `NamedSandboxRule` in `src/sandbox.rs`
       (snake_case serde makes it parse as `"cross_filesystem"`), add a
       `CROSS_FILESYSTEM_REASON` default constant following the existing reason-constant
       pattern, dispatch it in `ResolvedRule::from_config`'s default-reason match, and
       add the variant to `SandboxRuleConfig::is_path_only()` so it is accepted under
       path tools. Update the enum's doc comment ("six structural checks" → seven).

2. [ ] Implement device comparison helpers in `src/sandbox.rs`:
       - `resolve_device(path: &Path) -> Option<u64>`: stat the path (following the
         already-canonicalized form) and return `MetadataExt::dev()` on unix; on
         non-unix always return `None` (so comparison never reports a difference);
       - pure `is_cross_device(candidate: Option<u64>, workspace_root: Option<u64>) -> bool`:
         `matches!((candidate, workspace_root), (Some(a), Some(b)) if a != b)` — any
         `None` on either side ⇒ `false` (metadata-unavailable ⇒ no match, per settled
         decision);
       - existing-ancestor handling: the candidate side must stat the deepest existing
         ancestor for missing targets — reuse the ancestor-walk logic already present
         in `canonicalize_best_effort` by adding a sibling helper that returns the
         canonicalized existing anchor (do **not** change
         `canonicalize_best_effort`'s signature/behavior; `src/store.rs::project_key`
         and `docs/persistence.md` depend on it).

3. [ ] Wire `CrossFilesystem` into evaluation:
       extend `evaluate_named_rule` (or the call site in
       `check_path_with_dot_path_access`) so the path-rule dispatch resolves both
       devices **per call** — workspace root: defensively re-canonicalized path +
       `resolve_device`; candidate: `resolve_device` of the existing-ancestor anchor —
       and reports a match when `is_cross_device` is true. Preserve ordering semantics
       exactly: configured-list/floor walk first, CLI catch-all glob last, last match
       wins, no match → `None`. Verify bash-only defensive arms and that
       `check_command` is unaffected. No changes to `src/agent/tools/read.rs` (it
       already forwards `check_path` verdicts; `None` → auto-approve).

4. [ ] Update profile validation in `src/config/profile.rs`:
       extend the `expected` string in `validate_tool_sandbox_rules` (currently
       ``"a `pattern` path-glob rule or a named path rule (`outside_workspace`/`dot_path`)"``)
       to mention `cross_filesystem`, and refresh the function's doc comment if it lists
       named path rules. No config struct shape changes are needed.

5. [ ] Tests — helpers and sandbox evaluation (`src/sandbox.rs` `#[cfg(test)]` module,
       follow existing `check_path_*` naming):
       - pure `is_cross_device` table test: equal ids ⇒ false; different ids ⇒ true;
         any `None` ⇒ false; both `None` ⇒ false;
       - `resolve_device` on an existing file returns `Some` on unix (and `None`
         behavior on non-unix if reachable); nonexistent path returns `None`;
       - same-device path inside workspace → `cross_filesystem` does not match
         (abstains, or yields the later/other rule's verdict);
       - cross-device path → matches: `#[cfg(unix)]` test stat-ing a path under `/dev`
         vs a workspace tempdir (real different device, no mounts);
       - missing target: nonexistent path under workspace (same device → no match) and
         nonexistent path whose ancestor is `/dev` (cross-device → match,
         `#[cfg(unix)]`);
       - each verdict: `auto_approved`, `auto_denied` (default reason asserted), and
         `user_approval_required`;
       - ordering: last-match-wins against `outside_workspace`/`dot_path` and a path
         glob in both orders; default floor unchanged (rule absent from floor);
       - CLI catch-all precedence: `--auto-approve-read`-style `**` rule still wins
         over a configured `cross_filesystem` deny/approval rule;
       - no-match abstention: unconfigured tool still falls back to default floor and
         a normal in-workspace path still returns `None`.

6. [ ] Tests — configuration and wiring:
       - `src/config/profile.rs`: TOML parse test asserting
         `SandboxRuleConfig::Named { rule: CrossFilesystem, .. }` loads from a
         `[tool.read] rules` table; hard-error tests confirming `cross_filesystem` is
         accepted under a path tool and rejected under `tool.bash` (path-only rule
         kind);
       - agent-scoped override: `[agents.<type>.tools.read]` with a `cross_filesystem`
         rule takes precedence over the profile-level list (mirror the existing
         agent-scoped test around `src/config/profile.rs` / `src/config/agent.rs`);
       - `src/agent/tools.rs`: registry-wiring test asserting the read tool's
         `needs_approval` reflects a configured `cross_filesystem` rule (mirror
         `default_registry_wires_configured_rules_per_tool_only`);
       - read-tool level: a `src/agent/tools/read.rs` test that a cross-device path
         yields `UserApprovalRequired` when so configured (async tests use
         `run_locked` per known-test-issues).

7. [ ] Documentation updates (same change set; docs are part of the commit):
       - `docs/configuration.md`: add `cross_filesystem` to the named-rule list
         (lines ~348–351), the default-floor table note (rule is opt-in, not in the
         floor), a `[tool.read]` example showing it, semantics (device comparison via
         unix `st_dev`, best-existing-ancestor for missing targets, verdicts,
         metadata-unavailable ⇒ no match, non-unix ⇒ never matches);
       - `docs/features.md`: extend the tool-approval / read tool sections (~72–77,
         184–210) with the new opt-in rule;
       - `docs/implementation-notes.md`: document the device-comparison helpers and
         per-call evaluation wiring;
       - `docs/blueprint.md` §4E (lines ~1383–1419): align the specified syntax with
         the shipped syntax and tick/adjust the Phase 10 checkbox if it tracks this
         rule;
       - `CONTEXT.md`: only if the glossary needs a term (likely a sentence on the
         Sandbox entry);
       - `README.md`: check the stale `- [ ] Per-tool configurable sandbox approval
         rules` checkbox (settled: feature exists, fix it);
       - `src/sandbox.rs` line ~163: fix the dangling doc pointer to the nonexistent
         `docs/plans/bash-rules-plan.md` (remove it or point at this plan — settled:
         fix it);
       - `docs/ideas/read-tool-auto-approve.md`: no edit required (it is the
         requirement input).

8. [ ] Verification: run `make format lint` and `make test-coverage`; fix any coverage
       threshold failures or lint findings. No ADR is required (settled: no recorded
       decisions are deviated from).

## Implementation Hints

- Rule evaluation pipeline: `SandboxContext::check_path_with_dot_path_access`,
  `src/sandbox.rs:773–876` — defensive re-canonicalize of `workspace_root` at line
  785, lexical normalize → best-effort canonicalize → rule walk (configured list or
  default floor, then CLI catch-all `**` glob), last match wins, `None` = abstain.
- Named-rule dispatch: `evaluate_named_rule`, `src/sandbox.rs:753–771`; path variants
  `OutsideWorkspace` (728–730) and `DotPath` (739–747).
- Enum/serde: `NamedSandboxRule` `src/sandbox.rs:102–131`,
  `#[serde(rename_all = "snake_case")]` → new variant auto-parses as
  `cross_filesystem`. `ApprovalVerdictConfig` (137–157) already covers all three
  verdicts; `SandboxRuleConfig` untagged variants at 267–294.
- Classification helpers to extend: `is_path_only()` `src/sandbox.rs:319–330`;
  default-reason dispatch in `ResolvedRule::from_config` `src/sandbox.rs:407–414`;
  reason constants at `src/sandbox.rs:58–100`.
- Best-existing-ancestor logic already exists inside
  `SandboxContext::canonicalize_best_effort` (`src/sandbox.rs:648–687`): canonicalize,
  else walk up to deepest existing ancestor, canonicalize it, reattach the tail. Add a
  sibling that exposes the anchor; do not modify this function.
- Workspace root: `Sandbox::new`/`with_root` (`src/sandbox.rs:441–471`) already
  canonicalize; `workspace_root` is re-canonicalized defensively inside the check
  pipeline.
- `st_dev` facts: `MetadataExt::dev()` returns the kernel's opaque numeric device ID
  of the mount instance; only equality is ever compared (never interpreted/converted
  to a string). IDs are stable while the mount is unchanged but may differ after an
  unmount/remount — which is why both sides are resolved per `check_path` call rather
  than cached. Bind mounts / repeated mounts of one volume may or may not share an ID,
  OS-dependent; the per-call fresh stat of both sides is the consistency mechanism.
- Config validation: `validate_tool_sandbox_rules`, `src/config/profile.rs:106–159`;
  `expected` string for path tools at 139–140.
- Config resolution tiers (most-specific-wins, full replace, no merging):
  `src/agent_factory.rs:317–350`; registry wiring `sandbox_context_for` closure
  `src/agent/tools.rs:701–713` (applies `with_configured_rules`, then catch-all).
- Read tool approval: `src/agent/tools/read.rs:380–405` — `None` → `AutoApproved`;
  explicit verdicts honoured as-is. No `read.rs` changes needed for this feature.
- Cross-device fixture without mounts: `/dev` (devfs/tmpfs) is a different device from
  a normal workspace on macOS and Linux; existing unix-only tests use
  `#[cfg(unix)]` (e.g. `src/sandbox.rs:1251`).
- No new dependency needed: no `same-file`/`nix` addition required for `dev()`.
- Existing tests to mirror: `configured_rules_mix_named_and_glob_last_match_wins`
  (`src/sandbox.rs:1801`), `auto_approve_catch_all_wins_over_configured_deny_rule`
  (`src/sandbox.rs:1856`), `tool_sandbox_rules_table_loads_named_and_glob_rules`
  (`src/config/profile.rs:806`), `backgrounded_named_rule_under_a_path_tool_is_a_hard_error`
  (`src/config/profile.rs:1018`), agent-scoped override test
  (`src/config/profile.rs:1058–1086`), `default_registry_wires_configured_rules_per_tool_only`
  (`src/agent/tools.rs:1028`).
- Test quirks (`docs/known-test-issues.md`): tests touching cwd/`Sandbox::new` or
  `capture_logs`-exercised functions must hold `SUBSCRIBER_LOCK` / use `run_locked`;
  integration tests cannot rely on `cfg(test)`.
- Docs facts: named-rule list documented at `docs/configuration.md:348–351`; floor
  table at 317–320; floor-replacement warning at 363–368; agent override at 370–415;
  blueprint spec of `cross_filesystem` at `docs/blueprint.md:1400–1419` ("Not
  configured by default (sandbox abstains)").
- Verification gate: `make format lint` + `make test-coverage` (≥80% line coverage
  on testable files) per `docs/development-workflow.md`; do not also run `make test`
  as a mandatory duplicate.
- `docs/plans/` and `docs/architecture-design-records/` currently contain only
  `.gitkeep`.
