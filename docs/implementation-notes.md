# Implementation notes

This is a contributor reference. Keep details only when code alone does not
explain them: cross-module interactions, constraints or workarounds, and design
decisions behind behavior described in [features](./features.md). Do not add
module inventories, constants or field lists; read the source.

## Session lifecycle ownership

- Session discovery/preparation runs as blocking tasks scoped by initiating
  root and picker IDs; stale results cannot publish. Durable clone outcomes
  survive cancellation and are reported explicitly.
- Switching freezes mutations through the outer loop's `PrepareSessionSwitch`
  barrier and rechecks quiescence before publication. Publication joins the
  old loop, replaces channels/engine/tools/replay, retargets the exit guard
  and drops obsolete stores. Subagent drill-in reads the *current* store (also
  after `/clear`), never a startup capture.
- The TUI keeps a *storeless* configuration factory so it does not pin the
  startup session lock after replacement; provider connection limiters stay
  shared.
- `SessionExitGuard` observes the shared current-store handle so `/clear` does
  not leave a stale startup identity; the binary prints it only after terminal
  restoration.
- `ylva profile` and `ylva session` dispatch before profile/provider
  initialization (`session` also before stdin consumption).
- Profile shims are planned as a whole before writing; duplicate claims are
  rejected and only unclaimed regular files are pruned. Shims only
  `exec ylva --profile`: a home shim inherits the caller's `YLVA_HOME`, a
  workspace shim first `cd`s to the root derived from its own path (`$0`).
  Unsafe operations exit 2 before anything is written.
- Session listing keeps the library's descending order but uses last-use
  timestamps, so roots and sibling groups print least recently used first.
  Missing parents are annotated and parent cycles produce diagnostics without
  hiding sessions.
- Locks: identity locks are independent of the raw file inode (including
  across title replacement); writer tasks hold the lock until their queue
  drains. `last_used_at` comes from the resolved transcript, never from
  metadata or mtimes. Date-based maintenance revalidates it after exclusion
  and skips a group used again since discovery. `store/archive.rs` is the
  single representation resolver for all replay readers and makes durable,
  lossless transitions.
- Tracing: `Store` owns an immutable session span; handlers snapshot it per
  async scope and spawned work instruments its own future, so rollover never
  mutates an existing span. Each admitted subagent run keeps its parent store.
- Manual compaction is a typed outer-loop command. Candidates are estimated
  before mutating/persisting the live view and must strictly reduce it. The
  presenter rejects it in a subagent view before Enter suppression, so it
  never reaches a subagent or hidden root. Request IDs stay out of
  user-facing notices.

See [persistence](persistence.md) and
[Session commands](cli-options.md#session-commands).

## Layered configuration and persona assembly

- `<ylva_…>` block delimiters do not establish trust. Recovery clips block
  bodies while reserving both tags and recognizes legacy handoff prefixes;
  existing transcript text is never rewritten.
- Unknown config keys use `serde_ignored` plus a Levenshtein suggestion from
  the flat `KNOWN_KEYS`. `SandboxRuleConfig` parses through a strict
  `deny_unknown_fields` raw struct so errors name the key.

  > **Keep `KNOWN_KEYS` in sync** when a config key is added, renamed or
  > removed; otherwise typos still warn, but without a suggestion.

- `ProviderKind` is a closed enum, so a typo fails at load.
- Request tasks use `spawn_supervised` (logs panics; the model stream also
  sends `AgentEvent::StreamError`). Layout code uses `saturating_u16`, not
  `as u16`.
- Skills: `AgentFactory` carries one immutable discovery snapshot and the
  host's approval capability across roots, subagents, `/clear` and session
  switches. One callable catalog per agent type drives prompt advertisement,
  loader membership and sidecar sandbox identities so they cannot drift.
  Subagents do not inherit the parent's filtered catalog.
- Loader approval uses recorded canonical paths without reopening sources.
  Unlike ordinary tools, an unconfigured loader *replaces* the sandbox floor
  with empty rules. Duplicate/shadowed copies never become permission grants
  or fallback sources. Sidecar access is a separate read-policy decision.
- Skill instructions enter context as ordinary tool results, not system
  messages. `requires_complete_delivery` is checked in `runtime/events.rs`
  against the live allowance (after generation checks, before insertion) to
  prevent partial delivery; it estimates the full serialized `ToolEnvelope`.
- Trusted-instruction discovery has a pure core so tests avoid mutating the
  process environment.
- `AgentFactory` validates every agent type's explicit engine reference before
  store or CLI/TUI startup, also for environment-discovered engines.
- `COMPACTION.md` resolves like `SOUL.md`. The summary request's code-owned
  framing marks message boundaries so source text is not mistaken for
  summarizer instructions. Subagent report reduction is deterministic and
  never starts an LLM request.

See [configuration](./configuration.md),
[skills](configuration.md#agent-skills) and
[skill commands](cli-options.md#skill-commands).

## Engine runtime and provider request configuration

- `EngineImpl` and `OpenAiCompatibleProvider` share one
  `EngineRuntimeConfigHandle`. The provider snapshots it before building a
  request and releases the lock before network I/O: updates affect later
  requests, never an in-flight one.
- Catalog hydration runs once per model selection before the first chat; a
  `/models` selection supplies the metadata itself and a direct model-only
  switch invalidates it. If hydration rejects a profile reasoning value, the
  engine queues a notice that `AgentLoop` forwards before `StreamStarted`.
- `ModelSelection` keeps named-engine shortcuts distinct from raw catalog
  options. Preview and confirmation share one resolution: named choices
  activate their exact map key; raw catalog choices use the active engine for
  that connection, or the sorted representative for another. The typed
  selection travels to the driver so equal provider/model pairs do not erase
  engine identity. Picker display metadata is a snapshot only; Engine runtime
  handles remain the source of request settings. Filtering matches engine
  name, connection and model ID, never formatted display text.
- `EngineConfig::context_length_for_model` scopes an override to the original
  configured model.
- `EngineImpl::configured_copy` gives explicit summarizers their original
  settings with independent request handles but shared HTTP clients and
  concurrency limits. Equal engine names do *not* imply shared settings.
- One semaphore per provider connection bounds concurrency. Catalog calls hold
  a permit until the body is read; streamed chat until the stream closes.
  Retries of `429`/`5xx` (honoring `Retry-After`) stay under the same permit.
- Provider clients use a connect timeout and an *idle* read timeout, neither
  configurable: the idle limit applies to headers and between chunks, so
  stalled servers fail while long, steadily streaming responses survive.

## Tool-result wire protocol

Each `role: "tool"` message carries JSON derived from `ToolEnvelope`:

```json
{
  "ok": true,
  "result": "...",
  "result_metadata": { "field": "value" },
  "result_metadata_schema": {
    "field": { "type": "...", "description": "..." }
  }
}
```

A failure is `{ "ok": false, "error": "..." }`. Metadata and schema are
omitted when absent; the schema is generated from the populated fields, so it
follows the data. See [features](./features.md#tools-to-be-used-by-models).

- The per-response tool-call limit (`MAX_TOOL_CALLS_PER_RESPONSE`) rejects the
  *whole* response: the stream is cancelled, the partial batch discarded, a
  hidden harness notice recorded, one tool-use iteration charged and the model
  re-prompted. No execution, approval or failure bubble exists for it. It is
  distinct from `max_tool_iterations` and from the lower per-batch *execution*
  cap, which closes excess positions as not-executed.
- Unknown tool names resolve synchronously during batch admission, after the
  full assistant batch entered `Context` and before the serial queue starts.
  They must **not** enqueue `AgentEvent::ToolError`: that path clears the
  active tool's cancellation token and advances the queue, which could start
  overlapping requests and reset another stream's pending approval to idle.

## Binary result projection

- Text reads keep only the selected window but validate to EOF. Autodetection
  is UTF-8 first; the first raw chunk recognizes media, then the same open
  handle is re-read.
- `ToolCallResult::text()` is the complete model-facing result;
  `display_text()` is an optional UI projection. For Base64 `read` it is
  `None` and the result is persisted with `result_section_ids::HIDDEN`:
  replay hides the UI section but keeps the model transcript intact.
- `ToolAttachment` references the result's Base64 instead of copying it. The
  provider serializer adds a wire-only `role: "user"` message after the
  tool-response group for models advertising the modality; it is never stored.
- The provider's request dump redacts headers and embedded data-URI payloads
  in a cloned body; the live request is untouched.

## Startup orchestration

`main.rs` alone calls `std::process::exit`, dropping the log guard first so
buffered events reach the logfile. `startup::run` returns
`StartupRun { session_id, result }`: setup failures are `Err`, while an app
failure travels in `result` so `main.rs` can log it inside the session span.

## YAML parsing policy

Agent-type and skill frontmatter use `serde-saphyr` through `yaml_options()`
in `lib.rs` (agent types via `with_deserializer_from_str_with_options`, so
`serde_ignored` still reports unknown keys):

- `no_schema`: unquoted numbers/booleans/nulls are not coerced to strings, so
  `name: 123` or `name: ~` fail.
- `with_snippet` off: single-line errors with line and column.
- Duplicate keys are rejected and alias expansion is budgeted (defaults).
- `strict_booleans` stays off because `bool_or_string` already accepts string
  forms.

## Path-like argument normalization

`ToolRegistry::create_call` applies `sandbox::expand_path_tokens` to keys
declared by `Tool::path_like_arguments()`, then `to_relative_path`, before
sandbox approval. `startup::run` exports the resolved `YLVA_HOME`, so sentinel
paths work in path tools and `bash`. See
[features](./features.md#tools-to-be-used-by-models).

## Sandbox policy and the `cross_filesystem` rule

- Configured rules replace the default floor; appended rules still run last;
  last match wins.
- `Sandbox::sandbox_root` is the canonicalized launch directory.
  `matches_outside_workspace` derives the workspace root from it on each
  evaluation; without a marker both roots are the same path. Floor, configured
  `outside_workspace` rules and snapshot checks all use this comparison.
  Relative paths and `cross_filesystem` keep the launch-directory anchor.
- `matches_dot_path` checks every component of the resolved absolute path
  without stripping either root (including hidden launch ancestors and paths
  outside the workspace). Enumeration keeps an explicit dot-path bypass.
- `resolve_device` reads `st_dev` on every check (no cache, so remounts apply
  immediately); a nonexistent target uses its deepest existing canonicalized
  ancestor. No device on non-Unix.

See [configuration](./configuration.md#sandbox-tool-approval-rules).

## `read`/`write`/`edit` approval-to-use binding

`create_call` runs before approval, so each call captures an `ApprovedTarget`
holding the canonical path the sandbox judged. Before use it re-canonicalizes
the requested path and refuses the call if it differs (symlink or parent
swapped while approval was pending), then opens by canonical path with
`O_NOFOLLOW` on Unix so a symlink planted in the final component fails. Check
and open are not atomic and parents are not pinned (no `openat`-style
handling): a swap inside that tiny window remains possible.

## `bash` tool process-group cleanup

On Unix, `run_bash` kills the process group with `SIGKILL` on timeout or
cancellation so grandchildren do not outlive the command. See
[features](./features.md#tools-to-be-used-by-models).

Memory-safety invariants for captured output:

- Both pipe readers feed one mutex-guarded collector (lock order = arrival
  order). After a cap is hit the readers keep draining so the command never
  blocks on a full pipe, but retain nothing. A single unterminated line is
  clipped while reading, so it cannot grow memory either. Lines decode
  lossily, so invalid UTF-8 does not end a reader.
- Web fetch bounds the body with a `BoundedBuffer`, cutting a chunk *before*
  storing it; media overflow fails, text overflow is truncated on a UTF-8
  boundary. Provider error bodies are read only up to a small prefix.
  That snippet reaches logs and the UI deliberately (users need it to
  diagnose provider failures) and is not redacted; request headers (API keys)
  are never part of it.

Security-relevant HTTP behavior:

- `http::redacted_url_for_display` is the single URL form for `tracing` fields
  and error strings: query values and userinfo are redacted, the fragment is
  dropped. `reqwest` errors are logged via `without_url()`. The opt-in request
  dump deliberately keeps the raw query.
- `HttpFetchBackend` uses a fresh client per hop with
  `redirect::Policy::none()` and follows `Location` itself. Each hop resolves
  the host, rejects private/reserved addresses (IPv4-mapped and NAT64
  unwrapped) unless the hop's key passes `check_private_address` (the
  `network_private_address` rule; configured rules only, so the CLI catch-all
  never grants it), and pins the client to the checked addresses with
  `resolve_to_addrs`, closing the DNS-rebinding gap. The hop `deny` closure
  consults `check_network_request` only when the hop's `scheme://host[:port]`
  differs from the requested URL's, and only `AutoDenied` refuses.

## CLI driver

- The CLI builds a separate registry for tool display; it must receive the
  same `[web_search]`/`[web_fetch]` configuration as the execution registry, or
  the displayed backend can differ from the one the agent calls.
- With `--auto-approve-spawn` the process stays alive after the root's
  `StreamDone` until every spawned call reaches a terminal outcome: the
  `spawn_subagent` result reports admission, not completion.
- SIGINT/SIGTERM become `AgentCommand::Stop`; the loop ends when the agent
  reports idle, which includes a root `ToolKilled` because a stop during a
  tool run emits no stream event. The TUI feeds the same `RunOutcome` recorder
  from `UiUpdate`s, the `DenyTool` effect and external signals in any state.
  `SubmitPrompt` with `steer: false` resets it and `StopAgent` records
  nothing, so only the latest turn decides the exit code
  ([exit codes](./cli-options.md#exit-codes)).

See [features](./features.md#cli-mode).

## Context management and compaction

### Runtime structure

- `agent/runtime.rs` owns the single mutable run state and a biased select
  coordinator; receivers stay in the coordinator, generation/cancellation and
  queued work in its owned state.
- `Context` keeps the full transcript (`messages`, never pruned) separately
  from per-message `entries` (cached token counts, `view_start`, compaction
  state, transcript-indexed `model_overrides`). The model view is: system
  block, summary/recovery handoff, latest task anchor, retained tail with
  bounded overrides. Background summaries get an immutable prefix snapshot.
- `summary_boundary_for_target` counts removable tokens from `model_view_start`
  so each boundary advances past the already summarized/dropped prefix; with too
  little visible tail it returns `None`, avoiding summary requests that cannot
  advance the view.

### Token estimation

- Estimates use `cl100k_base` as a text-level approximation, not the
  provider's tokenizer. The tool-definition count is cached per schema
  snapshot, keyed by exact name, description and serialized parameter text
  (including JSON field order).
- The transport estimates the serialized JSON body (after provider fields and
  tools) and calls the estimate callback *before* awaiting the response, so
  the footer follows the latest request even when the provider is slow.
- After a summary or synchronous drop the loop emits a model-view estimate
  until a later request estimate replaces it; startup and `RecordModelChanged`
  emit one too, so fresh/resumed sessions and window-changing switches refresh
  the footer. Headless runs drop usage updates.
- `YLVA_TOKEN_USAGE_DUMP_PATH` writes metadata-only records pairing the local
  estimate with provider-reported usage; it never serializes messages,
  credentials or bodies and is not session data or billed usage
  ([CLI options](./cli-options.md#debug)).

### Model usage cost ledger

- `llm/usage_cost.rs` defines `RequestCost` and `CostLedger`, a cloneable,
  mutex-guarded per-currency total. `CostLedger::process()` is the
  process-wide ledger that providers use by default, so every engine (root,
  subagent, utility, compaction) shares one total without per-caller plumbing;
  providers accept an own ledger only for tests.
- Reported costs are not part of the OpenAI Chat Completions protocol, so the
  shared `OpenAiCompatibleProvider` transport only offers a `UsageCostParser`
  hook (default: none) applied to every SSE payload, independent of
  `parse_sse_payload` because usage-only frames have no choices.
  `OpenRouterProvider` supplies the parser for its `usage.cost` field (USD);
  Ollama and plain OpenAI-compatible providers report none. The first cost frame of a request is recorded
  immediately, before the terminal stream event is sent, so the redraw that
  follows completion already sees it, and so failed or cancelled streams keep
  a cost already reported. Each record is logged at `debug` level (connection,
  model, amount, currency).
- The TUI render state carries `cost_total_text`, read from the ledger on every
  redraw (no event of its own); the footer places it left of the context
  usage and drops it first when the row is narrow. `resume_hint_text` appends
  the `Costs:` line from the same ledger at exit. Costs never enter
  `SessionExitSnapshot` or the store.
- `MockProvider::with_reported_cost` scripts a cost for tests.

### Policy and scheduling

- `CompactionRuntime::sweep` runs only from the outer loop at safe boundaries
  (after completed turns, before tool-loop follow-ups). It applies a finished
  summary, honors a forced fallback, then re-evaluates thresholds read from
  the *active* engine, so a `/models` switch re-prices the budget next turn.
- An explicit summarizer uses an independent copy of its named engine's
  original configuration; otherwise it follows the live conversation engine.
  Engine and timeout precedence are independent, and the deadline comes from
  `[compaction]`, not the compact engine's own settings.
- Scheduling after each tool outcome uses only completed eligible prefixes and
  never applies snapshots inside an open batch.
- `start_request` is the shared preflight for every continuation: apply ready
  state, defer oversized requests until an existing summary's absolute
  deadline, then measure staged recovery. Generation and request identity are
  checked before clearing in-flight state; each waiting continuation is
  consumed once; preflight notices are acknowledged by the outer loop before
  dispatch to preserve order. An idle subagent waits for its in-flight summary
  and resumes if its view changed.
- A runaway summary (`COMPACTION_SUMMARY_MAX_BYTES`) fails like an empty one:
  history kept, fallback policy applies.
- The `· Compacting` footer activity is per conversation and never touches
  `AgentMode` or input routing. Failure/timeout clears it immediately; success
  keeps it until `sweep` actually applies the snapshot, so it never implies
  completion before the view changed. Request IDs stop a late completion from
  clearing a newer indicator.

### Capacity checks

- Startup checks cover every agent type; the first conversation request waits
  for them so a fast CLI turn cannot exit before unused types are checked.
  Identical warnings are merged. Model switches schedule an immediate recheck
  polled alongside commands, so controls never block on catalog metadata.
- Startup, request and switch checks share an async lock so they neither
  duplicate the root warning nor race its catalog lookup. Neither check clips
  sources or changes policy.
- `EngineDisplayInfo` summarizer data is view-only; it is not stored in
  session provider/model records.

### Hard-limit recovery

`Context::recover_model_view` runs staged reducers: clip older outcomes, drop
older safe groups (leaving a real-content handoff/anchor), clip retained
outcomes, shorten summary/handoff, and only then shorten the request itself.
The safe ceiling reserves reply, safety margin and overhead; marker costs are
reserved before truncation. An irreducible valid continuation fails explicitly
before dispatch. A recognized provider context rejection retries once with a
tighter request-local ceiling, only before response text or tool-call
accumulation. No operations are replayed. A later summary reaching its
boundary supersedes the recovery handoff.

### Tool batch budgeting

- The remaining allowance is divided among unresolved budgeted calls (all but
  `agent_finish`); `advance_tool_queue` skips a call whose share is below the
  minimum useful size and approval recomputes the allowance. A `BTreeMap`
  assembles calls in original index order; unknown/invalid/denied slots are
  never backfilled.
- Canonical output is always preserved; only *model-only* overrides are
  enforced after execution. Tools keep their own source byte/line/network
  limits; the harness never destructively enforces context pressure through
  execution-time output hooks. Binary payload and descriptor are kept whole or
  omitted together.
- Subagent event projection keeps valid structured lifecycle JSON and reduces
  only result/error text.
- Subagent completion is checked against the parent's view at delivery. An
  oversized report gets a deterministic model-only reduction
  (`result_reduced=true`, `result_summarized=false`); canonical records and
  drill-in bubbles keep the full report and their flags stay false.

### Persistence of compaction state

- `StoreEvent::Compaction` has two boundaries: `boundary` (live context
  restore) and `transcript_boundary` (TUI marker; system prompts excluded as
  they are never persisted). `CompactionApplied` carries `run_id` end to end;
  an unknown ID is logged and discarded, never treated as root compaction.
- `StoreEvent::ModelProjection` is versioned by `projection_revision` with
  transcript-relative coordinates. Replay takes the highest revision at
  unchanged or advancing boundaries, validates IDs/names and safe cuts, and
  rebases against rebuilt system messages. Dirty state is recorded before
  dispatch and after sweeps; hydration never duplicates synthetic messages in
  the canonical transcript. Clone copies the events.
- Subagent replay reads messages and the latest boundary together
  (`Store::read_session_state`); snapshots without a boundary load without a
  marker.
- The provider binds a request-scoped failure callback to the response dump so
  compaction timeouts can append their reason after the stream future was
  dropped.

Compaction diagnostics are advisory local estimates without message content;
negative savings are valid.

## Persistence ownership and session cloning

See [persistence](./persistence.md) for on-disk contracts.

- `Store` clones share one locked file and one writer task. The writer keeps
  its lock after replacing the metadata inode and drains queued events when
  the last handle drops. Replay never touches title-generation state.
- `private_fs::replace_file` is the single atomic, durable publish path: a
  create-new temp file beside the target with the final mode set *before*
  publishing, `fsync`, `rename()`, directory `fsync`; the temp file is removed
  on failure so the old file stays intact. Title rewrite, archive transitions,
  the workspace `.gitignore` update and shim scripts all use it. New files are
  `0600`/directories `0700` on Unix by default.
- `Store::set_title` rewrites the first line via temp file plus `rename()`. If
  the writer cannot reopen the file afterwards it **fails closed**: later
  events are dropped (and logged) and later title writes error, rather than
  appending to the replaced inode.
- Subagent titles: the terminal result (via `AgentFinishArgs::resolved_status`,
  shared with execution) wins over intermediate assistant text; root
  eligibility remains text-reply based. Title generation is bounded in bytes
  and time; on expiry the future is dropped (cancelling the stream) and there
  is no fallback title.
- `Store::clone_session` reads the source under a nonblocking exclusive lock
  held only for the read, and rejects a subagent transcript or missing header
  before creating anything. It writes a fresh header (`cloned_from`,
  `parent_session = None`) plus every event except the source header,
  rebasing `subagent_start`/`subagent_result` onto the new id. The source is
  never repaired, rewritten or appended to; subagent transcript files stay in
  place and are reused as read-only history. `--clone` requires
  `--conversation-id` (clap `requires`).

## Subagent delegation internals

- `spawn_subagent` returns the tool's single admission result; the terminal
  outcome arrives later as a hidden harness notice because the protocol allows
  one result per `tool_call_id`. The root gets one continuation after all
  spawned work finishes. `Stop` and `/clear` cancel running and queued work
  (awaiting every cancelled task) without resuming the root.
- Queued spawns beyond `max_concurrent_subagents` run FIFO; `None` internally
  means unlimited. The cap spans the one-level delegation tree
  (subagent-role registries do not expose `spawn_subagent`).
- "End your response when no independent work remains" and "give a
  self-contained answer when `all_done`" are model guidance, not enforcement;
  ending a response leaves subagents running. Completion notices keep this
  guidance in canonical and reduced projections.
- Pending approval/clarify updates carry `run_id`; replies are wrapped in
  `AgentCommand::ForSubagent`. A stale or unknown `run_id` is logged and
  dropped.
- Ordering: streamed text/thinking is tagged with the subagent's `run_id` in
  the factory's forward task, so concurrent streams are never misattributed.
  The shared subagent UI channel is unbounded (no update dropped, never blocks
  the root loop). Each subagent reports finish on a private capacity-1
  channel that the driver forwards only after that subagent's UI forwarder has
  drained, so its last UI update always precedes its finish; the root loop
  also drains queued subagent UI updates before handling a finish.
- The TUI shows only the head of the FIFO approval queue; a subagent awaiting
  a later prompt stays blocked without pausing its siblings.

### Drill-in TUI conversation view internals

See [features](./features.md#drill-in-tui-conversation-view).

- `SubagentStarted` binds `run_id` to the exact `spawn_subagent` bubble id;
  drill-in resolves that id, not the most recently started subagent.
- Elapsed timers are keyed by bubble id so root bubble and drill-in header
  share one; each render samples whole seconds once for title and cache key so
  subsecond redraws reuse cached chrome. Completed durations keep
  millisecond precision.
- `active_view`, `running_subagents` and `subagent_conversations` are separate;
  finished conversations stay available. `SubagentManager` consumes subagent
  stream lifecycle events so they cannot change the root TUI mode.
- `note_interactive_pause` switches to the view owning a pending bubble
  (root for `None`/unknown `run_id`) regardless of what is on screen, saving
  `pre_pause_view`, and skips the switch if that view is already open so a
  manual drill-in triggers no automatic return. Overlapping pauses preserve the
  first `pending_input_snapshot`; restoration waits until no approval owns the
  input.

## Implementation diagrams

These diagrams describe internal code structure and selected implemented behavior,
not user-facing workflows, planned features, or exhaustive generated inventories.

### Diagram scope

- [Interface and ownership diagram](diagrams/class-diagram.mmd): selected application,
  agent, engine/provider, configuration, and driver relationships. Boxes marked
  `module` are Rust modules, not structs. Members are selective and signatures
  omit async/channel wrappers. `RunState`, not the retained `AgentLoop` handle,
  owns the running context; shared handles use aggregation.
- [Reducer state diagram](diagrams/tui-reducer-state-diagram.mmd): separate projections
  of `TuiMode` and `Focus`, not an exhaustive product of valid combinations.
  `ConversationView`, `SlashOverlay`, compaction activity, and per-subagent
  state are independent dimensions. An idle root mode does not imply that
  all subagents have finished. Edges describe the current presenter, including
  immediate Ctrl+C denial in approval focus.
- [Turn-execution sequence](diagrams/turn-execution-sequence.mmd): model requests,
  serial tool advancement, approval/error paths, and safe steer injection.
- [Subagent lifecycle](diagrams/subagent-lifecycle-sequence.mmd): admission versus
  completion, FIFO scheduling, routed interaction, cancellation, and the final
  aggregate continuation.
- [Compaction data flow](diagrams/compaction-data-flow.mmd): immutable transcript
  versus model-only summaries, bounded projections, preflight, recovery, and replay.
- [Session lifecycle](diagrams/session-lifecycle-state-diagram.mmd): root startup,
  clear, and TUI picker preparation/publication.
- [Session storage](diagrams/session-storage-state-diagram.mmd): the separate
  offline raw/archive representation lifecycle and explicit deletion.
- [Configuration and policy flow](diagrams/configuration-policy-flow.mmd): distinct
  resource precedence rules, per-agent engine/tool/skill resolution, prompt
  composition, and approval enforcement, including the loader's empty-rules
  default and independent sidecar read policy.
- [TUI event flow](diagrams/tui-event-flow.mmd): driver orchestration, pure
  reduction, ordered effects, asynchronous result handling, and render feedback.

All Mermaid sources live in `docs/diagrams/` and are authoritative. See
[diagram rendering](development-workflow.md#diagram-rendering) for generating
SVGs from one or all sources; generated SVGs are ignored by Git.

## TUI internals

The TUI renders the same headless `AgentLoop`; it does not own agent state.
The presenter is a pure reducer emitting ordered effects without terminal or
channel I/O. Pending approval/clarify destinations stay driver-local and are
recorded *before* reducer transitions. Catalog cancellation, agent shutdown,
terminal teardown and title restoration keep a fixed order.

- User-facing paths go through `display_path` (home shown as `~`); logs,
  tracing fields, dump files and agent tool results keep absolute paths.
- Scrollbar clicks map through the published `ChatScrollbarLayout` (exact
  maximum offset), not the selection-line count, which can include a trailing
  spacer. Help and slash-overlay hit-testing take priority.
- Input height and hit-testing use ratatui-textarea's native wrapped screen
  map, not a `textwrap` approximation. The real viewport top is read after
  rendering from an origin cursor clamped with `CursorMove::InViewport` on a
  clone, so recalled/replaced drafts cannot inherit a stale offset.

### Conversation layout cache

- `ConversationRenderCache` shares immutable styled rows via `Arc`, keyed by
  slot/revision, width, theme, collapse state and rendering/title variant.
  Cheap chrome (titles, spinners, elapsed timers) is cached separately from
  expensive bodies, so chrome updates never reparse bodies.
- `render::render` borrows transcript, turn buffer, queued steers and header
  through `ConversationSource`; selection vectors move into the legacy shell
  and back instead of being cloned per frame.
- Expansion feedback: before the normal frame the driver draws the *collapsed*
  layout with an `Expanding…` title; only after that draw succeeds does
  `BubbleExpansionFeedbackShown` allow the expanded layout. The feedback frame
  preserves scroll state so a temporary collapsed bottom cannot re-enable
  auto-scroll. Collapse, clear and view navigation discard pending feedback.
- Invalidation is revision-based and opt-in (`enable_render_cache`): mutation
  helpers advance revisions; no warm lookup hashes or compares payloads.
  Callers editing public `AppState` fields directly must call
  `invalidate_render_cache`; `AppState::new` callers take a conservative
  rebuild path. Collapse is a layout dependency, not a content revision.
  Compaction insertion invalidates the owning transcript; background subagent
  updates leave an unrelated active transcript valid. Root queued steers are
  omitted from a subagent's view.
- Only visible rows are cloned/highlighted; offsets stay `usize` so bottoms
  beyond `u16::MAX` work. Cached base styles never receive selection overlays,
  and selection uses terminal-width graphemes, not cells.
- Optional expanded layouts retained after collapse share
  `COLLAPSED_BODY_BUDGET_BYTE_SIZE`, a derived-layout allocation estimate (not
  resident memory) counting shared allocations once. Over-budget optional
  layouts are not retained; others evict least-recently-retained. Required
  current layouts stay resident even if oversized.

## Slash commands

Draft normalization, model-filter extraction, suggestions and help all derive
from the constants and allowlist in `slash_command.rs`. Engine-dependent
effects are handled by the driver, not the reducer; `/models` runs in a
cancellable background task so input and agent updates continue. The pending
input snapshot stores the slash overlay with the draft, so restoring after
approval/clarify keeps fetched model choices. See
[features](./features.md#slash-commands).

## Steer ordering

While an agent is busy, the presenter shows a new steer as queued and the loop
keeps it outside `Context`. After `SteerApplied`, the loop commits it after the
current round's emitted messages, so persisted order matches what the model
saw. If the round is cancelled first, the steer is not sent.
