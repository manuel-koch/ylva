# Phase 10.2: Agent-Type Files, Global SOUL Inheritance & Project `.env`

Implementing agent: flip each `[ ]` to `[x]` immediately after completing that
step and rewrite this file in place, so progress is resumable across
sessions. Do not batch checkbox updates until the end.

## Context

Ylva (a Rust TUI agent-harness) models agent types (`research`, `code_edit`,
`default`, ...) as named bundles of behavior (persona, tool whitelist, engine
override, timeout) declared in config and usable in any role (root or
child) — see `docs/blueprint.md` §4G "Agent Types & Delegation" (lines
~1424-1922). Today (Phase 7), an agent type's `AgentConfig` can only come
from an inline `[agents.<type>]` TOML table inside the single profile
`config.toml` file (`src/config/agent.rs`, `src/config/profile.rs`); there is
no way to define an agent type's persona/tools in a standalone file, nor to
share one across profiles or projects.

This plan is **sub-phase 10.2** of `docs/blueprint.md`'s Phase 10 ("Full
Configuration & Persona System", §7, around line 2912; full design at §4G).
It implements blueprint Phase 10 checklist items **4** (global agent-type
definitions, `~/.ylva/agents/*.md`), **5** (profile-scoped agent-type
definitions, `~/.ylva/profiles/<name>/agents/*.md`), **6** (project-scoped
agent-type definitions, `.ylva/agents/*.md`), and **8** (project-scoped
`.env` auto-loading plus a seeded `.gitignore`).

It also completes the global-persona inheritance contract introduced by Phase
10.1: the resolved `SOUL.md` is automatically included in every root and
child agent context, while each agent type contributes only its additional
specialization.

**This plan depends on** `docs/plans/phase10.1-config-layering-and-init.md`
being implemented first: it reuses that plan's

- three-layer file-resolution core (`src/config/layered.rs`,
  `discover_workspace_root`), with code-owned built-in defaults rather than an
  embedded repository configuration tree,
- `run_init_project` function in `src/init_command.rs` (10.1), which this
  plan extends to also write a `.gitignore`.

Phase 10.1 currently passes the resolved `SOUL.md` only to root loops. That is
an implementation boundary, not the final persona semantics. This plan must
carry the same resolved global text through the child-agent factory path so
child agents inherit it automatically and exactly once.

Read `docs/blueprint.md` §4G in full before implementing, especially
"Agent-Type File Formats," "Profile Agent-Type Assignment & Overrides," and
"Config-Driven Types" → "Distribution — `ylva init`." This plan does not
restate that prose, only the concrete steps and the additional rules agreed
for this implementation (see Key Constraints).

### Goal & motivation

Let agent-type behavior be authored once, as a standalone Markdown-with
-frontmatter (or pure-TOML) file, shared across profiles (global scope),
scoped to one profile, or scoped to one project — composing with the
existing profile-inline `[agents.<type>]` TOML override mechanism instead of
replacing it, so a team can check in shared agent specializations without
duplicating the global Ylva identity in every type. The resolved `SOUL.md`
forms the global baseline for every context; type-specific persona and
system-prompt content is additive.

### Acceptance criteria

- Global (`~/.ylva/agents/*.md`), profile-scoped
  (`~/.ylva/profiles/<name>/agents/*.md`), and project-scoped
  (`.ylva/agents/*.md`) agent-type files are all discovered, parsed (both
  the pure-TOML and `+++`-frontmatter-Markdown formats from
  `docs/blueprint.md` §4G "Agent-Type File Formats"), and merged into the
  same `ProfileConfig.agents` map consumed by `AgentFactory`.
- Agent-type files are user-authored external configuration. There is no
  embedded `config/agents/` floor: the code-owned `default` agent remains the
  fallback required for a runnable installation, while `research` and
  `code_edit` are documentation examples that users may copy and customize.
- Precedence for the **same type name** appearing in more than one file
  source: **project-scoped > profile-scoped > global** (most-specific-to
  -the-current-run wins; whole-entry replace between file sources, not a
  field merge — matching how profile-scoped already shadows global for that
  profile only per existing blueprint prose).
- A profile's own inline `[agents.<type>]` TOML table inside `config.toml`
  (the pre-existing Phase 7 mechanism) still applies **on top of** whichever
  file-sourced definition won, overriding specific fields
  (`engine`/`allowed_tools`/`timeout_secs`/`enabled`/etc.) exactly as it does
  today for the in-code `default` type — this is a *different, still-field
  -level* mechanism layered after file-source resolution, not replaced by
  it.
- **Same-scope conflict is a hard error**: if the *same scope tier* defines
  the same agent-type name via **both** its own `agents/<type>.{md,toml}`
  file **and** a full definition inside its own `config.toml`'s
  `[agents.<type>]` table, loading fails with a clear error naming the type,
  the scope, and both conflicting paths. Concretely (see Implementation
  Hints for the precise algorithm): the *active* `config.toml` (whichever
  layer — project or home — it resolved from per `phase10.1`) has a
  "sibling" `agents/` directory at that same layer (`.ylva/agents/` if
  `config.toml` came from the project layer, `~/.ylva/profiles/<name>/agents/`
  if it came from the home layer). If that sibling directory has a file for
  type `X` **and** the active `config.toml` also defines `[agents.X]` as a
  full/base definition for a type that has no other file-sourced
  definition, that is the ambiguous case → hard error. A `[agents.X]` table
  that is instead **adapting** a type resolved from a **different**
  (lower-precedence) scope's file — e.g. a profile's `config.toml`
  overriding fields of a *global* `agents/research.md` — remains allowed,
  unchanged from today.
- Persona composition: an `AgentConfig.persona` value (from any
  `config.toml`'s inline `[agents.<type>]` table) is **always prepended** to
  the resolved `system_prompt` body, regardless of that body's source
  (built-in file, global file, profile file, project file, or absent
  entirely for a TOML-only type). No special-casing where `persona` is
  rejected or ignored because a file-sourced body also exists.
- Global persona composition: the resolved `SOUL.md` text is inherited by
  every root and child context exactly once. It is the shared identity
  baseline, not part of the agent-type file replacement chain. The effective
  prompt order is global SOUL, agent-type persona/body, tool protocol, and —
  for a child — its task briefing and child-runtime constraints. An
  agent-type definition does not need to repeat the global identity.
- A project-scoped `<workspace_root>/.ylva/profiles/<name>/.env` is
  auto-loaded the same way the existing home-scoped one is. Precedence when
  the same variable name is defined in both: **project `.env` wins over home
  `.env`**; an already-exported process environment variable still wins
  over both (unchanged from today).
- `run_init_project` (from `phase10.1`) is extended to also write
  `<workspace_root>/.ylva/.gitignore` containing at least `.env`, so a
  project's committed `.ylva/` never leaks secrets.
- `make lint`, `make test`, and `make test-coverage` all pass (≥80% line
  coverage) after the change.
- `ylva init` does not seed agent-type files; its SOUL/profile
  starters are defined by Phase 10.1's code-owned built-ins, and richer
  examples live in documentation.
- `docs/configuration.md` and the relevant `docs/blueprint.md` checkboxes
  (items 4, 5, 6, 8) are updated in the same commit as the code they
  describe.

### Key constraints

- **File-source precedence is whole-entry replace; profile TOML override is
  field-level merge — these are two distinct mechanisms layered in
  sequence**, not one unified merge:
  1. Resolve which single file (project/profile-scoped/global/built-in)
     defines type `X`'s base `AgentConfig` + `system_prompt` (whole-entry
     replace, highest-precedence source wins outright).
  2. Then apply the active `config.toml`'s inline `[agents.X]` table (if
     present) as a field-level override on top of that resolved base — this
     step already exists today (`ProfileConfig::load_file` in
     `src/config/profile.rs`) and needs no behavior change, only to run
     *after* step 1 instead of only ever seeing the in-code
     `default_agent_config()` floor.
- **Same-scope conflict detection** (see Acceptance Criteria) is new
  validation logic — it must not falsely trigger on the legitimate,
  pre-existing cross-scope override case (profile/project `config.toml`
  adapting a *global* file-defined type), only on the genuinely ambiguous
  same-scope case.
- **`AgentConfig` gains a file-sourced `system_prompt`.** The existing
  `src/config/agent.rs::AgentConfig` struct has `persona: Option<String>`
  (a profile-level override, prepended) but no field for a whole
  file-authored system prompt body — this plan must add that without
  breaking the existing `persona` semantics or its TOML deserialization
  (`persona` must remain settable independently of any file source, for
  agent types with no file at all).
- **Global SOUL is inherited, not copied into agent definitions.** The
  resolved text is supplied once by the root factory and captured by the
  child-agent construction closure. Root and child contexts use the same
  global baseline; only the agent-type prompt, child briefing, and
  role-specific runtime constraints differ.
- **Markdown frontmatter is strict**: a `.md` agent-type file without a
  leading `+++ ... +++` block is a parse error (per blueprint §4G), not a
  silent fallback to treating the whole file as a plain prompt.
- **No new dependency anticipated.** `+++`-delimited frontmatter parsing is
  "TOML between two `+++` lines, then a plain-text body" — a manual
  splitter is sufficient (YAGNI); prefer that over adding a Markdown/
  frontmatter crate.
- **Coverage threshold ≥80%** (`make test-coverage`) applies to all new
  non-UI files this plan adds.
- **Docs-are-part-of-the-change-set** and **ADR-consistency** rules apply
  (see `AGENTS.md`/`docs/development-workflow.md`); `docs/architecture
  -design-records/` is currently empty.

### Concrete starting point

First files to open: `src/config/agent.rs` (current `AgentConfig` shape),
`src/config/profile.rs::load_file` (where inline `[agents.*]` tables are
currently applied), and `src/config/layered.rs` (the resolution core added
by `phase10.1`, which this plan reuses), followed by
`src/agent_factory.rs::build_child_agent_manager` and
`src/agent.rs::AgentLoop::run` (the child construction and context assembly
paths that must inherit the global SOUL). First test to run:
`cargo test --lib config::` and `cargo test --lib agent_factory::` to confirm
the `phase10.1` + Phase 7 baseline passes before changing anything (requires
`phase10.1` already merged).

## Implementation Plan

1. [ ] Add `src/config/agent_type_file.rs` implementing the two file
       formats from `docs/blueprint.md` §4G "Agent-Type File Formats":
       - Pure TOML (`.toml`): deserializes directly into `AgentConfig` plus
       a `system_prompt: String` field read from a top-level
       `system_prompt` key (the format shown in blueprint's example
       embeds it as a `"""..."""` TOML string).
       - Markdown with `+++` frontmatter (`.md`): the TOML between the
       first and second `+++` line (each on its own line) deserializes
       into `AgentConfig`; everything after the closing `+++` (trimmed)
       becomes `system_prompt`. A `.md` file whose content does not start
       with a `+++` line is a parse error (new `ConfigError` variant
       naming the file).
       Both formats produce the same output shape: `(AgentConfig,
       system_prompt: String)`, or a struct wrapping both — call it
       `AgentTypeDefinition`.
2. [ ] Add `system_prompt: Option<String>` to `AgentConfig`
       (`src/config/agent.rs`) alongside the existing `persona` field, with
       a doc comment clarifying: `system_prompt` is the file-authored body
       (never set by an inline `config.toml` `[agents.*]` table — TOML
       tables only ever set `persona`, `engine`, `allowed_tools`, etc.);
       `persona` remains a profile-level prepend, independent of
       `system_prompt`'s presence.
3. [ ] Implement discovery of agent-type files across the three sources,
       reusing `src/config/layered.rs`'s per-file resolution logic
       (generalized, if needed, to enumerate a *directory's* files rather
       than resolve one named file) — each of `~/.ylva/agents/`,
       `~/.ylva/profiles/<name>/agents/`, `.ylva/agents/` (when a project
       root is discovered) is scanned for `*.md`/`*.toml` files, keyed by
       file stem = agent-type name. Do not add an embedded agent-file floor:
       `default` comes from the code-owned fallback, while other names exist
       only when a user-authored file or inline profile table defines them.
4. [ ] Implement the precedence merge: for each type name appearing in any
       source, pick the single highest-precedence file
       (project-scoped > profile-scoped > global), producing one
       `AgentTypeDefinition` per name. `default` always has at least the
       code-owned fallback; other names have no floor unless a file or inline
       profile table defines them somewhere.
5. [ ] Implement the same-scope conflict check (see Acceptance Criteria):
       after step 4 resolves each type's file-sourced definition (if any),
       check whether the *active* `config.toml`'s own layer (project or
       home, whichever layer that file actually resolved from) has its own
       sibling `agents/<name>.{md,toml}` file **and** the active
       `config.toml` also defines `[agents.<name>]` with fields beyond a
       pure override of a *lower*-scope file (i.e., the winning file-sourced
       definition for that name came from that *same* layer's sibling
       directory, not from a lower-precedence scope). If both are true,
       return a new `ConfigError` variant (e.g.
       `ConfigError::AmbiguousAgentTypeDefinition { agent_type, scope,
       toml_path, file_path }`) naming the type, the scope tier, and both
       paths. Add targeted unit tests for both scope tiers independently
       (project `config.toml` + project `agents/x.md` conflict; home/profile
       `config.toml` + profile-scoped `agents/x.md` conflict) and a
       regression test confirming the *cross*-scope case (profile
       `config.toml` adapting a *global* `agents/research.md`) does **not**
       trigger the error.
6. [ ] Splice the resolved, precedence-and-conflict-checked
       `AgentTypeDefinition` map into `ProfileConfig.agents` **before** the
       profile's own inline `[agents.<type>]` TOML tables are applied as
       field overrides (`ProfileConfig::load_file` in
       `src/config/profile.rs`) — implement as a new
       `ProfileConfig::load_agent_types` helper called from `load_file`,
       passing in the resolved workspace root and Ylva home from `phase10.1`.
       Existing field-override behaviors ("Profiles can adapt global agent
       types by overriding specific fields," "can disable," local-only
       `agents/` shadowing) continue to work unchanged on top of this
       splice.
7. [ ] Update system-prompt/persona assembly across
       `src/agent_factory.rs` and `src/agent.rs`:
       - Always prepend `AgentConfig::persona` (when present) to the
       resolved `AgentConfig::system_prompt` body (when present),
       regardless of which scope/source supplied the body. When the body is
       absent (a TOML-only agent type), preserve the existing
       `persona`-alone / no-persona behavior.
       - Treat the resolved `SOUL.md` text as a separate global baseline.
       Pass it to both `Role::Root` and `Role::Child` loops, removing the
       current root-only guard in `AgentFactory::build` and carrying the
       same value through `build_child_agent_manager`'s spawn closure.
       - Compose the effective context without duplication: global SOUL
       first, then the agent-type persona/body, then the tool protocol; a
       child additionally receives its briefing and child-runtime
       constraints. Keep `Context::new()` as the compatibility fallback
       for direct library/test contexts that do not have a resolved global
       persona.
8. [ ] Extend `load_profile_environment`/`ProfileConfig::load*`
       (`src/config.rs`, `src/config/profile.rs`) so that, when a project
       root is discovered (via `phase10.1`'s `discover_workspace_root`), a
       project-scoped `<workspace_root>/.ylva/profiles/<name>/.env` is also
       loaded and merged: project `.env` values take precedence over home
       `.env` values for the same key; an already-exported process variable
       still wins over both (unchanged).
9. [ ] Extend `run_init_project` (`src/init_command.rs`, added in
       `phase10.1`) to also write `<workspace_root>/.ylva/.gitignore`
       containing at least an `.env` line, using the project `.ylva/` root
       path that function already returns/exposes. Write-if-missing /
       `--force` semantics match the rest of `run_init_project`'s output.
10. [ ] Unit/integration tests:
        - Global-only type resolves; profile-scoped type shadows a global
        type of the same name for that profile only (unaffected in
        another profile); project-scoped type shadows both.
        - A `.md` file without `+++` is a load error naming the file.
        - A pure `.toml` agent-type file works (format 1 from §4G).
        - The same-scope conflict error fires for both scope tiers
        independently (see step 5), and does not fire for the legitimate
        cross-scope override case.
        - `persona` is prepended to a file-sourced `system_prompt`;
        `persona` alone still works when no file exists for that type.
        - A project-scoped `.env` value is visible for `${ENV_VAR}`
        interpolation in the project's `config.toml`; process env still
        wins over both project and home `.env`; project `.env` wins over
        home `.env` for the same key.
        - `ylva init --project` produces a `.gitignore` that ignores a
        written `.env` file (a simple content assertion, or
        `git check-ignore` semantics if a live git repo is set up in the
        test).
        - A customized project/home `SOUL.md` is present in both a root
        request and a child request, exactly once per context; a child does
        not need to repeat the global identity in its agent-type definition.
        - A root and child using the same agent type receive the same global
        SOUL plus that type's persona/body, while the child additionally
        receives only its own briefing and child-runtime constraints.
11. [ ] Update `docs/configuration.md`: add an "Agent-type files
        (`agents/*.md`, `agents/*.toml`)" subsection documenting the two
        supported formats, the three discovery locations and their
        shadowing precedence, the same-scope-conflict hard error, and how
        file-based resolution composes with a profile's inline
        `[agents.<type>]` overrides (including the `persona`-always
        -prepends rule). Document that global `SOUL.md` is inherited by root
        and child contexts, while agent-type persona/body content is
        additive. Update the "Profile file" / `.env` section to document
        project-scoped `.env` and its precedence over home `.env`.
12. [ ] Update `docs/features.md` and `docs/implementation-notes.md` to
        describe the global-SOUL → agent-type → role-specific prompt
        composition and the fact that child contexts inherit the global SOUL
        automatically.
13. [ ] Update `docs/blueprint.md` §4G's persona and child-agent lifecycle
        prose to distinguish the global SOUL baseline from agent-type
        persona, then flip Phase 10 checklist items 4, 5, 6, and 8 in §7
        from `- [ ]` to `- [x]` once each is completed and verified.
14. [ ] Run `make lint`, `make test`, and `make test-coverage`; fix any
        finding before reporting this plan done.
15. [ ] Run `rumdl fmt` on every Markdown document edited in steps 11-13.

## Implementation Hints

- `src/config/agent.rs::AgentConfig` today: `engine`, `description`,
  `allowed_tools`, `timeout_secs`, `persona`, `can_spawn` — no
  `system_prompt` field (added by this plan's step 2).
- `src/config/profile.rs::ProfileConfig::load_file` is where inline
  `[agents.<type>]` tables are deserialized today (into
  `profile.agents: HashMap<String, AgentConfig>`) and where the in-code
  `default_agent_config()` floor is inserted
  (`profile.agents.entry(DEFAULT_PROFILE_NAME).or_insert_with(...)`) — this
  is the exact call site step 6 must preserve: file-sourced definitions and
  inline tables are loaded before the code-owned `default_agent_config()`
  fallback, which remains the only built-in agent-type floor.
- `src/agent_factory.rs::AgentFactory::create`/`build` resolve an agent
  type's `AgentConfig` purely via `profile.agents.get(agent_type)`;
  `profile.agents` already being fully populated by the time `AgentFactory`
  reads it is the configuration integration point. The same module currently
  applies its optional `system_prompt` only inside an `is_root` guard, while
  `build_child_agent_manager` passes `None` for the child build; step 7 must
  remove that root-only restriction and capture the resolved global SOUL in
  the child spawn closure.
- `src/agent.rs::AgentLoop::run` already constructs a context from its
  optional `system_prompt`, then appends a child briefing and child-runtime
  budget message. `Context::with_system_prompt` also installs the tool
  protocol message, so prompt assembly must preserve the documented order
  without inserting the global SOUL twice.
- `src/main.rs` resolves `load_soul(workspace_root)` once and supplies the
  resulting text through `AgentFactory::with_system_prompt`; that value is
  the global baseline to propagate to child loops, not an agent-type-specific
  prompt.
- `docs/blueprint.md` §4G "Agent-Type File Formats" gives both example
  formats verbatim (pure TOML with a `system_prompt = """..."""` string;
  Markdown with `+++`-delimited frontmatter and a plain-text body) — mirror
  those exactly rather than inventing a new shape.
- `docs/blueprint.md` §4G also documents the pre-existing field-override
  ("Profiles can adapt global agent types by overriding specific fields"),
  disable (`enabled = false`), and local-only-shadowing behaviors this plan
  must not regress — read that subsection fully before changing
  `src/config/profile.rs`.
- `phase10.1`'s `src/config/layered.rs` provides `discover_workspace_root`
  and the single-file resolution core; this plan's step 3 needs a
  directory-enumeration variant of the same idea (list files present at
  each of the three `agents/` locations) rather than resolving one named
  file — implement it alongside the existing core, sharing its
  layer-precedence logic rather than duplicating it.
- `research` and `code_edit` are documentation examples, not built-in
  resources. Tests should create their user-authored files in temporary
  project, profile, or home layers and should verify that no embedded
  agent-file fallback is required.
- `phase10.1`'s `src/init_command.rs::run_init_project` is the function
  this plan's step 9 extends — read its signature/return type in
  `phase10.1`'s implementation before adding the `.gitignore` write, so the
  project `.ylva/` root path is reused rather than re-derived.
- `docs/development-workflow.md` conventions to follow: shared integration
  test helpers go in `tests/common.rs`/`tests/common/<name>.rs`; spawn the
  real binary via `Command::new(env!("CARGO_BIN_EXE_ylva"))` with
  `env_clear()` + an environment allowlist + `tempfile::TempDir` for
  behavior only observable in the assembled binary.
- `docs/known-test-issues.md` "Process-global cwd mutation in tests" and
  "Tracing subscriber isolation in tests" apply to any new unit test that
  reads `current_dir()` or asserts on `tracing` output.
- Coverage/lint gates: `make lint`, `make test`, `make test-coverage`
  (≥80%, excludes `tui_widgets.rs`/`render.rs`/`main.rs`).
- No new dependency anticipated; manual `+++`-delimiter splitting is
  sufficient for frontmatter parsing (see Key Constraints).
