# Session picker and manual compaction slash commands

## Context

Extend Ylva's Rust TUI with `/session` and `/compact`. The session picker makes
explicitly resuming or cloning a prior conversation possible without exiting the
TUI and copying its conversation ID. Manual compaction lets the user request a summary
for the root agent context without waiting for automatic usage thresholds.

This is an implementation plan, not authorization to implement source changes.
The scope excludes `/agent`, `/status`, automatic startup resume, session
maintenance, new clone formats, configurable command registries, subagent manual
compaction, and new providers. Existing clones remain eligible for resume.
The headless CLI keeps its existing behavior. The policy below was refined
through the grilling skill and explicitly confirmed by the user. The user
subsequently added picker cloning and confirmed its controls and cancellation
policy.

### Required behavior

- `/session` opens a popup of recent sessions, showing a title and last-used
  date/time, sorted newest first. A visible Resume / Clone selector chooses
  whether confirmation resumes that root session for append or activates a
  new lossless clone. Opening or cancelling the picker does not switch sessions.
- `/compact` requests compaction only while the root conversation is displayed.
  Compaction changes only the model view, never deletes transcript messages,
  and uses that agent type's resolved compaction engine, prompt, and timeout.
- Both commands join the existing suggestions, completion, help, keyboard, and
  mouse surfaces. Recognized commands are not user messages or queued steers.
- Session loading, resume failures, unsupported compaction targets, and
  compaction outcomes are visible to the user and produce appropriate structured
  diagnostics. Failures never silently create a fresh or unpersisted session.

### Constraints and acceptance criteria

- The outer agent loop remains the sole context mutator. The presenter remains
  pure; filesystem/network work stays in the driver and existing runtime owners.
- Session selection respects identity locks, archive resolution, interrupted-tool
  reconstruction, restored compaction/model projections, and child-run replay.
  Historical messages must not be appended again during hydration.
- Clone activation gives the user a fresh root session ID with `cloned_from`
  identifying the source. Source transcript/archive bytes and historical child
  transcripts remain unchanged. Copied parent lifecycle links name the clone;
  all subsequent writes go to the clone, never to the source.
- A successful switch updates the actual root agent, command/update channels,
  tools, engine display, title handling, replay store, and final exit identity,
  not merely the visible chat entries. An unsuccessful switch leaves the
  current conversation usable.
- Manual requests bypass automatic soft/hard trigger decisions, not tool-pair
  boundary checks, engine capacity warnings, request deadlines, generation
  checks, or safe request ceilings. Duplicate requests cannot start parallel
  summaries for the same agent.
- All original messages and tool bubbles remain browsable after manual
  compaction and after later resume. Summary persistence and the existing
  root compaction marker/footer must work. Automatic subagent compaction is
  unchanged.
- Tests cover the decisions below, complete TUI-to-runtime paths, failure
  isolation, and unchanged `/models`, `/tools`, `/clear`, and CLI behavior.
  Final implementation verification follows `docs/development-workflow.md`,
  including aggregate line coverage of at least 80%.

### Confirmed session-picker policy

- List up to 32 root sessions in the active store's project group;
  include archives and clones, exclude subagent transcripts and the active root.
  The limit is fixed, not a profile setting. Use the shared persisted-activity
  timestamp and its existing UUID tie-breaker, not last-message-only time.
  Display local date/time with a timezone indication, preserving timestamp
  visibility before eliding sanitized titles.
- Allow browsing while work runs, but reject switching until root work,
  scheduled/running subagents, and in-flight or pending compaction are quiescent.
  Do not cancel work implicitly. Empty sessions remain selectable with an
  `Untitled` display label; duplicate titles remain distinct selectable IDs.
- The picker is available from root and subagent views. Successful selection
  shows the target root transcript; cancellation or failure preserves the
  current view. A failed selection keeps the picker open with a visible error
  and permits another selection or Esc.
- Each newly opened picker defaults to Resume. Left/Right or clicking an action
  selects Resume or Clone; Enter confirms the highlighted session with that
  action. Clicking a session row only highlights it. The chosen action remains
  visible, including in errors; navigation does not reset it.
- Resume with the selected profile and workspace unchanged, restore the target's
  stored root agent type, and initialize its conversation engine from that type's
  current profile configuration. A legacy session without a stored type uses
  `default`; an unavailable stored type is an error. Launch `--agent` and the
  previous session's `/models` selection do not override picker resumes or
  clones. Clones inherit the source's latest effective stored root type, title,
  transcript, and projection state under the existing clone contract.
- Target preparation has a cancellable `Resuming` or `Cloning` state. Rendering and
  interrupt/quit controls remain responsive; prompt submission, duplicate
  selection, `/models`, `/clear`, and `/compact` are blocked until resolution.
  Esc cancels before publication; cancelled or stale results cannot activate
  the target. The final publication rechecks quiescence.
- If a clone has been durably created but cancellation or subsequent activation
  failure prevents publication, keep that clone and visibly report its ID as
  `Clone created but not activated`. Do not delete it implicitly or silently
  abandon it. It remains available for explicit later resume. A clone creation
  error never falls back to appending to the source. Missing, locked, invalid,
  or child sources fail before creating a clone under the existing contract.

### Confirmed manual-compaction policy

- The root is the only supported target. From any subagent view, `/compact`
  refuses explicitly: manual compaction is available only in the root
  conversation. It never operates on a hidden root or rewrites a child store.
- Manual compaction summarizes a useful completed prefix before the latest
  explicit user task, even below the soft threshold or configured target ratio.
  That task and every following message remain verbatim. No useful older
  prefix, including empty/one-task/unchanged contexts, produces a no-op notice.
  This guarantee concerns the manual operation: it does not undo existing
  model-only projections or prevent independent request-pressure recovery.
- A busy root immediately snapshots an eligible completed prefix without
  cancelling work or queueing a request for the end of the turn. The result
  applies only at a safe boundary. Queued steers not yet committed do not
  become part of the snapshot; normal steering order is unchanged.
- A summary already running or awaiting application produces an
  `Already compacting` notice. No additional pass is queued and its deadline
  is not restarted, regardless of whether its trigger was automatic or manual.
- A manual summary applies only if the model-view token estimate strictly
  decreases and retained-task/tool-pair invariants hold. Empty, failed, timed-out,
  or non-shrinking results leave the model view unchanged and report failure
  or `No reduction`. Pressure-driven request safety recovery remains separate;
  manual failure alone does not enable lossy fallback.

## Implementation Plan

An implementing agent must change each `[ ]` to `[x]` immediately after completing
that step and rewrite this file in place. Preserve completed states if steps are
renumbered. Each step includes its focused verification; update the relevant
documentation alongside behavior changes, with the final documentation pass in
step 10. Do not stage or commit without explicit consent.

1. [x] Establish the focused baseline and owner-local fixtures for the confirmed
   contract. Start by opening `src/slash_command.rs`; the first baseline is
   `cargo test --lib slash_command::tests`. Recheck
   `docs/architecture-design-records/` before implementation. Use existing mock
   engines and temporary stores for red/green feature assertions in later steps.
2. [x] Add no-argument `Session` and `Compact` entries in
   `src/slash_command.rs`. Extend command-to-effect translation in
   `src/presenter/keyboard.rs` and the typed effect surface in
   `src/tui_reducer_state/events.rs`. Do not send either command as a prompt.
   Test exact names, suggestions, partial-command Enter, Tab, trailing
   whitespace, rejected arguments, leading-space ordinary chat, and the
   shared `/c` prefix without changing `/clear` behavior.
3. [x] Implement the session-picker state alongside `SlashOverlay` and
   owner-local state tests under `src/tui_reducer_state/`. Load candidates from
   the active store's `chats_root()` and `project_key()` using
   `Store::list_sessions_with_diagnostics`. Apply the settled root/current
   filtering and fixed 32-session limit after sorting. Keep listing read-only,
   surface diagnostics, and perform blocking transcript/archive reads outside
   the UI event loop. Test empty lists, untitled/duplicate titles, offset-aware
   timestamp ordering, ties, archives, and exclusion before truncating. A fixture
   with more than 32 eligible roots must return exactly the newest 32.
4. [x] Wire asynchronous picker loading and result handling in
   `src/tui_driver.rs` and its effect owner; add a driver-local session owner
   module only if needed to keep responsibilities separate. Tag load results
   with the initiating session/picker identity so cancel, `/clear`, and a newer
   load invalidate stale results. Add rendering/layout in
   `src/tui_widgets/overlays.rs` and navigation/selection in
   `src/presenter/keyboard.rs` and `src/presenter/mouse.rs`. Support arrows,
   page keys, wheel, click-to-highlight, Enter-to-confirm, and Esc-to-cancel.
   Add a visible Resume / Clone action selector defaulting to Resume on open;
   Left/Right and action clicks change it without activating a session.
   Carry both source ID and the selected typed action in the confirmation effect.
   Show loading, empty, selection-error, and cancellable `Resuming`/`Cloning` states.
   Consume the no-argument command rather than adding an undocumented filter.
   Prevent ordinary typing/Enter while the picker is open from submitting a
   prompt accidentally; Esc dismisses and clears the command draft, consistent
   with existing slash overlays. Preserve approval routing when temporarily
   interrupted by approval/clarify UI. Test availability from both conversation
   views, short/narrow terminals, elided sanitized titles, local timezone labels,
   selection bounds, empty-list Enter, cancel during loading, action selection
   by keyboard/mouse, default restoration on reopen, and action retention after
   a failed selection. Check the displayed action agrees with the dispatched
   effect; modified Enter is not a required clone control.
5. [x] Add explicit root-session replacement orchestration to `TuiApp` and the
   driver. Expose a factory construction path that takes a target store and
   resolved root agent type without retaining the startup store as a hidden
   owner. Extract reusable root-agent selection from `src/main.rs` into a
   library owner rather than duplicating CLI resume rules. Parameterize the
   explicit override so startup CLI semantics stay unchanged while picker
   resumes/clones use stored type/default with no launch override. Validate quiescence
   authoritatively in the runtime, not solely from potentially stale TUI mode:
   include active turns/tools, pending steers, child queues/runs, and summary
   activity/pending application. Freeze state-changing submissions during
   `Resuming`/`Cloning`; recheck the guard before publication.
   Dispatch the typed action to `Store::resume` or `Store::clone_session`,
   respectively. Resolve the source's latest stored agent type for a clone and
   pass that effective type into the existing clone API. Prepare and validate
   the resulting target store, current profile, and replay snapshot before
   releasing the old active runtime. The activated identity is the selected
   ID for Resume, but the new returned ID for Clone.
   Validate available type/configuration and root eligibility before append-open
   where possible and revalidate against the opened target. Construct fresh
   conversation Engine runtime handles from the selected type's configured
   settings: do not mutate the old session's shared engine during preparation
   or inherit its live `/models` settings. Preserve provider connection limits.
   Keep target preparation failures local and leave the old loop, view, and
   identity intact, with the error in the still-open picker. Add
   explicit loop shutdown/join support if needed instead of forcibly aborting
   work or relying on command-channel closure while senders remain alive.
   Release cancelled/failed prepared targets and prevent late preparation
   results from publishing; blocking archive/open/clone operations may still finish
   after cancellation, so cancellation is not a promise to undo archive
   restoration or normal repair-on-open. Keep a separate creation outcome for
   cloning: if creation succeeded but activation did not, retain the durable
   clone and report its ID even when the picker load result is stale/cancelled.
   Do not let stale-result suppression hide a newly created clone, and never
   automatically retry creation or convert Clone to Resume.
   Test locked, missing, removed-since-listing, corrupt, archived, and
   unknown-agent-type targets, plus root/child/compaction busy guards,
   cancellation during prepare, blocked mutations, and a guard race at publish.
   Cover raw/archived clone sources, already-cloned sources, clone creation
   failure, cancellation before and after creation, activation failure after
   creation, and one clone per explicit confirmation. Assert failed source
   validation creates no session.
6. [x] Complete successful-switch publication atomically from the user's
   perspective: replace the agent, receiver, sender, engine handle, available
   tools, and displayed metadata; rebuild transcript/child lifecycle replay,
   compaction marker, input history, and view/cache state using the existing
   replay helpers. Replace the startup-captured replay store with the current
   store so drill-in works after both `/session` and `/clear`. Retarget title
   generation and terminal title, ignore old-session title/load/update results,
   and make the application exit guard observe the final active identity across
   replaced loops and subsequent `/clear`, rather than retaining the initial
   loop's handle. Preserve the selected profile/theme/workspace metadata.
   Release obsolete store clones and returned contexts so the
   old writer eventually drains and unlocks. Record the activated engine as
   CLI resume does; preserve target metadata and do not duplicate transcript
   messages. Test switching A to B to A, `/clear` then selection, appends only
   to B, replay of tools/children/projections, title isolation, final resume
   hint identity, and old-session lock release. For Clone, hydrate/replay from
   the returned clone store rather than the source; assert a fresh root ID,
   correct `cloned_from`, inherited title/type, rebased child lifecycle links,
   unchanged source bytes/archives, shared historical child drill-in, copied
   compaction/projection state, and new writes exclusively to the clone.
   Route created-but-not-activated notices to the still-active TUI even after
   picker dismissal; on quit, report that clone ID after terminal restoration
   without replacing the exit identity of the session that was actually active.
7. [x] Add a typed manual-compaction command and explicit scheduling outcomes
   in `src/agent/protocol.rs`, `src/agent/runtime/commands.rs`, and
   `src/agent/compaction.rs`. Reuse the existing summary request builder,
   engine/prompt/timeout resolution, outcome channel, and safe application
   pipeline. Add a manual boundary selection path beside the existing selectors
   in `src/agent/context/recovery.rs` that implements the chosen retained-tail
   rule when the automatic target selector would do nothing. Distinguish
   started, already pending, nothing eligible, disabled/unavailable, and
   failure outcomes; do not reset a pending request's original deadline.
   Keep automatic threshold behavior unchanged and separate manual failures
   from automatic fallback scheduling according to the settled policy.
   Test below-soft/below-target requests, repeat requests, empty/one-turn
   contexts, existing summaries/recovery handoffs, open tool groups,
   safe boundaries, latest-task preservation, timeout, empty/growing summaries,
   configured/following compaction engines, and unchanged automatic behavior.
   The manual selector chooses the furthest advancing safe boundary at or before
   the latest explicit task, with genuinely new eligible source since the
   existing view boundary. Include prior summary/handoff as source without
   resummarizing only an unchanged summary. Record trigger provenance so only
   automatic failures retain their existing fallback behavior. Before manual
   application, evaluate the candidate without mutating/persisting the live view,
   compare estimates against the same current transcript/tail and tool schemas,
   and require strictly positive savings. Test zero savings, growth, concurrent
   tail additions, and absence of compaction events on rejection.
8. [x] Route root-view `/compact` through the presenter and driver effect to
   the root command channel. Explicitly reject it in every subagent view,
   whether live, queued, finished, or replay-only; do not send `ForChild` or
   compact the hidden root. Handle exact typed commands and suggestion Enter
   consistently before ordinary child-view Enter suppression; allow `/session`
   there without accidentally enabling ordinary child prompts or changing
   existing child behavior for other commands. Keep root request identity
   internal to runtime/activity updates and structured diagnostics, not in
   visible acceptance/no-op/duplicate/failure notices. Preserve scoped
   application and refreshed context estimates. Surface command-channel
   delivery errors instead of implying acceptance. Test view changes during
   compaction, busy safe application, stop/clear/session replacement, and late
   outcomes. Preserve automatic child compaction and approval/clarify routing.
9. [x] Add assembled regression coverage in
   `tests/tui_render_state/overlays.rs`,
   `tests/tui_render_state/subagent_views.rs`, and the appropriate existing agent,
   factory, store, and driver owner-local suites. Drive commands through
   textarea/suggestions rather than testing only helper calls. Use temporary
   stores and mock engines, drain bounded UI channels concurrently, and wait
   for observable completion instead of fixed sleeps. Assert exact persisted
   message counts, compaction snapshots, safe model payloads, final identity,
   and no cross-session writes. Exercise both picker actions end-to-end and
   assert cancelled/failed activation leaves a discoverable clone with an
   explicit notice when creation already succeeded. Keep `tests/session_commands.rs` and
   `tests/store_persistence_e2e.rs` behavior unchanged; export necessary
   library surfaces through `src/lib.rs`, not the binary.
10. [x] Finish documentation reconciliation in `README.md`,
    `docs/features.md`, `docs/keybindings.md`, `docs/persistence.md`,
    `docs/implementation-notes.md`, `docs/configuration.md`,
    `docs/cli-options.md`, and `docs/ideas/slash-commands.md`. Describe the
    settled picker eligibility/limit/timestamp and cancellation policies,
    same-process resume/clone/type/engine selection, root-only manual compaction,
    failure/no-reduction behavior, and keyboard/mouse controls.
    Keep fresh-start/no-startup-picker and CLI contracts explicit. Keep
    `docs/cli-options.md` focused on launch options with a short `/session`
    cross-reference and `docs/configuration.md` focused on configuration with
    a short slash-command cross-reference; document TUI slash-command behavior
    in `docs/features.md` and controls in `docs/keybindings.md`. Remove only
    the implemented `/session` and `/compact` proposals from the ideas backlog;
    preserve `/agent` and `/status`. Document current-profile resume/clone behavior,
    action controls, source immutability, shared historical child transcripts,
    and retained-but-not-activated clone notices;
    the fixed picker cap adds no configuration key. Reconcile the ideas file's
    introductory deferred status without overwriting unrelated user edits.
11. [x] Run focused owner/assembled tests during the steps, format/check all
    changed Markdown with `rumdl` and `.rumdl.toml`, then run the mandatory
    implementation gates `make format lint` and `make test-coverage`.
    Resolve failures and preserve at least 80% aggregate line coverage;
    do not run `make test` as a duplicate mandatory gate. Confirm all
    acceptance criteria and checkbox states before reporting completion.

## Implementation Hints

- User-confirmed picker decisions: fixed cap 32; root sessions in
  the current store's launch-directory group; archives/clones/empty sessions
  included; active root and child transcripts excluded. The shared
  persisted-activity definition intentionally replaces the idea's
  message-only timestamp. Local timezone-labelled dates and sanitized
  titles with an `Untitled` fallback are the selected presentation.
- User-confirmed interaction decisions: `/session` is available in both views;
  browsing while busy is allowed but switching while root/subagent/compaction
  work remains is rejected. Failures retain the picker/current view.
  `Resuming`/`Cloning` permits cancellation/rendering/interrupt/quit but blocks other
  mutations; final publication rechecks the guard.
- User-confirmed selection decisions: stored root type (legacy missing type
  uses `default`), current-profile configured engine, unchanged profile and
  workspace; neither launch `--agent` nor the prior live `/models` selection
  applies to picker resumes or clones.
- User-confirmed clone decisions: the picker supports both Resume and Clone.
  A visible action selector defaults to Resume; Left/Right or action clicks
  select the operation, and Enter confirms. A durably created clone that is
  not activated is retained with an explicit ID-bearing notice, never
  implicitly deleted. The existing exclusion of the active root is unchanged.
- User-confirmed manual decisions: root only, explicit refusal from child
  views, latest explicit task plus following tail retained, immediate safe
  completed-prefix snapshot even while busy, and no-op if nothing useful is
  eligible. Existing summaries are not duplicated or given a fresh deadline.
  Failed/empty/non-shrinking manual results preserve the model view;
  pressure-driven recovery is independent, not manual-failure fallback.
- `src/slash_command.rs` owns the closed command allowlist and parser;
  `/models` is the only command currently accepting an argument. Suggestions
  and exact parsing are distinct paths in `src/presenter/keyboard.rs`.
- `src/tui_reducer_state/model_picker.rs` currently owns `SlashOverlay`;
  presenter keyboard/mouse, reducer layout/selection, and
  `src/tui_widgets/overlays.rs` share its variants and row geometry.
  `/models` provides existing async loading/cancellation and picker patterns in
  `src/tui_driver/models.rs`, but its provider filter is not a session contract.
- `src/store/replay.rs` owns `SessionSummary`, listing diagnostics,
  `read_session_state`, and `read_latest_agent_type`. Listing already sorts
  descending RFC 3339 instants, then descending UUID, includes archives,
  and supplies `parent_session` for root filtering. It scans full transcripts.
  The CLI deliberately reverses the library ordering.
- Session grouping hashes the canonicalized launch directory, not the
  discovered workspace root. `Store::chats_root()` and `Store::project_key()`
  identify the current group. `Store::resume` takes a launch-directory path
  and ID and opens for append, including locked archive restoration/repair.
- `src/store.rs::Store::clone_session(chats_root, cwd, source_id, agent_type)`
  already creates a lossless root clone with a fresh ID and `cloned_from`.
  It takes a nonblocking source identity lock, copies logical events with
  parent lifecycle IDs rebased, shares historical child transcripts, and never
  repairs or appends to the source. Archived sources are read without restoration.
  The returned store already owns a durable cloned transcript before any TUI
  activation. `docs/persistence.md` describes this existing clone contract.
- `src/main.rs::resolve_root_agent_type` implements explicit `--agent`, stored
  type, then `default` precedence. Startup creates the engine from the selected
  type's current profile configuration and records `model_changed`; it does
  not restore live-only `/models` engine identity or reasoning selections.
- `src/agent_factory.rs::create/build` constructs tools, prompt, agent-type
  settings, compaction selection, and child manager as one root runtime.
  `AgentFactory::with_store` currently stores a clone of the startup store.
- `src/tui_driver.rs::run_tui_with_title` captures `replay_store`, command
  sender, engine, available tools, and agent join handle at startup.
  `TuiApp::run` captures the initial loop's `SessionExitGuard`.
  `src/app.rs` bases the final identity on that loop's shared current-store
  handle, which already follows `/clear` but not replacement with another loop.
- `/clear` uses `AgentCommand::ClearContext` in
  `src/agent/runtime/commands.rs`: it resets compaction, increments generation,
  cancels stream/tools/subagents, and updates context/store/child-manager
  handles. It is cancellation prior art, not a transactional resume operation.
- `AgentLoop::run` in `src/agent/runtime.rs` returns a join handle yielding
  `Context`; store clones retained by the factory, context, driver, or exit
  guard can keep a session writer/identity lock alive. `Store::flush` exists,
  but dropping a store does not synchronously release its writer lock.
- `src/agent/compaction.rs::CompactionRuntime::schedule_summary` requires soft
  pressure; `start_summary` uses a supplied boundary selector.
  `Context::summary_source` includes an existing summary/recovery handoff.
  `src/agent/context/recovery.rs` owns the existing summary boundary selectors;
  `src/agent/context/projection.rs` owns summary-source/model-view construction.
  The automatic model-target selector is not sufficient to promise useful
  manual compaction below the target ratio.
- `src/agent/runtime/compaction_updates.rs` publishes application notices,
  markers, failures, and estimates. Runtime sweeps apply persisted snapshots
  and can trigger recovery after failed summaries. Outcomes carry generation
  and request IDs, and activity is scoped to root or subagent run ID.
- `AgentCommand::ForChild` forwards only to a running child's sender; a stale
  target currently logs and drops the command. Root-only manual compaction
  does not require changing that forwarding contract.
- `src/presenter/keyboard.rs` checks suggestions before ordinary child-view
  Enter suppression. `src/tui_driver/updates.rs` handles lazy persisted child
  replay, and `src/tui_reducer_state/conversation/replay.rs` rebuilds bubbles.
- `docs/architecture-design-records/` contains only `.gitkeep` at planning time.
  No recorded ADR constrains these changes.
- `docs/known-test-issues.md` documents subscriber/cwd isolation, asynchronous
  writer lock release, integration-test side effects, bounded UI-channel
  draining, and observable TUI completion. Existing test helpers and file
  ownership are catalogued in `docs/development-workflow.md`.
