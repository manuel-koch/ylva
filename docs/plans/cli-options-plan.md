# Select a configured CLI engine and finish the `models` listing

## Context

Ylva is a Rust agent harness with headless and TUI chat startup. The approved
replacement for one-off provider/model overrides is `--engine <name>`: select
an existing explicit `[engines.<name>]` entry from the selected profile, with
its complete resolved configuration, for the root agent of this run. This
keeps connection, model, reasoning and limits together instead of inventing a
second configuration mechanism.

This plan supersedes the earlier corrective `--provider`/`--model` tasks, including their special
blocking catalog check and reasoning-reset questions. Those flags are removed, not retained as
aliases. The implementation now selects a named engine by changing the resolved root agent type's
in-memory mapping. The independent `models` command includes the trust/environment, scores,
favorites and terminal-safe aligned-output corrections.

The user authorized both the plan update and implementation. Preserve unrelated
working-tree edits, including `docs/ideas/bash-approval.md`; no git branch, stage,
commit or stash operations are authorized.

### Acceptance criteria

- `--engine` is a top-level, value-taking, non-repeatable chat-only option for
  both headless and TUI startup, including fresh, resumed and cloned sessions.
  Top-level non-global flags precede subcommands. Every subcommand, including
  `models`, `connect` and `trust`, rejects a leading `--engine` before dispatch
  or command side effects; trailing placement is also rejected. Existing
  `--version` precedence remains unchanged.
- The value names an explicit `[engines.<name>]` key in the selected profile, not a provider kind,
  connection, implicit connection engine or environment-discovered name. Unknown, empty and
  whitespace-only values fail with exit
  `2` before provider requests or session creation, resume append, clone creation or other session
  writes.
- After resolving the explicit/restored root agent type, select the engine by
  changing only that type's in-memory `engine` mapping. Do not rewrite engine
  definitions, `default_engine`, personas, tools, permissions, other agent
  types' mappings or profile files on disk. Use the complete resolved named
  engine configuration, including its connection, model, reasoning, context,
  output limit and other settings; ordinary defaults and validation still apply.
- Keep existing runtime sharing for subagents. A subagent resolving the same
  agent-type mapping or named runtime follows existing rules; this is not a
  new per-instance isolation feature. Subagents mapped to other engines are
  not retargeted. Fixed explicit compaction and utility engines remain fixed,
  even when their name matches the selected engine. Unset compaction/utility
  roles retain existing fallback/following rules.
- Use normal named-engine catalog behavior: no extra CLI-only catalog fetch,
  blocking check, missing-model rejection, metadata switch, timeout fallback
  or reasoning-reset policy. Existing configuration validation, hydration,
  advisories and provider errors remain authoritative.
- Fresh-session metadata, initial `ModelChanged` parameters, runtime requests
  and initial TUI state agree with the selected engine. Resume/clone history
  records the effective selection through existing events without changing
  the clone source. Resume hints retain a shell-safe explicit `--engine`
  value; omit it when unset rather than inferring it from history or `/models`.
- `ylva models` is independent of chat engine selection and covers every
  provider connection, including those unused by explicit engines. Remove
  its provider filter and all `--provider`/`--model` CLI support. It opens no
  session, consumes no stdin and makes no profile, favorite or selection writes.
- Listing uses normal profile selection, workspace trust, `.env`, interpolation,
  process-environment precedence, warning/strict-config handling, credential
  supplementation and environment discovery when neither engines nor
  connections are configured. ADR 0003 applies before materializing untrusted
  workspace settings or sending credentials; home profiles need no approval.
- Query each connection once, with a 15-second total catalog/enrichment budget
  per connection. Apply allowlists, attach local `model-scores.toml` scores
  with the picker's lookup semantics, list favorites first in configured order,
  omit unavailable favorites without deleting them, and sort other rows
  deterministically without duplicates. Connection representatives and group
  order are deterministic.
- Group listing output by connection with aligned columns and blank separation,
  connection-qualified IDs, aligned favorite markers, catalog details and
  optional scores. Sanitize all external/configured text to terminal-safe
  single-line text before intentional ANSI styling. Honor the global color
  policy used by `skill list`. Do not leak credentials or sensitive endpoint
  details through logs, raw provider errors or diagnostics.
- A successful empty catalog counts as success. Listing exits `0` when at least
  one connection succeeds (warning on partial failure), `1` when all fail or
  runtime output fails, and `2` for usage/startup errors. Setup failures occur
  without opening sessions or consuming stdin.

### Planned extension: save picker parameters as named engines

`ylva models` remains useful as non-TUI catalog discovery even though CLI chat
launches select configured engines. The next extension makes the `/models`
parameters page the bridge between experimenting with a model and a repeatable
`--engine` launch. The grilling interview settled the following contract:

- Ship creation and update together. The parameters page has **Apply**, **Save…**,
  and the existing conditional **Reset**, not separate create/update/use buttons.
  Apply remains runtime-only; Save opens a form without discarding the draft.
- The save form chooses **Create new engine** or **Update this engine**. Update
  is available only for an identified explicit named engine, never inferred from
  whichever engine was active before selecting a raw catalog model. Creation
  requires a new name. Show destination, complete resulting settings and an update
  change preview. Final buttons are **Save** and **Save & apply**; Esc returns to
  parameters with edits intact. Initial focus stays on the form, not a commit
  button. Enter in a text field does not save; Enter activates a focused button.
- Save the displayed draft, including edits not applied to the conversation.
  Preserve default-versus-explicit choices: provider-default reasoning stays
  default, and catalog-derived context/output values do not become fixed limits.
  Copying or updating an existing engine preserves its non-editable settings;
  creation from a raw catalog model uses standard defaults, never hidden settings
  inherited from the previously active engine. Preview the resulting settings.
- Persist only to the actual profile document loaded at startup, showing its
  destination before confirmation. Disable saving with setup guidance when a
  suitable writable source or persistent provider-connection reference is absent.
  Never silently create profiles/connections, copy credentials/endpoints, or
  change default engine, agent, compaction or utility mappings.
- New names use 1–64 ASCII characters: first a letter/digit, then letters,
  digits, `_` or `-`. Matching remains case-sensitive. Existing names outside
  this grammar remain selectable and updatable. Creation rejects names already
  used by any explicit or implicit runtime engine. `default` is covered when
  actually occupied; compaction is a role, not a reserved name. An engine named
  `compaction_engine` is protected by the same actual-name collision rule.
- Save publishes the new configured definition for subsequent selections and
  newly constructed consumers without changing the current runtime. Save & apply
  additionally activates it in the current conversation. Running subagents and
  existing fixed compaction/utility instances retain their snapshots. Publication
  must reach future roots, subagent construction and reopened pickers, not merely
  add a visible row. No automatic activation follows creation through Save alone.
- A confirmed workspace save is immediately usable in the current session.
  Explain before confirmation and after a changed save that the next launch needs
  `ylva trust`; never update the trust store automatically. Home saves need no
  workspace approval and no-op saves need no new trust warning.
- On persistence failure, preserve the draft and previous runtime. On activation
  failure after successful persistence, retain the saved definition, preserve the
  previous runtime and report **Saved, but could not activate**. Do not roll back
  disk; retry activation through the picker.
- External profile edits block saving instead of being merged/overwritten. Keep
  the draft and require configuration reload/restart before retrying. Engine and
  favorite writes share coordinated source tracking so successful internal writes
  do not conflict. Serialize background profile writes, block duplicate engine
  submissions, and await started writes during normal shutdown. Dialog closure
  does not cancel a started write; leaving the dialog or changing session/selection
  suppresses delayed activation while still reporting save completion.
- Preserve keyboard conventions: Tab/Shift-Tab reach every field group and enabled
  button; arrows retain existing parameter editing behavior. Esc from Save keeps
  the draft; Esc from parameters discards it as today. No new Space activation or
  arrow-to-button behavior is required.

All actions must remain usable in narrow terminals. Action buttons wrap into
multiple rows based on available width, including a vertical layout, with
scrolling/focus visibility for short terminals and consistent keyboard/mouse
behavior after resize on both parameters and save pages. Keep the same logical
focus and draft on resize and scroll focused controls into view. Below the usable
minimum size, show resize guidance while preserving edits and Esc cancellation;
do not replace actions with ambiguous truncated labels. Acceptance includes
narrow-width rendering and actual navigation/hit-testing, not merely an unclipped
screenshot.

Only this plan update is authorized for the extension. Existing steps 1–11 and
their verification remain complete; new steps 12–19 are implemented.

### Verification status and starting point

Verified with `make format lint`, default-feature `cargo clippy --all-targets`,
targeted `rumdl check`, and `make test-coverage` (88.92% aggregate line coverage;
76.15% for `src/models_command.rs`). Engine launch regression tests cover
fresh/resume/clone selection, complete settings/history, actual HTTP request
model/limits, shell-safe hints, invalid selection without session writes, removed
flags, subcommand rejection, and explicit legacy engine/default-model support.
Factory tests cover fixed compaction/utility and other agent mappings; the
existing sharing/TUI tests remain in the full gate. Listing tests directly
exercise ordering, scoring, formatting, policy, safe failures, I/O and bounded
timeouts, plus binary trust/environment/color/read-only contracts.

Start future changes at `src/cli.rs`, `src/startup/profile.rs`, `src/startup.rs`,
`src/agent_factory.rs` and `src/models_command.rs`; focused regressions live in
`tests/cli_models.rs`, `tests/models_listing.rs`, and `src/models_command/tests.rs`.

## Implementation Plan

The implementing agent must flip `[ ]` to `[x]` immediately after completing
and verifying each step and rewrite this file in place. Keep numbers consecutive.
Do not mark the feature complete until the acceptance tests and final gate pass.

1. [x] Replace the old CLI surface in `src/cli.rs` and `src/main.rs` with
   `--engine <name>` and an unfiltered `Models` command. Reject engine selection
   with every subcommand before dispatch, including `connect` and `trust`.
   Add parser/binary regressions for both placements, missing/empty/whitespace
   and repeated values, removed old flags, and `--version` precedence. Remove
   tests asserting silently ignored flags.
2. [x] In `src/startup/profile.rs` and `src/startup.rs`, validate engine names
   against explicit profile entries and apply the selection only after root
   type resolution, before provider requests and `open_store`. Change only the
   selected type's in-memory engine mapping. Add fresh/resume/clone tests with
   explicit `--agent` and a restored non-default type, complete engine settings,
   default-model inheritance, invalid names and connection-only/environment rejection, plus legacy
   explicit-table acceptance. Assert no profile or failed-startup session writes.
3. [x] Remove the superseded provider/model override machinery and special
   catalog-check state from `src/startup/profile.rs`, `src/agent_factory.rs`,
   `src/llm/engine.rs` and `src/llm/engine/runtime.rs` wherever it remains.
   Preserve normal named-engine construction, validation, hydration and catalog
   notices. Test that selecting an engine adds no CLI-specific catalog request
   or missing-model gate, including catalog outages and strict-config runs.
   Do not add a new effort reset or raw-model switching path.
4. [x] Verify `src/agent_factory.rs` role resolution without retargeting other
   mappings or changing sharing. Cover same/different named subagent runtimes
   and subagents using the selected root type. Test explicit same/different-named
   compaction and utility engines, plus unset roles in TUI/headless paths.
   Retain configured-copy isolation and existing unset-role following/fallback
   behavior across startup and later `/models` switches.
5. [x] In `src/startup.rs` and `src/startup/session.rs`, derive initial metadata,
   `ModelChanged` parameters and runtime state from the selected named engine.
   Retain only explicit engine selection in shell-safe resume hints. Add HTTP
   regressions using genuinely different engines/settings for fresh/resume/clone,
   asserting actual requests and recorded parameters agree, source/profile bytes
   stay unchanged, and hints omit unset selections and quote accepted names.
6. [x] Share the minimal trusted provider/profile startup path between chat and
   `src/models_command.rs` without importing session, skills or UI startup.
   Remove the listing's duplicate reduced loader and provider-filter plumbing.
   In `src/main.rs`, replace error-prose matching with a typed result/exit
   contract and propagate diagnostic/output I/O failures. Test untrusted and
   changed workspaces send no requests, trusted/home profiles work, `.env`
   interpolation and process precedence match chat, strict warnings fail,
   credentials are supplemented safely, and no stdin/session writes occur.
7. [x] Finish all-connection listing resolution in `src/models_command.rs` and
   `AgentFactory::connection_engines`, including unreferenced connections and
   environment-only discovery. Keep representatives/group order deterministic,
   allowlist filtering and one bounded catalog/enrichment query per connection.
   Add tests for successful empty catalogs, partial/total failures, timeouts,
   setup errors, and multiple engines sharing a connection. Confirm listing
   never depends on a chat root engine selection.
8. [x] Attach model scores with `src/llm/model_scores.rs` lookup/loading semantics
   used by `src/tui_driver/models.rs`. Order favorites first in configured order,
   then deterministic remaining entries, with no duplicates or preference writes.
   Add direct unit tests for allowlists, favorites, unavailable favorites, score
   attachment, malformed score warning/strict behavior and stable ordering.
9. [x] Finish grouped, aligned, terminal-safe listing rendering in
   `src/models_command.rs` using existing safe-text helpers. Sanitize IDs,
   connection names, reasoning strings, scores and diagnostics before styling;
   redact sensitive provider errors. Test CR/newline/ESC/ANSI/other controls,
   slash-containing IDs, multi-connection blank separation, visible-width
   alignment and `auto`/`yes`/`no` under redirection, `TERM=dumb` and `NO_COLOR`.
10. [x] Reconcile the approved-target documentation with the verified behavior in
    `docs/cli-options.md`, `docs/configuration.md` and `docs/features.md`;
    preserve superseded status in `docs/ideas/cli-model-override.md` and
    `docs/ideas/cli-provider-override.md`. Update this plan's verified progress
    and remove pending labels only after tests pass. Check ADR 0003 and existing
    persistence contracts for consistency without changing unrelated docs or
    creating new ones; report any additional required doc change for approval.
11. [x] Run `make format lint`, default-feature `cargo clippy` per the workflow,
    targeted `rumdl check` on the six named Markdown files, and the single full
    gate `make test-coverage`. Fix all failures/warnings, maintain at least 80%
    aggregate line coverage, and inspect exercised coverage of new listing and
    startup paths rather than relying on aggregate coverage alone. Run
    `make audit` if dependencies change. Re-audit every acceptance criterion
    against source/tests before marking completion; no duplicate mandatory
    `make test` or git mutations.
12. [x] Establish the engine-save architecture against the settled contract above.
    Check ADR 0003 and recorded engine-isolation decisions. Design coordinated
    profile-source ownership for favorites/engines and live definition publication
    before implementing writes. Preserve existing consumer snapshots and account
    for a root starting with one bare engine as well as a multi-engine router.
    Obtain approval before creating any new doc or ADR if one is needed. This
    extension remains planning-only until implementation is authorized.
    Decided design (no new doc/ADR needed; ADR 0003 holds because trust is never
    touched): `FavoriteModelsWriter` is the single owner of the startup profile
    snapshot and gains `save_engine`, so favorites and engine saves share one
    snapshot. One driver owner serializes both. Live publication makes
    `EngineRouter.engines` a `RwLock` map with `register_named_engine`, shares
    `AgentFactory.engines` between factory copies, and TUI roots are always a
    router whose sole engine stays "transparent" (no named rows) until a second
    engine is registered. Subagents of an existing root keep their
    construction-time snapshot plus the inherited live router.
13. [x] Extend the model-picker parameters state and actions in
    `src/tui_reducer_state/model_picker.rs` and the relevant presenter/driver
    handlers with **Apply**, **Save…**, and conditional **Reset** on the parameters
    page. Save opens a create/update form with **Save** and **Save & apply** final
    buttons. Represent origin-engine identity and draft/default provenance
    explicitly, independently of the active conversation selection. Preserve edits
    through save-form cancellation and validation/write failures. Initial form
    focus must not be on a commit button; Enter only commits a focused button.
14. [x] Implement the save form's Create new engine/Update this engine modes,
    naming validation, full configuration preview and update diff. Show the
    startup-loaded destination and workspace re-trust consequence. Limit Update
    to the identified explicit origin engine; raw-model selections offer creation
    only. Use the settled grammar for new names without restricting existing
    names on Update. Reject collisions across the complete explicit/implicit
    runtime namespace, not role names as reserved words. Validate source and
    persistent connection eligibility/model policy; otherwise show setup guidance.
    Do not offer destination selection or change engine/role/default mappings.
15. [x] Implement source-aware engine configuration persistence using existing
    configuration writer conventions. Store a provider connection reference,
    never copied endpoints or credentials. Persist explicit parameter choices,
    not catalog-derived runtime values: unset context/output limits stay unset,
    and provider-default reasoning stays default. Preserve all supported editable
    settings; preserve non-editable settings on both copy and Update, and use
    standard defaults for raw-model creation. Preserve unrelated TOML, comments,
    interpolation and supported fields. Coordinate source snapshots with favorite
    saves and serialize background writes. External edits cause a conflict that
    retains the draft and requires reload/restart; do not merge or overwrite them.
    Publish atomically, retaining existing race-limit guarantees rather than
    claiming rename is compare-and-swap. Block duplicate submissions, let started
    writes finish after dialog closure, and await them during normal shutdown.
    Leave disk/runtime unchanged on write failure; never write `.env` or trust.
    Progress: the writer layer (`save_engine`, name grammar, shared snapshot,
    unit tests in `src/config/favorite_models/tests.rs`) is implemented; the
    driver owner is open.
16. [x] Publish saved definitions coherently to live engine discovery, configured
    baselines and future root/subagent construction after confirmed persistence.
    Saving a new engine never changes the active runtime, whereas updating the
    active engine takes effect in the session; Save & apply activates only in its
    originating conversation/selection context. Track operation ownership so
    leaving the dialog or changing session/selection suppresses delayed activation
    but not save completion reporting. Existing consumers retain snapshots.
    Activation must preserve the previous runtime on failure, retain the saved
    definition without disk rollback, and report Saved, but could not activate.
    Support retry through the picker. Confirmed workspace changes are usable in
    session without auto-trust; display next-launch re-trust guidance. Reopened
    pickers and future CLI launches see saved engines (after required re-trust),
    including registration of a second engine into an initially single-engine
    session. Record model history only for an actual successful runtime change.
17. [x] Make parameters-page action layout responsive in
    `src/tui_widgets/overlays.rs` and its shared overlay-layout/hit-test state.
    Measure visible label widths and available interior width; wrap actions into
    additional rows, including one action per row when necessary. Do not squeeze
    all actions into one clipped line or silently hide persistence actions.
    Include wrapped rows in popup height, scrolling and selected-row visibility.
    Apply the same layout policy to Save and Save & apply on the save page. On
    short terminals, scroll the focused control into view rather than drawing
    outside the viewport. Tab/Shift-Tab reach all field groups/enabled buttons;
    preserve existing arrow editing behavior and Enter-on-focused-button
    activation, with no Enter submission from text fields. Esc from Save retains
    the draft; Esc from parameters discards it. Clip mouse hit areas to rendered
    controls, use terminal display widths, and retain logical focus after resize.
    Below the usable minimum, show resize guidance, preserve edits/focus and
    permit Esc cancellation; do not silently hide actions in an otherwise usable
    layout or use ambiguous truncated labels.
    Progress: buttons wrap by measured display width (several per row when
    they fit), scroll into view and have clipped hit areas (Save page
    included), and below the minimum width resize guidance is shown while
    edits/focus are kept and Esc still works.
18. [x] Add reducer, rendering/layout, writer and integration regressions for the
    save/update flows. Cover creation from a raw catalog row, cloning an existing
    engine under a new name, explicit Update with preview, default-versus-explicit
    parameter provenance, duplicate/invalid names, cancellation, read-only or
    missing sources, conflicts, failed writes, workspace trust, live refresh,
    active selection/history and subsequent CLI launch. Include Save without
    activation, Save & apply, activation failure without rollback, dialog/session/
    selection changes while saving, shutdown awaiting writes, duplicate submission,
    interleaved favorite/engine saves, existing non-slug names and implicit-name
    collisions. Assert no implicit reservations for compaction role names. Test
    both parameters and save pages at
    widths such as 80, 60, 40 and 24 columns, with long engine names and Unicode
    labels, and narrow/short combinations. Assert every enabled action is
    keyboard-reachable, visible when focused, and has accurate mouse hit boxes
    whenever rendered. Exercise wide-to-narrow-to-wide resize without losing
    edits, focus or button access; test the below-minimum-size fallback too.
    Progress: reducer, owner (interleaving, shutdown, failure, skipped
    Save & apply), router/factory publication, render/hit-box (widths
    80/60/40/24, Unicode name, wrapped-row clicks), resize guidance, and an
    integration test (`tests/provider_favorites.rs`) covering a save into a
    workspace profile, comment preservation, create-over-existing refusal and
    a following `--engine` launch.
19. [x] Update affected existing docs in `docs/features.md`,
    `docs/keybindings.md`, `docs/configuration.md` and `docs/cli-options.md` for
    runtime Apply versus persistent Save as/Update, destinations, trust,
    default-value semantics, sharing effects and responsive button navigation.
    Retain `ylva models` as read-only provider-catalog discovery; show the picker
    as the bridge from experimentation to repeatable `--engine` launches. An
    `engines` CLI listing is a possible separate feature, not part of this
    extension. Re-run the workflow's formatting/lint/Markdown/coverage gates and
    audit all extension acceptance criteria before marking steps 12–19 complete.

## Implementation Hints

- `src/cli.rs` owns clap parsing; `src/main.rs` dispatches commands before chat
  startup. The working tree may already contain engine-selection scaffolding;
  its presence does not establish passing acceptance tests.
- `src/startup/profile.rs` owns `load_profile`,
  `trusted_profile_workspace_root`, `load_selected_profile_environment` and
  in-memory CLI profile selection. `src/startup/engines.rs` owns provider
  environment loading, credential supplementation and environment discovery.
- `src/startup.rs` resolves the root agent type before engine/store construction.
  `ProfileConfig::engine_name_for` resolves that type's engine; `default_engine`
  is not necessarily the root's active engine. `profile.engines` is the explicit
  table map, unlike the factory's implicit/discovered runtime engines.
- An engine's complete resolved configuration includes ordinary profile defaults
  such as `default_model`; selecting it does not require every setting to be
  written literally in its table. Local allowlists and profile validation still
  apply to named engine construction.
- Root selection changes the chosen agent type's in-memory mapping, not engine
  definitions or the global default. The factory uses agent-type mappings for
  subagent construction too; existing sharing remains intentional.
- `configured_copy()` provides explicit compaction's isolated configured settings.
  `utility_engine` is fixed when configured; when unset, root TUI titles follow
  the active conversation engine, while headless/subagent titles use the existing
  default-engine fallback. Unset compaction follows the conversation engine.
- `src/startup/session.rs` owns resume hints and `open_store`; startup records the
  effective provider/model and parameters using existing history events. No new
  persistence schema or automatic restoration of the CLI engine name is required.
- ADR 0003 is
  `docs/architecture-design-records/0003-trust-workspace-configuration-by-content-hash.md`.
  Listing uses endpoints and credentials, so it is not a provider-independent
  discovery command like `skill list`. Refusing untrusted/changed workspace
  configuration rather than silently using home configuration is mandatory.
- `src/models_command.rs` contains catalog timeouts and rendering scaffolding.
  `Engine::list_model_options` already applies connection allowlists. The old
  listing loader/filter and missing score/safe-output work require verification
  against the current working tree, not assumptions from earlier completion claims.
- `src/tui_driver/models.rs` provides picker score/timeout precedents;
  `src/llm/model_scores.rs` owns score loading. `src/skill_command.rs` and
  `crate::skills::terminal_safe` provide safe-text/color precedents.
- Integration coverage belongs in `tests/cli_models.rs` with
  `tests/common/mock_llm_server.rs` via `mod common;`; direct module tests are
  needed too. A successful empty mock catalog cannot prove arbitrary raw-model
  selection and is not grounds for restoring the superseded blocking check.
- `docs/known-test-issues.md` records tracing, environment/current-directory and
  subprocess side-effect constraints; `docs/development-workflow.md` owns testing
  and coverage policy. The completed engine-selection design has no open decisions;
  the picker-persistence extension decisions are settled in its Context contract.
- `src/tui_widgets/overlays.rs` currently packs parameter buttons into rows and
  records their rectangles in overlay layout state. `src/tui_reducer_state/model_picker.rs`
  owns parameter selection state; responsive layout must agree with those logical
  focus/action indices rather than introducing rendering-only wrapping.
- `src/config/favorite_models.rs` and `src/tui_driver/favorites.rs` provide
  source-captured write capability, serialized background saving, stale-source
  detection and atomic publication precedents. Favorite persistence is not an
  engine writer; its trust/source constraints are relevant, not permission to
  overwrite arbitrary profile layers.
- Engine names are currently exact, case-sensitive TOML keys with no engine-specific
  slug restriction. The agreed creation grammar is new UI validation, not a new
  restriction on reading/selecting/updating existing engine keys.
- Factories and routers currently own engine maps, configured baselines are
  immutable, subagent closures capture maps, and a single-engine root need not
  have a router. Disk persistence or changing picker rows alone does not provide
  live registration. Fixed compaction copies have intentionally isolated objects.
- The favorite writer captures the exact startup-selected document and advances
  its source snapshot after success. Independent writers for the same document
  would invalidate each other's snapshots without coordinated ownership. Its
  atomic rename does not prevent an uncooperative external writer racing the
  final comparison and publication.
- Existing favorite saves publish immediately in-session without updating trust;
  changed workspace saves warn that the next launch needs re-trust. The confirmed
  engine-save contract explicitly adopts that session/next-launch distinction.
- Settled persistence policy is startup-source-only, with persistent connection
  eligibility; no destination chooser or automatic profile/connection creation.
  Save and Save & apply separate definition publication from runtime activation.
  Existing consumers retain snapshots; external conflicts require reload/restart.
- Current parameter editing includes context, reasoning and output limits, not
  temperature or compaction thresholds. Draft/default provenance is distinct from
  displayed effective catalog values. Parameters currently use Tab for buttons,
  Enter for focused actions, and Esc to discard; Space is not an activation key.
