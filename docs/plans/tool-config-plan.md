# Unify Per-Tool Timeout Configuration and Optional Sandbox Rule Inheritance

## Context

Ylva configures per-tool behavior across two disconnected configuration surfaces today:

1. `[tools.<name>]` and `[agents.<type>.tools.<name>]` (plus file-authored agent-type
   `[tools.<name>]` / YAML `tools.<name>`) configure per-tool sandbox approval rules via
   `ToolSandboxConfig { rules: Vec<SandboxRuleConfig> }`. Because `rules` has `#[serde(default)]` as
   a bare `Vec`, any `[tools.<name>]` table that omits `rules` deserializes `rules = []`, silently
   replacing the lower tier's rules (or the built-in default floor) with an empty rule list. In
   addition, inline `[agents.<type>]` overrides currently replace the entire `AgentConfig::tools`
   map when `overrides.tools.is_some()`, rather than merging per tool and per configured field like
   `[agents.<type>.compaction]`.
2. Tool execution timeouts are inconsistently configured: `web_fetch` and `web_search` each carry
   `timeout_secs` (default `30`, silently clamping `0` to `1`) on their top-level `[web_fetch]` and
   `[web_search]` backend tables with no per-agent override, while `bash` hardcodes
   `DEFAULT_BASH_TIMEOUT_SECS = 300` with no configuration knob at all.

This change unifies tool execution timeouts and sandbox rule inheritance under `[tools.<name>]` and
`[agents.<type>.tools.<name>]` without introducing new top-level configuration sections:

- **Goal**:
  1. Move `timeout_secs` out of `[web_fetch]` and `[web_search]` into `[tools.<name>].timeout_secs`
     and `[agents.<type>.tools.<name>].timeout_secs` (including `.toml` and `.md` agent-type files),
     and support `timeout_secs` on `bash` (`300` s default), `web_fetch` (`30` s default), and
     `web_search` (`30` s default).
  2. Make `rules` optional (`Option<Vec<SandboxRuleConfig>>`) so omitting `rules` in a
     `[tools.<name>]` or `[agents.<type>.tools.<name>]` table inherits the lower tier's rules
     (`agent` → `profile` → built-in default floor, or `load_skill`'s auto-approving empty default),
     while an explicit `rules = []` or `rules = [...]` replaces the lower tier's rule list.
- **Motivation**: Long-running headless or delegated `bash` commands (for example multi-minute
  builds, test suites, or orchestration scripts) and specialized subagents need profile-level or
  agent-type-scoped tool timeouts without accidentally wiping out `bash`, `web_fetch`, or
  `web_search` sandbox rules—and without duplicating `rules` across tiers just to tune
  `timeout_secs`. Putting all per-tool timeouts in `[tools.<name>]` gives Ylva one consistent,
  Orthogonal Single Source of Truth (SSOT) for per-tool settings while keeping `[web_fetch]` and
  `[web_search]` focused strictly on backend selection and backend parameters.
- **Key constraints and invariants**:
  - **Safety-critical unknown-key rejection inside `[tools.<name>]`**: `[tools.<name>]` tables must
    retain `#[serde(deny_unknown_fields)]` so typos such as `rule = [...]` or `timeout_sec = 60`
    remain hard load errors rather than silently falling back to defaults.
  - **Stale `[web_fetch].timeout_secs` / `[web_search].timeout_secs` handling**: `[web_fetch]` and
    `[web_search]` deserialize through `deserialize_reporting_unknown_keys`, so removing
    `timeout_secs` from `WebFetchConfig` and `WebSearchConfig` must emit a standard `UnknownKey`
    `ConfigWarning` (and fail under `--strict-config`). `"timeout_secs"` remains in `KNOWN_KEYS`
    because `[agents.<type>]` and `[tools.<name>]` use it.
    Do not add compatibility aliases or migration handling for the removed settings.
  - **Strict `timeout_secs` validation (YAGNI + fail-fast)**:
    - `timeout_secs` is valid **only** on `bash`, `web_fetch`, and `web_search`. Setting
      `timeout_secs` on any other tool (for example `read`, `write`, `edit`, `glob`, `search`,
      `clarify`, `load_skill`) is a hard `ConfigError` during profile/agent-type validation naming
      the config path, scope (profile or agent type), and tool name.
    - `timeout_secs = 0` is a hard `ConfigError` during startup validation (consistent with
      `max_tool_iterations = 0` and `compaction.timeout_sec = 0`), replacing the previous silent
      `.max(1)` floor on `web_fetch` and `web_search`.
    - `${ENV_VAR}` string interpolation into numeric `timeout_secs` (for example
      `timeout_secs = "${BASH_TIMEOUT}"`) must work in `config.toml`, `.toml` agent-type files, and
      `.md` (TOML/YAML frontmatter) agent-type files, matching `[agents.<type>].timeout_secs`.
  - **Independent field inheritance across all tiers**:
    1. Inline `[agents.<type>.tools.<name>]` in `config.toml` merges onto a lower-scope
       file-authored `AgentTypeFile`'s `tools.<name>` entry per tool and per present field (`rules`
       if `Some`, `timeout_secs` if `Some`), preserving untouched tools and untouched fields on the
       same tool.
    2. Effective `rules` for `(agent_type, tool_name)` resolves `agent.tools[name].rules` →
       `profile.tools[name].rules` → built-in default floor (`None` passed to `SandboxContext`,
       except `load_skill` where `None` means empty rules). Explicit `Some(vec![])` (`rules = []`)
       replaces the lower tier with an empty rule list.
    3. Effective `timeout_secs` for `(agent_type, tool_name)` resolves
       `agent.tools[name].timeout_secs` → `profile.tools[name].timeout_secs` → built-in default
       (`300` for `bash`, `30` for `web_fetch`, `30` for `web_search`).
  - **Backend HTTP timeout vs. outer tool timeout**: For `web_fetch` and `web_search`, the resolved
    `timeout_secs` is the per-request HTTP backend timeout, and the `Tool::timeout_secs()` outer
    guard remains `timeout_secs + OUTER_TIMEOUT_GRACE_SECS` (`+ 10` s) so the HTTP client surfaces a
    structured timeout error before the outer cancellation guard fires. Because `AgentFactory`
    builds a `ToolRegistry` per agent, per-agent `web_fetch` and `web_search` timeouts must flow
    into `WebFetchConfig::build_backend` / `WebSearchConfig::build_backend` as well as
    `WebFetchTool::new` / `WebSearchTool::new`.
- **Acceptance criteria**:
  - `[tools.bash].timeout_secs`, `[tools.web_fetch].timeout_secs`, and
    `[tools.web_search].timeout_secs` configure execution timeouts at profile scope and can be
    overridden per agent type via `[agents.<type>.tools.<name>].timeout_secs` or agent-type files
    (`.toml` and `.md` TOML/YAML frontmatter), including `${ENV_VAR}` interpolation.
  - Omitting `rules` in `[tools.<name>]` or `[agents.<type>.tools.<name>]` (for example
    `[tools.bash]\ntimeout_secs = 900` or an empty `[tools.read]` table) preserves lower-tier rules
    or the built-in default floor; explicit `rules = []` clears them; explicit `rules = [...]`
    replaces them.
  - Inline `[agents.<type>.tools.<name>]` overrides merge per tool and per present field onto
    lower-scope agent-type files without discarding other configured tools or fields.
  - `timeout_secs = 0` and `timeout_secs` on tools other than `bash`, `web_fetch`, and `web_search`
    fail profile loading with descriptive `ConfigError` variants.
  - `timeout_secs` is no longer accepted on `[web_fetch]` or `[web_search]` and produces an
    unknown-key `ConfigWarning` when present.
  - All affected unit/integration tests, offline replay helpers
    (`tests/sandbox_evaluation/bash_policy_replay.rs`), and documentation (`docs/configuration.md`,
    `docs/features.md`, `docs/diagrams/configuration-policy-flow.mmd`) are updated and pass
    `rumdl check`, `make format lint`, and `make test-coverage`.
- **Concrete starting point**:
  - Open `src/config/profile.rs` around `ToolSandboxConfig` and `validate_tool_sandbox_rules`, plus
    `src/config/agent.rs` (`deserialize_optional_u64_or_string`).
  - Run `cargo test --lib config::profile::tests` to verify the baseline config test suite before
    making edits.

## Implementation Plan

As you implement this plan, flip each completed step's `[ ]` checkbox to `[x]` immediately after
finishing that step and rewrite `docs/plans/tool-config-plan.md` in place so progress remains
resumable across sessions.

1. [x] Extend per-tool config schema with optional `rules` and `timeout_secs`, and merge inline agent
   tool overrides per field.
   1. [x] In `src/config/profile.rs` (and re-exports in `src/config.rs`), rename `ToolSandboxConfig`
          to `ToolConfig` across the crate and `tests/sandbox_evaluation/bash_policy_replay.rs`
          (retaining `#[serde(deny_unknown_fields)]`), change `rules` to
          `Option<Vec<SandboxRuleConfig>>` (`#[serde(default)]`), and add
          `timeout_secs: Option<u64>`
          (`#[serde(default, deserialize_with = "deserialize_optional_u64_or_string")]`).
   2. [x] Make `deserialize_optional_u64_or_string` `pub(super)` in `src/config/agent.rs`
          (deduplicating the copy in `src/config/agent_type_file.rs`) so `AgentConfig`,
          `AgentConfigOverrides`, and `ToolConfig` share one SSOT deserializer across TOML and YAML
          frontmatter.
   3. [x] Update `apply_agent_config_overrides` in `src/config/profile.rs` so `overrides.tools`
          merges per tool name into `config.tools`, overwriting `entry.rules` only when
          `override_tool.rules.is_some()` and `entry.timeout_secs` only when
          `override_tool.timeout_secs.is_some()`.
   4. [x] Add `ProfileConfig` helper methods for effective per-tool resolution given
          `agent_type: Option<&str>` and `tool_name: &str` (for example `effective_tool_rules`
          returning `Option<&[SandboxRuleConfig]>` and `effective_tool_timeout_secs` returning
          `Option<u64>`, or typed per-tool timeout getters), resolving `agent.tools[tool]` →
          `profile.tools[tool]`.
   5. [x] Interpolate agent-type file configuration values with the resolved profile environment,
          including numeric tool timeouts in TOML and YAML frontmatter; keep Markdown bodies
          literal.

2. [x] Validate per-tool `timeout_secs` and remove `timeout_secs` from `WebFetchConfig` and
   `WebSearchConfig`.
   1. [x] Add `ConfigError` variants in `src/config.rs` for unsupported `timeout_secs` on a tool
          outside `{"bash", "web_fetch", "web_search"}` and for `timeout_secs = 0`, including the
          config `path`, scope description (profile `[tools.<tool>]` vs. agent type
          `[agents.<agent>.tools.<tool>]`), and `tool` name.
   2. [x] Extend tool config validation in `src/config/profile.rs` (`validate_tool_sandbox_rules` /
      `validate_tool_configs`, called in both `load_file_with_source` and
      `load_agent_types_into_profile` for profile-level and every agent's `tools` map) to:
      - iterate `tool_config.rules.as_deref().unwrap_or(&[])` for existing sandbox rule validation,
        and
      - reject `timeout_secs` on unsupported tool names and reject `timeout_secs == Some(0)`.
   3. [x] In `src/config/web_fetch.rs` and `src/config/web_search.rs`, remove the `timeout_secs`
          field and `http_timeout(&self)` method from `WebFetchConfig` and `WebSearchConfig`, expose
          `pub const DEFAULT_WEB_FETCH_TIMEOUT_SECS: u64 = 30` and
          `pub const DEFAULT_WEB_SEARCH_TIMEOUT_SECS: u64 = 30` as the single sources of truth, and
          change `build_backend(&self, http_timeout: Duration)` on both structs to accept the
          resolved HTTP timeout from the caller.

3. [x] Wire resolved tool timeouts and optional `rules` through `BashTool`, `ToolRegistryConfig`,
   `AgentFactory`, `skills::policy`, CLI display registry, and `bash_policy_replay`.
   1. [x] In `src/agent/tools/bash.rs`, export `pub const DEFAULT_BASH_TIMEOUT_SECS: u64 = 300`, add
          a `timeout_secs: u64` field to `BashTool` and `BashCall` (defaulting to
          `DEFAULT_BASH_TIMEOUT_SECS` in `BashTool::new` and customizable via
          `BashTool::with_timeout(timeout_secs, sandbox_context)`). Preserve the existing
          `BASH_OUTER_TIMEOUT_GRACE_SECS` on `ToolCall::timeout_secs` so the inner timeout can
          return partial output before the outer cancellation guard fires.
   2. [x] In `src/agent/tools.rs`, add per-tool timeout fields to `ToolRegistryConfig`
          (`bash_timeout_secs: u64`, `web_search_timeout_secs: u64`, `web_fetch_timeout_secs: u64`,
          defaulting in `ToolRegistryConfig::default()` to `DEFAULT_BASH_TIMEOUT_SECS`,
          `DEFAULT_WEB_SEARCH_TIMEOUT_SECS`, and `DEFAULT_WEB_FETCH_TIMEOUT_SECS`), and update
          `default_registry` to construct `BashTool::with_timeout(config.bash_timeout_secs, ...)`,
          pass `Duration::from_secs(config.web_search_timeout_secs)` into
          `config.web_search.build_backend(...)` and `WebSearchTool::new(...)`, and pass
          `Duration::from_secs(config.web_fetch_timeout_secs)` into
          `config.web_fetch.build_backend(...)` and `WebFetchTool::new(...)`.
   3. [x] In `src/agent_factory.rs`, update `tool_sandbox_rules` assembly so only tools with
          `Some(rules)` (resolving `agent.tools[name].rules` then `profile.tools[name].rules`) are
          inserted into `ToolRegistryConfig::tool_sandbox_rules`—preserving the built-in default
          floor when `rules` is `None` and passing an empty `Vec` when `rules` is `Some(vec![])`—and
          pass the resolved per-agent `bash`, `web_search`, and `web_fetch` timeouts into
          `ToolRegistryConfig`.
   4. [x] Update `src/skills/policy.rs` (`resolve_policy`), `src/cli_driver/console_app.rs`
          (`display_registry_config`), and `tests/sandbox_evaluation/bash_policy_replay.rs` to
          unwrap/filter `Option<Vec<SandboxRuleConfig>>` cleanly while preserving `load_skill`'s
          rule inheritance (`agent.rules` → `profile.rules` → `&[]`).

4. [x] Update existing tests and add focused unit and integration tests for timeout resolution,
   validation, and independent `rules` inheritance.
   1. [x] Update existing fixtures in `src/config/web_fetch.rs`, `src/config/web_search.rs`,
          `src/config/profile/tests/backends.rs`, `src/config/profile/tests/sandbox.rs`,
          `src/agent_factory/tests/{configuration,roles}.rs`, and `src/skills/tests.rs` for the new
          `ToolConfig` shape and removal of `[web_fetch].timeout_secs` /
          `[web_search].timeout_secs`.
   2. [x] Add config loading and validation tests in `src/config/profile/tests/` and
      `src/config/agent_type_file.rs` covering:
      - `[tools.<name>].timeout_secs` and `[agents.<type>.tools.<name>].timeout_secs` (inline TOML,
        `.toml` agent file, `.md` TOML/YAML frontmatter, and `${ENV_VAR}` interpolation) for `bash`,
        `web_fetch`, and `web_search`;
      - `timeout_secs = 0` rejection at both profile and agent scopes;
      - `timeout_secs` rejection on unsupported tools (e.g. `read`, `load_skill`) at both profile
        and agent scopes;
      - unknown-key `ConfigWarning` when stale `timeout_secs` is placed under `[web_fetch]` or
        `[web_search]`, while unknown keys under `[tools.<name>]` remain hard errors;
      - inline `[agents.<type>.tools.<name>]` merging per tool and per field onto a lower-scope
        agent-type file without dropping sibling tools or unmentioned `rules`/`timeout_secs`.
   3. [x] Add registry and `AgentFactory` tests in `src/agent/tools/tests/registry.rs` and
      `src/agent_factory/tests/` verifying:
      - `[tools.bash]\ntimeout_secs = 900` (with `rules` omitted) sets `BashTool` / `BashCall`
        timeout to `900` while preserving `bash`'s built-in default sandbox floor (`backgrounded`
        auto-denied, unapproved commands requiring approval);
      - `[agents.worker.tools.bash]\ntimeout_secs = 900` (with `rules` omitted) inherits
        `[tools.bash].rules` from the profile while overriding only `bash`'s timeout for `worker`,
        leaving `default` agent at profile/default timeout;
      - explicit `rules = []` at agent or profile tier still replaces lower-tier rules with an empty
        list;
      - per-agent `web_fetch` and `web_search` `timeout_secs` update `Tool::timeout_secs()`
        (`timeout_secs + 10` s outer grace) independently per agent.

5. [x] Update affected project documentation and diagrams.
   1. [x] Update `docs/configuration.md`:
      - remove `timeout_secs` from the `[web_search]` and `[web_fetch]` field tables and TOML
        examples, cross-referencing `[tools.web_search]` / `[tools.web_fetch]`;
      - document `timeout_secs` under `[tools.<name>]` and `[agents.<type>.tools.<name>]` (supported
        tools `bash` default `300`, `web_fetch` default `30`, `web_search` default `30`; `>= 1`
        validation; outer grace period for `web_fetch`/`web_search`; `${ENV_VAR}` support; rejected
        on other tools);
      - refine the `[tools.<name>]`, `[agents.<type>.tools.<name>]`, `load_skill`, and inline
        agent-override prose so it accurately states that an explicit `rules` list (including
        `rules = []`) replaces the lower tier's rules without merging rule items, while omitting
        `rules` inherits the lower tier's rules independently of `timeout_secs`.
   2. [x] Update `docs/features.md`:
      - fix the stale `Default timeout: 30 s.` sentence under `### bash` to `300 s` and note that
        `[tools.bash].timeout_secs` / `[agents.<type>.tools.bash].timeout_secs` configures it;
      - adjust the sandbox `[tools.<name>]` summary around rule-list replacement vs. `rules`
        omission so it stays consistent with `docs/configuration.md`.
   3. [x] Check `docs/diagrams/configuration-policy-flow.mmd` around tool-policy resolution and
          adjust any label that says a `[tools.<name>]` *table* (rather than an explicit `rules`
          *list*) unconditionally replaces lower rules.

6. [x] Run documentation and code verification gates.
   1. [x] Run `rumdl fmt docs/configuration.md docs/features.md docs/plans/tool-config-plan.md` and
          `rumdl check docs/configuration.md docs/features.md docs/plans/tool-config-plan.md`.
   2. [x] Run `make format lint` and the single mandatory test-and-coverage gate
          `make test-coverage`, fixing any formatting, clippy, test, or coverage issues before
          reporting completion.

## Implementation Hints

- **Existing struct & validation locations**:
  - `ToolSandboxConfig` and `validate_tool_sandbox_rules` live in `src/config/profile.rs:195-298`
    and are re-exported in `src/config.rs:45`. `ProfileConfig::load_file_with_source`
    (`src/config/profile.rs:809-814`) and `ProfileConfig::load_agent_types_into_profile`
    (`src/config/profile.rs:857-860`) both run per-tool validation—any new validation must run in
    both paths so profiles falling back to `ProfileConfig::default()` still validate discovered
    `.ylva/agents/*` files.
  - `unknown_keys_in_sandbox_rule_tables_are_hard_errors` in
    `src/config/profile/tests/unknown_keys.rs:127-149` explicitly asserts that
    `[tools.bash]\nrule = [...]` is a hard load error (`#[serde(deny_unknown_fields)]` on the
    `[tools.<name>]` struct).
  - `validate_profile_agent_tool_names` in `src/agent_factory.rs:1013-1033` (tested by
    `src/agent_factory/tests/configuration.rs:785-830`) validates that keys in `profile.tools` and
    `agent.tools` belong to `DEFAULT_TUI_TOOLS`. In those unit tests, `profile.tools` inserts an
    empty config (`rules: None, timeout_secs: None`) under `"not-a-real-tool"`; validating
    `timeout_secs` only when `timeout_secs.is_some()` preserves that startup check order cleanly.
- **Inline agent overrides vs. agent-type files**:
  - `AgentConfig` (`src/config/agent.rs:52`) and `AgentConfigOverrides`
    (`src/config/agent_type_file.rs:284`) both store per-tool entries.
    `apply_agent_config_overrides` (`src/config/profile.rs:1091-1130`) already merges
    `AgentCompactionConfig` field-by-field (`engine`, `timeout_sec`) when `overrides.compaction` is
    `Some`; `overrides.tools` at line 1127 currently overwrites `config.tools` wholesale and must
    instead merge per tool key and per `Some` field.
  - `deserialize_optional_u64_or_string` already exists in both `src/config/agent.rs:82-98` and
    `src/config/agent_type_file.rs:287-305` using
    `#[serde(untagged)] enum TimeoutValue { Number(u64), Text(String) }`, which works identically
    across `toml` and `serde_yaml` (`.md` YAML frontmatter).
- **Web tool timeouts and grace periods**:
  - `WebFetchConfig` (`src/config/web_fetch.rs:28-100`) and `WebSearchConfig`
    (`src/config/web_search.rs:31-315`) currently pass `self.http_timeout()` into
    `backend_for(...)`.
  - `WebFetchTool::new` and `WebSearchTool::new` receive the backend, backend configuration and
    sandbox context. Pass each resolved HTTP timeout into its constructor while retaining the
    configuration needed for the backend title and search result limit. Each tool adds its
    `OUTER_TIMEOUT_GRACE_SECS` of 10 seconds.
  - `BashTool` already supports `with_timeout` and its call adds
    `BASH_OUTER_TIMEOUT_GRACE_SECS` to the inner command timeout. Preserve that existing grace
    period: the inner timeout drains and reports partial output before the outer guard cancels it.
  - Agent-type files do not yet interpolate environment variables. Pass the resolved profile
    environment into file loading and interpolate parsed configuration values without modifying
    Markdown persona bodies.
- **Other `ToolConfig::rules` call sites**:
  - `src/agent_factory.rs:536-555`: builds
    `tool_sandbox_rules: HashMap<String, Vec<SandboxRuleConfig>>` per agent; `default_registry`
    (`src/agent/tools.rs:883-893`) only calls `SandboxContext::with_tool_rules` for keys present in
    `tool_sandbox_rules`, so omitting keys whose resolved `rules` is `None` automatically retains
    `SandboxContext`'s built-in default floor.
  - `src/skills/policy.rs:146-153`: resolves `load_skill` rules via `agent.tools.get("load_skill")`
    → `profile.tools.get("load_skill")` → `&[][..]`; chaining
    `.and_then(|tool| tool.rules.as_deref())` preserves `rules = None` inheriting
    `profile.tools["load_skill"]` while `rules = Some(vec![])` overrides with `&[]`.
  - `src/cli_driver/console_app.rs:87-99` (`#[cfg(test)] fn display_registry_config`): maps
    `profile.tools` into `ToolRegistryConfig` for title rendering tests; use
    `.filter_map(|(name, cfg)| Some((name.clone(), cfg.rules.clone()?)))`.
  - `tests/sandbox_evaluation/bash_policy_replay.rs:118-123`: already asserts
    `table.contains_key("rules")` before deserializing `[tools.bash]`, so
    `configuration.rules.unwrap_or_default()` (or `.expect("checked rules key above")`) keeps the
    offline replay binary working unchanged.
- **Rust idioms to apply**:
  - Pass `u64` / `Duration` (`Copy` types) by value rather than reference, borrow
    `&[SandboxRuleConfig]` slices in read-only helpers, avoid redundant `.clone()` calls until
    constructing the owned `ToolRegistryConfig` map, and return structured `thiserror` `ConfigError`
    variants instead of `unwrap()`/`expect()` in library code.
