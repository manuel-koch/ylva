# Refactor large Rust modules by responsibility

## Context

Ylva is a Rust terminal and headless agent-harness. This plan reorganizes its
existing library and related tests into cohesive modules without
changing behavior. The motivation is navigation and maintainability, not a
smaller line count at any cost.

The investigation on 2026-10-08 inventoried all 73 Rust files under `src/`,
all 20 Rust files under `tests/`, the root `build.rs`, and inline module
declarations. Source outlines were screened across the entire inventory;
the candidates below received additional implementation and dependency
inspection. The existing graph was queried for relationships, with current
source used to verify the proposed boundaries.

Two different problems contribute to large files:

- Mixed production responsibilities: `agent.rs` has 5,464 lines before its
  main unit-test module; `tui_widgets.rs` has 4,434.
- Large inline test suites: `presenter.rs` has 2,451 production-region lines
  and 3,932 test-region lines; `config/profile.rs` has 982 and 2,137.
  Moving tests alone would not resolve production responsibility mixing.

### Scope and acceptance criteria

The selected scope is the nine highest-value parents: agent, widgets,
reducer state, presenter, TUI driver, store, sandbox, engine and read.
Steps 2-12 implement these splits, with related tests and documentation.
The remaining nine credible candidates are documented follow-ups, not
implementation checklist items. Extraction may introduce private semantic
methods and named handlers, but must preserve ownership and behavior.
This document is a plan, not permission to implement or commit it.

An implementation is done when:

- Every selected responsibility has one clear owner; original module paths
  remain compatibility facades where callers already use them.
- Existing public names, signatures, struct field shapes, trait behavior,
  serialization, CLI output, TUI buffers, and error semantics are preserved.
  No dependency, configuration, session-schema, or tool-protocol change is
  introduced merely to enable a split.
- Neither `AgentLoop::run` nor the large tool-bubble renderer is merely
  relocated unchanged and presented as a completed decomposition.
- Shared state retains its current owner and ordering rules. New child
  modules do not introduce additional tasks, channels, locks, or state copies.
- Production-region files above 1,000 lines receive a responsibility review.
  Cohesive exceptions are documented here; this is a review trigger, not a
  hard ceiling. Tests are counted separately.
- Existing test cases remain registered and meaningful, including expensive
  rendering fixtures and concurrency cases. New regression assertions cover
  any newly exposed boundary not already exercised.
- All project source, including moved renderers and the binary, participates
  in coverage without custom filename exclusions; cargo-llvm-cov's default
  filters remain in effect. This policy supersedes the original renderer
  exclusions with user approval. The aggregate line-coverage threshold
  remains at least 80%.
- Directly affected documentation and this checklist reflect the final
  layout. `make format lint` and `make test-coverage` pass.

### Constraints

The single-writer agent loop owns mutable `Context`. Background summaries
receive immutable snapshots and return outcomes for that loop to apply at
safe boundaries. The full transcript, model view, compaction coordinates,
projection revisions, tool-call/result pairing, generation checks,
cancellation, and serial queue admission remain unchanged.

The presenter remains a pure reducer, the TUI driver owns I/O, reducer state
does not depend on widgets, and `render` remains the adapter between reducer
and rendering state. This is not a redesign of those layers or of the
existing agent-to-replay-projection dependency.

Use `foo.rs` plus `foo/child.rs`, never `foo/mod.rs`. Prefer private child
modules and explicit re-exports; use `pub(super)` or `pub(in ...)` only for
an actual internal consumer. Do not make fields public merely to make a move
compile, add a generic `utils` module, or introduce a parameter bag to silence
Clippy. Preserve all existing `src/lib.rs` exports.

Keep operational resources with their owners: a store's locked file and
writer lifetime, a provider's shared Engine runtime handle and HTTP permit,
the driver's pending approval routing and terminal guards, and the render
cache's revision and selection storage.

## Implementation Plan

The implementing agent must flip `[ ]` to `[x]` immediately after completing
each step and rewrite this file in place. A step includes its targeted
checks and affected documentation; it is not complete before those updates.
Obtain explicit implementation consent before source edits. Execute one
subsystem at a time; do not combine all moves into an unverified repository-wide change.
No staging, branch creation, or commits are authorized by this plan.

1. [x] Establish the baseline and final extraction map. Open
   `src/agent/tools/read.rs` first, especially its streaming dispatch,
   window scanners, media recognition and inline tests.
   Read `docs/development-workflow.md` and `docs/known-test-issues.md`.
   Run `cargo test --lib agent::tools::read::tests` as the first targeted check, then
   enumerate existing cases with `cargo test -- --list` and retain the
   case-name inventory outside the tracked tree. Recheck the worktree, the
   module inventory, and `docs/architecture-design-records/` for changes
   since this scan. Confirm the selected scope in this file.

2. [x] Pilot the layout pattern by splitting read internals without
   generalizing every tool. Keep `ReadTool`, typed arguments, call adapter,
   results, metadata, public constants and schema contract in
   `src/agent/tools/read.rs`.
   Add `agent/tools/read/stream.rs` for incremental UTF-8 validation,
   bounded chunk reads and I/O errors; `agent/tools/read/windows.rs`
   for line/character scanners; and `agent/tools/read/media.rs` for
   media recognition, Base64 result construction and source filenames.
   Keep the dispatch into explicit/autodetected read modes in the parent
   unless it remains a demonstrably mixed-responsibility block.
   Preserve existing crate-internal media-constant paths needed by
   `web_fetch` and providers. Place scanner/stream tests beside their
   implementation and retain whole-tool tests at the parent.
   Run read library tests and
   `cargo test --test tools_binary_data --test tools_agent_loop`.
   Preserve EOF UTF-8 validation, chunk-boundary behavior, byte/line caps,
   complete-media exceptions and attachment fidelity.
   Update the relevant read mechanics paths in `docs/implementation-notes.md`.

3. [x] Split engines into `llm/engine/config.rs`,
   `llm/engine/selection.rs`, `llm/engine/router.rs` and
   `llm/engine/runtime.rs`. Retain the `Engine` trait and compatibility
   re-exports in `src/llm/engine.rs`; config owns immutable and runtime
   configuration types/deserializers; selection owns typed picker targets
   and display metadata; router owns `EngineRouter`; runtime owns
   `EngineImpl` and catalog hydration. Preserve existing aliases and
   constants, one shared mutable runtime handle per engine, and no lock
   held across network I/O. Selection preview and activation must continue
   to use the same target-resolution rules.
   Run engine/factory library tests and
   `cargo test --test http_provider --test cli_driver`.
   Update `docs/implementation-notes.md`.

4. [x] Split reducer state without changing `AppState`'s public field shape.
   Keep `AppState`, its constructor, and cross-cutting coordination in
   `src/tui_reducer_state.rs`. Add `tui_reducer_state/conversation.rs`
   for entries, tool state, turn buffers, subagent conversation types, replay
   projection, and conversation navigation methods;
   `tui_reducer_state/selection.rs` for selection/coordinate primitives;
   `tui_reducer_state/model_picker.rs` for choices, overlay state, filtering,
   labels, and picker methods; `tui_reducer_state/input_history.rs` for
   history navigation; `tui_reducer_state/layout.rs` for geometry types; and
   `tui_reducer_state/events.rs` for input/domain events and effects.
   Preserve `result_section_ids`' existing public path and all current
   type/function paths with explicit re-exports.
   Keep impl blocks in descendants when they need access to the parent's
   private state; do not introduce a second `AppState`.
   Run reducer-state library tests, presenter library tests, and
   `cargo test --test tui_render_state`. Update
   `docs/implementation-notes.md` with state ownership and replay paths.

5. [x] Split the pure presenter into `presenter/keyboard.rs`,
   `presenter/mouse.rs`, `presenter/domain.rs`, and
   `presenter/tool_bubbles.rs`. Retain `update`, `TuiEvent`, and
   `TextareaContext` as the public entry surface in `src/presenter.rs`.
   Keep subagent lifecycle handling in the domain owner and common tool
   transitions in the tool-bubble owner; do not duplicate transition logic
   between keyboard and domain paths. Put only genuinely shared presenter
   helpers in the parent. Split its tests by these same responsibilities
   with shared local fixtures. Run presenter library tests and
   `cargo test --test tui_render_state`. Preserve focus priority, double
   press semantics, pending approvals, partial-text flushes, and effect order.
   Update `docs/implementation-notes.md`.

6. [x] Make coverage accounting relocation-safe. Both `test-coverage` and
   `test-coverage-missing` in `Makefile` now omit custom filename exclusions,
   including the former renderer/binary exclusions. Keep cargo-llvm-cov's
   default filters and the unchanged aggregate threshold. Confirm renderer,
   binary and other production owners appear in the final report, and update
   `docs/development-workflow.md` and module coverage comments.
   Initially, steps 5-6 corrected filename boundaries and enumerated renderer
   descendants, verified against 24 examples. The user subsequently approved
   removing that brittle enumeration entirely after an unfiltered report
   demonstrated aggregate coverage above the existing gate.

7. [x] Split `src/tui_widgets.rs` by rendering responsibility. Retain
   `draw`, `draw_borrowed`, frame composition and `ButtonAreas` at the facade.
   Add `tui_widgets/chat.rs` for cached/legacy conversation drawing and
   scrolling, `tui_widgets/bubbles.rs` for shared message bubble chrome,
   `tui_widgets/tool_bubble.rs` for tool titles and bodies,
   `tui_widgets/footer.rs` for status/hints/approval controls,
   `tui_widgets/overlays.rs` for slash/model and clarify rendering, and
   `tui_widgets/help.rs` for help content and layout.
   Factor `append_tool_layout` into title/chrome and section/body helpers;
   both cached and legacy paths must call the same rendering implementation.
   Keep highlight/selection primitives with their rendering owner and use
   explicit internal exports. Repair the source-backed fixture in
   `tests/tui_render_state.rs` so it still includes a genuinely large source
   body and checks text actually present in its new location.
   Run `cargo test --test tui_render_state` and widget library tests.
   Compare full buffers, cache-build counters, painted-row bounds, and
   selection-storage reuse using the existing assertions. Update module
   ownership in `docs/implementation-notes.md`; keep implementation details
   out of the user-facing `docs/features.md`.

8. [x] Split TUI driver integrations into `tui_driver/effects.rs`,
   `tui_driver/models.rs`, `tui_driver/input.rs`,
   `tui_driver/terminal.rs`, and `tui_driver/updates.rs`.
   Retain `TuiApp`, `run_tui`, the main select loop, and frame orchestration
   in `src/tui_driver.rs`. Effects own command routing and
   `PendingRunIds`; models own catalog tasks, abort/restart, selection, and
   display refresh; updates own UI-to-domain mapping and lazy child replay;
   input owns textarea/mouse/history operations; terminal owns clipboard
   implementations, terminal/title guards, and SIGINT relay.
   Preserve the existing public `ClipboardSink` and related paths through
   the facade. Keep title-task shutdown and terminal teardown ordering.
   Run driver library tests and `cargo test --test tui_render_state`.
   Update `docs/implementation-notes.md`.

9. [x] Split persistence into `store/event.rs`, `store/session_file.rs`,
   `store/writer.rs`, and `store/replay.rs`. Keep `Store`, `StoreInner`,
   its public API, and session lifecycle coordination in `src/store.rs`.
   Event owns persisted types/constants; session-file owns lock/open/header
   operations and trailing-corruption repair; writer owns writer commands,
   locked-file lifetime, flush barriers, and title rewrite; replay owns
   read/query implementations and child-run reconstruction.
   Keep title snapshot ownership in the Store/writer lifecycle rather than
   copying it into replay. Keep Unix/non-Unix implementations together.
   Relocate tests by persistence responsibility and run store library tests
   plus `cargo test --test store_persistence_e2e` without live-provider flags.
   Preserve source bytes on clone, JSON field behavior, locked atomic title
   replacement, and queued-event durability. Update
   `docs/implementation-notes.md` and `docs/known-test-issues.md`.
   Check `docs/persistence.md` for path references without rewriting its
   unchanged external contract.

10. [x] Split sandbox policy into `sandbox/rules.rs`,
    `sandbox/path.rs`, `sandbox/command.rs`, and `sandbox/network.rs`.
    Retain `Sandbox` and `SandboxContext` ownership and public methods in
    `src/sandbox.rs`; descendant impl blocks can operate on their private
    parent-owned rule state. Rules own serialized and compiled rule types;
    path owns expansion, normalization, canonicalization and device checks;
    command owns command-glob/structural evaluation; network owns network
    approval evaluation. Keep the existing `bash_command_extractor.rs`
    and `network_target_pattern.rs` as their specialist modules.
    Avoid separate rule stores or duplicate normalization.
    Preserve all current public sandbox paths, including the helper used by
    `store::project_key`. Run sandbox library tests and
    `cargo test --test tools_agent_loop --test web_fetch_backend`.
    Preserve last-match-wins, configured-rule replacement, approval reasons,
    and runtime invalid-rule diagnostics. Update
    `docs/implementation-notes.md`; review unchanged configuration contracts
    against `docs/configuration.md`.

11. [x] Extract the agent's conversation and protocol model. Add
    `src/agent/message.rs` for `Message`, `ToolResult`, `ToolAttachment`,
    `ToolCallRequest`, assistant/user status types and their existing methods.
    Add `src/agent/protocol.rs` for commands, events, UI updates, modes,
    stream deltas and command diagnostics. Keep constants in their current
    public path or explicitly re-export them there. Add
    `src/agent/context.rs` for `Context`, token bookkeeping, hydration,
    model projection and recovery methods. Replace the few runtime writes
    into Context internals with narrowly scoped semantic methods; do not
    expose its message vectors or projection flags.
    Preserve `ylva::agent::*` and crate-root exports.
    Relocate context/protocol tests and run the agent library selector plus
    `cargo test --test tools_binary_data --test system_prompt_instructions`.
    Update the Context paths in `docs/implementation-notes.md`.

12. [x] Decompose agent execution while keeping one event-loop owner.
    Add `src/agent/runtime.rs` for the `AgentLoop::run` implementation and
    its owned run state; `src/agent/request.rs` for `start_request`,
    stream accumulation, preflight notices and context-rejection handling;
    `src/agent/tool_execution.rs` for `PendingTool::execute`; and
    `src/agent/tool_queue.rs` for admission, iteration checks, serial queue
    advancement and spawn admission. Keep the `AgentLoop` construction API
    and configuration methods reachable through `src/agent.rs`.
    Extract command, agent-event and subagent-completion handling into
    `agent/runtime/commands.rs`, `agent/runtime/events.rs` and
    `agent/runtime/subagents.rs`, keeping the select coordinator in
    `runtime.rs`. If shared locals become an owned run-state type, that type
    must actually own the transition lifecycle, not just package parameters.
    Preserve `biased` select ordering, guards, all `continue`/termination
    decisions, generation checks and resource cleanup. Handler outcomes must
    explicitly preserve whether to continue polling or evaluate the
    root/child completion tail; extracting a handler must not turn an outer
    `continue` into a normal fall-through.
    Run agent library tests and
    `cargo test --test tools_agent_loop --test steering --test interrupt --test concurrent_steers`.
    Keep bounded UI channels drained concurrently in loop tests. Update
    source-backed agent fixtures that currently read `src/agent.rs` with
    fixed limits so they still return enough real content to trigger the
    intended context-pressure path; assert that pressure, not just success.
    Update `docs/implementation-notes.md` and moved-test references in
    `docs/known-test-issues.md`.

13. [x] Finish test layout consistently across the selected splits.
    Use a parent `#[cfg(test)] mod tests;` and
    `<owner>/tests.rs` with responsibility-named descendants for large
    unit suites, or keep small tests directly beside their extracted owner.
    Keep cross-owner regression tests at the facade. Share local fixtures
    within the local test tree and cross-owner helpers via existing
    `src/test_support.rs`; preserve lock acquisition and paused-time helpers.
    Split `tests/tui_render_state.rs` into explicitly declared modules
    under `tests/tui_render_state/` for cache, bubbles, selection, overlays
    and subagent views, keeping the original integration-test target.
    Apply the same pattern to `tests/tools_agent_loop.rs` for queue/
    approvals, cancellation and subagents; retain its target and common
    helpers rather than creating automatically discovered test binaries.
    Keep other integration suites intact unless a concrete helper/test
    dependency from a selected move requires changes. Compare registered
    test cases with the baseline by normalized original function identity,
    accounting for intentional module-path changes; no case may disappear.
    Update test-layout references in `docs/development-workflow.md`,
    `docs/known-test-issues.md`, and any moved module-level comments.

14. [x] Complete the documentation reconciliation. Update
    `docs/implementation-notes.md` with the final module ownership map,
    single-writer constraints, replay paths and engine/driver interfaces;
    keep `docs/features.md` focused on user-visible behavior, not module paths
    or architecture internals; update `docs/development-workflow.md` with
    all-source coverage policy and
    unit/integration-test layout; and `docs/known-test-issues.md` with
    relocated examples. Inspect `docs/configuration.md`,
    `docs/keybindings.md`, `docs/persistence.md`, `README.md`, `CONTEXT.md`
    and `AGENTS.md` for references affected by the selected moves and
    update only those references. Do not rewrite unchanged user-facing
    contracts or historical idea documents as a general cleanup.
    Run `rumdl check` on the changed Markdown files. New documentation
    files other than this requested plan require separate permission.

15. [x] Verify the completed scope and record the resulting layout here.
    Review every facade and `src/lib.rs` export, relative fixture path,
    feature-gated item, test registration and coverage file set.
    Run default-feature `cargo clippy --all-targets --quiet` as well as
    the required `make format lint` and `make test-coverage`.
    Inspect coverage output for all new production files and confirm
    aggregate line coverage is at least 80%; do not weaken the threshold
    or add exclusions to make the refactor pass. Do not run `make test`
    as a duplicate mandatory gate. Record production/test-region counts
    for the selected parents and new children, plus responsibility
    exceptions justified by cohesion. Remove only temporary artifacts
    created for this work and leave unrelated worktree changes untouched.

## Proposed Improvements

The following candidates are deliberately outside the selected implementation
scope. Their boundaries are useful for separately approved follow-up work;
they do not block completing the nine selected parents.

| Candidate | Potential cohesive children | Important retained contract |
| --- | --- | --- |
| `config/profile.rs` | validation, interpolation, agent-type layering | Load order and path-rich errors |
| `agent_factory.rs` | engine construction, subagent completion | Role/tool validation and compaction precedence |
| `agent/compaction.rs` | pure policy, summary request construction | One coordinator and warning lock |
| `agent/tools.rs` | display types, registry, default composition | Tool traits and schema property order |
| `llm/openai_compatible_provider.rs` | redaction, SSE/catalog/usage parsing | Transport and request snapshot |
| `cli_driver.rs` | output formatting | Loop, commands and title shutdown |
| `http.rs` | dumps, retry policy | Response-lifetime permits and cancellation |
| `main.rs` | library-owned startup resolution | Initialization, exit codes and environment precedence |
| `markdown.rs` | table layout | Shared inline style and terminal-column wrapping |

## Implementation Hints

### Refinement decisions

- Execution started on 2026-10-08 with explicit source-edit consent. The
  worktree was clean, the inventory still contained 73 source and 20 test
  Rust files, and the ADR directory still contained only `.gitkeep`.
  The nine-parent scope and extraction map above remain unchanged.
  All 57 read baseline tests passed. The complete registered-case inventory
  is retained in the executing Copilot session's `files/` directory, outside
  the tracked tree. The earlier stale `tree-sitter` artifact issue in the
  shared Cargo target directory is resolved. No `CARGO_TARGET_DIR` override
  is required; Cargo and Make commands use their normal target-directory
  configuration.
- The read pilot now has private `stream`, `windows` and `media` children,
  with whole-tool tests in `read/tests.rs`. Its existing 57 cases remain
  registered; five new stream/scanner boundary cases exercise the extracted
  owners. Read library tests, `tools_binary_data`, `tools_agent_loop` and
  default-feature all-target Clippy passed. Read ownership and unchanged
  streaming/media constraints are recorded in `docs/implementation-notes.md`.
- Engines now have private `config`, `selection`, `router` and `runtime`
  owners, with explicit re-exports at the unchanged `llm::engine` facade.
  All 38 original engine cases remain; a display-snapshot regression was
  added, and preview/activation target identity assertions were strengthened.
  Engine/factory library tests, `http_provider`, `cli_driver` and
  default-feature all-target Clippy passed. Ownership and local test-suite
  layout are documented in `docs/implementation-notes.md` and
  `docs/development-workflow.md`.
- Reducer state now retains one `AppState` at its original facade, with private
  conversation, selection, model-picker, input-history, layout and event owners
  and explicit compatibility re-exports. Conversation replay, navigation and
  bubble mutation have focused descendants to keep each production file below
  the review trigger. All 69 original reducer cases remain, with three new
  history/terminal-column boundary assertions. Reducer, presenter and
  `tui_render_state` targeted checks and default-feature all-target Clippy passed.
  State ownership and replay paths are updated in `docs/implementation-notes.md`.
- Presenter routing now has private keyboard, mouse, domain and tool-bubble
  owners behind the unchanged `update`/event/textarea facade. Subagent lifecycle
  handling stays in domain, common bubble transitions stay in the bubble owner,
  and each production file is below the review trigger. All 96 original cases
  remain, with two new cross-mode/effect-order boundary assertions. Presenter,
  `tui_render_state` and default-feature all-target Clippy checks passed.
  Ownership and local test layout are updated in `docs/implementation-notes.md`.
  Coverage verification found the old exclusion regex also matched `domain.rs`.
  At that checkpoint, both coverage targets matched exact parent filenames at
  directory boundaries; renderer-descendant handling remained pending in step 6.
- Widgets now retain frame composition, borrowed inputs and approval geometry
  at `tui_widgets.rs`, with private chat, bubble, tool-bubble, footer, overlay
  and help owners. Cached conversation layout lives in `chat/cache.rs`; tool
  layout coordinates shared title/chrome and section/body helpers rather than
  relocating the old monolith. Pure selection, diff and path-label children
  stay covered. All 25 original widget cases remain, with three new boundary
  regressions. Widget library tests, all 68 `tui_render_state` cases and
  default-feature all-target Clippy pass. The source-backed fixture now reads
  `chat/cache.rs`, asserts more than 500 source and selection rows, and checks
  a real function in that implementation. Updated ownership, test layout and
  coverage are documented in the affected contributor docs.
- The user approved removing all custom coverage exclusions on 2026-10-08.
  This supersedes the renderer-exclusion constraint and the explicit matcher
  introduced in step 6. Both coverage targets now rely only on cargo-llvm-cov's
  default filters, so new source files require no exclusion-list maintenance.
  Renderer and binary coverage are included without lowering the 80% gate.
- The user confirmed this refined plan on 2026-10-08. Confirmation applies
  to the saved plan only; source implementation and Git operations are not
  authorized.
- The user selected nine highest-value parent modules; the other nine
  production split candidates remain follow-ups only.
- Private semantic methods and named event handlers are permitted.
  State ownership, observable behavior and concurrency architecture remain
  unchanged; strictly mechanical relocation is not the selected approach.
- A 1,000-line production-region size is a responsibility-review trigger,
  not a hard maximum. Documented cohesive exceptions are acceptable and
  unit tests are counted separately.
- Related unit suites and the two large integration suites are included
  in the selected refactor. `tui_render_state` and `tools_agent_loop` retain
  their original Cargo test-target names.
- Execution proceeds through read and engine pilots, the TUI layers,
  persistence/sandbox, then agent execution. Each subsystem receives
  targeted verification before the next starts.

### Execution checkpoint: read and engine pilots

At this earlier checkpoint, steps 1-3 were complete and steps 4-15 remained
pending. The first execution batch
stops at this verified subsystem boundary, before reducer-state extraction.
No staging, branch creation or commits were performed.

The normalized registered-case comparison retained all 1,618 baseline cases
and found six added cases (1,624 total). `make format lint`, default-feature
all-target Clippy, the targeted checks named above and `make test-coverage`
passed. Aggregate line coverage was **92.29%**, above the unchanged 80% gate.
The report included all seven extracted production children; no coverage
exclusion was changed. Changed Markdown files passed `rumdl check`.

Physical-line counts follow the inventory convention below. The two facade
test regions contain only their external test-module declarations. None of
these production regions exceeds the responsibility-review trigger, so no
cohesive size exception is needed for either pilot.

| Module | Production | Tests |
| --- | ---: | ---: |
| `agent/tools/read.rs` | 875 | 2 |
| `agent/tools/read/media.rs` | 89 | 0 |
| `agent/tools/read/stream.rs` | 197 | 114 |
| `agent/tools/read/windows.rs` | 283 | 86 |
| `agent/tools/read/tests.rs` | 0 | 1667 |
| `llm/engine.rs` | 331 | 2 |
| `llm/engine/config.rs` | 240 | 0 |
| `llm/engine/router.rs` | 431 | 0 |
| `llm/engine/runtime.rs` | 349 | 0 |
| `llm/engine/selection.rs` | 193 | 0 |
| `llm/engine/tests.rs` | 0 | 400 |
| `llm/engine/tests/config.rs` | 0 | 174 |
| `llm/engine/tests/router.rs` | 0 | 391 |
| `llm/engine/tests/runtime.rs` | 0 | 306 |
| `llm/engine/tests/selection.rs` | 0 | 85 |

### Execution checkpoint: reducer state and presenter

Steps 1-5 are complete; steps 6-15 remain pending, apart from the coverage
matcher boundary correction brought forward from step 6. This continuation
completed the user-selected reducer/presenter scope. No staging, branch
creation or commits were performed.

All 1,625 cases registered at the start of this continuation remain registered;
five boundary regressions bring the total to 1,630. `AppState` field declarations
and all original reducer/presenter production function identities are preserved.
The reducer suite passed 72 cases, the presenter suite passed 98, and the
unchanged `tui_render_state` integration target passed 68.

`make format lint`, default-feature all-target Clippy and `make test-coverage`
passed. Final aggregate line coverage was **91.78%**, above the unchanged 80%
gate. The report includes all thirteen extracted production children, including
`presenter/domain.rs`; both coverage matchers passed ten exact-filename boundary
examples. The optional rustdoc build succeeded with existing unrelated
broken/private-link warnings; no unresolved links were introduced by these
splits. Changed Markdown files passed `rumdl check`.

Physical-line counts use the convention below. Each production owner is below
the responsibility-review trigger; no cohesive size exception is needed.

| Module | Production | Tests |
| --- | ---: | ---: |
| `tui_reducer_state.rs` | 684 | 2 |
| `tui_reducer_state/conversation.rs` | 741 | 0 |
| `tui_reducer_state/conversation/navigation.rs` | 292 | 0 |
| `tui_reducer_state/conversation/replay.rs` | 378 | 0 |
| `tui_reducer_state/conversation/tools.rs` | 344 | 0 |
| `tui_reducer_state/events.rs` | 407 | 0 |
| `tui_reducer_state/input_history.rs` | 79 | 0 |
| `tui_reducer_state/layout.rs` | 165 | 0 |
| `tui_reducer_state/model_picker.rs` | 520 | 0 |
| `tui_reducer_state/selection.rs` | 219 | 0 |
| `tui_reducer_state/tests.rs` | 0 | 272 |
| `tui_reducer_state/tests/conversation.rs` | 0 | 1142 |
| `tui_reducer_state/tests/input_history.rs` | 0 | 45 |
| `tui_reducer_state/tests/layout.rs` | 0 | 113 |
| `tui_reducer_state/tests/model_picker.rs` | 0 | 441 |
| `tui_reducer_state/tests/selection.rs` | 0 | 28 |
| `presenter.rs` | 67 | 2 |
| `presenter/domain.rs` | 892 | 0 |
| `presenter/keyboard.rs` | 592 | 0 |
| `presenter/mouse.rs` | 623 | 0 |
| `presenter/tool_bubbles.rs` | 358 | 0 |
| `presenter/tests.rs` | 0 | 998 |
| `presenter/tests/domain.rs` | 0 | 854 |
| `presenter/tests/keyboard.rs` | 0 | 738 |
| `presenter/tests/mouse.rs` | 0 | 1090 |
| `presenter/tests/tool_bubbles.rs` | 0 | 450 |

### Execution checkpoint: coverage accounting and widgets

Steps 1-7 are complete; steps 8-15 remain pending. The user selected steps 6-7
for this continuation. No staging, branch creation or commits were performed.

The widget facade retains its public draw functions, borrowed-input shapes
and `ButtonAreas`; `src/lib.rs` exports are unchanged. All original production
function identities remain. The old tool-layout monolith is now a small
coordinator over shared chrome and body helpers, with one section-merging rule
used by both. Cache revisions, scroll geometry, selection storage and state
ownership are unchanged.

All 1,630 cases registered at the start remain registered; three new tool-layout
and terminal-column selection regressions bring the total to 1,633.
The widget suite passed 28 cases and the unchanged `tui_render_state` integration
target passed 68, including full-buffer comparisons, warm build counters,
painted-row bounds and source-backed selection-storage reuse.

`make format lint`, default-feature all-target Clippy and `make test-coverage`
passed. Aggregate line coverage was **91.81%**, above the unchanged 80% gate.
The final report includes `chat/selection.rs`, `help/paths.rs` and
`tool_bubble/diff.rs`, while omitting the explicitly named renderer-only
children. Presenter/reducer logic, `markdown.rs` and
`conversation_render_cache.rs` remain included. Both coverage targets share
the same matcher; all 24 boundary examples and every actual production child
match the intended treatment. Changed Markdown files passed `rumdl check`.

Physical-line counts follow the inventory convention below. Every widget
production owner is below the responsibility-review trigger, so no cohesive
size exception is needed.

| Module | Production | Tests |
| --- | ---: | ---: |
| `tui_widgets.rs` | 639 | 0 |
| `tui_widgets/bubbles.rs` | 323 | 2 |
| `tui_widgets/bubbles/tests.rs` | 0 | 52 |
| `tui_widgets/chat.rs` | 450 | 2 |
| `tui_widgets/chat/cache.rs` | 616 | 0 |
| `tui_widgets/chat/selection.rs` | 123 | 84 |
| `tui_widgets/chat/tests.rs` | 0 | 57 |
| `tui_widgets/footer.rs` | 589 | 2 |
| `tui_widgets/footer/tests.rs` | 0 | 22 |
| `tui_widgets/help.rs` | 325 | 2 |
| `tui_widgets/help/paths.rs` | 30 | 33 |
| `tui_widgets/help/tests.rs` | 0 | 32 |
| `tui_widgets/overlays.rs` | 552 | 0 |
| `tui_widgets/tool_bubble.rs` | 137 | 2 |
| `tui_widgets/tool_bubble/body.rs` | 462 | 0 |
| `tui_widgets/tool_bubble/diff.rs` | 38 | 41 |
| `tui_widgets/tool_bubble/title.rs` | 560 | 0 |
| `tui_widgets/tool_bubble/tests.rs` | 0 | 508 |

### Coverage policy follow-up: no custom exclusions

After the widget checkpoint, the user approved removing the filename matcher
from both coverage targets. Coverage now includes `main.rs`, `render.rs`,
`tui_widgets.rs` and every production widget child; cargo-llvm-cov's default
filters still apply. The earlier checkpoint percentages describe their
historical file sets, not the current policy.

`make format lint` and a fresh `make test-coverage` passed under this policy.
Aggregate line coverage was **90.91%**, above the unchanged 80% gate.
The report confirms the formerly excluded files are included. Both targets
omit custom exclusion flags, and `test-coverage-missing` retains
`--show-missing-lines`. Workflow documentation, implementation notes and
renderer module comments now reflect the policy; changed Markdown passed
`rumdl check`. Plan steps 8-15 remain pending.

### Execution checkpoint: TUI driver

Steps 1-8 are complete; steps 9-15 remain pending. This continuation reaches
the driver subsystem boundary, before persistence extraction.
No staging, branch creation or commits were performed.

The driver facade retains `TuiApp`, `run_tui`, `ActionOutcome`, the select
coordinator, frame orchestration and session-title task coordination.
Private effects, models, input, terminal and update owners separate command
routing, catalog/selection/display integration, textarea operations, OS
resources and UI/replay mapping. Clipboard types retain their public facade
paths and `src/lib.rs` exports are unchanged. Pending approval destinations
and mouse-drag fields remain private to their owners; no additional state,
tasks, channels or locks were introduced.
All 34 original standalone production functions remain; 29 retain identical
bodies after visibility and formatting normalization. The five changed
functions delegate to their new responsibility owners, preserving loop
control and effect order.

All 1,633 cases registered at the start remain registered after normalizing
driver test-module paths; three new boundary regressions bring the total to
1,636. The driver suite passed 70 cases and the unchanged `tui_render_state`
integration target passed 68. New assertions cover child/root clarify
destinations after reducer transitions, ordered textarea/command effects and
quit short-circuiting, and disabled catalog polling after cancellation.
The existing scrolled-input mouse fixture now exercises the extracted
wheel handler and checks the actual rendered viewport and selection.
Default-feature all-target Clippy passed without warnings.

`make format lint`, default-feature all-target Clippy and `make test-coverage`
passed. Aggregate line coverage was **90.52%**, above the unchanged 80% gate.
The report includes the driver facade and all five new production children,
alongside the renderer and binary; no exclusion or threshold was changed.
Changed Markdown files passed `rumdl check`.

Physical-line counts follow the inventory convention below. Every driver
production owner is below the responsibility-review trigger; no cohesive
size exception is needed.

| Module | Production | Tests |
| --- | ---: | ---: |
| `tui_driver.rs` | 704 | 2 |
| `tui_driver/effects.rs` | 249 | 0 |
| `tui_driver/input.rs` | 291 | 0 |
| `tui_driver/models.rs` | 444 | 0 |
| `tui_driver/terminal.rs` | 167 | 0 |
| `tui_driver/updates.rs` | 407 | 0 |
| `tui_driver/tests.rs` | 0 | 600 |
| `tui_driver/tests/effects.rs` | 0 | 1006 |
| `tui_driver/tests/input.rs` | 0 | 784 |
| `tui_driver/tests/models.rs` | 0 | 853 |
| `tui_driver/tests/terminal.rs` | 0 | 32 |
| `tui_driver/tests/updates.rs` | 0 | 452 |

### Execution checkpoint: persistence and sandbox

Steps 1-10 are complete; steps 11-15 remain pending. The user selected
steps 9-10 for this continuation. No staging, branch creation or commits
were performed. An unrelated `Makefile` change adding a graphify target
appeared during execution and was left untouched.

Persistence retains `Store`, `StoreInner`, session lifecycle coordination
and the shared title snapshot at the facade. Private event, session-file,
writer and replay owners separate wire types, locked file operations,
writer lifetime/title replacement and read-only reconstruction. Existing
public names, serialization, source-preserving cloning and title ownership
are unchanged. Shared fixtures and cross-owner assertions live in
`store/tests.rs`, with responsibility suites under `store/tests/`.

Sandbox retains one shared workspace handle and the existing ordered rule
storage at its facade. Private rule, path, command and network owners reuse
that state, with the existing command extractor and target-pattern parser
unchanged. Public rule/path-helper names retain their facade paths, and no
fields were exposed for relocation. Owner-local tests keep private helpers
private and share the fixture from `sandbox/tests.rs`.

All 58 original store and 45 original sandbox production functions retain
their signatures and bodies after visibility/formatting normalization.
Public type, constant and standalone-function inventories are preserved;
`src/lib.rs` is unchanged. All 1,636 cases registered at the start remain
registered after normalizing moved test paths. Four new cross-owner
regressions bring the total to 1,640: shared title rewrite/append lock
lifetime, queued-event draining on last-handle drop, shared workspace updates,
and configured/appended rule precedence across evaluators.

The store suite passed 41 cases; the sandbox selector passed 118, including
the unchanged specialist suites. `store_persistence_e2e` passed eight offline
cases with its three live-provider cases still ignored; `tools_agent_loop`
passed 20 and `web_fetch_backend` passed 12.
`make format lint`, default-feature all-target Clippy and `make test-coverage`
passed. Aggregate line coverage was **90.10%**, above the unchanged 80% gate.
The report includes the two facades and all eight new production children,
alongside the renderer and binary; no exclusion or threshold changed.
Changed Markdown files passed `rumdl check`. The optional rustdoc build
succeeded with existing broken/private-link warnings; the refactor introduced
no unresolved links. Unchanged persistence/configuration contracts and
recorded decisions were checked; the ADR directory still contains only
`.gitkeep`.

Physical-line counts follow the inventory convention below. Every selected
production owner is below the responsibility-review trigger, so no cohesive
size exception is needed.

| Module | Production | Tests |
| --- | ---: | ---: |
| `store.rs` | 896 | 2 |
| `store/event.rs` | 225 | 0 |
| `store/replay.rs` | 391 | 0 |
| `store/session_file.rs` | 201 | 0 |
| `store/writer.rs` | 230 | 0 |
| `store/tests.rs` | 0 | 655 |
| `store/tests/event.rs` | 0 | 134 |
| `store/tests/replay.rs` | 0 | 508 |
| `store/tests/session_file.rs` | 0 | 152 |
| `store/tests/writer.rs` | 0 | 382 |
| `sandbox.rs` | 176 | 2 |
| `sandbox/command.rs` | 233 | 2 |
| `sandbox/network.rs` | 113 | 2 |
| `sandbox/path.rs` | 530 | 2 |
| `sandbox/rules.rs` | 519 | 2 |
| `sandbox/tests.rs` | 0 | 157 |
| `sandbox/command/tests.rs` | 0 | 310 |
| `sandbox/network/tests.rs` | 0 | 412 |
| `sandbox/path/tests.rs` | 0 | 854 |
| `sandbox/rules/tests.rs` | 0 | 125 |

### Execution checkpoint: agent execution and final layout

Steps 1-15 are complete, including the final mandatory verification gates.
No staging, branch creation or commits were performed.
Concurrent changes to `Makefile`, `docs/backlog-issues.md` and the custom
live-provider profile were left untouched.

The agent facade retains construction/configuration APIs, constants and explicit
compatibility re-exports. Private message and protocol owners preserve persisted
field shapes, serialization and all public paths. Context owns transcript,
token accounting and projection flags; projection and recovery descendants use
its private state, while runtime and compaction callers use semantic methods.
Summary requests receive an immutable source snapshot.

One owned run state now coordinates request, queue and handler transitions.
The biased select order and guards are unchanged. Command, agent-event and
subagent handlers return explicit poll-again, completion-tail or finished
outcomes, preserving outer-loop continuation and early-return behavior.
Receivers remain in the coordinator and no new task, channel, lock or Context
copy was introduced. Batch admission and serial advancement share the queue
owner; request preflight/stream assembly and per-tool execution remain separate.

All 20 original public type/field shapes and 57 public function signatures,
constants and statics match the original agent facade; `src/lib.rs` exports
are unchanged. All 1,640 cases registered at this continuation's start remain
registered after normalizing module paths. Four new boundary regressions bring
the total to 1,644: canonical-versus-projected harness notices, Context-owned
retry/batch bookkeeping, ignored handler inputs bypassing child completion,
and distinct normal/terminal handler outcomes.

Large unit suites now follow their owners; shared fixtures and cross-owner
assertions stay at their local parent. Both large integration targets retain
their Cargo names and explicitly declare responsibility descendants.
The large agent read fixture uses the unchanged compaction implementation and
asserts its canonical result exceeds the output allowance, rather than accepting
a successful read that never created pressure. Relocation exposed a pre-existing
fixed-delay TUI shutdown race on a cold tokenizer; the assertion now waits for
a completed rendered frame before closing the action channel.

Affected contributor docs describe the final owners, single-writer constraints,
test layout and all-source coverage policy. Configuration, keybindings,
persistence, README, domain language and agent guidelines need no path updates;
their external contracts are unchanged. User-facing features retain behavior
descriptions rather than internal module maps. The ADR directory still contains
only `.gitkeep`, so there is no recorded decision to migrate or override.
The optional rustdoc build succeeds with the existing unresolved/private-link
warnings, without new broken links from these moves.

The agent selector passes 515 cases with one existing ignored case; the two
relocated integration targets pass 68 rendering and 20 tool-loop cases.
Steering, interrupt, concurrent-steer, binary-data and system-instruction
selectors also pass. `make format lint`, default-feature all-target Clippy
and the mandatory `make test-coverage` gate pass; aggregate line coverage is
**89.71%**, above the unchanged 80% threshold. All selected production owners,
the renderer descendants and the binary appear in the report, without custom
exclusions. Changed Markdown passes `rumdl check`; the unrelated backlog file
still reports its existing long-line warning during repository formatting.

Physical counts use the inventory convention below. All selected production
files are below the 1,000-line review trigger; no cohesive size exception is
needed. Unchanged descendants retain the detailed counts in their earlier
checkpoints; the largest production descendant and final facade are summarized
here.

| Selected facade | Production | Tests | Largest production owner |
| --- | ---: | ---: | ---: |
| `agent.rs` | 530 | 2 | 688 |
| `agent/tools/read.rs` | 875 | 2 | 875 |
| `llm/engine.rs` | 331 | 2 | 431 |
| `tui_reducer_state.rs` | 684 | 2 | 741 |
| `presenter.rs` | 67 | 2 | 892 |
| `tui_widgets.rs` | 639 | 0 | 639 |
| `tui_driver.rs` | 704 | 2 | 704 |
| `store.rs` | 896 | 2 | 896 |
| `sandbox.rs` | 176 | 2 | 530 |

| New agent owner | Production | Tests |
| --- | ---: | ---: |
| `agent/message.rs` | 153 | 2 |
| `agent/protocol.rs` | 479 | 2 |
| `agent/context.rs` | 688 | 2 |
| `agent/context/projection.rs` | 300 | 0 |
| `agent/context/recovery.rs` | 368 | 0 |
| `agent/runtime.rs` | 468 | 2 |
| `agent/runtime/commands.rs` | 560 | 0 |
| `agent/runtime/events.rs` | 357 | 0 |
| `agent/runtime/subagents.rs` | 318 | 0 |
| `agent/runtime/compaction_updates.rs` | 186 | 0 |
| `agent/request.rs` | 399 | 0 |
| `agent/tool_execution.rs` | 177 | 0 |
| `agent/tool_queue.rs` | 622 | 0 |
| `agent/tool_queue/spawn.rs` | 138 | 0 |
| `agent/tests.rs` | 0 | 858 |
| `agent/tests/request.rs` | 0 | 365 |
| `agent/tests/compaction_updates.rs` | 0 | 676 |
| `agent/tests/runtime.rs` | 0 | 232 |
| `agent/tests/subagents.rs` | 0 | 591 |
| `agent/tests/tool_execution.rs` | 0 | 134 |
| `agent/tests/tool_queue.rs` | 0 | 795 |
| `agent/context/tests.rs` | 0 | 301 |
| `agent/context/tests/projection.rs` | 0 | 337 |
| `agent/context/tests/recovery.rs` | 0 | 472 |
| `agent/context/tests/tokens.rs` | 0 | 304 |
| `agent/message/tests.rs` | 0 | 196 |
| `agent/protocol/tests.rs` | 0 | 199 |
| `agent/runtime/tests.rs` | 0 | 148 |

| Integration-test owner | Test/support lines |
| --- | ---: |
| `tests/tui_render_state.rs` | 447 |
| `tests/tui_render_state/cache.rs` | 419 |
| `tests/tui_render_state/bubbles.rs` | 2,050 |
| `tests/tui_render_state/selection.rs` | 416 |
| `tests/tui_render_state/overlays.rs` | 1,563 |
| `tests/tui_render_state/subagent_views.rs` | 814 |
| `tests/tools_agent_loop.rs` | 532 |
| `tests/tools_agent_loop/queue_approvals.rs` | 744 |
| `tests/tools_agent_loop/cancellation.rs` | 200 |
| `tests/tools_agent_loop/subagents.rs` | 595 |

### Inventory interpretation

The counts below are physical lines, including comments and blank lines.
`Production` ends immediately before the main `#[cfg(test)] mod tests`
block; isolated test-only helpers before that block are included in that
region. `Tests` is the remaining inline test block, not an independently
measured executable-code count. Line count is a screening signal, not proof
of poor cohesion.

`Split` identifies a credible boundary, not automatic implementation scope.
The selected parents and follow-ups are distinguished in Context and
Proposed Improvements. `Keep` means
the current module has a cohesive purpose or is already a narrow facade.
`Keep; tests` means file length is dominated by tests and does not justify
a production split; relocating tests with an affected owner is sufficient.

### Complete source-file assessment

All paths in this table are relative to `src/`.

| Module | Production | Tests | Assessment |
| --- | ---: | ---: | --- |
| `agent.rs` | 5464 | 5204 | Split: protocol, messages, Context, runtime, request and tool queue |
| `presenter.rs` | 2451 | 3932 | Split: keyboard, mouse, domain and tool transitions |
| `tui_reducer_state.rs` | 3620 | 1906 | Split: state concepts and impl blocks; preserve one AppState |
| `tui_driver.rs` | 2074 | 3446 | Split: effects, model picker, input, terminal and update adapter |
| `tui_widgets.rs` | 4434 | 593 | Split: conversation, bubbles, tool body, footer and overlays |
| `store.rs` | 1831 | 1664 | Split: event model, file lifecycle, writer and replay |
| `sandbox.rs` | 1475 | 1656 | Split: rules, paths, commands and network policy |
| `config/profile.rs` | 982 | 2137 | Split: validation, interpolation and agent-type layering |
| `agent/tools/read.rs` | 1364 | 1678 | Split: streaming, windows and media internals |
| `agent_factory.rs` | 1004 | 1802 | Split: engine construction and subagent lifecycle |
| `llm/engine.rs` | 1444 | 1249 | Split: configuration, selection, router and runtime |
| `agent/compaction.rs` | 1140 | 1211 | Split: pure policy and summary requests; retain coordinator |
| `agent/tools.rs` | 951 | 1051 | Split: registry, composition and display contract |
| `llm/openai_compatible_provider.rs` | 888 | 859 | Split: parsing and redaction; retain transport |
| `render.rs` | 527 | 1142 | Keep; tests: cohesive projection/draw synchronization adapter |
| `cli_driver.rs` | 1168 | 427 | Split: formatting; keep CLI loop and title orchestration |
| `http.rs` | 838 | 658 | Split: request diagnostics and retry policy |
| `agent/tools/glob.rs` | 665 | 772 | Keep; tests: one glob capability |
| `agent/tools/search.rs` | 762 | 543 | Keep: one search pipeline; size alone is insufficient |
| `agent/tools/edit.rs` | 621 | 591 | Keep: one edit capability and its result/diff model |
| `agent/tools/bash.rs` | 612 | 586 | Keep: subprocess lifetime and output collection belong together |
| `agent/child_agent_manager.rs` | 513 | 651 | Keep: scheduling and structured completion model |
| `markdown.rs` | 814 | 280 | Split narrowly: table layout subsystem |
| `agent/tools/web_fetch.rs` | 561 | 530 | Keep: backend adapter, result projection and metadata |
| `main.rs` | 800 | 282 | Split: testable startup resolution into library |
| `agent/tools/web_search.rs` | 451 | 578 | Keep; tests: backend call, throttle and result formatting |
| `config/instructions.rs` | 408 | 497 | Keep; tests: trusted-instruction discovery and composition |
| `agent/tools/write.rs` | 425 | 412 | Keep: one write capability |
| `config/web_search.rs` | 317 | 453 | Keep; tests: search backend configuration |
| `agent/tools/backends/web_search/brave.rs` | 322 | 439 | Keep; tests: provider adapter and response decoding |
| `llm/provider_registry.rs` | 471 | 252 | Keep: provider construction and environment discovery |
| `llm/openai_compatible_messages.rs` | 316 | 368 | Keep: existing wire-message serializer |
| `sandbox/bash_command_extractor.rs` | 319 | 335 | Keep: existing syntax extraction boundary |
| `config/agent_type_file.rs` | 331 | 294 | Keep: agent-type file formats and discovery |
| `cli.rs` | 272 | 348 | Keep: argument parsing and narrow flag interpretation |
| `agent/tools/backends/web_fetch.rs` | 360 | 257 | Keep: fetch backend contract and content conversion |
| `agent/tools/backends/web_search/searxng.rs` | 279 | 337 | Keep; tests: one search backend |
| `agent/tools/backends/web_search/duckduckgo.rs` | 277 | 332 | Keep; tests: one search backend |
| `llm/ollama_provider.rs` | 360 | 236 | Keep: one provider's request/catalog specialization |
| `agent/compaction/recovery.rs` | 384 | 188 | Keep: already separated recovery reducers |
| `logging.rs` | 263 | 296 | Keep: subscriber and text format lifecycle |
| `sandbox/network_target_pattern.rs` | 327 | 202 | Keep: existing network-pattern parsing/matching boundary |
| `session_title.rs` | 309 | 205 | Keep: shared title attempt/generation/normalization |
| `config/web_fetch.rs` | 94 | 69 | Keep: narrow fetch configuration |
| `conversation_render_cache.rs` | 255 | 227 | Keep: existing cache/revision/layout boundary |
| `agent/tools/backends/web_search.rs` | 268 | 196 | Keep: search backend contract and construction |
| `agent/tools/spawn_subagent.rs` | 266 | 197 | Keep: tool admission schema/display adapter |
| `llm.rs` | 329 | 75 | Keep: provider contract, shared limiter and re-export facade |
| `config.rs` | 332 | 57 | Keep: configuration errors, environment loading and facade |
| `agent/tools/agent_finish.rs` | 238 | 143 | Keep: structured subagent completion capability |
| `tui_render_state.rs` | 363 | 0 | Keep: cohesive render-only state projection |
| `config/layered.rs` | 157 | 156 | Keep: configuration resource precedence |
| `highlight.rs` | 244 | 58 | Keep: theme/highlight cache and wrapping |
| `agent/tools/clarify.rs` | 202 | 72 | Keep: clarification schema and answer display |
| `init_command.rs` | 179 | 84 | Keep: configuration scaffolding |
| `events.rs` | 136 | 121 | Keep: terminal input normalization and event bridge |
| `agent/tools/backends/web_fetch/native.rs` | 242 | 0 | Keep: native HTTP fetch implementation |
| `test_support.rs` | 200 | 0 | Keep: existing unit-test support and isolation |
| `llm/mock_provider.rs` | 112 | 108 | Keep: scripted provider test double |
| `slash_command.rs` | 152 | 83 | Keep: command parsing/catalog |
| `llm/openrouter_provider.rs` | 147 | 50 | Keep: thin provider wrapper |
| `llm/token_usage.rs` | 120 | 66 | Keep: request-token diagnostic records |
| `llm/token_estimate.rs` | 85 | 64 | Keep: token estimate/truncation |
| `config/compaction_prompt.rs` | 76 | 66 | Keep: layered compaction prompt |
| `config/agent.rs` | 136 | 0 | Keep: agent configuration data/deserialization |
| `build_info.rs` | 83 | 33 | Keep: build provenance rendering |
| `lib.rs` | 113 | 0 | Keep: module declarations and public re-exports |
| `ylva_home.rs` | 66 | 41 | Keep: Ylva home resolution |
| `theme.rs` | 97 | 0 | Keep: semantic color constants |
| `config/soul.rs` | 42 | 45 | Keep: layered global SOUL |
| `config/provider.rs` | 38 | 0 | Keep: provider connection configuration |
| `agent/tools/backends.rs` | 24 | 0 | Keep: backend module facade |
| `app.rs` | 21 | 0 | Keep: application runner trait |

The only non-test inline module is
`tui_reducer_state::result_section_ids`; the other discovered inline module
declarations are unit-test modules. The root `build.rs` is 57 lines and has
a single build-provenance responsibility, so no split is justified.

### Complete integration/support assessment

All paths in this table are relative to `tests/`.

| File | Lines | Assessment |
| --- | ---: | --- |
| `tui_render_state.rs` | 5464 | Split by responsibility under the same test target |
| `tools_agent_loop.rs` | 2019 | Split queue/approval, cancellation and subagent cases under same target |
| `cli_driver.rs` | 1495 | Keep target: root/child CLI orchestration regressions |
| `http_provider.rs` | 1290 | Keep target: shared mock server and provider wire-contract tests |
| `store_persistence_e2e.rs` | 1060 | Keep: shared offline/live persistence assertion flow |
| `common/mock_llm_server.rs` | 779 | Keep: cohesive scripted HTTP server |
| `cli_profile.rs` | 510 | Keep: profile-backed binary startup cases |
| `web_fetch_backend.rs` | 433 | Keep: fetch backend wire behavior |
| `system_prompt_instructions.rs` | 295 | Keep: instruction assembly regressions |
| `tools_binary_data.rs` | 254 | Keep: binary results/attachment behavior |
| `cli_init.rs` | 231 | Keep: initialization behavior |
| `steering.rs` | 220 | Keep: steering semantics |
| `interrupt.rs` | 171 | Keep: cancellation semantics |
| `logging.rs` | 163 | Keep: binary logging behavior |
| `common.rs` | 148 | Keep: integration support entry point |
| `concurrent_steers.rs` | 98 | Keep: concurrent command regression |
| `cli_help.rs` | 90 | Keep: CLI usage output |
| `cli_version.rs` | 80 | Keep: build-info output |
| `empty_prompt.rs` | 60 | Keep: initial prompt behavior |
| `build_info.rs` | 40 | Keep: build provenance |

### Grounded dependency and compatibility facts

- `Cargo.toml` uses Rust edition 2021. The same crate supplies the binary
  `src/main.rs` and the library `src/lib.rs`. The binary imports library
  items using `ylva::...`; integration tests cannot reach binary internals.
- `src/lib.rs` currently exports both module paths and many crate-root
  symbols, including agent, tool, factory, engine, persistence, driver,
  presenter and reducer types. A re-export preserves access to a moved
  type without introducing a second definition.
- Rust descendants can access their ancestor's private items. A method
  implementation in a private child can therefore operate on a parent-owned
  struct without exposing its fields to other layers.
- `AgentLoop::run` currently spans roughly 1,775 lines and owns mutable
  context, serial tools, steers, cancellation tokens, generation, subagent
  completion and compaction polling inside one spawned task.
- Outside the current Context implementation, agent runtime code writes
  `retry_after_context_rejection`, batch counters, `model_overrides`,
  `projection_dirty` and `compaction_trigger` directly. These are the
  concrete encapsulation seams for the proposed Context extraction.
- `send_compaction_applied` derives a UI-relative boundary using
  `tui_reducer_state::project_messages`. That existing cross-layer call is
  part of today's behavior, not an architectural dependency removed by
  this plan.
- `Context` maintains transcript-index-aligned bookkeeping, a schema-token
  cache keyed by exact serialized inputs, bounded model overrides and
  monotonic projection revisions. Serde JSON property order is preserved
  by the configured dependency features.
- `CompactionRuntime` owns its background task, activity identity,
  continuation deadline and shared capacity-warning lock. Its `Drop`
  implementation aborts the task and clears activity.
- `AppState` has many public fields used directly by rendering and tests.
  Several related state concepts already exist (`TurnBuffer`,
  `ChildConversation`, `ConversationListState`, `InputSnapshot`); splitting
  their files does not require changing that public shape.
- `render.rs` is a 527-line production adapter that projects state and
  synchronizes draw artifacts. `conversation_render_cache.rs` is already
  a separate cache owner. Neither is evidence of another missing layer.
- `append_tool_layout` is roughly 930 lines. Existing cached and legacy
  conversation rendering share it; title/body extraction must not fork it.
- At scan time, `tests/tui_render_state.rs` included `../src/tui_widgets.rs`
  as a large source-backed read-result fixture and checked `fn draw_chat`.
  Step 7 moved that fixture to `../src/tui_widgets/chat/cache.rs`, checks
  `fn draw_cached_chat`, and explicitly enforces its large-source and
  full-selection row counts.
- Agent request tests also read `src/agent.rs` with fixed line limits;
  shrinking that facade changes fixture size even when the path remains.
  Those cases have context-pressure assertions dependent on result size.
- Provider unit-test fixtures currently resolve through
  `../../tests/fixtures/` relative to
  `src/llm/openai_compatible_provider.rs`.
- `EngineImpl` and the OpenAI-compatible provider share an
  `EngineRuntimeConfigHandle`. `EngineRouter` picks the active engine for
  its provider connection and sorted representatives for other connections.
  Named-engine identity is distinct from provider/model identity.
- `ToolRegistry::create_call` normalizes declared path arguments before
  approval. `default_registry` constructs backend-configured tools;
  CLI/TUI display registries use the same backend configuration.
- `Store` clones share one writer task and locked file. Async writer
  shutdown means dropping the last handle does not synchronously release
  the file lock. Its title rewrite replaces the header atomically while
  preserving subsequent transcript events.
- Sandbox configured rules replace the default floor and are evaluated in
  order, with later matches winning; auto-approval catch-all rules are
  appended after them. Bash syntax extraction and network-target matching
  are already separated. `store::project_key` uses sandbox
  canonicalization rather than Git-root discovery.
- At scan time, both coverage targets used an unbounded parent-filename
  matcher. Steps 5-6 replaced it with the shared, directory-bounded
  `COVERAGE_RENDERER_EXCLUSIONS` matcher, naming actual renderer descendants
  while retaining pure extracted helpers and presenter/reducer logic.
  The subsequent user-approved all-source policy removed that matcher;
  both targets now use no project-specific filename exclusions.
  The configured gate is aggregate line coverage, not 80% per file.
- Existing `tests/common.rs` submodules use explicit
  `#[path = "common/<name>.rs"]` declarations. Shared integration support
  has the documented dead-code allowance because it compiles separately
  in each test crate. Unit support is `src/test_support.rs`.
- The existing ADR and plan directories contained only `.gitkeep` at scan
  time. No recorded ADR currently blocks these moves; this is not evidence
  that behavior or ownership invariants can be changed without review.
- Useful directly affected documentation is
  `docs/implementation-notes.md` (module/ownership paths),
  `docs/features.md` (user-facing behavior only, not implementation paths),
  `docs/development-workflow.md` (coverage and test layout), and
  `docs/known-test-issues.md` (relocatable examples).
