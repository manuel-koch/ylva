# Provider favorites and allowed models

## Context

### Goal and scope

Add explicit provider-connection favorites and enforce allowed model patterns throughout
Ylva's profile-built engines. Ctrl+F toggles favorite membership for the highlighted model
in both the `/models` list and its parameter popup. Changes persist in the active profile.

The original idea proposed inspecting sessions and ranking historical usage. The user
removed that requirement during grilling. There is no session scanning, inferred favorite,
usage score, history cache, clone accounting, or session schema change in this feature.

The configuration is provider-connection scoped:

```toml
[provider_connections.openrouter]
provider = "openrouter"
api_base_url = "https://openrouter.ai"
api_key = "${OPENROUTER_API_KEY}"
favorite_models = [
    "anthropic/claude-haiku-5.5",
    "openai/gpt-6-luna",
]
allowed_models = [
    "anthropic/*haiku*",
    "openai/gpt-6*",
]
```

These IDs illustrate syntax, not current model availability.

### Confirmed behavior

- Omitted `allowed_models` means unrestricted. Explicit `[]` is a configuration error.
- Allowed patterns use ASCII case-insensitive, full-ID glob matching. Support `*`, `?`,
  and character classes, OR across patterns, with `/` treated as ordinary ID content.
  Malformed patterns fail configuration loading. Never rewrite a provider's actual model ID.
- Enforce restrictions across profile-built engines, including initial/default/agent models,
  named/raw/direct selections, subagents, utility requests, and compaction. Disallowed
  nonempty configured engine models and configured favorites are configuration errors.
  Empty implicit engines remain discovery-only until an allowed model is selected.
- `favorite_models` contains exact, case-sensitive IDs, not patterns. Adding an existing
  favorite is idempotent. Unmark removes every exact duplicate. Removing the final entry
  removes the key. Preserve untouched raw interpolation; refuse ambiguous expression-backed
  removal with an explanation instead of materializing expanded values.
- Ctrl+F initiates immediate favorite-only persistence in both picker contexts. It never
  activates the model or confirms/discards draft parameters. Enter retains existing behavior.
  Esc does not undo a committed favorite. Plain `f`/`F` still edits the list filter.
- Favorites require a current allowed catalog entry to appear as selectable favorite rows.
  Missing IDs stay configured but hidden and are not configuration errors. Policy-compliant
  named-engine shortcuts remain selectable independently of catalog discovery. Distinguish
  outages, no eligible models, and a text filter matching nothing.
- Order named engines, then favorites, then remaining catalog models. Preserve configured
  favorite array order within deterministically sorted connections; append newly marked IDs.
  Remove favorite pairs from the raw remainder, not distinct engine shortcuts. Favoriting a
  shortcut changes its connection/model pair's membership, not its settings or engine identity.
- Persist only to the exact existing active TOML profile and backing connection table.
  No profile creation, connection materialization, migration, or other-layer fallback.
  Disable unavailable persistence with guidance, including unsupported environment-only or
  legacy connections. A shared home profile is still the target if it is the active source.
- Preserve comments, unknown fields, interpolation expressions, file permissions, credentials,
  and unrelated settings. Never serialize the resolved runtime profile back to disk.
- Retain prior favorite/UI state on save failure and allow retry. Regroup after success while
  preserving exact highlighted identity and parameter drafts. No unrelated settings reload.
- Serialize favorite saves. Ignore additional Ctrl+F while pending, but allow ordinary
  navigation and closing. Completed saves apply independently of popup/session visibility.
  Reconcile parked approval/clarify snapshots. Normal shutdown waits for a started write;
  closing the picker does not guarantee filesystem cancellation.
- Reject external source changes against the startup/last-successful-save snapshot and
  request restart, not a document rebase. Unchanged-source I/O failures remain retryable.
  Check source identity/content before publication and serialize local writes. Atomic rename
  is not compare-and-swap against noncooperating editors; its remaining narrow race is an
  explicit limitation, not a promised lost-update guarantee.
- Workspace saves keep ADR-0003's explicit trust requirement. Warn that the next launch needs
  `ylva trust`; never approve the changed profile automatically. Current-session preference
  updates do not authorize loading other on-disk changes.
- Same-provider-kind `connect --force` retains favorite/allowed lists. Reject incompatible
  retained settings instead of silently discarding restrictions.

### Status and boundaries

All grilling questions are resolved and the user confirmed the final design.
The plan is finalized. Implementation has not been authorized or started. Confirmation is
not permission to change application code or other documentation, stage files, create a
branch, or commit.

Preserve named-engine routing, live context/reasoning editing, fixed compaction settings,
current-configuration resume behavior, existing catalog caching, and modal-input precedence.
No automatic model fallback, favorites database, provider-login redesign, general live config
reload, or Unicode case-folding dependency is included.

### Acceptance criteria

- Hand-edited favorites and Ctrl+F use one persisted provider-connection list and survive restart.
- Allowed-model validation and execution checks cover both bare-engine and router paths;
  rejection leaves the entire runtime state unchanged and sends no disallowed chat data.
- Glob syntax, omitted/empty lists, invalid patterns, configured-model conflicts, inherited
  defaults, exact favorite identity, and original ID spelling have focused offline tests.
- Favorite-only actions leave active model, engine settings, transcript, parameter draft,
  and agent-command stream unchanged. Help/approval/clarify retain input priority.
- Favorites and named shortcuts follow the availability, ordering, identity, and grouping
  rules above. Headers are not selectable; filtering is local and identity-based.
- Keyboard, mouse highlighting/copying, paging, scrollbars, narrow one/two-row layouts,
  active preselection, parameter confirm/cancel, and compaction previews remain correct.
- Targeted saves preserve the raw profile outside the intended membership change, detect
  stale sources, handle publication failures honestly, and never leak resolved secrets.
- Successful preferences survive `/clear`, `/session`, parked snapshots, popup closure,
  and normal shutdown without provider reconstruction or unrelated configuration reload.
- Unsupported write targets are explicit; workspace trust is never silently updated.
- `connect --force` preserves the new fields under the confirmed same-provider-kind contract.
- Affected documentation is updated and mandatory formatting, lint, and coverage gates pass.

## Implementation Plan

An implementing agent must flip each `[ ]` to `[x]` immediately after completing and
verifying that step, rewriting this file in place. Keep numbering consecutive and preserve
completed states during edits. A step is incomplete while its affected docs are stale.

1. [ ] Obtain explicit implementation permission before code changes; final design
   confirmation is already recorded. Recheck current source and ADR-0003; the chosen trust
   behavior needs no exception or superseding ADR. Preserve unrelated user edits. Start with
   `src/config/provider.rs` and the baseline test in step 2; no original idea or conversation
   is needed to implement this self-contained plan.
2. [ ] Establish the picker baseline with
   `cargo test --lib models_lists_the_candidates_and_preselects_the_active_model`.
   Add failing configuration tests in the existing loading/validation suites for exact
   favorites, ASCII case-insensitive full-ID globs, slash matching, `*`/`?`/character classes,
   OR semantics, rejected empty/malformed policies, omitted-unrestricted behavior, disallowed
   configured models/favorites, defaults, and discovery-only implicit engines. Include old
   profiles, interpolation, duplicate identity, unknown-key hints, and whole-file precedence.
3. [ ] Add provider-connection preferences and compiled policy in `src/config/provider.rs`,
   existing profile validation, and `src/config/unknown_keys.rs`. Reuse `glob` with explicit
   ASCII case-insensitive string matching and ordinary slash handling. Keep exact favorite
   membership separate from permission matching. Validate inherited nonempty models and
   favorites against their connection's policy without requiring catalog discovery. Empty
   implicit engines cannot chat until bound. Verify step 2 and update `docs/configuration.md`.
4. [ ] Carry immutable policy through `src/agent_factory.rs`, `src/llm/provider_registry.rs`,
   `src/llm/engine.rs`, and `src/llm/engine/runtime.rs`, including bare-engine operation,
   implicit engines, shared subagent engines, and `configured_copy`. Guard initial activation,
   preview, direct/raw/named selection, and all profile-built chat paths before hydration or
   dispatch. Audit `apply_model_option`, which currently edits reasoning before switching.
   Reject before any model/effort/window/modality/active-engine/persistence mutation. Verify
   factory/runtime/router/selection tests plus startup/resume wiring; document policy ownership
   in `docs/implementation-notes.md` and enforcement in `docs/features.md`.
5. [ ] Apply eligibility to selectable catalog options in bare-engine and router paths without
   discarding provider transport metadata, hydration data, partial-provider success, current
   cache behavior, or failure retries. Refactor `src/tui_driver/models.rs` so compliant
   shortcuts survive absent/empty/all-filtered/failed catalogs. Distinguish these outcomes
   from text-filter misses. Test independent same-kind connection aliases, one-engine use,
   missing favorite IDs, shortcut activation during outages, and preserved context/reasoning
   metadata. Update the discovery description in `docs/features.md`.
6. [ ] Add failing targeted persistence tests at the configuration writer's owner boundary.
   Cover exact/idempotent adds, all-exact-duplicate removals, final-key removal, comments,
   unknown fields, permission bits, untouched credentials/interpolation, ambiguous expression
   removal, `providers` alias, inline/dotted tables, missing backing tables, read-only targets,
   source disappearance/replacement, external modifications, successful snapshot advancement,
   and unchanged-source retry. Exercise pre-publication and post-rename failure outcomes,
   explicit workspace trust reminders, no trust-store mutation, and no unrelated reload.
7. [ ] Implement a narrow raw-TOML favorite editor, not resolved ProfileConfig serialization.
   Use `toml_edit` and `src/private_fs.rs::replace_file` where suitable. Capture source
   identity/raw contents from the document actually used to load the profile; compare with
   the startup/last-successful snapshot and recheck before publication. Reject changes with
   restart guidance, without rebase. Preserve raw untouched entries; refuse ambiguous
   interpolation removal. Update only membership and remove an empty favorite key. Never
   create connections/profiles, switch layers, write `.env`, or update trust. Advance the
   expected snapshot only after confirmed success. If an error occurs after rename, report
   possible publication/durability uncertainty and reconcile safely rather than claiming
   the file stayed unchanged. Document the external-editor race limitation. Verify step 6
   and update save behavior in `docs/configuration.md`.
8. [ ] Add a TUI-lifetime profile-preferences/save owner and explicit startup-sourced write
   capability, separate from providers, engine request handles, agents, transcripts, and
   resettable conversation state. Thread provenance through `TuiApp`/`TuiRuntime`, not a
   guessed cwd or display label. Disable missing capability with setup guidance. Serialize
   background filesystem operations, suppress repeated Ctrl+F while pending, and publish
   authoritative membership only after confirmed persistence. Preserve completed state
   across popup closure, `/clear`, `/session`, and approval/clarify snapshot restoration.
   Let normal shutdown await a started write and surface its outcome. Test pending-repeat
   suppression, late completion, failure/retry, and shutdown. Document lifecycle ownership
   and trust notices in `docs/implementation-notes.md` and `docs/configuration.md`.
9. [ ] Implement modifier-aware Ctrl+F in both model-list and parameter-popup handlers.
   Pass full KeyEvent through `src/presenter/keyboard.rs`; recognize exact CONTROL + Char('f')
   before textarea fallthrough and retain existing modal precedence. Resolve typed identity
   from the filtered list selection or popup choice, never labels or active engine state.
   Emit desired membership with exact connection/model ID, not a replay-sensitive disk toggle.
   Explicitly classify/start the new save effect and handle completion in the outer loop.
   Consume Ctrl+F with no matches, pending save, or unavailable capability without activating
   a model or editing text. Preserve Enter, Esc, ordinary `f`/`F`, parameter drafts, and mouse
   highlight/drag-copy behavior. Add no favorite action row or mouse favorite button. Test
   both contexts, modifiers, help/approval/clarify priority, no engine/agent-command mutation,
   repeat suppression, and drafts during completion. Update status rendering, both relevant
   footer hints in `src/tui_widgets/footer.rs`, `docs/keybindings.md`, and `docs/features.md`.
10. [ ] Assemble named engines, favorites, and raw remainder in confirmed order in
    `src/tui_driver/models.rs`. Preserve favorite array order inside deterministically sorted
    connections; append new IDs and deduplicate raw favorite pairs. Keep named shortcuts
    distinct and favorite catalog rows on provider-based routing. Add nonselectable headers
    using shared row data in `src/tui_reducer_state/model_picker.rs`, not fake ModelChoice
    entries or parsed strings. Update drawing, navigation, scroll/page mapping, hit tests,
    and copied overlay text together. Regroup after success while preserving exact highlighted
    row identity and rebind any popup choice index safely so later Enter cannot switch the
    wrong model. Test filters, empty groups, aliases, active preselection, narrow layouts,
    parameter drafts, compaction preview, and footer visibility through driver/reducer/
    presenter tests and `tests/tui_render_state/overlays.rs`. Update picker documentation.
11. [ ] Preserve favorite/allowed lists during same-provider-kind `connect --force` updates
    in `src/connect_command.rs`. Retain raw preference fields, order, and interpolation rather
    than resolved values. Reject incompatible retained restrictions, never silently remove
    an allowlist. Add focused connect tests for preserved fields and no change on validation
    failure. Keep other connection-update behavior unchanged. Update `docs/cli-options.md`
    and `docs/configuration.md` for the preference-preservation contract.
12. [ ] Add assembled regression tests using existing public/executable targets. Verify
    save/reload/unmark, independent connections, policy bypass rejection, current-configuration
    startup/resume, bare-engine/router operation, unsupported environment/legacy writes,
    and workspace trust changes without auto-approval. Show untrusted startup sends no
    provider request. Use mocks, temporary directories, and explicit async completion gates.
    Favor owner-local edge tests; do not expose production internals solely for test access.
13. [ ] Complete affected docs in `docs/configuration.md`, `docs/features.md`,
    `docs/implementation-notes.md`, `docs/keybindings.md`, and `docs/cli-options.md`.
    Reconcile `docs/ideas/provider-model-favorites.md` with completed manual favorites and
    removal of session inference, preserving staged user additions. Check README's picker
    summary and change only if needed. If canonical terminology changes, load `domain-modeling`
    before updating `CONTEXT.md`. Session persistence is unchanged, so do not add a favorites
    history contract to `docs/persistence.md`. Do not edit generated changelogs or unrelated
    stale documentation. New doc files require separate consent.
14. [ ] Run targeted tests throughout, then `make format lint` and the mandatory
    `make test-coverage`, enforcing at least 80% aggregate line coverage. Do not duplicate
    the final gate with mandatory `make test`. Run `make audit` if dependencies changed.
    Run `rumdl check` on changed Markdown. Review the diff for unrelated edits, license
    headers, and secret exposure. Report failures honestly. No staging, branch, or commit
    is authorized by this plan.

## Implementation Hints

### Resolved grilling decisions

- The user confirmed the final design after all questions were resolved. This confirmation
  finalizes the plan only; it does not authorize implementation or git mutations.
- Q2 confirmed execution-wide allowed-model enforcement for profile-built engines.
- Q3/Q5/Q17 confirmed ASCII case-insensitive full-ID globs, `*`, `?`, character classes,
  OR across patterns, ordinary slash handling, malformed-pattern errors, and original ID spelling.
- Q4/Q11 confirmed omitted unrestricted, explicit empty invalid, disallowed configured
  nonempty engine models/favorites invalid, and empty implicit engines discovery-only.
- Q6 superseded Q1's automatic inference. Q7/Q8/Q10 history questions were explicitly skipped.
  No session analytics or inferred ranking remains in scope.
- Q9 confirmed catalog-listed eligible favorites, retention of absent IDs in configuration,
  independent compliant shortcuts, and distinct outage/empty/search-miss outcomes.
- Q12/Q20a/Q20b confirmed immediate Ctrl+F saves in both picker contexts, no favorite action
  row, no model activation, preserved drafts/Enter behavior, and no undo of committed saves on Esc.
- Q13 confirmed exact active source, no materialization/migration/other-layer fallback, and
  disabled persistence with guidance when no backing source/table exists.
- Q14 confirmed explicit renewed workspace trust with a warning and no automatic approval.
- Q15 confirmed engine/favorites/remainder order, per-connection array order, deterministic
  connection order, appended new IDs, and pair membership rather than engine settings.
- Q16/Q22 confirmed detected source conflicts require restart, unchanged-source I/O failures
  permit retry, failed saves preserve prior favorite/UI state, successful regroup preserves
  identity/drafts, and no unrelated reload or document rebase. Local serialization/source
  checks do not claim CAS against arbitrary external editors.
- Q18 confirmed exact case-sensitive identity, idempotent adds, all-exact-duplicate removals,
  last-entry key removal, preserved raw expressions, and refused ambiguous expression removal.
- Q19 confirmed same-provider-kind `connect --force` preserves both lists and rejects
  incompatible retained settings rather than discarding restrictions.
- Q21 confirmed serialized writes, ignored extra pending Ctrl+F, permitted navigation/closure,
  authoritative completion across popup/session changes, reconciled snapshots, and waiting
  for a started write during normal shutdown. Closing is not cancellation.

### Configuration, source identity, and writing

- `ProviderConnectionConfig` is in `src/config/provider.rs`. Unknown-key diagnostics are in
  `src/config/unknown_keys.rs`; profile loading/validation is in `src/config/profile.rs`.
- Profiles are TOML-only `profiles/<name>/config.toml`. YAML frontmatter is for agent-type
  Markdown, not profiles. Workspace profile files replace home files as whole resources;
  there is no per-connection/list merge. Environment files follow separate merge rules.
- Interpolation recursively expands array strings. Runtime ProfileConfig also receives
  defaults, CLI overrides, discovered agents, and environment credentials. It is not a
  lossless source document and does not derive Serialize.
- `provider_connections` accepts legacy alias `providers`. TOML can also use inline/dotted
  tables. A writer must not create competing canonical/alias sections.
- `profile_config_source_path` returns the selected existing file or None for built-ins.
  `layered::ResolvedFile` has raw contents, layer, and path; loading consumes that provenance.
- `TuiDisplayMetadata` in `src/tui_reducer_state/panels.rs` contains profile name/path,
  Ylva home, and workspace root. Startup populates it; session replacement preserves it.
  It is display information, not a raw-document version or validated write authority.
- `TitleDriver` holds the in-memory profile/factory used for session preparation. `/session`
  does not reload config from disk. Preferences stored only in one AppState would be lost.
- Factory creates implicit engines; `ProfileConfig::apply_default_model` fills explicit
  missing models. Environment discovery is in `src/startup/engines.rs`. Legacy flat engines
  can lack distinct connection tables. Adding a table can change startup resolution and
  suppress other environment-derived connections, so it is not a harmless favorite save.
- Cargo already includes `glob = "0.3"` and `toml_edit = "0.25.17"`. Installed glob supports
  `MatchOptions` with `case_sensitive: false`, `require_literal_separator: false`, and
  `require_literal_leading_dot: false`; case folding is ASCII-only.
- `connect_command::plan_connection` parses raw TOML into DocumentMut. `persist_connection`
  rereads but replaces the target table and may write `.env`; it is not a favorite editor.
  Its existing permission/raw-document patterns are reusable, not whole-operation semantics.
- `private_fs::replace_file` writes/syncs a same-directory temporary file, renames it, then
  syncs the directory. It provides atomic visibility, not locks/CAS. Directory-sync errors
  occur after publication. Renaming over a symlink replaces the link, so source identity
  checks matter. There is no general source-bound config save/CAS service today.

### Trust and execution contracts

- ADR-0003 is `docs/architecture-design-records/0003-trust-workspace-configuration-by-content-hash.md`.
  Lists are covered by existing effective TOML hashing. Legitimate workspace edits, including
  `connect --workspace`, require explicit trust again; no exception is needed or approved.
- `workspace_trust::trust_workspace` approves all current covered configuration, not just
  favorite changes. Calling it after a toggle could approve unrelated profile/.env/agent
  edits. `workspace_trust_notice` supplies reminder text. Trust gating occurs at startup,
  not continuously during the current TUI or same-process session replacement.
- `apply_model_option` in `src/llm/engine.rs` currently changes reasoning before switching.
  A rejection only inside the final switch permits partial mutation.
- `EngineImpl::configured_copy` creates independent original request settings for fixed
  summarizers and prepared sessions. Policy must survive copies; favorites are not request
  settings and do not require engine reconstruction or general reload.
- Resume resolves current configured models, not saved historical selection. `/clear`
  retains the live engine while replacing the session.

### Catalog and picker integration

- Router representatives are chosen per connection: the active engine for its connection,
  then deterministic engine order elsewhere. Successful router catalogs cache for 30 minutes;
  failures retry next listing and other connections' successes remain available.
- `AgentFactory::build` returns a bare engine for a single-engine configuration. Default
  `Engine::list_model_options` lacks router TTL caching and named shortcuts. The broad cache
  statement in `docs/features.md` omits this existing exception; this feature is not a cache refactor.
- Ollama metadata requests occur before picker filtering. Copilot endpoint-support tracking
  is separate transport data. Filtering options does not imply fewer discovery requests.
- `src/tui_driver/models.rs` runs cancellable catalog tasks with a 15-second timeout; its
  current empty-catalog path runs before shortcut assembly and needs the scoped change above.
- Named choices activate exact engines with their live settings. Raw/favorite catalog rows
  use provider-based routing; a pair does not identify a particular named engine.
- `ModelChoice`, `ModelParameterEdit`, `ParameterField`, and picker row helpers live in
  `src/tui_reducer_state/model_picker.rs`. Parameters store a full-choice-vector index;
  reorder without rebinding can make later Enter select the wrong model.
- `matching_model_indices` searches every case-insensitive filter word against engine name,
  connection, and ID, not metadata. Current row mapping handles only one engine/catalog divider.
- Picker keyboard handling currently receives KeyCode, not modifiers. Ctrl+F otherwise reaches
  textarea cursor-forward in the list and is ignored inside parameter editing. The new
  behavior therefore needs deliberate modifier-aware interception in both contexts.
- Help/tool approval and clarify/session picker have priority over model input. A picker
  parked in InputSnapshot does not own keys while another modal owns input. The snapshot
  includes fetched choices and parameter drafts, so restored favorite state needs reconciliation.
- Keyboard effects are split in the outer event loop. New save effects need explicit routing
  and completion, not agent commands. Existing mouse dispatch differs, but no mouse favorite
  action is requested; preserve its highlight and drag-copy behavior.
- Footer arrays differ for list and parameters and render unwrapped. A new hint is not
  automatically visible at narrow widths. Parameter rows currently have no mouse activation.
- The terminal bridge forwards Press events only, but held keys can still repeat as Press.
  Ctrl+F needs no new protocol; modified-key distinctions depend on the terminal. No live
  terminal smoke test was performed during planning.

### Verification and repository state

- Existing suites include `src/config/profile/tests/{loading,validation,unknown_keys}.rs`,
  connect writer tests, `src/agent_factory/tests/configuration.rs`,
  `src/llm/engine/tests/{runtime,router,selection}.rs`, `src/tui_driver/tests/models.rs`,
  driver effects/session tests, reducer picker tests, presenter keyboard/mouse tests,
  `tests/tui_render_state/overlays.rs`, `tests/cli_profile.rs`, and `tests/cli_workspace_trust.rs`.
- `docs/development-workflow.md` defines license headers, test boundaries, and gates.
  `docs/known-test-issues.md` documents tracing/cwd isolation, asynchronous store-lock release,
  and draining bounded UI channels concurrently. Tests use offline fixtures and explicit gates.
- The staged user change in `docs/ideas/provider-model-favorites.md` predates the plan.
  Unrelated edits to `docs/backlog-issues.md` and `tests/e2e/bash-tool.zsh` also exist; none
  authorizes changing them while refining this plan.
- Research queries incidentally refreshed `graphify-out/cache/last_query_stamp`. Source
  inspection, not graph freshness, grounds implementation facts. No application source was
  changed while preparing this plan.
