# Refine conversation compaction

## Context

Make root and subagent conversation compaction robust against large tool batches,
summary failures, and destructive hard-limit recovery. This is an implementation
plan, not authorization to implement or commit changes.

The motivating ten-fetch incident and its expected refined behavior are recorded
in [conversation-compaction](../ideas/conversation-compaction.md#ten-fetch-incident-and-refined-behavior),
the single owner of that write-up. Generated deterministic fixtures reproduce the
growth and failed-summary patterns without copying sensitive dump content.

The user confirmed the following design through a grilling interview:

- Prefer continued execution, including explicitly disclosed lossy recovery,
  over stopping merely because a summary failed or timed out.
- Preserve the latest explicit user request verbatim when feasible, a bounded
  existing summary/handoff, and the newest complete tool batch where it fits.
  Only shorten protected content when necessary, with explicit omission markers.
- Budget all potentially large textual tool results across the batch, not only
  `web_fetch`. Binary attachments remain whole or are omitted/rejected explicitly.
- Start background summaries of completed immutable prefixes while later tools
  run. Apply snapshots only at safe boundaries; allow one summary request per run.
- At a hard-limit request boundary, wait for an already-running summary within
  its remaining existing timeout, then recover lossily if necessary.
- Keep the existing advisory estimator, context window, ratios, 10% reply reserve,
  and 5% safety margin. Introduce no tokenizer changes or new configuration knobs.
- Preserve full committed transcripts. Persisted compaction state may change
  format freely: no backward-compatible loading of older compaction records is
  required.
- Fail explicitly only when no valid bounded continuation request can fit.

The subsequent refinement adds budget-aware admission of queued calls and a
uniform six-call batch-count backstop. Crossing the soft trigger starts summarization; it
does not by itself cancel tools. Pause not-yet-started work only when its bounded
result allowance cannot fit. Already-running tools normally finish because
cancellation can leave partial side effects. Smaller model-generated batches
create more protocol boundaries; executing one existing batch in smaller chunks
does not.

A second review reordered the work: the incident's root cause (recovery leaving
a system-only request) is fixed first by a minimal change; heavier machinery
(async scheduling, gating, persisted projections) follows only as far as
needed. No ADR is created for this change.

### Acceptance criteria

- A batch that crosses the soft threshold starts a background summary before its
  queue drains when a completed eligible prefix exists. No snapshot includes
  unmatched tool calls or mutates context while tools run.
- No continuation of an active task is sent with only system messages or only an
  omission notice. A task anchor remains, with disclosed shortening if necessary.
- The budget accounts for projected messages, tool schemas, result envelopes,
  omission markers, and available multimodal accounting. Text estimates remain
  advisory; no exact provider-token guarantee is claimed.
- Every dispatched continuation fits the shared local safety ceiling. The
  compaction target is best-effort when meeting it would destroy continuity.
- Summary failure, timeout, oversized output, or insufficient reduction cannot
  trigger an unbounded retry/wait cycle or silently erase the task.
- Tool-call IDs and their single matching results/errors remain valid after all
  projections. Binary payloads are never text-truncated into corrupt attachments.
- Every queued call is admitted against enforced result allowances and reserved
  response framing. Unknown result sizes are not predicted solely from earlier
  results: a call is admitted only if the remaining allowance divided by the
  number of unresolved calls reaches a minimum useful result size, otherwise it
  is skipped. Soft-threshold crossing alone does not abort execution.
- Budget pauses are bounded and remain responsive to stop/clear. If pending calls
  cannot be admitted, each receives exactly one explicit not-executed tool error
  before any continuation request; no unmatched calls or boundary deadlocks remain.
- The batch-count backstop is enforced by the harness as well as model guidance.
  Excess calls are visibly skipped, not silently dropped or automatically retried.
  Admitted calls retain their original order and ordinary approval requirements.
  Only the first six requested positions are eligible, uniformly across tools
  and root/subagent runs. Invalid or denied slots are not backfilled; token
  admission may reduce execution further. Repeated oversized batches cannot
  increase the cap or bypass the existing tool-use iteration limit. Ordering
  dependencies between calls remain the model's responsibility: a later call
  may run although an earlier one was skipped.
- Skipped-call errors state that the call was not executed and why, and ask the
  model to resubmit only skipped calls in smaller batches. A resubmitted call is
  executed at most once per request.
- Original committed messages/results remain browsable and unchanged. Any
  persisted recovery state restores the model view on resume/clone without
  synthetic transcript duplication.
- Root and child runs share the policy; stop, clear, model switching, steers, and
  late outcomes cannot apply stale state or clear a newer activity indicator.
- Deterministic tests reproduce the ten-fetch failure pattern without network
  access and verify the exact next model request, not just a smaller token count.
- One compact log line per compaction records trigger, reduction stage reached,
  and before/after estimates.
- Affected documentation is current; repository verification gates pass with at
  least the configured 80% line coverage.

## Implementation Plan

Execute steps in order. Flip each `[ ]` to `[x]` immediately after completing
the step and rewrite this file in place so another session can resume the work.
Each step includes focused tests and its directly affected documentation; step 11
reconciles the complete documentation set. Step 9 is conditional: skip it, with
a note, if earlier steps already satisfy the acceptance criteria (YAGNI).

1. [x] Establish deterministic regressions before changing behavior.
   Start with `src/agent.rs`, `src/agent/compaction.rs`, and
   `tests/tools_agent_loop.rs`. Run `cargo test --lib compaction` for the existing
   baseline. Add a mock-engine, gated-tool batch reproducing the observed jump
   from below soft to above hard, and a separate failed-summary regression;
   assert the captured next request is not system-only, keeps the task anchor,
   and has correctly paired tool responses. Assertions for behavior delivered
   by later steps (summary starting before the last tool completes) may be
   added and marked ignored until their step lands.
   Use smaller controlled windows and generated payloads rather than copying
   sensitive dump contents.
   Add execution-counter regressions for budget admission, excess batch calls,
   and side-effecting tools: skipped calls never execute, admitted calls execute
   once in order, and already-running calls are not cancelled by soft pressure.

2. [x] Minimal incident fix: never recover to a system-only request.
   In `DropFallback` / `drop_model_view_to`, always keep the newest explicit
   user message (root) or briefing plus newest applicable steer (child), plus a
   deterministic handoff built only from the request, any prior summary, and
   actual tool outcomes (never fabricated conclusions), with an explicit lossy
   marker. Extend `tool_output_token_limit` to `web_fetch`, `web_search`, and
   `bash` so the existing runtime allowance applies to them; keep tool-local
   ceilings and metadata/envelopes intact. Step 1's system-only regressions
   must pass after this step. Update the recovery description in
   `docs/features.md`.

3. [x] Define one shared budgeting and continuation policy.
   Consolidate budgeting in the existing context/compaction components rather
   than adding per-tool policy copies. Reuse the current reply reserve, safety
   margin, and fixed overhead allowance; include projected result framing and
   marker costs. Keep target, soft trigger, hard trigger, and safe request ceiling
   distinct. Derive the batch allowance after protected content, schemas, and
   reserves; distribute it across unresolved textual results and recompute after
   each completion. Reserve minimal error-response framing for every requested
   call, including calls excluded by admission or the count cap. Use enforced
   projected output ceilings rather than speculative future result estimates.
   Cover zero allowance, tiny windows, large schemas, UTF-8,
   very large arguments/errors, and a model switch that shrinks the window.
   Update budget terminology in `CONTEXT.md` and `docs/implementation-notes.md`.

4. [x] Budget all large textual results using the shared policy.
   Wire result admission through the common agent result/error path, covering
   `web_fetch`, `web_search`, `bash`, file tools, child reports, and partial error
   output without damaging structured metadata or tool envelopes. Reuse existing
   tool-local ceilings and token-limiting helpers, distinguishing source-level
   truncation from model-view reduction. Do not reduce the canonical committed
   output merely to satisfy the context budget. Preserve original data already
   limited by tool/network safety caps; this plan does not require unbounded
   fetching or reconstructing data that a tool never returned.
   Preserve complete binary descriptors/payloads together, or substitute an
   explicit valid textual result with no attachment in the model view.
   Test an entire batch of individually acceptable but collectively oversized
   results, long error bodies, envelope validity, and attachment projection.
   Update result-limit contracts in `docs/features.md`.

5. [x] Add queued-call admission and a batch-count backstop.
   In the common tool assembly/queue lifecycle in `src/agent.rs`, check the
   remaining shared allowance before starting each call. At the soft trigger,
   schedule a useful summary (once step 6 lands) and continue calls whose bounded
   result projections fit; do not cancel already-running tools solely for context
   pressure. When allowance is insufficient, pause queued work only for a useful
   in-flight older-prefix summary within its remaining deadline. Apply a ready
   snapshot only where the existing safe-boundary contract permits it; otherwise
   finish the current group with explicit not-executed errors before applying
   recovery. Never wait for a queue-empty boundary while indefinitely withholding
   the outcomes needed to reach it. If no useful summary can run or admit more
   work, resolve skipped calls and proceed to bounded recovery.
   Preserve original call order, approvals, and completed outcomes. Give each
   skipped call one matching tool error explaining that it was not executed due
   to context budget or batch limit and asking for resubmission of only skipped
   calls in smaller batches; keep this distinct from failure after actual
   execution. Do not automatically retry skipped or partially executed operations.
   Add model guidance requesting small batches and enforce a named code-owned
   count cap of six, without a new configuration knob. Only the first six
   requested positions are eligible across all tools, including writes and
   `spawn_subagent`, and both root and subagent runs. Do not backfill invalid,
   denied, or budget-skipped positions with later calls. Keep child concurrency
   limits separate. The cap never increases adaptively.
   Execute only the ordered admitted subset; resolve every excess requested call
   explicitly. Count and token limits are independent safeguards, and batching
   execution internally does not authorize splitting an assistant message into
   invented protocol boundaries.
   Test sufficient-budget progress, bounded pauses, summary timeout/no eligible
   prefix, batches of 0/1/6/7/10 calls, denied/invalid early slots without
   backfill, mixed side-effecting tools (including a later write after a skipped
   earlier call), huge single results, large call arguments, repeated requests
   for skipped calls, and exactly-once side effects.
   Keep iteration limits effective: rounds with skipped calls still consume the
   existing round allowance, but skips are not reported as successful execution.
   Update `docs/features.md`, `docs/implementation-notes.md`, and the existing
   tool guidance's affected documentation.

6. [x] Separate asynchronous summary scheduling from safe-boundary application.
   After each committed tool outcome, evaluate usage and select a completed older
   prefix that excludes open tool groups and preserves the current task anchor.
   Start a summary when useful even if later tools remain queued/running; do not
   mutate the live view there. Include the existing summary as source material
   when summarizing further prefixes so successive summaries retain accumulated
   context. Skip scheduling when no eligible prefix can advance the view.
   Carry request identity, generation, and source boundary through outcomes;
   ensure an old outcome cannot overwrite newer recovery state (note
   `on_outcome` currently clears in-flight state before checking generation).
   Test in-flight deduplication, no eligible prefix, cumulative summaries, late
   outcomes after reset/recovery, and interleaved root/child activity.
   Update scheduling descriptions in `docs/features.md` and
   `docs/implementation-notes.md`.

7. [x] Gate continuation requests on bounded asynchronous recovery.
   At queue-empty and other request boundaries, first apply a ready valid summary
   and reevaluate the actual projection. If the hard threshold requires recovery
   and a summary is running, defer request dispatch while remaining responsive
   to commands and outcomes (this changes `plan_compaction`, where the hard
   threshold currently wins over an in-flight summary). Wait only for the
   remaining deadline established when that summary started, never a restarted
   timeout. On failure, timeout, invalid/empty summary, or inadequate reduction,
   proceed to step 8's bounded lossy recovery rather than repeatedly requesting
   summaries. Audit all root/child continuation paths, including steer injection
   and child completion delivery, so none bypass the gate. Test stop/clear during
   waiting, readiness before/at deadline, duplicate outcomes, model switches,
   summary growth, and preservation of tool iteration accounting. Update the
   existing compaction activity and timeout documentation.

8. [x] Replace the minimal fallback with full continuity-preserving recovery.
   Preserve leading system instructions and tool protocol validity. Keep the
   latest explicit request verbatim where feasible; retain a bounded prior
   summary/handoff and newest complete batch where possible. Use this fixed
   ordered reduction sequence, at most five stages, each required to reduce the
   estimate measurably or the next stage runs:
   (a) clip bodies of old tool results; (b) drop older complete groups;
   (c) clip bodies of retained results; (d) shorten the old summary/handoff;
   (e) shorten the user request. Every clip carries an explicit marker, and all
   markers/framing count within the ceiling. If only the target is unattainable,
   continue below the safe ceiling rather than deleting the task. If immutable
   overhead or minimal valid task/tool framing cannot fit, stop the turn with a
   visible actionable error. On a provider context-window rejection, log estimate
   versus rejection and retry at most once with extra margin applied to that
   request only (no configuration change) while retaining the anchor; otherwise
   surface the failure.
   Test absent prior summary, oversized request/summary/arguments, no-progress
   recovery, repeated compactions, exact pairing, and irreducible overhead.
   Mark the original fallback design in
   `docs/plans/blueprint-finalization-plan.md` as superseded by this refinement.
   Update recovery semantics in `docs/features.md`.

9. [x] Conditional: persist model-view projections.
   Only if resume/clone cannot already reproduce the recovered model view, extend
   `Context`, compaction snapshots, and `src/store.rs` to persist bounded result
   bodies, task anchors, and handoffs by stable transcript/message coordinates
   and tool IDs. No legacy compatibility handling or version fields beyond a
   `projection_revision` are required; older records need not load. Keep canonical
   committed bodies and display sections unchanged; apply reductions when
   constructing model messages. Bound override/handoff size and clean obsolete
   state after advancing boundaries. Support newer revisions at an unchanged
   prefix boundary. Test latest-state selection, resume, clone, changed rebuilt
   system lengths, and exact model-view restoration. Update `docs/persistence.md`
   and context mechanics documentation. If skipped, record why here.

10. [x] Expose truthful recovery notices and diagnostic evidence.
    Reuse scoped notice/activity/context-usage updates in `src/agent.rs` and their
    existing CLI/TUI presentation. Distinguish summary scheduling, waiting,
    summary application, queued-call budget pauses, not-executed calls, lossy
    projection, and irreducible failure. State that original transcript content
    remains available and identify omitted model context without implying
    successful summarization. Log per compaction one line with trigger, safe
    boundary, request identity, reduction stage reached, estimated before/after
    budget, projected message count, clipped-result count, admitted/skipped call
    counts, batch cap, remaining result allowance, and terminal reason, without
    message contents or credentials. Test ordering, root/child scoping,
    cancellation, no stale indicators, and estimates derived from the actual
    projected view. Update `docs/features.md`, `docs/implementation-notes.md`, and
    `docs/keybindings.md` only where visible state descriptions change.

11. [x] Reconcile related documentation and regression coverage.
    Update `CONTEXT.md`, `docs/features.md`, `docs/implementation-notes.md`,
    `docs/configuration.md`, `docs/persistence.md` (if step 9 ran), and
    `docs/ideas/conversation-compaction.md`, which is the single owner of the
    incident write-up and expected behavior; update `README.md` and
    `docs/keybindings.md` if their existing descriptions are affected.
    Preserve unrelated worktree edits.
    Add cross-surface tests verifying root/child behavior, resume/clone, and the
    incident-pattern continuation after successful and failed summaries.
    Use a controlled cap-four versus cap-six comparison to measure model round
    trips, skipped calls, lossy recovery, and retained task continuity. Any
    alternative cap is test-only, not a runtime adaptation or public knob.
    Correctness gates determine readiness; record measurements honestly rather
    than claiming six is optimal. A later cap change needs an explicit decision.
    Record the bounded best-effort guarantee, estimator limitations, unchanged
    configuration defaults, batch cap, skipped-call semantics, additional model
    round-trip tradeoff, and exact transcript-versus-projection distinction.
    No ADR is created; obtain consent before creating any other new document.

12. [x] Complete repository verification and mark progress accurately.
    Run focused tests while developing, then `make format lint` and the single
    mandatory full gate `make test-coverage`; do not duplicate it with
    `make test`. Run `rumdl check` on affected Markdown using `.rumdl.toml`.
    Confirm the incident regression checks the final request shape, the shared
    local ceiling, tool pairing, and preserved task continuity. Resolve failures
    before marking completion. Do not stage, branch, or commit without separate
    explicit user consent.

## Original baseline and implementation hints

The descriptions of previous behavior below are historical investigation notes,
not the current runtime contract. The completed steps and progress notes describe
the replacement policy.

- The user selected six as the uniform code-owned batch cap after considering
  four versus six. It is a pragmatic latency/safety tradeoff, not a measured
  optimum. With sufficient token allowance and a model that resubmits only
  unfinished work, ten operations need at least two batches at six versus three
  at four. Lower total cost is not guaranteed. First-six-position admission,
  no backfill, no adaptive increase, and no automatic replay were confirmed.
  Cap tests derive boundary sizes from the shared constant; the motivating
  ten-call case remains an independent regression fixture.
- `src/agent.rs` owns `Context`, `model_messages`, cached token estimates,
  `summary_boundary_for_target`, `drop_model_view_to`, result admission,
  `available_result_token_budget`, and the request/serial tool-queue lifecycle.
  `advance_tool_queue` currently sweeps only when the queue is empty.
- `tool_output_token_limit` currently selects `read`, `search`, and `glob`.
  It returns no runtime token allowance for `web_fetch`.
- `src/agent/compaction.rs` owns `plan_compaction`, `safe_boundaries_from`,
  `CompactionRuntime`, snapshot construction, timeout handling, and effectiveness
  logging. Hard-threshold evaluation currently wins over an in-flight summary.
- `safe_boundaries_from` protects a whole assistant tool-call group, not merely
  a user-to-assistant conversational turn. Result-body projection can reduce a
  retained group without splitting its protocol.
  Pausing between results does not create another boundary. Explicit matching
  errors can close skipped calls; a request with unanswered calls is not valid.
- `CompactionOutcome` currently carries generation and boundary; runtime
  activity has a request ID. `on_outcome` clears in-flight state before checking
  generation, which matters when stale/new outcomes overlap.
- `Context::model_messages` currently projects only leading system messages,
  an optional summary, and one contiguous tail. Noncontiguous preserved anchors
  and result overrides require explicit projection representation.
- Existing summary snapshots contain only boundary and summary. Store replay
  currently selects snapshots by advancing boundary; that alone cannot express
  repeated projection updates at the same boundary (relevant only to step 9).
- `src/store.rs` owns append-only compaction events, hydration state, and cloning.
  `docs/persistence.md` distinguishes live and transcript-relative boundaries.
- `src/agent/tools.rs` exposes canonical model text, separate display text,
  structured metadata, attachments, and runtime output-limit hooks.
  `src/agent/tools/web_fetch.rs` already marks source-level text truncation and
  guarantees complete recognized media. Existing backend byte caps remain valid.
- OpenAI-compatible attachment projection is provider-facing and can introduce
  synthetic wire messages. Canonical Base64, attachment descriptors, projection
  estimates, and synthetic message grouping must remain consistent.
- Explicit user messages differ from implicit tool notes and harness notices.
  Root recovery anchors to the newest explicit request; child recovery must
  retain its briefing and newest applicable user steer instead of treating a
  generated notice as the task.
- Relevant existing tests live in `src/agent.rs`, `src/agent/compaction.rs`,
  `src/store.rs`, tool modules, `tests/tools_agent_loop.rs`, and
  `tests/store_persistence_e2e.rs`. Live-provider persistence tests are distinct
  from the deterministic mock regressions required by this plan.
- `docs/development-workflow.md` defines mandatory verification, library exports
  for integration tests, and shared helpers in `tests/common.rs`.
  `docs/known-test-issues.md` documents tracing/cwd locks and asynchronous store
  lock release. Gated mock tools and channels avoid wall-clock-dependent tests.
- The architecture-decision directory contained only `.gitkeep`; the user
  decided no ADR is created for this change.
- Planning found unrelated edits in documentation and TUI source files. This
  plan does not authorize modifying or reverting those edits.

## Progress notes

All 12 steps are complete. The ten-page success/failure regressions capture the
next request, task anchor, safe ceiling and complete tool pairing, and count six
ordered executions with four explicit skips. Projection/retry/persistence and
summary-identity/deadline regressions cover the shared policy. Step 9 is required:
canonical messages cannot reconstruct bounded overrides and handoffs exactly,
so revisioned transcript-relative projection events restore resume/clone state.
The controlled cap-four/six measurements are recorded in the incident owner;
there is no public or adaptive cap knob. `make format lint` and the full
`make test-coverage` gate passed, with 92.08% line coverage. Affected Markdown
passes `rumdl check`; formatting still reports four pre-existing MD013 warnings
in unrelated `docs/backlog-issues.md` lines 40–43, which were left unchanged.
The user approved reducing the separate response-accumulation backstop from 128
to 12; the execution cap remains six. Exactly 12 calls remain processable with
six eligible positions; a thirteenth rejects the whole response before execution.
No files were staged and no branch or commit was created.
