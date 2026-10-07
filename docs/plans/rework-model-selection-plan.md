# Rework `/models` engine selection

## Context

Ylva is a Rust terminal and command-line agent harness. Its `/models` picker
offers both named-engine shortcuts and provider catalog rows. An Engine is a
reusable model configuration binding a model, provider connection, and request
settings; its Engine runtime is the live selection and effective settings. Two
named engines can use the same provider connection and even the same model
while having different temperature, reasoning, context-window, or
compaction-threshold settings.

The picker currently retains the engine name for display but discards it on
confirmation. Routing then chooses a representative engine for the provider
connection, not necessarily the engine the user selected. This can mutate the
wrong engine, leave the requested sampling settings inactive, and unexpectedly
change a summarizer sharing that engine.

The goal is to preserve named-engine identity end to end and explain the
relationship between the conversation engine and the engine summarizing its
context. This plan addresses the "Clarify/fix `/models` engine selection and
its effect on compaction" backlog item. The user authorized execution of this
plan; implementation and verification are complete. Changes remain unstaged
and uncommitted.

### Agreed behavior

- A named-engine shortcut activates that exact existing Engine runtime.
  Only a reasoning effort changed by the user in the picker is reapplied and
  validated against the advertised alternatives.
- Shortcuts represent current Engine runtime selections, not commands to restore
  engines to their original profile values.
- A raw catalog row retains existing routing: on the active provider connection
  it changes the active named engine's model; on another connection it selects
  the representative engine chosen by deterministic engine-name ordering.
- An explicitly selected summarizer stays bound to its resolved named engine.
  Mutating that engine changes subsequent summary requests; activating a
  different conversation engine does not retarget that summarizer.
- When neither the agent type nor the profile explicitly selects a compaction
  engine, summarization follows that agent's active conversation engine.
  This applies to root and subagent runs and intentionally changes the current
  documented implicit fallback. "Default" means this implicit selection, not
  an engine literally named `default` or the profile's `default_engine`.
- Already-dispatched summaries retain their request snapshots, original
  deadlines, and normal safe-application rules. Switching affects subsequent
  requests; it does not cancel/restart a summary or force compaction.
- The picker detail previews the resulting compaction provider-connection/model
  and effective context window. It does not repeat the conversation
  engine/model already shown in the TUI footer. The detail is orange when a
  fixed compaction engine would remain on a different provider/model or
  effective context window from the highlighted choice; following compaction
  and choices that update the compaction engine show the resulting pair
  without a mismatch warning. Switch notices use compaction terminology to
  explain the resulting selection.
- Named-engine settings remain owned by their existing Engine runtime handles. No
  provider reconstruction, engine cloning, configuration writes, or new knobs
  are required.
- Existing provider/model persistence and resume behavior remain unchanged.
  Engine identity and reasoning effort are not newly persisted.

The user confirmed this contract through a grilling interview. All product
questions are resolved. This contract was implemented and verified under the
execution authorization.

### Acceptance criteria

- Selecting either of two named engines on one provider connection activates
  exactly the chosen engine, including when their model IDs are identical.
  Its temperature, explicit context length, and compaction thresholds take
  effect; an unselected sibling is not mutated.
- The presenter and effect payload preserve the selected engine identity.
  Named shortcut active markers and initial selection use engine identity;
  raw catalog active markers continue to use provider connection/model.
- Invalid engine names, connection mismatches, and unsupported explicitly
  edited reasoning efforts fail visibly without activating a different
  engine or recording a successful switch.
- A shortcut whose model is missing from the catalog still activates its
  existing Engine runtime state. Unknown reasoning metadata does not make an unchanged
  effort invalid; existing lazy hydration and warnings remain available.
- Catalog context lengths follow Engine runtime model changes only when the engine
  has no explicit context length. Input modalities and reasoning metadata
  remain consistent with the selected model.
- An explicit shared summarizer follows mutations of its own engine; an
  explicitly separate summarizer retains its model, settings, and window.
  Switching away leaves an explicit summarizer bound, whereas an implicit
  summarizer follows the newly activated conversation engine.
- The TUI detail identifies the compaction provider-connection/model and
  effective context window that will result from the highlighted choice,
  without duplicating the conversation engine/model already visible in the
  footer or mistaking identical provider/model pairs for shared Engine runtime
  identity. A fixed compaction engine is orange when its provider/model differs
  from the highlighted choice. The detail is available while idle and
  inspecting `/models`; highlighting previews the effect without mutating any
  Engine runtime.
- Capacity warnings still run immediately after successful switches, remain
  cancellable and responsive, and do not repeat an unchanged warning on the
  next prompt. Selection failure does not trigger a success record.
- Existing filtering, keyboard/mouse navigation, reasoning cycling, raw-row
  routing, transcript preservation, subagent inheritance, utility isolation,
  and request snapshot semantics are preserved.
- A regression test fails on the original same-connection shortcut bug and
  passes after the fix. Related documentation is updated and the mandatory
  implementation verification gates pass.

## Implementation Plan

An implementing agent must change each `[ ]` to `[x]` immediately after
completing that step and rewrite this file in place. Keep numbering consecutive
and preserve completed checkbox states when refining the plan.

1. [x] Establish the regression baseline before source implementation.
   Read `src/presenter.rs` at the model-picker Enter handler first, then
   `ModelChoice` and `Effect` in `src/tui_reducer_state.rs`, and
   `EngineRouter::switch_model_option` in `src/llm/engine.rs`.
   Run `cargo test --lib engine_router` as the initial routing baseline.
   Add a failing regression using two real routed test engines on one
   connection with distinguishable settings; confirm the shortcut's identity
   is lost through the actual presenter/effect/driver path rather than testing
   only a newly added router method.
2. [x] Preserve the selection target in a typed engine-selection operation.
   Distinguish named-engine activation from raw provider/model selection; do
   not make the Engine layer depend on TUI `ModelChoice` or display labels. Carry the
   identity from the presenter's Enter handler through `Effect::SwitchModel`
   and `execute_slash_effects`. Route named selections by exact map key,
   and activate the existing engine without restoring a picker snapshot of its
   model or blindly reapplying its unchanged reasoning effort. Carry enough
   information to distinguish an edited effort from an untouched one, including
   an explicit change back to provider default. Validate edited efforts
   against that model's advertised alternatives. Retain existing raw-row
   routing and plain `Engine` compatibility.
   Validate before mutation where possible and publish the active-engine
   change only after success. Test unknown names, connection mismatches,
   invalid reasoning, same-model siblings, cross-connection activation, and
   the existing single/unnamed-engine path. If a named operation carries
   provider/model metadata, reject mismatched identity rather than accidentally
   rewriting the selected engine to a stale or foreign picker value.
3. [x] Correct picker identity and metadata in
   `src/tui_driver.rs::apply_model_list_result` and the related label/selection
   helpers in `src/tui_reducer_state.rs`. Use the actual engine key for named
   shortcut active status and preselection; initialize a shortcut's reasoning
   from that engine's current effective settings rather than borrowing the active sibling's
   value solely because their provider/model pairs match. Keep effective
   shortcut window display separate from genuine provider-catalog metadata
   so displaying an explicit override cannot become a catalog override.
   Keep shortcut activation usable when that model has no catalog metadata:
   preserve unknown metadata and unchanged effort rather than fabricating
   unsupported values or rejecting the shortcut. Preserve existing overall
   catalog-loading errors and empty-result behavior.
   Test duplicate provider/model pairs, engine-name filtering, reopening
   after an Engine runtime model/effort change, unavailable catalog metadata, empty
   filters, no matches, reasoning cycling, and mouse highlight versus Enter
   confirmation. Preserve one catalog query per connection.
4. [x] Implement explicit versus implicit summarizer binding in
   `src/config/profile.rs`, `src/agent_factory.rs`, and `src/agent.rs`.
   Retain selection provenance instead of inferring explicitness from the
   fallback name returned by `compaction_engine_name_for`.
   Per-agent explicit engine wins over profile explicit engine; timeout
   resolution stays independent. Bind explicit selection to its named engine,
   and implicit selection to that loop's conversation engine/router so
   subsequent summaries follow named activation and raw model changes.
   Do not pass `None` to disable compaction for implicit selection.
   Update startup capacity-check construction consistently for root and
   configured subagent types. Test missing overrides, timeout-only overrides,
   both explicit precedence levels, explicit references to `default` and
   `default_engine`, inherited subagent routers, and independent subagent
   engines. Preserve current request snapshots and deadlines, without
   cancellation/restart on switch.
5. [x] Expose truthful conversation/compaction display metadata from
   `src/agent.rs` and factory wiring in `src/agent_factory.rs`. Keep explicit
   compaction identity separately from the conversation router; implicit
   compaction identity follows that router's active engine. Derive sharing
   from resolved Engine runtime identity, not model equality.
   Carry display-only snapshots through `UiUpdate`, presenter state,
   `src/tui_render_state.rs`, and root/subagent projections as needed.
   Render only the highlighted selection's compaction outcome in a picker
   detail area in `src/tui_widgets.rs`; do not duplicate the conversation
   engine/model already shown in the TUI footer. Show the resulting
   provider-connection/model and effective context window, and color the detail
   orange when a fixed compaction engine would remain on a different pair.
   Keep switch notices consistent with this terminology. Share target and
   context resolution with engine routing instead of reproducing
   representative-engine ordering in the UI. Distinguish "mutates shared
   engine", "implicit summarizer follows", and "explicit summarizer stays".
   Use the existing update path; do not duplicate Engine runtime configuration into
   TUI-owned state or add blocking metadata requests. Show unknown window
   metadata honestly. Test startup, idle switching, highlight-only preview,
   separate engines with identical model IDs, subagent drill-in, clear, and
   narrow-terminal behavior using `TestBackend`; controls remain usable when
   space cannot fit full details.
6. [x] Verify compaction and shared-Engine-runtime behavior through focused tests in
   `src/llm/engine.rs`, `src/agent_factory.rs`, `src/agent.rs`, and
   `src/agent/compaction.rs`, plus `tests/tui_render_state.rs`.
   Cover a model mutation while conversation and explicit summarizer share
   engine A, activation of B leaving explicit summarizer A bound, and later
   mutation of A while it is used for summaries. Separately cover implicit
   summarization following activation A to B, including a root agent initially
   overriding `default_engine`. Verify new summary requests use B while an
   already-dispatched request on A completes under its original settings.
   Cover explicit versus catalog-derived windows and different thresholds;
   assert both effective settings and the resulting capacity warnings,
   including when implicit sharing eliminates a formerly separate-window
   warning. Verify independent timeout resolution in both modes.
   Keep existing cancellable model-change capacity checks and in-flight
   request snapshots intact. Strengthen existing subagent inheritance tests
   in `tests/cli_driver.rs` for exact named activation without redefining
   inheritance, and verify utility-engine isolation. Do not introduce live
   provider dependencies.
7. [x] Update `docs/features.md` (model provider, slash commands, compaction),
   `docs/configuration.md` (initial selection, engine settings, compaction
   fallback and sharing), `docs/implementation-notes.md` (identity routing
   and display snapshots), `docs/keybindings.md` (Enter target semantics),
   `docs/persistence.md` (explicitly document unchanged engine/effort
   persistence boundaries), `docs/cli-options.md` (catalog/routing
   description), and `README.md` (the `/models` summary).
   Describe the intentional change: omitted compaction-engine selection now
   follows the active conversation Engine runtime; users who want a fixed summarizer
   must explicitly configure its engine. Explain that explicit sharing still
   follows mutations of that named engine, and preserve timeout independence.
   Mark only the corresponding `docs/backlog-issues.md` item complete when
   its implementation is verified. Reconcile relevant `/models` statements
   in `docs/blueprint.md`; avoid unrelated documentation cleanup.
8. [x] Run the smallest combined targeted tests covering the changes while
   implementing. Before reporting completion run `make format lint`, then
   `make test-coverage` as the single mandatory full-suite gate, enforcing
   at least 80% line coverage. Do not also run `make test` as a duplicate
   mandatory gate. Check changed Markdown with repository-configured
   `rumdl`. Record implementation progress and any justified adjustments in
   this plan; do not stage or commit without separate user consent. The
   factory tests use a selectable mock provider because `MockProvider`
   intentionally rejects model changes. `make format lint` and
   `make test-coverage` passed; the coverage gate ran all suites with
   1,388 unit tests passing, all integration suites passing, and 92.17% total
   line coverage. `rumdl` formatting and checks passed for the changed Markdown
   except for four pre-existing MD013 warnings in an unrelated quoted log
   block in `docs/backlog-issues.md`.

## Implementation Hints

- `src/llm/engine.rs` defines typed `ModelSelection` variants for named-engine
  activation and raw provider/model routing. The router resolves raw catalog
  targets through its active engine for that connection or the first
  engine-name-sorted match for other connections. Selection previews use the
  same resolver and do not mutate Engine runtime state.
- Named selection validates provider/model identity and only changes reasoning
  when the picker records an explicit edit. It activates the existing Engine runtime
  handle; it does not restore profile snapshots. Catalog-derived model metadata
  remains distinct from a named engine's effective context-window override.
- `src/tui_reducer_state.rs::ModelChoice` and `Effect::SwitchModel` carry the
  selection identity through `src/presenter.rs` and `src/tui_driver.rs`.
  Shortcut active state and preselection use engine identity; raw catalog
  activity continues to use provider connection/model.
- `EngineImpl` and its provider share `EngineRuntimeConfigHandle`. Provider
  request settings are snapshotted before network I/O, so later model changes
  do not rewrite an already-dispatched request.
- `ProfileConfig::explicit_compaction_engine_name_for` preserves the
  distinction between an explicit engine key and an implicit fallback.
  Per-agent engine selection wins over profile selection; timeout resolution
  remains independent. A timeout-only override therefore does not make the
  summarizer explicit.
- `AgentFactory` binds explicit summaries to the configured engine and
  implicit summaries to that agent's conversation engine/router. Startup
  capacity checks use the same distinction for root and configured agent
  types. Child-agent inheritance still compares the child's configured engine
  name with the parent's original configured engine name.
- `src/agent/compaction.rs::CompactionRuntime` owns the selected summarizer
  handle. In-flight summaries retain their request snapshots and deadlines;
  completed results are applied at existing safe boundaries.
- `EngineDisplayInfo` and `CompactionEngineDisplayInfo` expose display-only
  conversation and summarizer identities, including whether the summarizer
  follows or shares the conversation engine. Child projections also indicate
  when they inherit the parent's Engine runtime.
- The `/models` detail area and switch notices are rendered by
  `src/tui_widgets.rs`; picker previews are generated by `src/tui_driver.rs`.
  The layout yields detail space on narrow terminals while keeping controls
  available.
- Successful switches still persist provider connection/model only and schedule
  the cancellable capacity check. Engine identity and reasoning effort are not
  persisted, and resume continues to resolve engines from current configuration.
- `docs/architecture-design-records/` contains only `.gitkeep`; no existing ADR
  governs this change. Project test and final-check conventions are defined in
  `docs/development-workflow.md`.
- The regression coverage is in `src/llm/engine.rs`, `src/agent_factory.rs`,
  `src/config/profile.rs`, `tests/cli_driver.rs`, and
  `tests/tui_render_state.rs`. The factory behavior tests use a selectable
  mock provider because `MockProvider` intentionally rejects model changes.
