# Improved rendering for large expanded bubbles

## Context

Ylva's Rust TUI synchronously rebuilds the whole displayed conversation on every
frame. Expanding a tool result containing more than 3,000 lines can therefore
make scrolling and keyboard/mouse input lag while consuming approximately one
CPU core. Markdown parsing and wrapping are repeated even when the content is
unchanged. Transcript projection, styled-line cloning, and selection-text
extraction also traverse or copy the full content on each redraw.

The goal is responsive interaction with large expanded results without changing
the transcript, Markdown semantics, displayed content, or existing controls.
This plan covers root and subagent conversation lists and the separately
scrollable subagent header bubble.

The agreed scope is per-entry layout caching, reuse of unchanged conversation
layout, and painting only the visible cached rows. This is viewport painting of
an exactly laid-out document, **not** lazy Markdown parsing or estimated-height
virtualization. Initial layout still parses/wraps the full changed entry.

True lazy Markdown virtualization, truncation or default content caps, a global
content-addressed Markdown cache, new user-facing configuration, dependency
changes, and benchmark tooling are out of scope. CLI output, persisted sessions,
model views, and compaction semantics must remain unchanged.

### Acceptance criteria

- After warming the current layout, unchanged-content redraws, wheel/page
  scrolling, mouse movement, and selection dragging do not parse/wrap completed
  entries, hash/compare their complete contents, deep-clone the transcript or
  full styled document, or regenerate selection text for all rows.
- Row copying and selection highlighting during painting are bounded by the
  visible conversation/header rows, not the size of expanded results.
  Small metadata lookups and viewport geometry updates are allowed.
- A growing stream rebuilds its changed entry, not previously completed
  entries. Spinner and elapsed-time updates remain live without rebuilding
  unchanged large bodies. Title wrapping/height changes update affected geometry.
- Exact content height, scrollbar positions, auto-scroll/manual scroll lock,
  collapse/expand behavior, pending-approval ordering, thinking bubbles, queued
  steers, and return-to-root bubble anchoring remain correct.
- Cached selection data and hit-test ranges match displayed rows, including
  wrapped titles, Markdown tables, wide Unicode, and stripped bubble chrome.
  Selection highlighting never contaminates cached base styles; copying across
  rows reached by scrolling preserves existing text semantics.
- All content, state, width, theme, replay, compaction-marker, clear, and
  navigation changes invalidate the appropriate cached data. Root and subagent
  content cannot leak into one another's view.
- Cache storage belongs to one active view, including its header, and one
  current width per rendering surface. Removed entries, old revisions/widths,
  previous views, and cleared sessions release their derived storage.
- Optional retained expanded bodies of collapsed entries share a 32 MiB budget
  and least-recently-used eviction. Required current-layout rows and selection
  data are exempt: this is not a total-memory or total-process cap. An oversized
  active entry must not be evicted and reparsed on every frame.
- Deterministic automated regression tests establish cache reuse, invalidation,
  and viewport-work bounds. Manual release-build checks establish that warm
  scrolling, selection, and input are responsive and do not sustain the reported
  CPU saturation. There is no hard wall-clock/p95 acceptance gate in this phase;
  a 16 ms warm-frame time is an aspiration, not a measured guarantee.
- A manual check records cold expansion, resize, view-switch, and growing-stream
  pauses separately. This phase does not promise bounded cold-operation latency.
- A completed implementation updates the named documentation, passes
  `make format lint` and `make test-coverage`, and leaves this checklist accurate.

### Starting point and constraints

Open `src/tui_widgets.rs`, starting at `draw_chat`, then inspect
`src/render.rs::project_for_draw` and `sync_from_draw`. The first implementation
baseline command is `cargo test --test tui_render_state`; add a failing
large-result/cache-reuse regression before changing behavior. Planning alone
does not require running the Rust gates or implementing source changes.

Keep rendering derived from the canonical view state and preserve the
presenter/view/driver separation. Do not persist cache keys or cache contents,
add render metadata to model messages, or make the presenter perform terminal
I/O. No source implementation, staging, branching, or commit is authorized by
the request to write this plan.

## Implementation Plan

An implementing agent must change `[ ]` to `[x]` immediately after completing
each step and rewrite this file in place. Execute steps in order, retaining
completed checkbox states if steps are revised or renumbered. Keep affected
documentation current as each logical change is completed; the documentation
step below is the final cross-check, not permission to leave earlier work stale.

1. [ ] Establish regression fixtures and current behavior in
   `tests/tui_render_state.rs`. Use offline `TestBackend` rendering of expanded
   completed tool results with 3,000 and 30,000 lines; include plain content,
   Markdown tables/lists, fenced code, and Unicode/wrapped titles. Exercise the
   production `render::render` path as well as existing direct widget tests.
   Capture exact visible buffers, selection text/prefix data, bubble ranges,
   ordering, and scroll bounds. Add a failing deterministic warm-redraw
   regression using narrowly scoped test instrumentation or an injected layout
   builder, not timing assertions or global counters. Run the targeted tests
   to establish the failure before fixing it. Synthetic view data may exceed
   the real `read` output cap.

2. [ ] Introduce a cohesive derived-layout cache in
   `src/conversation_render_cache.rs`, wired through `src/lib.rs` and the relevant
   view state in `src/tui_reducer_state.rs`. Store immutable base
   `Line<'static>` rows, row counts, selection text/prefix data, and entry-local
   tool/thinking hit-test ranges together. Separate expensive body layout from
   dynamic title/cursor rows. Scope keys by active-view/session generation,
   entry slot and revision, effective body width, collapse state, active theme,
   and rendering variant/title metadata. Keep the header rendering variant
   distinct from the conversation variant. Do not scan/hash source content
   on a warm frame. Unit-test key discrimination, relative ranges, storage
   sharing, and replacement of obsolete revisions/widths. Keep this pure
   cache/layout logic inside the coverage gate.

3. [ ] Wire mutation-driven invalidation through `src/presenter.rs` and the
   state helpers in `src/tui_reducer_state.rs`. Centralize production transcript
   mutations sufficiently that append, replacement, status/content updates,
   queued-steer changes, stream segmentation/flush/cancellation, notices/errors,
   collapse toggles, replay/lazy loading, and compaction-marker insertion cannot
   bypass revision updates. Track root and each subagent independently;
   background updates must not invalidate an unrelated active transcript,
   except when its displayed root header bubble changes. Appends preserve
   earlier cached slots; splices/replay may invalidate or remap affected slots
   so index shifts never associate old rows with new entries. Width/theme
   changes invalidate layout; viewport height/offset/selection changes alone
   do not invalidate bodies. Tests must cover all mutation families, clear,
   navigation, identical-length replacements, same-slot replacement, and
   index shifts. Public direct-state/direct-widget callers must remain correct:
   use an explicitly conservative compatibility path when revision tracking
   cannot be guaranteed, rather than silently serving stale data.

4. [ ] Remove full-content snapshot work from the production frame path in
   `src/render.rs` and `src/tui_widgets.rs`. Introduce a borrowed internal view
   of the active transcript, stream, queued steers, and root header bubble,
   alongside mutable derived layout/geometry. Reuse existing rendering helpers
   rather than duplicate the UI. Eliminate production-frame clones of
   `chat_entries`, result payloads, and full selection/style buffers; retain
   the owned legacy `TuiState` API and returned driver snapshots where required
   by callers/tests. Update `src/tui_render_state.rs` and
   `src/tui_driver.rs::app_state_to_tui_state` only as necessary for compatibility.
   Do not move full-content comparison or cloning into a supposedly cheap
   adapter/cache lookup. Verify the production path through `TestBackend`,
   while existing direct widget calls still reflect arbitrary state edits.

5. [ ] Integrate cached entry/body layout into `draw_chat` and the header
   preparation in `src/tui_widgets.rs`. Reuse unaffected entries and cached
   selection data; rebuild the conversation's ordered row index and translated
   hit-test ranges only when layout dependencies change. Preserve the initial
   wordmark, separators, queued steers, live thinking/assistant placement,
   deferred pending-approval bubbles, and trailing-blank handling. Update
   spinner/elapsed-title/cursor rows independently; if a title's wrapped height
   changes, repair downstream offsets without reparsing its unchanged body.
   Give the header its own width/height and scroll bounds while sharing the
   active-view lifecycle. Tests must prove unchanged ticks/scroll/drag reuse
   layout, changed stream text rebuilds only the stream entry, and terminal
   tool transitions update chrome and content correctly.

6. [ ] Paint only exact visible row slices in conversation and header widgets.
   Use cached total heights and an indexed row lookup to resolve the viewport;
   create mutable copies only for displayed rows and apply selection overlays
   to those copies. Avoid `Paragraph::new(lines.clone())` over the entire
   document and avoid narrowing absolute `usize` scroll offsets to `u16`;
   render the already-sliced rows at local offset zero. Continue to support
   full-document selection/copy using cached immutable selection data, doing
   selected-range extraction only when requested. Preserve row alignment,
   border/title mouse routing, auto-scroll clamping, header sizing, and
   `pending_scroll_to_bubble` resolution. Add viewport-work-count assertions,
   cross-scroll clipboard tests with an injected/no-op clipboard, and a
   synthetic wrapped document exceeding 65,535 rows to verify bottom scrolling
   does not wrap its offset.

7. [ ] Implement active-view cache release and optional collapsed-body retention.
   Use one code-owned 32 MiB budget shared by the list and its header; conservatively
   account retained row/span/string/vector allocation capacities without
   double-counting shared allocations. Document the accounting definition,
   rather than claiming an exact resident-memory measurement. Evict optional
   retained bodies least-recently-used, skip retention of bodies exceeding the
   allowance, and keep required active layout resident. Collapsing releases or
   moves the body to optional storage; expanding reuses it only if revision,
   width, theme, and variant still match. Navigation/clear release all previous
   active-view derived storage, and invalidation removes obsolete optional
   copies. Unit-test the exact budget boundary, oversized bodies, eviction
   order, header/list sharing, revision pruning, and clear/view-switch release.

8. [ ] Update affected existing documentation:
   `docs/features.md` (large-result responsiveness and unchanged full-content
   behavior), `docs/implementation-notes.md` (cache ownership, keys/invalidation,
   borrowed projection, exact viewport painting, dynamic chrome, selection
   data, and memory accounting), and `docs/ideas/improved-rendering.md` (which
   suggestions were delivered versus deliberately deferred). Keep this plan's
   checklist and manual evidence current. Existing configuration and
   keybindings stay unchanged, so their documents need no behavioral edits.
   Do not create an ADR or another documentation file without separate consent.
   Check the ADR directory again before implementation; if a new decision
   conflicts, resolve it before proceeding. Run `rumdl fmt` and
   `rumdl check` on the changed Markdown files.

9. [ ] Complete automated and manual verification. Run targeted cache/state/
   presenter/render tests while implementing, then `make format lint` and
   the single mandatory full-suite gate `make test-coverage` (no redundant
   mandatory `make test`). With an optimized release build, manually expand
   large plain/Markdown/code results, wheel/page-scroll top/middle/bottom,
   drag-select while scrolling and copy, type in the input, and check spinner/
   timers during background work. Repeat for subagent transcripts and their
   header, collapse/re-expand, resize, navigate back, and clear/resume.
   Record terminal dimensions, approximate fixture sizes, observed warm
   responsiveness/CPU behavior, and separate cold/streaming observations in
   this plan. A 120-by-40-cell terminal is a useful common check, not the only
   supported size. If warm lag remains, identify residual full-content work
   and fix it within scope before marking this step complete. No automated
   benchmark or hard latency assertion is required.

## Implementation Hints

- These decisions were confirmed during plan refinement: caching plus layout
  reuse and viewport painting; full content/exact Markdown; root/subagent/header
  coverage; active-view/current-width lifetime; 32 MiB optional collapsed-body
  retention with required-layout exemption; deterministic tests and manual
  performance checks instead of a timed benchmark.
- `src/tui_driver.rs::draw_frame` calls `render::render`;
  `run_tui_with_title` draws once per loop iteration. Continuing to redraw for
  animations/input is compatible with this plan; skipping frames is not the fix.
- `src/render.rs::project_for_draw` currently clones root or subagent messages,
  stream strings, queued steers, header tool state, and selection buffers into
  an owned `TuiState`. Its comment describes a shallow projection, but the
  owned strings/boxed tool state are actually deep-cloned.
- `src/render.rs::sync_from_draw` writes geometry, scrolling, selection data,
  and hit-test ranges back to canonical `AppState`. It resolves
  `pending_scroll_to_bubble` against the root bubble's newly rendered row.
- `src/tui_widgets.rs::draw_chat` appends all entries, live thinking/assistant
  text, queued steers, and finally pending-approval tool bubbles. It extracts
  selection data for all rows, highlights selected rows, clones all styled
  lines into a `Paragraph`, and casts the absolute scroll offset to `u16`.
- `src/tui_widgets.rs::draw` separately constructs the subagent header using
  `append_tool_bubble_lines`, with the title override `Subagent`. Its height is
  capped at 40% of terminal height, and it has its own scrolling/selection data.
- `append_tool_bubble_lines` takes `ToolBubbleArgs`, including spinner frame,
  subagent start time, registry-derived display title, and subtype argument.
  Caching the whole output solely by content/width/collapse would freeze live
  indicators and omit relevant rendering inputs.
- `src/tui_reducer_state.rs::ChatEntry` uses owned strings. Only tool bubbles
  have UUIDs; thinking hit-test ranges use entry indices, and streaming thinking
  uses `STREAMING_THINKING_ENTRY_INDEX`. A cache must not assume every entry has
  a stable UUID or that indices survive structural edits.
- `ConversationListState` exists as two distinct shapes in
  `src/tui_reducer_state.rs` and `src/tui_render_state.rs`. Canonical geometry is
  in `AppLayout`; the legacy rendering state stores rectangles with area data.
- Canonical mutation/navigation starting points include `TurnBuffer`,
  `AppState::replay_messages`, `load_child_messages`, `target_messages_mut`,
  `find_tool_bubble_mut`, `push_user_bubble`, `push_notice_to`,
  `commit_queued_steer_messages`, collapse helpers, view-switch helpers, and
  `clear_conversation`. Presenter domain handlers also mutate entry vectors
  directly. Entry-length-only change detection is insufficient.
- `ChatSelection` uses absolute rendered-line positions.
  `extract_chat_selection_text` and `extract_header_selection_text` depend on
  full ordered selection text and per-line chrome-prefix widths. Viewport-only
  buffers cannot simply replace these with visible-only data.
- `src/markdown.rs::render_markdown` returns owned `Vec<Line<'static>>` and
  handles complete Markdown input, including code blocks and tables. Source-line
  slicing before parsing would lose parser context.
- `src/highlight.rs::highlighted_lines_cached` caches syntax highlighting in
  a thread-local content-hash map, but still hashes source text and clones
  highlighted tokens on a hit. The agreed warm layout path avoids invoking it
  for unchanged bodies; redesigning that separate cache is out of scope.
- `active_theme()` supplies the syntax theme; the profile normally selects it
  once at startup, but `set_active_theme` exists. Theme is a layout dependency.
- `read` output has a 262,144-raw-byte cap. Very large performance/correctness
  fixtures can therefore be synthetic view-model entries rather than requests
  to increase tool limits.
- `tests/tui_render_state.rs` already contains direct widget and production
  `render::render` tests using `TestBackend`, plus an `InspectableTestBackend`.
  Existing tests build/edit owned public state directly, so revision tracking
  cannot silently assume every external caller uses presenter events.
- `src/lib.rs` exposes library modules and types for integration tests. New
  integration-test-visible items must be reachable there, not through `main.rs`.
  Unit-only helpers belong in `src/test_support.rs`; integration helpers belong
  in `tests/common.rs` or its established file-per-module submodules.
- `docs/known-test-issues.md` requires subscriber locking for tracing capture
  and cwd-sensitive tests, concurrent draining of bounded agent UI channels,
  and injected side effects rather than relying on library `cfg(test)` guards
  from integration tests.
- The `Makefile` coverage threshold is at least 80% line coverage. `render.rs`,
  `tui_widgets.rs`, and `main.rs` are excluded; the new pure cache module is not.
  `make test-coverage` already runs the full suite.
- At planning time, `docs/architecture-design-records/` and `docs/plans/` had no
  files. The worktree had an unrelated modification to `Cargo.toml`; it is not
  part of this plan and must not be overwritten, staged, or committed.
