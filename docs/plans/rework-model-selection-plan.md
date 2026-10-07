# Rework `/models` engine selection

## Context

Ylva is a Rust terminal and command-line agent harness. Its `/models` picker
offers both named-engine shortcuts and provider catalog rows. An engine is a
reusable runtime selection binding a model, provider connection, and request
settings. Two named engines can use the same provider connection and even the
same model while having different temperature, reasoning, context-window, or
compaction-threshold settings.

The picker currently retains the engine name for display but discards it on
confirmation. Routing then chooses a representative engine for the provider
connection, not necessarily the engine the user selected. This can mutate the
wrong engine, leave the requested sampling settings inactive, and unexpectedly
change a summarizer sharing that engine.

The goal is to preserve named-engine identity end to end and explain the
relationship between the conversation engine and the engine summarizing its
context. This plan addresses the "Clarify/fix `/models` engine selection and
its effect on compaction" backlog item. It is not authorization to implement,
stage, or commit source changes.

### Agreed behavior

- A named-engine shortcut activates that exact existing runtime engine.
  Only a reasoning effort changed by the user in the picker is reapplied and
  validated against the advertised alternatives.
- Shortcuts represent current runtime selections, not commands to restore
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
- The picker has a detail area showing the current conversation engine,
  summarizer engine/model, shared versus separate, and the highlighted
  selection's effect on summarization. Switch notices explain the resulting
  relationship. The normal footer stays compact.
- Named-engine settings remain owned by their existing runtime handles. No
  provider reconstruction, engine cloning, configuration writes, or new knobs
  are required.
- Existing provider/model persistence and resume behavior remain unchanged.
  Engine identity and reasoning effort are not newly persisted.

The user confirmed this contract through a grilling interview. All product
questions are resolved; implementation still requires separate authorization.

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
  existing runtime state. Unknown reasoning metadata does not make an unchanged
  effort invalid; existing lazy hydration and warnings remain available.
- Catalog context lengths follow runtime model changes only when the engine
  has no explicit context length. Input modalities and reasoning metadata
  remain consistent with the selected model.
- An explicit shared summarizer follows mutations of its own engine; an
  explicitly separate summarizer retains its model, settings, and window.
  Switching away leaves an explicit summarizer bound, whereas an implicit
  summarizer follows the newly activated conversation engine.
- The TUI identifies the conversation and summarizer selections and whether
  they share the same engine, without mistaking identical provider/model pairs
  for shared runtime identity. This information is available while idle and
  inspecting `/models` and after a successful switch, without sending a prompt.
  Highlighting previews the effect without mutating any runtime engine.
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

1. [ ] Establish the regression baseline before source implementation.
   Read `src/presenter.rs` at the model-picker Enter handler first, then
   `ModelChoice` and `Effect` in `src/tui_reducer_state.rs`, and
   `EngineRouter::switch_model_option` in `src/llm/engine.rs`.
   Run `cargo test --lib engine_router` as the initial routing baseline.
   Add a failing regression using two real routed test engines on one
   connection with distinguishable settings; confirm the shortcut's identity
   is lost through the actual presenter/effect/driver path rather than testing
   only a newly added router method.
2. [ ] Preserve the selection target in a typed runtime operation. Distinguish
   named-engine activation from raw provider/model selection; do not make
   the LLM runtime depend on TUI `ModelChoice` or display labels. Carry the
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
3. [ ] Correct picker identity and metadata in
   `src/tui_driver.rs::apply_model_list_result` and the related label/selection
   helpers in `src/tui_reducer_state.rs`. Use the actual engine key for named
   shortcut active status and preselection; initialize a shortcut's reasoning
   from that engine's runtime rather than borrowing the active sibling's
   value solely because their provider/model pairs match. Keep effective
   shortcut window display separate from genuine provider-catalog metadata
   so displaying an explicit override cannot become a catalog override.
   Keep shortcut activation usable when that model has no catalog metadata:
   preserve unknown metadata and unchanged effort rather than fabricating
   unsupported values or rejecting the shortcut. Preserve existing overall
   catalog-loading errors and empty-result behavior.
   Test duplicate provider/model pairs, engine-name filtering, reopening
   after a runtime model/effort change, unavailable catalog metadata, empty
   filters, no matches, reasoning cycling, and mouse highlight versus Enter
   confirmation. Preserve one catalog query per connection.
4. [ ] Implement explicit versus implicit summarizer binding in
   `src/config/profile.rs`, `src/agent_factory.rs`, and `src/agent.rs`.
   Retain selection provenance instead of inferring explicitness from the
   fallback name returned by `compaction_engine_name_for`.
   Per-agent explicit engine wins over profile explicit engine; timeout
   resolution stays independent. Bind explicit selection to its named engine,
   and implicit selection to that loop's conversation runtime/router so
   subsequent summaries follow named activation and raw model changes.
   Do not pass `None` to disable compaction for implicit selection.
   Update startup capacity-check construction consistently for root and
   configured subagent types. Test missing overrides, timeout-only overrides,
   both explicit precedence levels, explicit references to `default` and
   `default_engine`, inherited subagent routers, and independent subagent
   engines. Preserve current request snapshots and deadlines, without
   cancellation/restart on switch.
5. [ ] Expose truthful conversation/summarizer display metadata from
   `src/agent.rs` and factory wiring in `src/agent_factory.rs`. Keep explicit
   summarizer identity separately from the conversation router; implicit
   summarizer identity follows that router's active engine. Derive sharing
   from resolved runtime engine identity, not model equality.
   Carry display-only snapshots through `UiUpdate`, presenter state,
   `src/tui_render_state.rs`, and root/subagent projections as needed.
   Render current relationship and highlighted-selection consequences in a
   picker detail area in `src/tui_widgets.rs`; include actual engine identity
   and resulting sharing in switch notices. Keep the normal footer unchanged.
   Share target resolution with runtime routing instead of reproducing
   representative-engine ordering in the UI. Distinguish "mutates shared
   engine", "implicit summarizer follows", and "explicit summarizer stays".
   Use the existing update path; do not duplicate runtime configuration into
   TUI-owned state or add blocking metadata requests. Show unknown window
   metadata honestly. Test startup, idle switching, highlight-only preview,
   separate engines with identical model IDs, subagent drill-in, clear, and
   narrow-terminal behavior using `TestBackend`; controls remain usable when
   space cannot fit full details.
6. [ ] Verify compaction and shared-runtime behavior through focused tests in
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
7. [ ] Update `docs/features.md` (model provider, slash commands, compaction),
   `docs/configuration.md` (initial selection, engine settings, compaction
   fallback and sharing), `docs/implementation-notes.md` (identity routing
   and display snapshots), `docs/keybindings.md` (Enter target semantics),
   `docs/persistence.md` (explicitly document unchanged engine/effort
   persistence boundaries), `docs/cli-options.md` (catalog/routing
   description), and `README.md` (the `/models` summary).
   Describe the intentional change: omitted compaction-engine selection now
   follows the active conversation runtime; users who want a fixed summarizer
   must explicitly configure its engine. Explain that explicit sharing still
   follows mutations of that named engine, and preserve timeout independence.
   Mark only the corresponding `docs/backlog-issues.md` item complete when
   its implementation is verified. Reconcile relevant `/models` statements
   in `docs/blueprint.md`; avoid unrelated documentation cleanup.
8. [ ] Run the smallest combined targeted tests covering the changes while
   implementing. Before reporting completion run `make format lint`, then
   `make test-coverage` as the single mandatory full-suite gate, enforcing
   at least 80% line coverage. Do not also run `make test` as a duplicate
   mandatory gate. Check changed Markdown with repository-configured
   `rumdl`. Record implementation progress and any justified adjustments in
   this plan; do not stage or commit without separate user consent.

## Implementation Hints

- `src/presenter.rs` currently clones only `choice.selection` in the Enter
  handler, dropping `choice.engine_name`.
- `src/tui_reducer_state.rs::ModelChoice` already contains
  `engine_name: Option<String>`, whereas `Effect::SwitchModel` currently
  contains only `ProviderModelOption`.
- The grilling interview resolved shortcuts as exact current-runtime
  activation, not restoration of original profile values. Only user-edited
  effort is applied, including a change to provider default. Missing catalog
  metadata does not invalidate unchanged effort; offline shortcut discovery
  is outside the agreed scope.
- `ProviderModelOption` and `EngineDisplayInfo` live in `src/llm/engine.rs`.
  The latter currently carries the conversation's engine name, provider
  connection, model, and reasoning effort, but no summarizer identity.
- `EngineRouter::provider_engines` chooses the active engine for its connection
  and the first engine by sorted name for other connections.
  `switch_model_option` uses that representative; `named_engines` returns
  sorted current runtime configuration snapshots, not immutable profile values.
- `apply_model_list_result` marks named shortcuts active by provider/model
  equality and also borrows the active effort using that equality. This is
  incorrect when separate named engines share a provider/model pair.
- Shortcut rows currently embed their effective context length in `ModelInfo`;
  raw catalog rows carry provider-reported lengths. An explicit profile window
  and advertised catalog capacity are different sources of information.
- `Engine::switch_model_option` validates provider connection and advertised
  reasoning effort, updates reasoning/model/modalities, and adopts catalog
  context length. `EngineConfig::effective_context_length` gives an explicit
  window precedence over catalog metadata and the built-in fallback.
- `AgentFactory::build` initially resolves the root agent's configured engine
  and constructs a router when there is more than one engine. The one-engine
  path uses a direct engine; `AgentLoop::engine_display_info` supplies its
  configured name as a display fallback.
- The factory resolves the compaction engine once with
  `ProfileConfig::compaction_engine_name_for` and passes its existing
  `Arc<dyn Engine>` to the loop. Per-agent selection precedes profile
  selection; with neither set, the configured conversation engine is used.
  It is not the mutable conversation router.
- The confirmed exception is selection provenance: absent agent and profile
  compaction-engine fields means following that agent's active conversation
  runtime. Explicit engine fields remain bound regardless of the engine's
  spelling or equality with `default_engine`. Timeout-only overrides do not
  count as explicit engine selection.
- `src/config/profile.rs::CompactionConfig` stores the optional engine field;
  `compaction_engine_name_for` currently collapses explicit selection and
  fallback into one returned name. The user confirmed independent per-agent
  and profile engine/timeout precedence stays unchanged.
- `src/agent/compaction.rs::CompactionRuntime` owns its summarizer handle.
  A `None` handle disables compaction; it does not mean "use conversation".
  The user confirmed already-dispatched summary requests keep their original
  snapshot/deadline and apply under existing safe-boundary rules.
- The confirmed TUI surface is a picker detail area plus switch notices, not
  a persistent summarizer footer. Picker information includes the current
  relationship and highlighted-selection consequences without runtime mutation.
- `AgentFactory::build_child_agent_manager` compares a subagent's configured
  engine name with the parent's original configured engine name. A match
  inherits the parent's runtime engine/router. A mismatch uses the subagent's
  independently resolved selection. This is an existing contract, not a
  comparison with the parent's current active engine name.
- `EngineImpl` and its provider share `EngineRuntimeConfigHandle`.
  Providers snapshot request settings before network I/O; later switches do
  not rewrite an already-dispatched request. Compaction budgets read active
  conversation settings at safe boundaries.
- Successful switch persistence goes through
  `AgentCommand::RecordModelChanged`; its outer-loop handler records
  provider/model and schedules the cancellable compaction capacity check.
  TUI switch notices are not inserted as model-facing system messages.
- `TuiDriver` refreshes conversation display metadata with
  `agent.engine_display_info`; subagent displays are separately keyed by run ID.
  `tests/tui_render_state.rs` has footer and responsive model-switch warning
  tests, and `tests/cli_driver.rs` has post-switch subagent inheritance tests.
- `docs/persistence.md` records provider/model on every process start and
  successful switch. Resume resolves engines from current configuration;
  these records do not restore a named engine's runtime settings.
- The user confirmed provider/model-only records and current-configuration
  resume remain in scope unchanged, including an engine-only switch between
  identical provider/model pairs. No named-engine/effort persistence or session
  schema expansion is part of this plan.
- `docs/configuration.md` currently says both "first configured engine" and
  "`default_engine`" are initially active. The factory actually resolves the
  selected root agent type's engine override, falling back to `default_engine`.
  The conflicting initial-selection sentence is directly related stale
  documentation.
- `docs/architecture-design-records/` currently contains only `.gitkeep`;
  there is no existing ADR governing this change. Other plan files are in
  `docs/plans/`; this file uses the user's corrected explicit destination.
- `docs/development-workflow.md` owns implementation verification and test
  conventions. `docs/known-test-issues.md` documents tracing/cwd locking,
  asynchronous store-lock release, library `cfg(test)` limitations, and
  bounded agent UI-channel draining. Integration helpers live in
  `tests/common.rs`; integration-facing types must be exported by `src/lib.rs`.
