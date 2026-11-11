# One-off CLI provider and model overrides and `models` listing (`--provider`, `--model`, `models`)

## Context

Headless (and TUI-launch) runs select their provider and model through the
active profile's `[engines.*]` tables and named
`[provider_connections.*]` tables. There is no CLI flag to run a single
one-shot session against a different provider connection or model; the only
alternative today is editing the profile. There is also no non-TUI way to
list the models a connection offers. This plan adds two top-level
one-off overrides, `--provider <name>` and `--model <id>`, without changing
the profile on disk, so the profile and its named engines remain the
repeatable source of truth. It also adds a `ylva models` listing
subcommand, reusing the same profile/catalog plumbing.

The work has two independent items:

1. **One-off overrides** — `--provider <name>` and `--model <id>` (steps
   1–8).
2. **`ylva models` listing** — a profile-loading, non-session subcommand
   that lists catalog models with picker-style rendering (steps 9–13).

Design decisions (resolving the open questions in
`docs/ideas/cli-provider-override.md` and `docs/ideas/cli-model-override.md`):

- `--provider` names a **provider connection** defined in the selected
  profile (`[provider_connections.<name>]` map key), not a provider
  implementation kind. Endpoints and credentials already live per connection;
  implementation-kind selection stays an environment-discovery concern.
- `--provider` works on any profile shape: with explicit engines, it swaps
  the root engine's connection; with provider connections but no engines, the
  selected connection's implicit engine becomes the root engine for the run
  (the others stay available to `/models`); in environment-discovery mode
  (neither engines nor connections configured) it is rejected — exit 2 —
  because no named connection exists to select.
- `--model` in environment-discovery mode is allowed: it overrides the model
  of the discovered connection's implicit engine.
- `--model` overrides the model identifier used by the engine that the root
  agent type resolves to (its `engine` field, falling back to the profile's
  `default_engine`). The override does not cascade: spawned subagents whose
  agent type names a different engine keep their configured engines — only
  agents resolving the same named engine (sharing the Engine runtime) see
  the override, matching `/models` switch behavior. Because agents resolving
  the same named engine share its Engine runtime, the override also reaches
  a compaction engine that is unset (it follows the conversation engine) or
  that resolves to the same named engine while the root agent uses
  `default_engine`; a compaction engine selecting a *different* named engine
  keeps its configured model. Context length, max-output tokens, and
  reasoning effort resolve exactly as a `/models` raw-catalog selection does
  today: the configured engine values apply only to the engine's configured
  model, the provider catalog supplies the overridden model's context length,
  and the provider's reasoning-effort policy validates or falls back.
- Empty values are usage errors: `--model ""` exits `2`; an empty id is
  never a valid model identifier.
- The catalog-outage warning on the `--model` path stays a plain warning
  even under `--strict-config`: that flag escalates *configuration* warnings
  (unknown keys, tool names, model-scores problems), and a runtime catalog
  outage is not configuration.
- Catalog handling for `--model`: `allowed_models` is enforced at startup
  (local, no network). The root engine's model-catalog fetch for the
  overridden id happens before the first chat (blocking on this path only):
  when the fetch succeeds and the id is absent from the catalog, that is a
  usage error (exit 2) naming the id and connection; when the fetch fails or
  times out, the run proceeds with degraded metadata
  (`DEFAULT_CONTEXT_LENGTH`, reasoning provenance `Unverified`) and a
  warning — the same semantics a named engine has in a catalog outage today.
- Both flags are run-scoped: they never mutate the profile, are never
  persisted as the profile's selection, and are forgotten when the process
  exits.
- The flags apply to both launch paths — headless and TUI. The override
  happens at engine construction, which both drivers share; `/models` in the
  TUI shows the overridden selection as the active runtime, and the user can
  switch away from it there as usual.
- The flags follow existing CLI precedence rules with no special handling
  (`--version` still wins over everything, subcommand placement rules are
  unchanged).
- Acceptance criteria:
  - `ylva --provider <name>` runs with that connection for this run only;
    an unknown connection name exits `2` before any session opens.
  - `ylva --model <id>` runs with that model; a model rejected by the
    connection's `allowed_models` policy exits `2`.
  - Combined, `--provider` and `--model` select both; `--provider` and
    `--model` each work alone; `--model` without any usable engine/connection
    is a startup error (`2`).
  - A resumed (`--conversation-id`) or cloned (`--clone`) run with the
    override flags records the actually used provider connection and model in
    its session history (`ModelChanged`), and the shutdown resume hint
    includes the explicit flags so the same run can be repeated.
  - Secrets and endpoints from the selected connection are never logged.
- `ylva models` listing decisions (second work item):
  - It is a **subcommand**, not a flag — matching `skill list` and
    `session list` precedent; it starts no session and consumes no stdin.
  - It loads the selected profile (top-level `--profile`, like
    `connect`/`trust`) because connection endpoints, credentials, favorites,
    and allowlists live there.
  - Without `--provider`, it lists models of **all** configured provider
    connections, connection-qualified, mirroring `/models`; with
    `--provider <name>`, only that connection. An unknown connection name
    exits `2`. In environment-discovery mode (no connections configured),
    the discovered connection's models are listed.
  - Rendering mirrors the TUI model picker: connection-qualified model ids,
    favorite markers (`favorite_models` order), model score from
    `model-scores.toml` when present, and only models permitted by the
    connection's `allowed_models` policy (favorites always survive —
    profile load already validates them against the allowlist).
  - Output follows `skill list` conventions: aligned columns, blank-line
    separation, semantic colors via the global `--color` policy, safe
    single-line rendering.
  - Catalog outage handling: a per-connection fetch failure is a stderr
    warning; the command exits `0` when at least one queried connection
    succeeded and `1` only when every queried connection failed —
    mirroring the `session` maintenance partial-failure convention.
  - Acceptance criteria for the listing:
    - `ylva models` prints every allowlisted model of every configured
      connection with favorite markers and scores, exit `0`.
    - `ylva --profile work models` and
      `ylva models` after top-level `--provider <name>` restrict the
      listing; an unknown `--provider` exits `2` before any request.
    - A single unreachable connection degrades to a warning with exit `0`;
      all-unreachable exits `1` with diagnostics naming each connection.

## Implementation Plan

An implementing agent must flip `[ ]` to `[x]` immediately after completing
each step and rewrite this sidecar file in place, so progress is resumable
across sessions.

1. [ ] Extend `Cli` in `src/cli.rs` with `--provider <name>` (string) and
   `--model <id>` (string) top-level flags. Both are value-taking,
   non-repeatable, must precede subcommands like `--profile`, and are
   rejected with a usage error (`2`) on subcommand runs that never load a
   profile (`init`, `session`, `profile`, `skill`) — except `models`, which
   accepts `--provider` (step 9) and rejects `--model`.
2. [ ] Add override plumbing to `src/startup/profile.rs` in
   `apply_cli_profile_overrides` (or a sibling function next to it): when
   `--provider` is set, verify the name exists in
   `ProfileConfig::provider_connections` — exit `2` naming the profile when
   it does not, including when the profile has neither connections nor
   engines (environment-discovery mode, where no named connection exists).
   When `--model` is set, reject an empty value (exit `2`) and otherwise
   record the override on the loaded profile state so engine construction
   can apply it.
3. [ ] Apply the overrides in `src/agent_factory.rs` where engines are built
   (`AgentFactory::from_profile`): resolve the root agent type's engine
   (explicit `engine` of the selected root agent type, else
   `default_engine`), then for that engine use the `--provider` connection
   instead of the engine's configured `provider_connection`, and the
   `--model` identifier instead of the engine's configured `model`. The
   model override does not cascade to subagents whose agent type names a
   different engine. Because
   agents resolving the same named engine share its Engine runtime, the
   override also reaches a compaction engine that is unset (implicitly
   follows the conversation engine) or that names the same engine the root
   uses while the root runs on `default_engine`; a compaction engine naming
   a different engine keeps its configured model. The utility engine always
   keeps its configuration.
   Leave every other engine
   exactly as configured. Keep the existing guarantees: an
   unreferenced-but-configured connection stays available to `/models`, and
   `validate_profile_agent_engines` still runs.
4. [ ] Enforce model policy and catalog verification for the overridden
   model: check the `--model` id against the effective connection's
   `allowed_models` (`ModelPolicy::ensure_allowed`, exit `2` on rejection)
   before any session opens. Then fetch the root engine's model catalog
   before the first chat (blocking on this path only): on success with the
   id absent from the catalog, exit `2` naming the id and connection; on
   fetch failure or timeout, warn and proceed with degraded metadata
   (`DEFAULT_CONTEXT_LENGTH` context fallback, reasoning provenance
   `Unverified`), matching named-engine catalog-outage behavior. Validate the
   reasoning effort via the provider's `ReasoningEffortPolicy` (unverifiable
   catalogs keep the value, as today). Context length and max-output tokens
   resolve through the existing `EngineConfig::effective_context_length` /
   `max_output_tokens_for_model` fallbacks against the provider catalog.
5. [ ] Wire session history and the resume hint: the initial
   `StoreEvent::ModelChanged` after session creation must record the actual
   provider connection and model in use (including overrides), and the
   shutdown resume hint in `src/startup.rs` must include `--provider` /
   `--model` when they were explicitly given (like `--profile`), so a
   resumed run reproduces the override.
6. [ ] Add tests: unit tests beside `apply_cli_profile_overrides` and in
   `src/agent_factory/tests.rs` for override resolution, connection
   substitution, `allowed_models` rejection, and engine isolation
   (compaction/utility engine isolation: an unset or same-named compaction
   engine follows the override, a different-named one keeps its model, the
   utility engine always keeps its model, subagents naming a different
   engine keep theirs); integration tests in `tests/` for
   exit code `2` on unknown `--provider` (including environment-discovery
   mode), on `--model ""`, on a `--model` rejected by
   `allowed_models`, and for the resume hint content. Follow
   `docs/development-workflow.md` and read `docs/known-test-issues.md` before
   tests involving tracing, cwd, or integration side-effects.
7. [ ] Update documentation in the same change: `docs/cli-options.md` (flags table entries,
       flag-precedence paragraph, exit-code examples, resume-hint section), `docs/configuration.md`
       (profile configuration section: CLI one-off overrides do not touch the profile;
       cross-reference from the engine-configuration section), and `docs/features.md` if it restates
       launch selection. Mark `docs/ideas/cli-model-override.md` and
       `docs/ideas/cli-provider-override.md` as implemented, each with a pointer to this plan file;
       do not delete them (they record the motivation).8. [ ] Run the verification gate:
       `make format lint` and `make test-coverage` (no dependency changes expected, so `make audit`
       is not needed unless `Cargo.toml`/`Cargo.lock` change).
8. [ ] Add a `Models` variant to `CliCommand` in `src/cli.rs`: `ylva models`
   — no session, no stdin consumption, respects the global `--color`
   policy (see `skill list`). It loads the selected profile (top-level
   `--profile`) and requires `--provider`/`--model`-free usage: reject
   `--model` alongside `models` (usage error, exit `2`; a listing takes no
   model override). `--provider <name>` before the subcommand restricts the
   listing to that connection; an unknown name exits `2` before any network
   request.
9. [ ] Implement the listing command (new module beside
   `src/skill_command.rs`, e.g. `src/models_command.rs`, re-exported
   through `src/lib.rs` like the other subcommand modules): resolve the
   connections to query (all configured connections, the `--provider`-named
   one, or the environment-discovered connection when the profile defines
   none), fetch each connection's catalog with the same timeout/outage
   handling as the `/models` picker, filter through each connection's
   `allowed_models` policy, and attach favorites (`favorite_models` order)
   and scores (`model-scores.toml`). Exit `0` when at least one queried
   connection succeeded; exit `1` only when every queried connection
   failed, with stderr diagnostics naming each failed connection.
10. [ ] Render the output picker-style, following `skill list` conventions:
    grouped per connection, connection-qualified model ids, aligned columns,
    a favorite marker in `favorite_models` order, the model score when a
    `model-scores.toml` entry exists, and semantic colors via the global
    `--color` policy (auto/yes/no; redirected output, `TERM=dumb`, and
    `NO_COLOR` stay plain in auto mode). All content on safe single lines.
11. [ ] Add tests for the listing: unit tests for connection selection,
    allowlist filtering, favorite ordering, and outage exit codes; integration
    tests in `tests/` for `ylva models` output shape, `--provider`
    restriction, unknown-connection exit `2`, and color handling. Read
    `docs/known-test-issues.md` before tests involving tracing, cwd, or
    integration side-effects.
12. [ ] Update documentation for the listing: `docs/cli-options.md` (a
    "Models command" section next to "Skill commands", flags-table note for
    `--provider` interaction, exit-code examples), `docs/features.md` if it
    cross-references model discovery, and a cross-reference from the TUI
    `/models` section to the CLI listing.

## Verification

Step 8 is the mandatory gate for steps 1–7; after steps 9–13, run
`make format lint` and `make test-coverage` again. When dependencies change
(`Cargo.toml`/`Cargo.lock`), also run `make audit`.

## Implementation Hints

- `src/cli.rs` (~1086 lines) holds the clap-derive `Cli` struct; add fields
  next to `profile` / `agent`. Precedent for value-taking one-off overrides:
  `max_concurrent_subagents` (line ~163) and `agent`.
- The override application point already exists:
  `apply_cli_profile_overrides(cli, &mut profile)` in
  `src/startup/profile.rs` (line 167), called from `src/startup.rs`
  (line 235) after profile load and `--strict-config` handling, before
  `AgentFactory` construction (startup.rs lines ~271–310).
- `ProviderConnectionConfig` and `ProviderKind` live in
  `src/config/provider.rs`; `ModelPolicy::ensure_allowed` gives ready-made
  rejection errors naming connection and model.
- `EngineConfig` (`src/llm/engine/config.rs`, line 47) already scopes
  `context_length` / `max_output_tokens` to the configured model
  (`context_length_for_model`, `max_output_tokens_for_model`), so a
  different `model` naturally falls back to catalog values — the override
  can reuse this without new resolution logic.
- `AgentFactory::from_profile` (`src/agent_factory.rs`, line 120) builds
  engines from `[engines.*]` tables via `engine_from_profile_config`, then
  validates `default_engine`, `compaction_engine_name()`,
  `utility_engine_name()`, and per-agent engines. `engine_config(name)`
  (line ~186) snapshots the engine for the initial `ModelChanged` event.
- `StoreEvent::ModelChanged` is defined in `src/store/event.rs` (line ~98);
  the initial event is recorded right after session creation so history is
  never missing its starting provider-connection/model.
- Exit-code contract lives in `src/exit_code.rs` (`ExitCode::UsageError` =
  2) and `docs/cli-options.md`; startup errors surface through
  `exit_with_error` in `src/main.rs`.
- Startup rejects runs with no usable provider (prints `ylva connect`
  guidance, exit `2`); the override flags must not bypass that check —
  `--model` with a nonexistent connection and no engines still exits `2`.
- Secrets discipline: connection `api_key` values are never logged; keep the
  override error messages naming connections, not credentials.
- Catalog verification (step 4): the catalog is fetched live per run and
  kept for the run's lifetime; it is not persisted across runs. The `/models`
  picker shows outage semantics in `src/tui_driver/models.rs`
  (`CatalogOutage::ProviderError/PartialFailure/Timeout`); catalog adoption
  onto a live engine happens via `adopt_catalog_context_length` /
  `adopt_catalog_max_output_tokens` in `src/llm/engine/runtime.rs` (lines
  ~696–720) and the `apply_model_option` path in `src/llm/engine.rs` (line
  ~655). The `--model` path must replicate: fetch before first chat, refuse
  absent ids on success, degrade on outage.
- `models` listing (steps 9–13): the catalog fetch machinery of the
  `/models` picker lives in `src/tui_driver/models.rs`
  (`start_model_list`, `MODEL_LIST_TIMEOUT`,
  `CatalogOutage`); reuse its timeout/error semantics from a
  provider-independent context. `ModelScores` lives in
  `src/llm/model_scores.rs`; favorites in
  `ProviderConnectionConfig::favorite_models` (`src/config/provider.rs`);
  `ProviderModelOption` and `ModelInfo` in `src/llm.rs`. Subcommand module
  precedent: `src/skill_command.rs` (+ `src/skill_command/` directory),
  re-exported in `src/lib.rs`, invoked early from `src/main.rs` before the
  Tokio session runtime path — but `models` needs providers, so it runs
  after profile loading (like `connect`/`trust`, which take top-level
  `--profile`). Environment-discovery fallback for connectionless profiles:
  `engines_from_environment` in `src/startup/engines.rs`.
- Secrets discipline applies to the listing too: connection `api_key`
  values are never logged or printed; catalog output contains model ids and
  metadata only.
