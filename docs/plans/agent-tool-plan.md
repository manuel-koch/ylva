# Per-Agent-Type Tool Sandbox Rule Configuration

Implementing agent: flip each `[ ]` to `[x]` immediately after completing
that step and rewrite this file in place, so progress is resumable across
sessions.

## Context

Ylva (a Rust TUI agent-harness) already lets a profile restrict *which*
tools an agent type may call, via `AgentConfig::allowed_tools`
(`src/config/agent.rs`, `[agents.<type>]` tables, documented in
`docs/configuration.md`'s "Per-agent configuration (`AgentConfig`)"
section) — this is the `tools_available` half of
`docs/ideas/agent-tools.md`'s request and is fully implemented already
(verified in `src/agent_factory.rs::build` and `src/agent/tools.rs`).

What is **not** implemented is the second half of that idea: letting an
agent type configure its own **sandbox tool rules** (the `[tool.<name>]`
`rules` lists documented in `docs/configuration.md`'s "Sandbox" section
and implemented by `docs/plans/phase10.3-sandbox-tool-rules.md`). Today
`[tool.<name>]` tables exist only at the **profile** level
(`ProfileConfig::tools: HashMap<String, ToolSandboxConfig>`,
`src/config/profile.rs`) and apply uniformly to every agent type in that
profile — an agent type cannot narrow or loosen a tool's rules for itself
only. `docs/ideas/agent-tools.md` asks for exactly this: an `[agents.
<type>.tools.<tool>]` TOML table (and its Markdown/YAML frontmatter
equivalent), e.g.:

```toml
[agents.custom]
description = "Custom agent definition."
engine = "ollama/local"
allowed_tools = ["read", "write"]

[agents.custom.tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { pattern = "**/build/**", verdict = "auto_approved" },
]
```

```yaml
---
engine: "ollama/local"
allowed_tools: ["read", "write"]
tools:
  write:
    rules:
      - rule: "outside_workspace"
        verdict: "auto_denied"
      - pattern: "**/build/**"
        verdict: "auto_approved"
---
You are a utility agent with customized tool config.
```

(Both snippets are verified syntactically valid TOML/YAML and use the
already-shipped `allowed_tools` field name and the `ToolSandboxConfig
{ rules: Vec<SandboxRuleConfig> }` shape consistently — no further
syntax fixes needed in `docs/ideas/agent-tools.md`.)

### Goal & motivation

Allow a specific agent type (e.g. a restricted "custom"/utility agent) to
have its own tool-approval rules that differ from the profile's global
`[tool.<name>]` rules or the built-in default floor, without changing
behavior for any other agent type in the same profile. This lets a
profile author give one agent type broader auto-approval (e.g. inside a
scratch directory) or stricter rules (e.g. deny all writes) while leaving
the root agent and other agent types on the profile default.

### Acceptance criteria

- `AgentConfig` (`src/config/agent.rs`) gains a new field,
  `tools: HashMap<String, crate::config::ToolSandboxConfig>`, populated
  from an agent type's `[tools.<name>]` tables (inline `[agents.<type>.
  tools.<name>]` in `config.toml`, or a top-level `[tools.<name>]`/
  `tools: { <name>: ... }` table in a file-authored `agents/<type>.toml`,
  `.md` `+++` TOML frontmatter, or `.md` `---` YAML frontmatter).
- `AgentConfigOverrides` (`src/config/agent_type_file.rs`) gains the same
  `tools` field so inline `[agents.<type>]` profile tables can also
  configure it; `apply_agent_config_overrides`
  (`src/config/profile.rs`) merges it using the same "present replaces
  entirely" semantics already used for `allowed_tools`
  (`Option`/non-empty-map replaces, absent leaves the file-sourced value
  untouched — decide exact semantics in step 3 and record the choice in
  Implementation Hints once decided).
- **Resolution order for a given `(agent_type, tool_name)` pair**, most
  specific wins, each tier fully replacing the next (no field-by-field or
  rule-list merging across tiers, consistent with the existing
  profile-vs-default-floor "full replace" rule in
  `docs/configuration.md`'s "Sandbox" section):
  1. `profile.agents[agent_type].tools[tool_name]` (new, this plan), if
     present.
  2. `profile.tools[tool_name]` (existing profile-level `[tool.<name>]`),
     if present.
  3. The unconditional built-in default floor
     (`outside_workspace`/`dot_path` → `AutoDenied`).
- `AgentFactory::build` (`src/agent_factory.rs`, around the existing
  `tool_sandbox_rules` construction near line 336) resolves this
  per-agent-type override before building `ToolRegistryConfig`, so each
  spawned agent (root or child) gets a `SandboxContext` reflecting its own
  `agent_type`'s resolved rules — not the profile's flat map applied to
  every agent type.
- `--auto-approve-write`/`--auto-approve-edit` continue to append their
  catch-all `AutoApproved` rule **after** whichever tier applies (unchanged
  behavior, exercised by existing tests in `src/agent/tools.rs`/
  `src/sandbox.rs` — must still pass unmodified).
- New unit/integration tests cover:
  - An agent type's `[tools.<name>]` table overriding the profile's
    `[tool.<name>]` table for that tool, for that agent type only —
    another agent type in the same profile still uses the profile-level
    table.
  - An agent type with no `tools` entry for a given tool name falls back
    to the profile-level `[tool.<name>]` table when one exists.
  - An agent type with no `tools` entry and a profile with no
    `[tool.<name>]` table for that tool falls back to the built-in default
    floor (regression, unchanged from today).
  - The same override expressed via a file-authored `agents/<type>.toml`
    and via `.md` YAML frontmatter (`tools: { <name>: { rules: [...] } }`),
    mirroring the existing per-format tests in
    `src/config/agent_type_file.rs`.
  - An inline `[agents.<type>].tools` profile-table override on top of a
    file-sourced agent-type definition (field-level override, same pattern
    as `allowed_tools` today).
- `make lint`, `make test`, and `make test-coverage` all pass (≥80% line
  coverage) after the change.
- `docs/configuration.md` is updated in the same commit: the "Per-agent
  configuration (`AgentConfig`)" table gains a `tools` row, and the
  "Sandbox" section documents the new three-tier resolution order
  (agent-type table → profile table → default floor) alongside the
  existing profile-vs-default-floor description.
- `docs/ideas/agent-tools.md` is updated (or removed, per Open Questions
  below) once this plan lands, so it no longer reads as an open request.
- Loading a profile whose `[tool.<name>]` or `[agents.<type>.tools.<name>]`
  table (or an agent-type file's `[tools.<name>]`/`tools: { <name>: ... }`
  table) names a tool that is not one of the seven registered tool names
  (`read`, `write`, `glob`, `bash`, `search`, `edit`, `clarify`) is a hard
  startup error naming the offending agent type (or "profile-level" for
  `ProfileConfig.tools`), the unknown tool name, and its source path,
  mirroring `validate_profile_agent_engines`'s existing "unknown engine"
  validation in `src/agent_factory.rs`.

### Key constraints

- **Do not change existing behavior when a profile has no `[agents.
  <type>.tools.*]`/agent-type-file `tools` tables.** Every current
  `src/sandbox.rs`, `src/agent/tools.rs`, and `src/agent_factory.rs` test
  that exercises `tool_sandbox_rules`/`ToolRegistryConfig` must keep
  passing unmodified — a required change to any of them signals an
  accidental regression, not an intended one (same policy
  `docs/plans/phase10.3-sandbox-tool-rules.md` used for its own refactor).
- **Reuse `ToolSandboxConfig`/`SandboxRuleConfig`/`ApprovalCheck`
  as-is.** Do not introduce a second, differently-shaped rule
  configuration type for the agent-scoped case — the whole point is the
  same `{ rule = "...", verdict = "..." }`/`{ pattern = "...", verdict =
  "..." }` entries, just resolved from an additional, more specific
  config location.
- **`AgentTypeDefinition`/`AgentConfig` is shared by both TOML-file,
  Markdown-frontmatter (TOML and YAML), and inline-profile-table parsing
  paths** (`src/config/agent_type_file.rs`). The new `tools` field must
  deserialize correctly through all three paths, matching the existing
  `allowed_tools` field's precedent.
- **File-sourced base vs. inline-table override precedence is unchanged
  by this plan.** This plan only adds one more field to the existing
  merge/override machinery (`apply_agent_config_overrides`); it does not
  change which scope (project/profile/global) or source (file vs. inline
  table) wins — that precedence is untouched.
- **Coverage threshold ≥80%** (`make test-coverage`) applies to all new/
  modified non-UI files this plan touches.

### Concrete starting point

First file to open: `src/config/agent.rs` (`AgentConfig` struct — add the
`tools` field here first). First test to run: `cargo test --lib
config::agent::` and `cargo test --lib config::profile::
tool_sandbox_rules_table_loads_named_and_glob_rules` to confirm the
current profile-level-only baseline passes before changing anything.

## Implementation Plan

1. [ ] Add `pub tools: HashMap<String, ToolSandboxConfig>` (with
       `#[serde(default)]`) to `AgentConfig` (`src/config/agent.rs`),
       importing `ToolSandboxConfig` from `crate::config::profile` (or
       relocate `ToolSandboxConfig` to a shared location if that avoids a
       circular/awkward import — check current module layout first).
       Update every existing `AgentConfig { ... }` struct-literal
       construction site (`src/config/profile.rs`'s `default_agent_config`
       and inline-override fallback, `src/agent_factory.rs` test fixtures,
       any other call site found via `grep -rn "AgentConfig {"`) to include
       `tools: HashMap::new()`.
2. [ ] Add the matching `tools: Option<HashMap<String, ToolSandboxConfig>>`
       field (with `#[serde(default)]`) to `AgentConfigOverrides`
       (`src/config/agent_type_file.rs`), and extend
       `apply_agent_config_overrides` (`src/config/profile.rs`) to replace
       `config.tools` wholesale when `overrides.tools` is `Some`
       (non-empty-map presence check, mirroring the existing
       `if overrides.allowed_tools.is_some()` pattern) — do not merge
       individual tool entries between the file-sourced base and the
       inline override.
3. [ ] Add parsing tests in `src/config/agent_type_file.rs` (following the
       existing `pure_toml_agent_type_files_parse_settings_and_system_prompt`/
       `markdown_agent_type_files_parse_yaml_frontmatter_and_trimmed_body`
       style) proving: a pure-TOML agent-type file with a `[tools.write]`
       table, a `.md` `+++` TOML frontmatter file with the same, and a
       `.md` `---` YAML frontmatter file with `tools: { write: { rules:
       [...] } }`, all deserialize into `AgentConfig.tools["write"].rules`
       correctly.
4. [ ] Add a profile-level test in `src/config/profile.rs` (near
       `tool_sandbox_rules_table_loads_named_and_glob_rules`) proving an
       inline `[agents.<type>.tools.<name>]` table parses into
       `ProfileConfig::agents[<type>].tools[<name>].rules`, independent of
       `ProfileConfig::tools`.
5. [ ] In `AgentFactory::build` (`src/agent_factory.rs`, replacing the
       current unconditional `let tool_sandbox_rules = profile.tools...`
       block near line 336), resolve per-tool rules with the three-tier
       fallback: for each tool name appearing in either
       `profile.agents[agent_type].tools` or `profile.tools`, prefer the
       agent-type-specific entry when present, else the profile-level
       entry; tool names absent from both stay absent from the resulting
       map (so `default_registry`'s existing "absent → default floor"
       behavior in `src/agent/tools.rs` is unchanged and does not need to
       change).
6. [ ] Add a test in `src/agent_factory.rs` (near
       `create_honors_per_agent_type_allowed_tools_override`) proving: two
       agent types share one profile where only `[tool.write]` is set
       profile-wide, one agent type additionally sets
       `[agents.<type>.tools.write]` with a different rule list, and the
       resulting `SandboxContext`/registry behavior differs between the
       two agent types for `write` while both still share the profile
       default for any other tool.
7. [ ] Add `validate_profile_agent_tool_names` (or extend
       `validate_profile_agent_engines`'s call site) in
       `src/agent_factory.rs`, invoked from the same startup path
       (`validate_profile_agent_engines`'s caller, around line 153) that
       already hard-validates `agent_config.engine` references. For every
       tool name key found in `ProfileConfig.tools` and in each
       `profile.agents[<type>].tools`, `anyhow::ensure!` it is one of the
       seven registered tool names (reuse or mirror the name list
       `DEFAULT_CLI_TOOLS`/`DEFAULT_TUI_TOOLS` draw from in
       `src/agent/tools.rs`), returning an error naming the agent type (or
       "profile-level" for the profile-wide map), the unknown name, and
       -- where available -- the config source path, matching the message
       style of the existing "unknown engine" error. Add a test near
       `create_honors_per_agent_type_allowed_tools_override`/the existing
       unknown-engine test proving an unrecognized tool name in
       `[agents.<type>.tools.<bad-name>]` fails profile load/validation
       with a message naming the agent type and the bad tool name, and
       another proving an unrecognized name in the profile-level
       `[tool.<bad-name>]` table fails the same way.
8. [ ] Update `docs/configuration.md`: add a `tools` row to the "Per-agent
       configuration (`AgentConfig`)" table (cross-referencing the
       "Sandbox" section), and extend the "Sandbox" section's "Per-tool
       `[tool.<name>]` config" paragraph with the new three-tier
       resolution order (agent-type table → profile table → default
       floor), including a short TOML example showing
       `[agents.<type>.tools.<name>]`, and a note that both `[tool.<name>]`
       and `[agents.<type>.tools.<name>]` now hard-validate the tool name
       against the registered tool set at startup (step 7).
9. [ ] Resolve `docs/ideas/agent-tools.md`'s disposition per the Open
       Questions section below (delete it, or replace its content with a
       pointer to this plan/the shipped docs section) once implementation
       is complete.
10. [ ] Run `make lint`, `make test`, and `make test-coverage`; fix any
        finding before reporting this plan done.
11. [ ] Run `rumdl fmt docs/configuration.md` (and `docs/ideas/agent-tools.md`
        if step 9 edits rather than deletes it).

## Implementation Hints

- `src/config/agent.rs::AgentConfig` currently has: `engine`,
  `description`, `allowed_tools`, `timeout_secs`, `persona`,
  `system_prompt`, `can_spawn`. `allowed_tools: Option<Vec<String>>`
  already fully implements the idea file's `tools_available` request
  (verified end-to-end: parsed by `agent_type_file.rs`, merged by
  `profile.rs::apply_agent_config_overrides`, and enforced by
  `AgentFactory::build`/`default_registry` in `src/agent/tools.rs`) — no
  work needed there; this plan is scoped to the tool-*rules* half only.
- `src/config/profile.rs::ToolSandboxConfig { rules: Vec<SandboxRuleConfig> }`
  and `crate::sandbox::{SandboxRuleConfig, NamedSandboxRule,
  ApprovalVerdictConfig}` are the existing, reusable types for rule-list
  config; see `src/sandbox.rs` lines ~39-190 for their definitions and
  `Deserialize` impls.
- `ProfileConfig.tools: HashMap<String, ToolSandboxConfig>` uses
  `#[serde(rename = "tool")]` to map TOML `[tool.<name>]` tables (singular
  key) onto a field named `tools` (plural). The new `AgentConfig.tools`
  field should NOT use that rename — `[agents.<type>.tools.<name>]` and
  `tools: { <name>: ... }` (YAML) both naturally address a field literally
  named `tools`, unlike the profile-root case.
- `src/agent_factory.rs::AgentFactory::build` (around line 336) currently
  builds `tool_sandbox_rules` unconditionally from `profile.tools`, with
  no reference to `agent_type` at all — this is the exact code this plan's
  step 5 must change to also consult `profile.agents.get(agent_type)`.
  `agent_type` is already in scope as a `&str` parameter of `build`.
- `src/config/agent_type_file.rs::AgentConfigOverrides` mirrors
  `AgentConfig`'s field set one-for-one for inline `[agents.<type>]`
  tables (minus `system_prompt`, which files/frontmatter provide and
  inline tables do not). `apply_agent_config_overrides`
  (`src/config/profile.rs`) is the single merge function — this plan adds
  one more `if overrides.tools.is_some() { config.tools = overrides.tools
  .unwrap_or_default(); }`-shaped branch there, following the existing
  branches' style exactly.
- Existing regression tests to keep passing unmodified: `src/sandbox.rs`
  (`check_path_outside_workspace_is_denied`,
  `check_path_dot_path_is_denied`,
  `check_path_allowing_dot_paths_abstains_inside_workspace`,
  `check_path_glob_rule_overrides_to_explicit_verdict`),
  `src/config/profile.rs`
  (`tool_sandbox_rules_table_loads_named_and_glob_rules`,
  `a_profile_with_no_tool_tables_has_an_empty_tools_map`),
  `src/agent_factory.rs`
  (`create_honors_per_agent_type_allowed_tools_override`).
- `docs/architecture-design-records/` is currently empty; phase10.3
  considered but did not require an ADR for the profile-level `[tool.*]`
  mechanism. This plan is an incremental extension of that same mechanism
  and likewise does not appear to need a new ADR, but re-check
  `docs/architecture-design-records/` at implementation time in case one
  was added since this plan was written.
- `make lint`/`make test`/`make test-coverage` are the same gates
  `docs/development-workflow.md` and `phase10.3` used; no new dependency
  is anticipated (only existing `serde`/`toml`/`serde_yaml`/`glob`
  machinery already used by the sibling profile-level feature).

## Open Questions

- Should `docs/ideas/agent-tools.md` be deleted once this plan ships, or
  rewritten to point at the shipped `docs/configuration.md` section (the
  repo's convention for other resolved ideas, e.g.
  `docs/ideas/monokai-theme.md`/`docs/ideas/slash-commands.md`, is unclear
  from a quick scan — both are still referenced live from `docs/
  blueprint.md` as *unresolved* fallback notes, not examples of a resolved
  idea being retired). Resolve at implementation time by asking the user,
  or by checking whether any other previously-resolved `docs/ideas/*.md`
  file was deleted vs. kept as historical record.
