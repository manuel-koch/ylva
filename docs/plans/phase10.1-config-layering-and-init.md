# Phase 10.1: Layered Configuration Resolution, `ylva init`, and SOUL.md Persona

Implementing agent: flip each `[ ]` to `[x]` immediately after completing that
step and rewrite this file in place, so progress is resumable across
sessions. Do not batch checkbox updates until the end.

## Context

Ylva (a Rust TUI agent-harness) currently (Phase 7) loads exactly **one**
flat file — `~/.ylva/profiles/<name>/config.toml` — into `ProfileConfig` /
`AgentConfig` / `EngineConfig`, and hardcodes its root agent's initial system
message as a Rust constant. This plan is **sub-phase 10.1** of
`docs/blueprint.md`'s Phase 10 ("Full Configuration & Persona System", §7,
around line 2912; full design at §4G, lines ~1424-1922), split into three
sibling plans because Phase 10's scope is too large for one linear
execution:

- **This plan (10.1)** implements blueprint Phase 10 checklist items **1**
  (three-layer fallback chain), **2** (`ylva init`/`ylva init --project`),
  and **3** (`SOUL.md` persona loader).
- `docs/plans/phase10.2-agent-type-files.md` implements items 4-6 (global/
  profile-scoped/project-scoped `agents/*.md` type files) and item 8
  (project-scoped `.env` + `.gitignore`). It **depends on this plan**: it
  reuses this plan's layered-resolution core (`src/config/layered.rs`),
  embedded `config/` tree, and `run_init_project` function (extending it to
  also write a `.gitignore`).
- `docs/plans/phase10.3-sandbox-tool-rules.md` implements item 9 (per-tool
  sandbox rule configuration). It has **no hard dependency** on this plan —
  it only needs the pre-existing `ProfileConfig` struct (already present
  before this plan) and adds Rust-literal default rules, not embedded-file
  ones — so it can be implemented in parallel with 10.1/10.2.
- Item 7 (profile-scoped `.env` auto-loading) is **already implemented**
  (checked `[x]` in the blueprint) — no work item in any of the three plans.

Read `docs/blueprint.md` §4G before implementing; this plan does not restate
its full prose, only the concrete steps needed to build the parts of it
assigned here.

### Goal & motivation

Replace the Phase 7 single-file, home-only profile loader with the full
three-layer (project `.ylva/` → home `~/.ylva/` → embedded `config/`)
resolution chain; add `ylva init`/`ylva init --project` to seed the
user-facing layers; and replace the hardcoded `DEFAULT_SYSTEM_PROMPT` Rust
constant with a resolvable `SOUL.md`, so behavior and persona become fully
file-driven and shareable across projects/users, without ever requiring a
config file to exist for Ylva to run out of the box.

### Acceptance criteria

- A fresh checkout with no `~/.ylva/` and no project `.ylva/` still runs out
  of the box: the embedded `config/` tree supplies a default profile and
  persona — never a hard error.
- `config.toml` resolution now walks project → home → built-in
  (whole-file-replace: the highest layer that defines the file wins
  outright; lower layers are ignored for that file), instead of only reading
  `~/.ylva/profiles/<name>/config.toml`. Explicitly-named-profile-missing and
  malformed/unreadable-file error semantics from Phase 7 are preserved
  (`ConfigError::ProfileNotFound` only when the profile is missing at every
  layer for an **explicitly** named profile; a malformed/unreadable file at
  any layer is always a hard error).
- `ylva init` seeds `~/.ylva/{SOUL.md,agents/{research,code_edit}.md,profiles/default/config.toml}`,
  write-if-missing, `--force` reseeds. (This plan seeds these files onto
  disk; it does **not** implement runtime loading/merging of
  `agents/research.md`/`agents/code_edit.md` into `ProfileConfig.agents` —
  that is `docs/plans/phase10.2-agent-type-files.md`'s job. Until 10.2
  lands, these two seeded files sit unused on disk — harmless,
  forward-compatible seeding. `agents/default.md`'s content only needs to
  stay consistent with the in-code `default_agent_config()` floor already
  used today.)
- `ylva init --project` seeds `.ylva/profiles/default/config.toml` with
  **placeholder** values (not the full built-in defaults) in the project
  root, write-if-missing, `--force` reseeds. (The `.gitignore` deployment
  named in blueprint checklist item 8 is implemented in
  `docs/plans/phase10.2-agent-type-files.md`, which extends this plan's
  `run_init_project` function — this plan only needs to leave that function
  easily extensible, e.g. by returning/accepting the project `.ylva/` root
  path so 10.2 can add a file write beside it.)
- A project `.ylva/SOUL.md`, if present, wins over `~/.ylva/SOUL.md`, which
  wins over the built-in default persona; the resolved text becomes the
  first `Message::System` in a freshly created **root** `Context`, replacing
  the hardcoded `DEFAULT_SYSTEM_PROMPT` constant for that one call site only.
- `make lint`, `make test`, and `make test-coverage` all pass (≥80% line
  coverage maintained, per `make test-coverage` in
  `docs/development-workflow.md`) after the change.
- `docs/configuration.md`, `docs/cli-options.md`, and the relevant
  `docs/blueprint.md` checkboxes (items 1-3) are updated in the same commit
  as the code they describe.

### Key constraints

- **No `clap` yet.** `src/main.rs` still hand-parses `std::env::args()`
  (Phase 12 introduces `clap`). `ylva init`/`ylva init --project` must be
  added as leading-subcommand-style dispatch consistent with the existing
  `--version`/`--no-tui`/`--color=` handwritten style — do not pull in
  `clap` early (see `docs/ideas/cli-options-parsing.md`, which explicitly
  defers that refactor to its own future phase).
- **Whole-layer replace, not deep-merge, per file.** The three-layer
  fallback chain (project → home → built-in) resolves **per file**: the
  highest layer that defines a given file wins outright for that file;
  lower layers are ignored for it. This applies to `config.toml` and to
  `SOUL.md` in this plan (agent-type files get the same treatment, but that
  resolution is `phase10.2`'s job).
- **Never a hard error when config is entirely absent.** Only an
  *unreadable* or *malformed* file at any layer is an error (matches the
  existing `ConfigError::Unreadable`/`Malformed` behavior for the profile
  file today; extend the same posture to `SOUL.md`).
- **Secrets never logged.** Any new logging around layered resolution
  reuses the existing `tracing` posture in `src/config.rs`
  (`load_profile_environment`) — presence indicators only, never values.
- **`Context::new()` has ~20 call sites** across `src/agent.rs`,
  `src/agent_factory.rs`, and `src/store.rs` (tests, child-agent contexts,
  session replay). Only the single root-agent construction call site in
  `src/agent_factory.rs` should receive the resolved SOUL.md text; every
  other call site must keep working unchanged.
- **Coverage threshold ≥80%** (`make test-coverage`) applies to all new
  non-UI files this plan adds.
- **Docs-are-part-of-the-change-set** and **ADR-consistency** rules from
  `AGENTS.md`/`docs/development-workflow.md` apply: `docs/architecture
  -design-records/` is currently empty; if the three-layer resolution
  algorithm proves to need a documented rationale beyond what
  `docs/configuration.md` covers, consider recording it as a new ADR via the
  `architecture-decision-record` skill.

### Concrete starting point

First file to open: `src/config.rs` and `src/config/profile.rs` (current
single-profile, home-only loader). First test to run: `cargo test --lib
config::` and `cargo test --test cli_profile` to confirm the current Phase 7
baseline passes before changing anything.

## Implementation Plan

1. [ ] Add a new `src/config/layered.rs` module implementing the
       **three-layer fallback chain** as a small, generic, testable core:
       given a relative file path (e.g. `"SOUL.md"`,
       `"profiles/default/config.toml"`), an optional project root, the home
       root (`ylva_root()`), and an embedded built-in source, return the
       resolved file's raw string plus which layer supplied it (for
       logging). Missing at every layer is `Ok(None)` (caller decides the
       built-in-default fallback text); unreadable/malformed is always an
       error from the concrete loader that parses the content (this module
       only resolves *which* file's bytes to use — it does not parse
       TOML/Markdown itself).
2. [ ] Add project-root discovery: walking up from
       `std::env::current_dir()` to the nearest ancestor containing a
       `.ylva/` directory, or (if none) the nearest ancestor containing
       `.git`, stopping at the filesystem root. No match ⇒ no project layer
       (home/built-in only). Implement as
       `pub fn discover_project_root(start: &Path) -> Option<PathBuf>` in
       `src/config/layered.rs`, unit-tested with `tempfile::TempDir`
       fixtures (nested dirs with `.ylva` present, `.git` present, neither
       present).
3. [ ] Add `pub const YLVA_PROJECT_DIR_NAME: &str = ".ylva";` in
       `src/config/layered.rs` (or co-located with
       `YLVA_HOME_ENVIRONMENT_VARIABLE` in `src/ylva_home.rs`, with a
       cross-reference from whichever file doesn't define it — pick one
       location, SSOT).
4. [ ] Create the embedded **source-of-truth** tree at `config/`
       (repository root, sibling to `src/`), mirroring the runtime
       `~/.ylva/` layout, per `docs/blueprint.md` §4G "Config Source of
       Truth & Distribution":
       - `config/SOUL.md` — built-in default persona text. Base content:
         adapt `src/agent.rs::DEFAULT_SYSTEM_PROMPT`'s current wording
         ("You are Ylva, a helpful agent with tool capabilities.") as the
         opening line; keep the whole file short (a long `SOUL.md` dilutes
         its own signal, especially for smaller models — see
         `docs/ideas/soul.md`'s closing guidance if drawing on any of its
         candidate topics for extra content).
       - `config/agents/default.md` — the always-available `default` agent
         type (full tool whitelist, `can_spawn = true`), matching what
         `default_agent_config()` in `src/config/profile.rs` currently
         hardcodes in code. Not yet wired into any runtime loader by this
         plan (see Acceptance Criteria); only needs to exist and be
         seeded by `ylva init`.
       - `config/agents/research.md`, `config/agents/code_edit.md` — the two
         example types named throughout `docs/blueprint.md` §4G. Use
         blueprint's inline `research` example body as a starting point;
         author a reasonable `code_edit` type (tools: `read`, `write`,
         `edit`, `glob`, `search`, `bash`; `can_spawn = false`). These are
         seeded by `ylva init` but — same note as `default.md` — not loaded
         at runtime until `phase10.2` lands.
       - `config/profiles/default/config.toml` — the built-in default
         profile: empty `[engines.*]`/`[provider_connections.*]` (so it
         still falls through to environment-based provider discovery, per
         current behavior), `default_engine = "default"`, `theme = "dark"`.
         Do **not** add `[agents.*]` tables beyond what
         `ProfileConfig::default()` already implies — additional agent-type
         wiring is out of scope for this plan.
       Embed these via `include_str!`, the pattern already used for fixtures
       in `src/llm/openai_compatible_provider.rs:480`
       (`include_str!(concat!("../../tests/fixtures/", $name))`), collected
       into one lookup (e.g. a `match`/array of `(relative_path, &str)`
       pairs) in a new `src/config/embedded.rs`.
5. [ ] Unit-test the resolution core: a file present only at the built-in
       layer, only at the home layer, only at the project layer, at
       multiple layers (highest wins, others ignored for that file), and
       missing everywhere (`None`, not an error).
6. [ ] Rewire `ProfileConfig::load`/`load_or_default`/`load_with_environment`
       (`src/config/profile.rs`) so `config.toml` is resolved through the
       layered core from step 1 (project → home → built-in) instead of only
       reading `~/.ylva/profiles/<name>/config.toml` directly. Preserve the
       existing explicit-vs-implicit error semantics exactly:
       - **Explicit** profile (`--profile <name>`): missing at *every* layer
         is still `ConfigError::ProfileNotFound`.
       - **Implicit** default profile: missing at project/home layers now
         falls through to the embedded `config/profiles/default/config.toml`
         (step 4) as the new floor, parsed the same way any other layer is;
         only if that embedded resource were somehow itself unreadable (a
         build-time invariant that should never happen in practice) does the
         code fall back further to the existing in-code
         `ProfileConfig::default()` as a last-resort safety net — add a unit
         test asserting the embedded floor parses successfully so this
         last-resort path is never exercised in practice.
       - Malformed/unreadable file at any layer remains a hard error, named
         with its resolved path.
7. [ ] Confirm `${ENV_VAR}` interpolation (already implemented in
       `src/config/profile.rs::interpolate_toml_strings`) continues to run
       unchanged after parsing whichever layer's `config.toml` won — no
       interpolation logic changes, only the source-selection step
       upstream of it.
8. [ ] Add a new `src/config/soul.rs` exposing
       `pub fn load_soul(project_root: Option<&Path>) -> Result<String, ConfigError>`,
       built on the layered resolution core from step 1: resolves
       `SOUL.md` at project → home → built-in, trims trailing whitespace,
       returns the resolved text (never empty — the built-in layer always
       supplies a default).
9. [ ] Change `Context` (`src/agent.rs`) so the initial system message is
       parameterized instead of hardcoded: add
       `Context::with_system_prompt(system_prompt: impl Into<String>) -> Self`
       , keeping `Context::new()`'s existing behavior (uses
       `DEFAULT_SYSTEM_PROMPT`) unchanged for its ~20 existing call sites.
       Update `DEFAULT_SYSTEM_PROMPT`'s doc comment (`src/agent.rs:549`,
       currently says "config phase (Phase 9 §4G layering) may source
       this...") to correct the stale "Phase 9" reference to "Phase 10.1"
       and state it is superseded by `load_soul` for root agents as of this
       plan.
10. [ ] Wire `load_soul`'s resolved text into the **root** `Context` only,
        at the single call site in `src/agent_factory.rs` where a
        `Role::Root` agent's initial `Context` is constructed (identify the
        exact line via `grep -n "Context::new()" src/agent_factory.rs` and
        confirm which of the call sites there is the root-context one vs.
        child-context ones before editing — child contexts keep using their
        existing hybrid-briefing construction from §4G "Child-Agent
        Lifecycle", unaffected by SOUL.md).
11. [ ] Add a new `src/init_command.rs` exposing
        `pub fn run_init(home_root: &Path, force: bool) -> Result<InitReport, ConfigError>`
        and `pub fn run_init_project(project_root: &Path, force: bool) -> Result<InitReport, ConfigError>`,
        where `InitReport` records which files were written vs. skipped
        (already-present, not forced) for a clear CLI summary. `run_init`
        seeds, under `home_root` (`ylva_root()`'s value): `SOUL.md`,
        `agents/research.md`, `agents/code_edit.md`,
        `profiles/default/config.toml` — copied verbatim from the embedded
        `config/` tree (step 4). `run_init_project` seeds, under
        `<project_root>/.ylva/`, only `profiles/default/config.toml` with
        **placeholder** values (author a small distinct placeholder TOML,
        not a byte-copy of the home default — e.g. commented-out example
        `[engines.*]`/`[provider_connections.*]` blocks with obviously-fake
        values, so a team adopting it is prompted to fill in real values).
        Both default to write-if-missing (an existing file is left
        untouched and reported as skipped); `force: true` overwrites
        unconditionally. Return/expose the project `.ylva/` root path used,
        so `phase10.2` can extend this function to also write a
        `.gitignore` beside it without re-deriving the path.
12. [ ] Wire `ylva init` as a leading-argument subcommand in `src/main.rs`,
        dispatched **before** the existing `--version`/logging/profile
        -loading pipeline (matching how `--version` is already
        special-cased at the very top of `fn main()`). A trailing
        `--project` flag on `ylva init` selects `run_init_project` (rooted
        at the discovered project root from step 2, or the cwd itself if
        none is found yet) instead of `run_init`; a trailing `--force` flag
        works with either form. Print the `InitReport` summary to stdout
        and exit `0` on success; an I/O failure while seeding is a hard
        error, exit code `2` (consistent with existing usage-error exit
        codes documented in `docs/cli-options.md`).
13. [ ] Add `tests/cli_init.rs` (new integration test file, following
        `tests/cli_profile.rs`'s pattern of spawning the compiled binary
        with `YLVA_HOME` pointed at a `tempfile::TempDir`): `ylva init` on
        an empty `YLVA_HOME` seeds all four files; running it again without
        `--force` leaves an edited file untouched; `--force` overwrites it;
        `ylva init --project` in an empty tempdir-as-cwd seeds only
        `.ylva/profiles/default/config.toml` (not the global agent types).
14. [ ] Add unit/integration tests for SOUL.md precedence: root agent picks
        up a project `SOUL.md` over a home one over the built-in default.
        Spawn the compiled binary per `docs/development-workflow.md`'s
        "Spawning the compiled binary in an integration test" pattern
        (this is observable only at the assembled-product level), reusing
        the `MockLlmServer` + `env_clear()`/allowlist pattern from
        `tests/store_persistence_e2e.rs`.
15. [ ] Update `docs/configuration.md`: replace the "Profile file" section's
        description of a single home-only flat file with the three-layer
        resolution chain (project → home → built-in), linking to the new
        discovery rule (nearest `.ylva/` or `.git` ancestor); add a
        "Persona (`SOUL.md`)" subsection documenting the resolution order
        and the built-in default text's role. Remove/adjust any "WIP" note
        that refers to this work as not-yet-implemented.
16. [ ] Update `docs/cli-options.md`: document `ylva init [--project]
        [--force]` alongside the existing flags table, including its exit
        codes on failure.
17. [ ] Update `docs/blueprint.md` §7 Phase 10: flip checklist items 1, 2,
        and 3 from `- [ ]` to `- [x]` once each is completed and verified
        (mirroring how Phase 7/8/9 are already fully checked). Leave items
        4-6, 8, 9 unchecked — they belong to the sibling plans.
18. [ ] Run `make lint`, `make test`, and `make test-coverage`; fix any
        finding before reporting this plan done.
19. [ ] Run `rumdl fmt` on every `docs/*.md` file edited in steps 15-17, per
        `AGENTS.md`'s markdown-linting instruction.

## Implementation Hints

- Current single-file loader: `src/config.rs` (profile/env path helpers,
  `ConfigError`), `src/config/profile.rs` (`ProfileConfig`, `load`/
  `load_or_default`/`load_with_environment`, `apply_default_model`, TOML
  `${ENV_VAR}` interpolation), `src/config/agent.rs` (`AgentConfig`),
  `src/config/provider.rs` (`ProviderConnectionConfig`).
- `src/ylva_home.rs::ylva_root()` resolves `~/.ylva` (or `$YLVA_HOME`
  verbatim) — reuse this for the home layer; do not reimplement home
  resolution elsewhere.
- No project-root/git-root discovery utility exists yet anywhere in the
  codebase (`src/store.rs::project_key` only hashes the raw cwd, it does
  not walk up looking for `.git`/`.ylva`) — this is genuinely new code
  (step 2).
- No `config/` directory exists at the repository root yet; it must be
  created from scratch (step 4). The existing embedding precedent is
  `include_str!(concat!("../../tests/fixtures/", $name))` in
  `src/llm/openai_compatible_provider.rs:480`.
- `Context::new()` (`src/agent.rs`, around line 596) unconditionally pushes
  `Message::System { content: DEFAULT_SYSTEM_PROMPT.to_string() }` then a
  `TOOL_PROTOCOL_SYSTEM_PROMPT` message. `DEFAULT_SYSTEM_PROMPT` is defined
  at `src/agent.rs:549`. There are ~20 `Context::new()` call sites across
  `src/agent.rs`, `src/agent_factory.rs`, and `src/store.rs` — the large
  majority are unit tests or child-agent/replay contexts that must keep
  working unchanged; only the root-agent construction path in
  `src/agent_factory.rs` needs the SOUL.md text spliced in.
- `docs/configuration.md`'s current "Profile file" section already
  documents today's home-only behavior and the existing explicit-vs-implicit
  missing-file error semantics — read it fully before rewriting, since
  those semantics must be *preserved*, only the resolution source changes.
- `docs/cli-options.md` documents all current flags including `--profile`,
  exit codes, and the resume-hint behavior; `src/main.rs` hand-parses
  `std::env::args()` with `--version` special-cased at the very top of
  `fn main()` — follow that exact precedent for dispatching `ylva init`
  before any profile/provider loading occurs.
- `docs/development-workflow.md` conventions to follow: shared integration
  test helpers go in `tests/common.rs`/`tests/common/<name>.rs` (never
  duplicated per-file); spawn the real binary via
  `Command::new(env!("CARGO_BIN_EXE_ylva"))` with `env_clear()` + an
  environment allowlist + `tempfile::TempDir` as both `YLVA_HOME` and cwd
  when testing behavior only observable in the assembled binary (`ylva
  init`, project-root discovery, SOUL.md resolution); run such tests from
  `spawn_blocking` under `#[tokio::test(flavor = "multi_thread")]`.
  `tests/cli_profile.rs` is the closest existing precedent to model new
  `tests/cli_init.rs` tests on.
- `docs/known-test-issues.md` "Process-global cwd mutation in tests" and
  "Tracing subscriber isolation in tests" apply to any new unit test that
  reads `current_dir()` or asserts on `tracing` output — hold
  `SUBSCRIBER_LOCK`/use `run_locked` per that doc's workaround section.
- Coverage/lint gates: `make lint` (clippy + check, `#[expect(...)]` not
  `#[allow(...)]` for any justified suppression), `make test`, `make
  test-coverage` (≥80%, excludes `tui_widgets.rs`/`render.rs`/`main.rs`).
- `Cargo.toml` already has `toml = "1.1.5"`, `dotenvy = "0.15"`, `dirs = "6"`,
  `serde`/`serde_json` — no new dependency is anticipated for this plan.
