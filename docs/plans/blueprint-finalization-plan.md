# Blueprint finalization: context management, agent controls, and session cloning

## Context

Ylva is a Rust TUI and headless agent harness. The core agent loop, provider
transport, tools, session persistence, profile layering, and child-agent
lifecycle are implemented. The remaining work selected for this plan is to
bound long-running model context, expose configured agent types at the root,
and make the tool-use iteration limit configurable per agent type.

The implementation must let long sessions continue without losing the user's
full transcript. It must also preserve the model-provider tool-call protocol:
each issued `tool_call_id` receives exactly one result before later user
content enters the model context. A steer during a pending tool sequence stays
queued; users interrupt that work through the existing TUI interrupt controls.

The decisions for this plan are:

- Cache approximate token estimates. Use `tiktoken-rs` `cl100k_base` for all
  models. These estimates guide context budgeting; they are not billed usage or
  model-specific tokenizer accuracy.
- Start background summarization after a completed turn at 70% of the active
  engine's context length. Apply it at the next safe boundary. At 90%, use a
  synchronous drop fallback. All three ratios are per-engine knobs with those
  defaults: `compact_target_ratio` (`0.5`), `soft_compaction_threshold`
  (`0.7`), and `hard_compaction_threshold` (`0.9`), validated at profile load
  to stay in `(0, 1]` and in ascending order.
  **Superseded recovery policy:** [refine-compaction-plan](./refine-compaction-plan.md)
  adds mid-batch scheduling, bounded continuation waiting, staged model-only
  projections and a protected task anchor. The original drop-only fallback must
  not be used to construct a system-only continuation.
- Use `[compaction].engine` to summarize an immutable old-prefix snapshot. Keep the
  full transcript and append a compaction snapshot event to the same JSONL
  session. Newer events supersede earlier snapshots through a monotonically
  advancing message boundary.
- Keep the full transcript visible in the TUI and show a compaction marker.
  The model receives the marked summary and the verbatim tail.
- Write the summary prompt from a user-overridable `COMPACTION.md`, resolved
  home then workspace like `SOUL.md`, falling back to the built-in prompt when
  neither exists.
- Show the estimated serialized-request size and its fill of the active
  window in the footer (`64.2K (45%)`). After compaction changes the
  model view, show its separate estimate (`9.9K (10%)`) until another
  request starts. Cost and billing totals stay deferred.
- Compact root and child agent contexts with their resolved compaction settings.
  Do not add a manual `/compact` command.
- Add per-agent `max_tool_iterations`, default `50`, used by root and child
  loops. Reject zero.
- Add `--agent <type>` for both TUI and headless runs. Persist the selected
  root type in new session metadata. On resume, an explicit `--agent` wins and
  is persisted as the latest selection; otherwise restore the stored type.
  Older sessions without this field use `default`. A named type missing from
  the loaded configuration is a startup error unless the user explicitly
  selects an available type.
- Add `--clone` to `--conversation-id <uuid>` in both TUI and headless modes.
  Clone only top-level sessions. A clone gets a new UUID and `cloned_from`
  metadata, copies the source title and full logical event stream, and receives
  all new writes. Fail if the source session is locked. Reuse child transcript
  files as read-only history.
- Do not create an ADR for this initial implementation.

The following work is deferred to existing or newly consolidated idea notes:
the 512 KiB per-result wire guard and bounded `glob`; JSON output, quiet mode,
output stream separation, buffering, and the CLI exit-code redesign; one-off
`--provider` and `--model` overrides; session deletion and file compression;
cost totals and billing output; additional providers, skills, TUI polish,
large-message rendering work, and expanded subagent concurrency or
communication. The file-tree sidebar and pending-approval steer override are
dropped. Spike 1's ten-minute memory soak and steering criteria are dropped;
production queued steering remains covered by `tests/steering.rs`.

Acceptance criteria:

- The model-facing context compacts before reaching the active model's hard
  limit, with a synchronous fallback if background summarization is too slow
  or fails.
- Every original user, assistant, and tool message remains in the persisted
  transcript and is visible after resume. The latest compaction event restores
  the same model-facing summary and tail.
- Compaction never splits a tool-call/result group and never mutates `Context`
  from a background task.
- `--agent` selects a configured root type in TUI and headless modes, restores
  it on resume, and persists an explicit override as the latest selection.
- `--conversation-id <uuid> --clone` creates a new session containing the
  top-level source conversation state. The source JSONL file remains
  unchanged; future messages append only to the clone. Child transcript IDs
  are not valid clone sources.
- `max_tool_iterations` is configurable per agent type, defaults to 50 for
  root and child loops, and rejects zero.
- The implementation updates all affected documentation and passes the
  required formatting, lint, and coverage checks.

## Implementation Plan

The implementing agent must change each `[ ]` to `[x]` immediately after
completing that step and rewrite this file in place so progress can resume in a
later session.

1. [x] Add the per-agent tool-use iteration limit.
   - [x] Add optional `max_tool_iterations` to `AgentConfig` and
         `AgentConfigOverrides`, and apply inline profile overrides to
         file-authored TOML/YAML agent definitions.
   - [x] Treat an absent value as 50 and reject zero during profile
         validation. Preserve the same effective limit when creating a root
         loop or a child loop.
   - [x] Pass the effective limit into the shared iteration guard in
         `src/agent.rs`; use it in the child budget message and error text.
   - [x] Add focused config tests for defaults, overrides, file formats, and
         zero rejection. Extend `tests/tools_agent_loop.rs` to verify custom
         root and child limits while retaining the existing default-50 guard
         coverage.

2. [x] Add root agent selection and persist it across resume.
   - [x] Add `--agent <type>` to `Cli` and pass the selected type through
         `src/main.rs` to both `TuiApp` and `ConsoleApp`.
   - [x] Resolve the selected type before choosing the initial engine and
         writing session metadata. An omitted flag on a fresh session selects
         `default`.
   - [x] Add an optional `agent_type` field to `SessionMeta`. Record it when
         creating a session and when `/clear` starts a replacement session.
         Read old metadata without the field as `default`.
   - [x] On resume, use explicit `--agent` first and otherwise restore the
         latest stored type. Append an `AgentTypeChanged` event when an
         explicit override changes the active type so later resumes keep the
         selection. Fail at startup when the effective type is absent from the
         selected profile; an explicit flag naming an available type resolves
         a stale stored name and is persisted.
   - [x] Add spawned-binary tests in `tests/cli_profile.rs` for valid and
         invalid types and the explicit override. Extend
         `tests/store_persistence_e2e.rs` to assert the metadata value,
         defaulting of legacy sessions, restoration on resume, and persistence
         of an explicit type override.

3. [x] Add tiered context state and cached token estimates.
   - [x] Refactor `Context` so committed transcript messages remain the
         durable/UI record while the agent can build a separate model-facing
         view. Keep the outer agent loop as the only writer.
   - [x] Add context tiers for the initial system instructions, stable initial
         context, recent turns, and compacted prefix. Preserve the leading
         system prompt and keep the recent tail verbatim.
   - [x] Add `tiktoken-rs` and cache an approximate token count when a message
         enters the context. Use the selected estimator for OpenAI-compatible
         models and the four-characters-per-token heuristic for Ollama.
   - [x] Add unit tests for count calculation, cached updates, tier ordering,
         and the effective context length supplied by the active engine.

4. [x] Implement automatic summary compaction and the hard-limit fallback.
   - [x] Use the profile's `[compaction].engine` for a tool-free summary of an
         immutable prefix snapshot. Start summarization after a completed turn
         at 70% usage and apply the result at the next safe boundary.
   - [x] Add `compact_target_ratio` to `EngineConfig`, defaulting to `0.5`,
         and make the triggers per-engine knobs too:
         `soft_compaction_threshold` (default `0.7`) and
         `hard_compaction_threshold` (default `0.9`). Validate at profile load
         that each ratio is in `(0, 1]` and that the target sits below the soft
         trigger and the soft trigger below the hard one. Re-read all three
         from the active engine on every sweep, so a `/models` switch
         re-evaluates the budget.
   - [x] At 90%, or when summary generation fails or times out, synchronously
         remove oversized old tool results or oldest complete turns until the
         target is met. Never split a tool-call/result pair or compact while
         the serial tool queue is pending.
   - [x] Add deterministic tests with mock summary engines for proactive
         scheduling, safe-boundary application, threshold switching, failure
         fallback, and result/turn boundary preservation.

5. [x] Persist compaction snapshots and restore them without changing the raw
   transcript.
   - [x] Add a versioned `StoreEvent::Compaction` containing a monotonically
         advancing message boundary and the summary for that prefix. Keep the
         session append-only; a newer snapshot supersedes an earlier one.
   - [x] Add a store read path that returns the full event stream, messages,
         and compaction snapshots (`Store::read_session_state` +
         `SessionState`). On resume, rebuild the model view from the
         latest snapshot plus the verbatim tail while leaving all original
         events available to the TUI and clone path. Do not add a
         mid-conversation `system` message; use a marked, provider-safe
         projection for the summary.
   - [x] Add store and context tests for event round-tripping, repeated
         monotonic snapshots, legacy files without compaction events, resume
         without duplicate writes, and preserved tool-call/result pairing.

6. [x] Write the compaction summary prompt from `COMPACTION.md`.
   - [x] Resolve `COMPACTION.md` like `SOUL.md`: workspace file first, then
         `~/.ylva/COMPACTION.md`, then the built-in prompt that ships with
         Ylva. Load it alongside the other layered instructions when the root
         loop is built and hand it to the compaction runtime.
   - [x] Keep the request contract unchanged: the prompt frames an immutable
         prefix snapshot, the summarizer stays tool-free, and the response is
         a single concise paragraph preserving decisions, open work, file
         paths, identifiers, user constraints, and the boundary before the
         verbatim tail.
   - [x] Add loader tests for workspace override, home fallback, built-in
         fallback, and a prompt whose text reaches `build_summary_request`.

7. [x] Show context usage in the TUI footer, identifying request and
       post-compaction model-view estimates separately.
   - [x] Emit the estimated serialized request token count and its fill of the
         active window as `UiUpdate::ContextUsage` when each request starts,
         map it through `DomainEvent::ContextUsage` to
         `AppState::context_usage`, and render it right-aligned on the footer's
         key-hints row as `64.2K (45%)`.
   - [x] Unit-test the number formatting, the driver-to-state routing, the
         right-aligned rendering, and the loop's emission; cost and billing
         totals remain deferred.

8. [x] Add the session-clone CLI option.
   - [x] Add `--clone` and require `--conversation-id <uuid>`. Without
         `--clone`, preserve today's resume-and-append behavior. With it,
         create a new session UUID in both TUI and headless modes.
   - [x] Require the source to be a top-level session. Acquire the existing
         nonblocking exclusive lock while reading it. Fail without creating a
         clone if it is a child transcript or another Ylva process holds its
         writer lock; never append to or repair the source file.
   - [x] Copy all logical source events except the old `Meta` record, including
         messages, model/agent selection changes, compaction snapshots, and
         child-run lifecycle records. Preserve child run IDs and use their
         transcript files as shared read-only references.
   - [x] Write a new top-level `Meta` record with a new session ID and
         `created_at`, `parent_session = None`, `cloned_from` source ID, and
         copied title/provider/model/initial-agent fields. Rebase copied
         parent lifecycle records to the new session ID without changing the
         source or child transcript files. Apply any explicit `--agent`
         override only to the clone.
   - [x] Add tests proving that the source file remains byte-for-byte
         unchanged, the clone gets a distinct ID and complete replay state,
         new messages append only to the clone, and cloning a child session or
         locked source fails cleanly.

9. [x] Show compaction boundaries in the TUI and verify resumed presentation.
   - [x] Keep rendering the full transcript and add a clear non-message marker
         at each boundary. The footer's context-usage readout (step 7) is the
         only usage display; cost and billing totals stay out of scope.
   - [x] Extend TUI replay tests to verify the marker appears in the right
         position and the original transcript remains browsable before and
         after resume.

10. [x] Capture per-request token estimate accuracy in an opt-in dump.
    - [x] Use `tiktoken-rs` `cl100k_base` as the local estimate for every model
          and retain the estimate made for the exact model request before it is
          sent. Do not treat the estimate as provider usage or as model-specific
          tokenizer accuracy.
    - [x] Enable and parse provider-reported prompt/input token usage where
          supported by the existing request stream. Associate the observation
          with the estimate for that same request, including separate requests
          within a tool-using turn. Preserve missing usage as unavailable.
    - [x] Add a dedicated JSONL dump controlled by
          `YLVA_TOKEN_USAGE_DUMP_PATH`. Write one metadata-only record per model
          request, with a request correlation ID, provider, model, estimate,
          estimate source, and reported input-token count/source when present.
          Do not write prompts, message contents, credentials, or other request
          payloads. Keep this output independent of HTTP request dumps and
          session persistence.
    - [x] Add tests for estimate/usage pairing, multiple model requests in one
          turn, providers or interrupted streams without usage, opt-in path
          behavior, JSONL output, and exclusion of conversation content.

11. [x] Update documentation and run the required checks.
    - [x] Update `README.md`, `CONTEXT.md`, `docs/blueprint.md`,
          `docs/features.md`, `docs/configuration.md`, `docs/cli-options.md`,
          `docs/persistence.md`, `docs/implementation-notes.md`,
          `docs/ideas/conversation-compaction.md`, and `docs/ideas/session.md`
          with the implemented context, resume, `--agent`, per-agent limit, and
          clone-session contracts. Keep visible usage and billing features
          marked as deferred.
    - [x] Update `docs/blueprint.md`, `CONTEXT.md`, `docs/cli-options.md`,
          `docs/features.md`, `docs/implementation-notes.md`, and
          `docs/ideas/conversation-compaction.md` with the per-request estimate,
          provider-usage observations, opt-in variable, and metadata-only data
          contract.
    - [x] Keep `docs/ideas/tool-result-size-guard.md` as the later per-result
          wire-guard and `glob` work; do not add the unselected 8–16 KiB
          insertion cap.
    - [x] Run `rumdl fmt` and the Markdown linter on changed Markdown files.
    - [x] Run `make format lint` and `make test-coverage` as the required code
          verification gate.

## Implementation Hints

- `src/agent.rs` owns `Context`, `Message`, `Context::set_store`, hydration,
  request snapshots, and the shared `bump_and_check_iteration_cap`. The
  fallback limit is `DEFAULT_MAX_TOOL_USE_ITERATIONS = 50`; child budget text
  also uses this constant when an agent type configures no override.
- `src/config/agent.rs` defines `AgentConfig`. `src/config/agent_type_file.rs`
  deserializes both TOML and YAML frontmatter into that same type.
  `src/config/profile.rs` merges file definitions with inline
  `[agents.<type>]` overrides.
- `src/agent_factory.rs` builds both root and child `AgentLoop`s from the
  selected agent type. It validates configured engine references at startup,
  but an unknown requested root type currently falls back to default-like
  behavior and must become an error.
- `src/cli.rs` has no `agent` field, and `src/main.rs` currently passes
  `"default"` to both app drivers. `main.rs` also records the profile's
  `default_engine` as the new session's initial engine, so this must follow the
  selected agent's engine.
- `src/store.rs` writes tagged JSONL `StoreEvent`s. `SessionMeta` currently has
  no root agent type or `cloned_from` field; `StoreEvent::ModelChanged` is the
  existing append-only pattern for runtime selection changes.
  `Store::read_messages` ignores non-message events, and `Context::set_store`
  uses it to hydrate the model context. The TUI separately reads messages in
  `src/tui_driver.rs` and projects them through `AppState::replay_messages`.
  Cloning requires a full-event read/copy path, not `read_messages`.
- Session files are append-only. `schema_version` is reserved for whole-file
  semantic breaks; event records already carry their own version and tolerate
  unknown fields. No ADR is requested for this change.
- `EngineConfig::effective_context_length` in `src/llm/engine.rs` resolves the
  configured value, then provider catalog value, then `DEFAULT_CONTEXT_LENGTH`
  (`128_000`). `ProfileConfig::compaction_engine_name()` falls back to
  `default_engine`.
- Existing focused tests include
  `iteration_guard_stops_after_max_reprompts` in
  `tests/tools_agent_loop.rs`, root steering coverage in `tests/steering.rs`,
  profile startup errors in `tests/cli_profile.rs`, session resume coverage in
  `tests/store_persistence_e2e.rs`, context hydration tests in `src/agent.rs`,
  store event tests in `src/store.rs`, and TUI replay tests in
  `tests/tui_render_state.rs`.
- First focused baseline: `cargo test --test tools_agent_loop iteration_guard_stops_after_max_reprompts`.
- Check [known test issues](../known-test-issues.md) before adding tests that
  mutate cwd, capture tracing, or have integration-test side effects.

## Completed follow-up: token estimate calibration

The compaction implementation uses approximate counts. This follow-up uses
`tiktoken-rs` `cl100k_base` as a consistent guideline across models and collects
provider-reported input usage to measure how well that estimate works in
practice. The estimate remains advisory and is not billed usage.

1. [x] Use `cl100k_base` for every model and preserve the estimate calculated
   for each exact model request before sending it. Keep estimate data paired to
   requests, not only to user turns, because tool use can send multiple model
   requests in one turn.
2. [x] Enable and parse provider-reported prompt/input token usage where the
   existing request stream supports it. Pair each observation with that
   request's pre-send estimate. Represent missing usage as unavailable, not
   zero, including when a stream is interrupted or a provider omits usage.
3. [x] Add an opt-in, dedicated JSONL statistics dump controlled by
   `YLVA_TOKEN_USAGE_DUMP_PATH`. Write one metadata-only record per model
   request to one growing file, with no rotation for now. Include a request
   correlation ID, provider, model, estimate, reported input-token count when
   present, and the usage source. Do not write prompts, message contents,
   credentials, or request payloads. Keep this file separate from HTTP request
   dumps and session persistence.
4. [x] Test per-request estimate/usage pairing, multiple model requests in one
   turn, missing provider usage, the opt-in path behavior, JSONL records, and
   that records contain no conversation content.
5. [x] Update `docs/ideas/conversation-compaction.md`, `CONTEXT.md`,
   `docs/cli-options.md`, `docs/features.md`, and
   `docs/implementation-notes.md` with the estimator baseline, provider usage
   observations, environment variable, and data contract.
6. [x] Run `rumdl fmt` and the Markdown linter on changed Markdown files, then
   run `make format lint` and `make test-coverage`.
7. [x] Update the footer from the estimate of the exact serialized request
   before waiting for the provider response. Keep this readout separate from
   the model-view estimate used by compaction, and update the feature and
   implementation docs to state when each estimate is reported.
