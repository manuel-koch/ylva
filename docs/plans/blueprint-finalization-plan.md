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

- Cache approximate token estimates. Use `tiktoken-rs` for OpenAI-compatible
  models and a four-characters-per-token estimate for Ollama. These estimates
  guide context budgeting; they are not billed usage.
- Start background summarization after a completed turn at 70% of the active
  engine's context length. Apply it at the next safe boundary. At 90%, use a
  synchronous drop fallback. The target is 50% by default and
  `compact_target_ratio` is configurable per engine.
- Use `compact_engine` to summarize an immutable old-prefix snapshot. Keep the
  full transcript and append a compaction snapshot event to the same JSONL
  session. Newer events supersede earlier snapshots through a monotonically
  advancing message boundary.
- Keep the full transcript visible in the TUI and show a compaction marker.
  The model receives the marked summary and the verbatim tail.
- Compact root conversations only. Do not add a manual `/compact` command.
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
visible token usage and cost totals; additional providers, skills, TUI polish,
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

1. [ ] Add the per-agent tool-use iteration limit.
   - [ ] Add optional `max_tool_iterations` to `AgentConfig` and
         `AgentConfigOverrides`, and apply inline profile overrides to
         file-authored TOML/YAML agent definitions.
   - [ ] Treat an absent value as 50 and reject zero during profile
         validation. Preserve the same effective limit when creating a root
         loop or a child loop.
   - [ ] Pass the effective limit into the shared iteration guard in
         `src/agent.rs`; use it in the child budget message and error text.
   - [ ] Add focused config tests for defaults, overrides, file formats, and
         zero rejection. Extend `tests/tools_agent_loop.rs` to verify custom
         root and child limits while retaining the existing default-50 guard
         coverage.

2. [ ] Add root agent selection and persist it across resume.
   - [ ] Add `--agent <type>` to `Cli` and pass the selected type through
         `src/main.rs` to both `TuiApp` and `ConsoleApp`.
   - [ ] Resolve the selected type before choosing the initial engine and
         writing session metadata. An omitted flag on a fresh session selects
         `default`.
   - [ ] Add an optional `agent_type` field to `SessionMeta`. Record it when
         creating a session and when `/clear` starts a replacement session.
         Read old metadata without the field as `default`.
   - [ ] On resume, use explicit `--agent` first and otherwise restore the
         latest stored type. Append an `AgentTypeChanged` event when an
         explicit override changes the active type so later resumes keep the
         selection. Fail at startup when the effective type is absent from the
         selected profile; an explicit flag naming an available type resolves
         a stale stored name and is persisted.
   - [ ] Add spawned-binary tests in `tests/cli_profile.rs` for valid and
         invalid types and the explicit override. Extend
         `tests/store_persistence_e2e.rs` to assert the metadata value,
         defaulting of legacy sessions, restoration on resume, and persistence
         of an explicit type override.

3. [ ] Add tiered context state and cached token estimates.
   - [ ] Refactor `Context` so committed transcript messages remain the
         durable/UI record while the agent can build a separate model-facing
         view. Keep the outer agent loop as the only writer.
   - [ ] Add context tiers for the initial system instructions, stable initial
         context, recent turns, and compacted prefix. Preserve the leading
         system prompt and keep the recent tail verbatim.
   - [ ] Add `tiktoken-rs` and cache an approximate token count when a message
         enters the context. Use the selected estimator for OpenAI-compatible
         models and the four-characters-per-token heuristic for Ollama.
   - [ ] Add unit tests for count calculation, cached updates, tier ordering,
         and the effective context length supplied by the active engine.

4. [ ] Implement automatic summary compaction and the hard-limit fallback.
   - [ ] Use the profile's `compact_engine` for a tool-free summary of an
         immutable prefix snapshot. Start summarization after a completed turn
         at 70% usage and apply the result at the next safe boundary.
   - [ ] Add `compact_target_ratio` to `EngineConfig`, defaulting to `0.5`.
         Keep the 90% hard trigger fixed and re-evaluate after `/models` changes
         the active context length.
   - [ ] At 90%, or when summary generation fails or times out, synchronously
         remove oversized old tool results or oldest complete turns until the
         target is met. Never split a tool-call/result pair or compact while
         the serial tool queue is pending.
   - [ ] Add deterministic tests with mock summary engines for proactive
         scheduling, safe-boundary application, threshold switching, failure
         fallback, and result/turn boundary preservation.

5. [ ] Persist compaction snapshots and restore them without changing the raw
   transcript.
   - [ ] Add a versioned `StoreEvent::Compaction` containing a monotonically
         advancing message boundary and the summary for that prefix. Keep the
         session append-only; a newer snapshot supersedes an earlier one.
   - [ ] Add a store read path that returns the full event stream, messages,
         and compaction snapshots. On resume, rebuild the model view from the
         latest snapshot plus the verbatim tail while leaving all original
         events available to the TUI and clone path. Do not add a
         mid-conversation `system` message; use a marked, provider-safe
         projection for the summary.
   - [ ] Add store and context tests for event round-tripping, repeated
         monotonic snapshots, legacy files without compaction events, resume
         without duplicate writes, and preserved tool-call/result pairing.

6. [ ] Add the session-clone CLI option.
   - [ ] Add `--clone` and require `--conversation-id <uuid>`. Without
         `--clone`, preserve today's resume-and-append behavior. With it,
         create a new session UUID in both TUI and headless modes.
   - [ ] Require the source to be a top-level session. Acquire the existing
         nonblocking exclusive lock while reading it. Fail without creating a
         clone if it is a child transcript or another Ylva process holds its
         writer lock; never append to or repair the source file.
   - [ ] Copy all logical source events except the old `Meta` record, including
         messages, model/agent selection changes, compaction snapshots, and
         child-run lifecycle records. Preserve child run IDs and use their
         transcript files as shared read-only references.
   - [ ] Write a new top-level `Meta` record with a new session ID and
         `created_at`, `parent_session = None`, `cloned_from` source ID, and
         copied title/provider/model/initial-agent fields. Rebase copied
         parent lifecycle records to the new session ID without changing the
         source or child transcript files. Apply any explicit `--agent`
         override only to the clone.
   - [ ] Add tests proving that the source file remains byte-for-byte
         unchanged, the clone gets a distinct ID and complete replay state,
         new messages append only to the clone, and cloning a child session or
         locked source fails cleanly.

7. [ ] Show compaction boundaries in the TUI and verify resumed presentation.
   - [ ] Keep rendering the full transcript and add a clear non-message marker
         at each boundary. Do not add a visible token count or cost display in
         this work.
   - [ ] Extend TUI replay tests to verify the marker appears in the right
         position and the original transcript remains browsable before and
         after resume.

8. [ ] Update documentation and run the required checks.
   - [ ] Update `README.md`, `CONTEXT.md`, `docs/blueprint.md`,
         `docs/features.md`, `docs/configuration.md`, `docs/cli-options.md`,
         `docs/persistence.md`, `docs/implementation-notes.md`,
         `docs/ideas/conversation-compaction.md`, and `docs/ideas/session.md`
         with the implemented context, resume, `--agent`, per-agent limit, and
         clone-session contracts. Keep visible usage and billing features
         marked as deferred.
   - [ ] Keep `docs/ideas/tool-result-size-guard.md` as the later per-result
         wire-guard and `glob` work; do not add the unselected 8–16 KiB
         insertion cap.
   - [ ] Run `rumdl fmt` and the Markdown linter on changed Markdown files.
   - [ ] Run `make format lint` and `make test-coverage` as the required code
         verification gate.

## Implementation Hints

- `src/agent.rs` owns `Context`, `Message`, `Context::set_store`, hydration,
  request snapshots, and the shared `bump_and_check_iteration_cap`. The current
  guard is `MAX_TOOL_USE_ITERATIONS = 50`; child budget text also uses this
  constant.
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
  (`128_000`). `ProfileConfig::compact_engine_name()` already falls back to
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
