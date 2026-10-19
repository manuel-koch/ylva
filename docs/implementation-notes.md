# Implementation notes

This is a contributor reference. Keep details only when code alone does not
explain them: cross-module interactions, constraints or workarounds, and design
decisions behind behavior described in [features](./features.md). Keep notes
brief and link to user-facing docs rather than repeat their contracts.

## Session lifecycle ownership

`tui_driver/sessions.rs` owns blocking discovery/preparation tasks, scoped by
initiating root and picker IDs. The pure `SessionPicker` holds typed
Resume/Clone action and Loading/Ready/Preparing state. Results can update a
parked input snapshot during approvals; stale results cannot publish.
Durable clone outcomes survive cancellation and are explicitly reported.
Preparation freezes mutations through the outer loop's authoritative
`PrepareSessionSwitch` barrier and rechecks quiescence before publication.
Publication joins the old loop, replaces channels/engine/tools and replay,
retargets the exit guard, and drops obsolete stores. Subagent drill-in consults
the current store, including after `/clear`, rather than a startup capture.

`agent_factory::select_root_agent_type` is shared by startup and picker
selection; only startup supplies an explicit override. `create_root_for_store`
uses independent configured engine copies and an explicit target store.
The TUI retains a storeless configuration factory so it does not pin the
startup session lock after replacement; provider connection limiters remain
shared.

Manual compaction is a typed outer-loop command. Its boundary selector
advances only through a completed prefix before the latest explicit task.
Trigger provenance distinguishes manual failures from automatic fallback;
manual candidates are estimated before mutating/persisting the live view and
must strictly reduce it. Summary outcomes retain generation/request identity
and apply at the existing safe boundaries. Request IDs remain internal to
runtime/activity updates and structured diagnostics; user-facing manual
compaction notices omit them. Presenter subagent-view handling
rejects manual compaction before ordinary subagent Enter suppression, never
forwarding it to a subagent or hidden root.

`profile_command.rs` dispatches `ylva profile` before profile/provider
initialization, like `session_command.rs`. It resolves the scope (home or
`--workspace`), refuses unsafe operations with exit 2 before writing, and
runs the editor for `edit` through `sh -c` with the path as `$1`. Every
`create`, `edit`, and `delete` reconciles shims through `profile_shim.rs`.
`profile_shim.rs` plans the whole change before writing: it collects claims
from each in-scope `shim_name` (or `ylva-<name>`), rejects duplicate claims,
and prunes only unclaimed regular files in the `shims` directory.
Shim scripts only `exec` `ylva --profile`, so a home shim inherits the caller's
`YLVA_HOME`. A workspace shim first `cd`s to the workspace root, which it derives
from its own path with `pwd -P`. `profile_command.rs` calls
`init_command.rs::seed_profile_starters` for new profiles. Invalid names in
profile directories are validated but get no shim claim.

`session_command.rs` dispatches `ylva session` before profile/provider
initialization or stdin consumption. Its CLI listing projection
keeps the library's descending order while using last-use timestamps: roots and
sibling groups are rendered least recently used first. With subagents included,
an iterative traversal follows
`parent_session`, marks nesting after each last-use timestamp, and lists each
transcript once. Missing parents are annotated; cyclic parent links produce
diagnostics without hiding sessions. `store/maintenance.rs`
validates selectors, selects deterministic identity groups, and coordinates
reference scans with clone/delegation publication. `store/archive.rs` owns shared
representation resolution and durable, lossless transitions; all replay readers
use it. `store/session_file.rs` retains stable identity locks independently of
the raw file inode, including across title replacement. Writer tasks hold the
lock until their command queue drains. `store/replay.rs::last_used_at` derives
activity from the resolved transcript without changing metadata or consulting
file modification times. Date-based maintenance revalidates it after exclusion,
skipping a group if a selected root/target was used again after discovery.

Drivers retain a `SessionExitGuard` observing the agent's shared current-store
handle. It snapshots the final persisted ID/title on success or error without
holding a stale startup identity across `/clear`. The binary prints that snapshot
only after terminal restoration.

`Store` owns an immutable session span; runtime handlers snapshot the current
store's span for each async scope. Spawned work instruments its own future, so
rollover never mutates an existing span. The subagent manager and factory share the
current-store handle for new delegations, while each admitted run retains its
parent store. Title tasks explicitly parent to their target store and writer
tasks parent to their own session. Text and JSON tests verify rollover and
subagent transcript correlation.

See [persistence](persistence.md) and
[Session commands](cli-options.md#session-commands) for the user contract.

## Layered configuration and persona assembly

`prompt.rs` owns paired `<ylva_…>` block framing for trusted instructions,
skill-source footers, harness notices, compaction summaries, and recovery
handoffs. Callers own wording and metadata serialization; delimiters do not
establish trust. Recovery clips block bodies while reserving both tags and
recognizes legacy handoff prefixes. Existing transcript text is never rewritten.

Unknown keys are found in `config/unknown_keys.rs` via `serde_ignored`; each
ignored path becomes a `ConfigWarning` with a Levenshtein suggestion from the
flat `KNOWN_KEYS` list. Warnings travel in `ProfileConfig.config_warnings` to
`main.rs` (log, `--strict-config`), `cli_driver` (stderr) and
`TuiRuntime.startup_notice`. `SandboxRuleConfig` parses through a strict
`deny_unknown_fields` raw struct so errors name the key.

> **Keep `KNOWN_KEYS` in sync.** Update it whenever a config key is added,
> renamed or removed; otherwise typos still warn, but without a suggestion.

`ProviderKind` is a closed enum (`mock`, `ollama`, `openrouter`), so a typo
fails at load; `ProviderRegistry::build` matches on it, not on connection names.

Request tasks in `agent/request.rs` use `spawn_supervised`, which logs panics
and, for the model stream, sends `AgentEvent::StreamError`. Layout code uses
`saturating_cast::saturating_u16` instead of `as u16`.

Skills separate discovery, per-agent policy, and instruction delivery.
`AgentFactory` carries one immutable discovery snapshot and the host's approval
capability across new roots, subagents, `/clear`, and TUI session switches.
`skills/policy.rs` resolves each agent type independently; one callable catalog
drives its prompt advertisement, loader membership, and sidecar sandbox identities
so these cannot drift. Subagents do not inherit their parent's filtered catalog.
Direct library factories start with no skills.

Loader approval checks use recorded canonical paths without reopening sources.
Unlike ordinary tools, an unconfigured loader replaces the sandbox floor with
empty rules. Duplicate detection is provenance only: resource fingerprints and
shadowed copies never become permission grants or fallback sources. Sidecar
access remains a separate read-policy decision, not a consequence of loading.

Skill instructions enter context as ordinary tool results, not system messages.
To prevent partial successful delivery, `requires_complete_delivery` is enforced
in `runtime/events.rs` against the live allowance, after generation checks and
before context insertion. It estimates the complete serialized `ToolEnvelope`,
not just the body; accepted results bypass bounded projection.

See [skill configuration](configuration.md#agent-skills) for discovery, access,
and snapshot/replay rules, and [skill commands](cli-options.md#skill-commands)
for provider-independent catalog listing.

See [configuration](./configuration.md) for file precedence, formats, and the
system-prompt contract.

`AgentFactory` passes the resolved global SOUL and trusted-instruction
baseline to root and subagent loops. Direct library or test loops without a
resolved prompt retain `Context::new()` as the compatibility fallback.

Trusted-instruction discovery has a pure
`load_trusted_instructions_from_roots(workspace_root, working_directory,
ylva_home)` core. `load_trusted_instructions` resolves Ylva home and logs
contributing files. Tests call the pure core without mutating process
environment state.

After building the selected profile's engines, `AgentFactory` validates every
agent type's explicit engine reference before store or CLI/TUI startup.
Environment-discovered engine sets use the same validation path.

`COMPACTION.md` resolves through the same layers as `SOUL.md` (workspace
`.ylva/`, then the Ylva home, then the built-in default) via
`load_compaction_prompt`. `AgentFactory` loads it where it wires each agent
type's compact engine, and those summary guidelines reach every background
summary request; loops built outside the factory keep the built-in prompt. The
`build_summary_request` appends code-owned framing to the resolved guidelines
in one system message, followed by the source messages and a final user trigger.
Conversation compaction uses this builder; subagent report reduction is deterministic
and does not start a separate blocking LLM request.
The framing derives its trigger description from the same constant as the final
user message and calls out message boundaries so source text is not mistaken for new
summarizer instructions. Positive compaction guidelines prioritize the latest
established state, merge repetition, and preserve actionable details and
uncertainty. The prompt requests only facts grounded in the supplied prefix;
the boundary of the unseen verbatim tail is managed by the conversation's
Engine runtime.

## Engine runtime and provider request configuration

`llm/engine.rs` retains the `Engine` trait and explicit compatibility
re-exports. Its private children separate configuration and deserializers
(`engine/config.rs`), typed picker targets and display snapshots
(`engine/selection.rs`), named-engine routing (`engine/router.rs`), and
provider-backed execution with catalog hydration (`engine/runtime.rs`).
`engine/tests.rs` retains shared fixtures and cross-owner assertions, with
responsibility-specific suites under `engine/tests/`.

`EngineImpl` and `OpenAiCompatibleProvider` share one
`EngineRuntimeConfigHandle`. The provider snapshots it before building a
request and releases the lock before network I/O. Updates therefore affect
later requests without changing settings in an in-flight request.

Catalog hydration resolves the effective context length and runs once for a
model selection before its first chat. A `/models`
selection supplies that model's metadata, so hydration skips another fetch.
The engine does not repeat a hydration lookup after its one attempt for that
selection; a direct model-only switch invalidates the previous metadata. Transient
HTTP `429`/`5xx` responses inside that attempt use the shared bounded retry
policy below. If hydration rejects a profile reasoning value, the engine
queues a notice; `EngineRouter` drains it and `AgentLoop` forwards
`UiUpdate::Notice` before `StreamStarted`.

Each provider wrapper supplies a request-body customization function. The
shared transport leaves provider-specific wire fields to those wrappers.

`ModelSelection` keeps named-engine shortcuts distinct from raw catalog
options. `EngineRouter` resolves preview and confirmation through the same
provider-to-engine target helper: named choices activate their exact map key,
while raw catalog choices use the active engine for that connection or the
sorted representative for another connection. The presenter carries this
typed selection to the driver, so equal provider/model pairs do not erase
engine identity. Picker display metadata is a snapshot only; the Engine runtime
handles remain the source of request settings. `matching_model_indices` shares
engine-name, provider-connection and model-ID filtering across picker rendering,
initial selection, live narrowing, navigation and confirmation; it does not
search formatted display metadata. While the agent is running, its
display snapshot retains a shared compaction-engine reference alongside the
background task, so frame refreshes continue to report the configured
compaction selection. A model-selection target carries the resolved effective
context window with its provider-connection/model identity. The picker uses
that preview for a following compaction engine, and colors a fixed engine's
detail orange only when its effective context window differs from the
highlighted target's window, not when provider/model identity alone differs.
`EngineConfig::context_length_for_model` scopes an override to the original
configured model; previews and catalog adoption share that resolution.
`EngineImpl::configured_copy` gives explicit summarizers their original settings
with independent provider request handles, sharing HTTP clients and connection
concurrency limits. Equal engine names no longer imply shared request settings.

`llm::provider_request_limiter` owns one shared generic HTTP semaphore per
provider connection name; provider connections use the default cap of four.
Catalog and Ollama detail calls acquire a permit until the response body is
read, while streamed chat calls retain it until the response stream closes.
The generic `http` JSON helpers own retry behavior for `429`/`5xx` and honor
`Retry-After` delta-seconds or HTTP-date values (bounded by the retry maximum),
keeping each retry sequence under the caller's same permit.

## Tool-result wire protocol

Each `role: "tool"` message carries JSON derived from `ToolEnvelope`.
Successful result:

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

Failed result:

```json
{ "ok": false, "error": "..." }
```

Metadata and its schema are omitted when a tool returns no structured
metadata. `result_metadata_schema` is generated from the populated fields of
the typed metadata result, so its shape follows the data. Tool descriptions
also include their argument schemas. See [features](./features.md#tools-to-be-used-by-models)
for tool behavior.

The agent's streaming task owns a bounded map of tool-call accumulators. It
rejects a new distinct index once `MAX_TOOL_CALLS_PER_RESPONSE` (12) is reached,
cancels the provider stream, and sends a dedicated limit event to the outer
loop. The outer loop discards the partial batch, records a hidden harness notice
explaining the limit, charges the rejection as one tool-use iteration, and
re-prompts when the per-turn iteration budget permits. No tool execution,
approval bubble, or per-call failure bubble is created for the rejected batch.
The per-response limit is separate from `max_tool_iterations`, which counts
rounds after tool execution completes. It also differs from the six-position
execution cap: up to 12 calls can be parsed with excess positions closed as
not-executed, while a thirteenth distinct call rejects the whole response.

Unknown tool names are resolved synchronously during batch admission, after
the assistant's full batch has entered `Context` and before the serial queue
starts. They must not enqueue `AgentEvent::ToolError`: that completion path clears
the active tool's cancellation token and advances the queue, so an unrelated
registry failure could start overlapping requests and let one stream's
completion reset another stream's pending approval to idle. An all-unknown batch
counts as one completed tool-use iteration.

## Binary result projection

`agent/tools/read.rs` owns the tool/schema/result contract and explicit versus
autodetected mode dispatch. Its private `read/stream.rs` child owns bounded
I/O and incremental UTF-8 validation; `read/windows.rs` owns selected
line/character windows and full-source line counting; `read/media.rs` owns
recognition, Base64 construction and safe source filenames. Media helpers and
constants retain their existing `agent::tools::read` paths for fetch backends
and providers. Whole-tool tests live in `read/tests.rs`, with stream and scanner
boundary tests beside their implementations.

Text reads retain only the selected window but validate to EOF, including
after window completion or truncation. Autodetection keeps UTF-8-first ordering
and uses the first raw chunk to recognize media, then re-reads from the same
open handle. Complete media results keep the existing separate size ceiling;
range reads and text windows keep the output cap.

`ToolCallResult::text()` remains the complete model-facing result.
`display_text()` is an optional UI projection. For Base64 `read`, it returns
`None` and the agent persists `result_section_ids::HIDDEN`. Replay suppresses
the UI section but keeps the original result in the model transcript.

`ToolAttachment` stores media metadata and references the containing result's
Base64 string rather than copying it. For a model that advertises the matching
input modality, the provider serializer adds a wire-only `role: "user"`
message after the tool-response group. The serializer sends it only in the
provider request. The session stores the descriptor alongside the canonical
Base64 result. See
[features](./features.md#tools-to-be-used-by-models) and
[persistence](./persistence.md#resume-contract) for the external contract.

`crate::http::dump_request` redacts a cloned request body. It leaves the live
request and tracing fields unchanged.

## Web search and fetch backends

`default_registry` injects `Arc<dyn WebSearchBackend>` and
`Arc<dyn WebFetchBackend>` backends built from profile configuration. Unit
tests replace them with stubs and make no network requests. See
[features](./features.md#tools-to-be-used-by-models) for both tool contracts.

## Path-like argument normalization

`ToolRegistry::create_call` applies `sandbox::expand_path_tokens` to keys
declared by `Tool::path_like_arguments()`, then calls `to_relative_path`
before sandbox approval. `expand_path_tokens` takes a lookup closure, so tests
avoid process-environment mutation. `src/main.rs` exports the resolved
`YLVA_HOME`, so sentinel paths work in path tools and `bash`.

See [features](./features.md#tools-to-be-used-by-models) for path-argument
behavior.

## Sandbox policy ownership and `cross_filesystem` rule

`sandbox.rs` retains `Sandbox`, `SandboxContext`, their shared sandbox
handle and ordered rule storage, with explicit re-exports for existing public
rule and path-helper names. Private `sandbox/rules.rs` owns serialized and
compiled rules, `sandbox/path.rs` owns expansion/canonicalization/device
checks and path approval, `sandbox/command.rs` owns command evaluation and
worst-case aggregation, and `sandbox/network.rs` owns target-aware network
approval. The existing command extractor and network-target parser remain
specialist children. Each evaluator uses the same parent-owned rule lists;
configured rules replace the floor and appended rules still run last.

Owner-local suites live under `sandbox/{rules,path,command,network}/tests.rs`,
sharing the sandbox fixture from `sandbox/tests.rs`. The facade suite checks
shared sandbox-root updates and rule replacement/precedence across owners.

`Sandbox::sandbox_root` stores the canonicalized launch directory.
`matches_outside_workspace` derives `workspace_root` from that anchor using the
shared workspace-discovery helper on evaluation. When no marker exists, the
workspace root defaults to the sandbox root, so both roots are the same path.
Both the default floor and configured `outside_workspace` rules use this
comparison, including snapshot checks. Relative paths and `cross_filesystem`
keep their launch-directory anchor. `matches_dot_path` instead checks every
component of the resolved absolute path without stripping either root, including
hidden launch ancestors and paths outside the workspace. Snapshot checks use the
same predicate; enumeration retains its explicit dot-path bypass. Configured
rules still replace the default floor, with last-match-wins precedence.

`sandbox/path.rs`'s `resolve_device` reads Unix `st_dev` on each check and
returns no device on other platforms. A nonexistent target uses its deepest existing canonicalized
ancestor as the device anchor. No cache means remounts affect the next check.
See
[configuration](./configuration.md#sandbox-tool-approval-rules) for the rule's
contract.

## `read`/`write`/`edit` approval-to-use binding

`create_call` runs before approval, so `ReadCall`, `WriteCall` and `EditCall`
each capture an `ApprovedTarget` (`agent/tools/approved_target.rs`) holding the
canonical path the sandbox judges. Before reading or mutating, it
re-canonicalizes the requested path and refuses the call when the result
differs (symlink or parent swapped while approval was pending). The file is
then opened by its canonical path with `O_NOFOLLOW` on Unix, so a symlink
planted in the final component fails instead of redirecting the access. The
check and the open are not atomic and parent directories are not pinned (no
`openat`-style handling), so a swap inside that sub-millisecond window remains
possible.

## `bash` tool process-group cleanup

On Unix, `run_bash` kills the process group with `SIGKILL` on timeout or
cancellation, so grandchildren do not outlive the command. See
[features](./features.md#tools-to-be-used-by-models) for timeout and output
limits.

`bounded_buffer.rs` provides `BoundedBuffer`, the shared accumulator that
stores at most its limit and records an overflow instead of growing. Both pipe
readers push lines into one shared `OutputCollector` (a mutex, so lock order is
arrival order) that adds the line cap on top of a `BoundedBuffer` of
`BASH_OUTPUT_LIMIT_BYTES`. After a cap is hit the readers keep draining so the
command never blocks on a full pipe, but retain nothing. `read_bounded_line`
keeps at most `BASH_OUTPUT_LIMIT_BYTES` + 1 bytes of any single line and
discards the rest, so one giant unterminated line cannot grow memory either.
Lines are decoded lossily, so invalid UTF-8 no longer ends a reader early.

`HttpFetchBackend` collects the response body in a `BoundedBuffer` of
`READ_MEDIA_MAX_BYTES`, so a chunk is cut before it is stored rather than
after the limit is exceeded. Media overflow fails with `TooLarge`; text
overflow is flagged truncated and trimmed to a UTF-8 boundary.

`http::response_body_detail` reads provider error bodies through a
`BoundedBuffer` of 8 KiB and stops there, so a huge or stalled error body
cannot be buffered whole; the 512-character snippet is cut from that prefix.
The snippet reaches the logs and the UI deliberately so users can diagnose
provider failures; it is not redacted. Request headers (API keys) are never
part of it.

`http::redacted_url_for_display` is the single URL form for `tracing` fields
and error strings in `src/http.rs` (`send_with_retry`, `status_error`): scheme,
host, port, path and query parameter names are kept, values become
`<REDACTED>`, userinfo becomes `<REDACTED>@` or `<REDACTED>:<REDACTED>@`, the
fragment is dropped, and an unparsable URL renders
as `<invalid URL>`. `reqwest` errors are logged via `without_url()` for the
same reason. The opt-in request dump deliberately keeps the raw query.

`HttpFetchBackend` (`backends/web_fetch/native.rs`) builds a fresh
`reqwest` client per hop with `redirect::Policy::none()` and follows
`Location` itself (`MAX_REDIRECTS` = 10). Each hop resolves the host with
`tokio::net::lookup_host`, rejects it via `is_private_or_reserved` (IPv4-mapped
and NAT64 addresses are unwrapped) unless the hop's key passes
`SandboxContext::check_private_address` (the `network_private_address` rule;
configured rules only, so the CLI catch-all never grants it), and pins the
client to the checked addresses with `resolve_to_addrs`, closing the
DNS-rebinding gap. The tool passes the backend a `HopPolicy` of two closures:
`deny`, built from `check_network_request` (only `AutoDenied` maps to a
refusal, consulted only when a hop's `scheme://host[:port]` key differs from
the requested URL's), and `allow_private_address`. Refusals surface as
`WebFetchError::Refused`.

## CLI driver

The CLI builds a separate registry for tool display. It must receive the same
`[web_search]` and `[web_fetch]` configuration as the execution registry, or
the displayed backend can differ from the one the agent calls.

With `--auto-approve-spawn`, `cli_driver` keeps the process alive after the
root's `StreamDone` until every spawned call reaches a terminal outcome. The
immediate `spawn_subagent` result reports admission, not completion. See
[features](./features.md#cli-mode) for the CLI behavior.

Each run records a `RunOutcome` (`src/run_outcome.rs`) from driver events and
maps it to an `ExitCode` (`src/exit_code.rs`) when it ends; the table is in
[exit codes](./cli-options.md#exit-codes). `StopSignals` (`src/stop_signal.rs`) relays SIGINT and SIGTERM and
sends `AgentCommand::Stop`. The loop then ends when the agent reports idle:
`StreamCancelled`, `AgentIdle`, or a root `ToolKilled`, because a stop during a
tool run emits no stream event. The TUI keeps the same recorder in
`PendingRunIds::run_outcome`. It is fed from `UiUpdate`s, the `DenyTool` effect,
and the driver loop's external SIGINT or SIGTERM (`PendingRunIds::record_stop_signal`,
any TUI state). `SubmitPrompt` with `steer: false` resets it, and
`StopAgent` (Ctrl+C or Esc in the TUI) records nothing, so only the latest turn
decides the code.

## Store and persistence

`Store` clones share one locked file and writer task. Record methods enqueue
without blocking; `flush` waits until the writer drains queued events. See
[persistence](./persistence.md) for message-commit, replay, and on-disk
contracts.

## Context management and compaction

`agent/runtime.rs` owns the single mutable run state and biased select
coordinator. Its command, agent-event and subagent handlers live in
`runtime/commands.rs`, `runtime/events.rs` and `runtime/subagents.rs`;
`runtime/compaction_updates.rs` translates safe-boundary outcomes into UI
updates. Handler outcomes distinguish polling again, evaluating the
root/subagent completion tail and finishing immediately, preserving the original
outer-loop `continue` and termination decisions. Receivers stay in the
coordinator; generation, cancellation and queued work stay in its owned state.
No new task, channel, lock or Context copy is introduced by this split.

`agent/request.rs` owns preflight, immutable request snapshots and stream
accumulation. `agent/tool_execution.rs` owns the existing per-tool task;
`agent/tool_queue.rs` owns batch admission, result allowances, iteration
checks and serial advancement, with spawn admission in `tool_queue/spawn.rs`.
Cross-owner assertions and shared unit fixtures remain in `agent/tests.rs`,
with responsibility suites under `agent/tests/`. Message and protocol suites
live beside those owners; Context's transcript suite remains in
`context/tests.rs`, with projection, recovery and token suites below it.

`agent.rs` retains the construction API and explicit compatibility re-exports.
Private `agent/message.rs` owns persisted messages and tool results;
`agent/protocol.rs` owns commands, events, UI updates and stream deltas.
`Context` in `agent/context.rs` holds both records at once: `messages` (the
transcript, never pruned) and the parallel `entries` vector that carries a
cached token count per message plus the current `view_start`/`compaction`
state and transcript-indexed `model_overrides`. `model_messages()` builds the
request from the leading system block, summary/recovery handoff, latest task
anchor, and retained tail with bounded overrides. Compaction
estimates this model view plus active tool definitions with `cl100k_base`.
Private `context/projection.rs` owns model projection and persistence;
`context/recovery.rs` owns safe-boundary selection and lossy recovery.
Runtime callers use semantic Context methods for retry, batch accounting and
notice projection; neither message vectors nor projection flags are exposed.
Background summary requests receive an immutable prefix snapshot from Context.

`Context` caches the tool-definition token count for one active schema snapshot,
keyed by the exact name, description, and serialized parameter text (including
JSON field order). Budget checks reuse it until those inputs change; message,
window, and engine-dependent calculations remain live. The request-deferral gate
skips estimation when no background summary is running.
The OpenAI-compatible transport separately applies `cl100k_base` to the
serialized JSON request body after provider-specific fields and tools have been
added. This treats the JSON representation as text; it does not simulate
provider-side request processing or the model's tokenizer. The transport calls
the agent's estimate callback before waiting for the response, so the TUI footer
follows the latest request even when the provider takes a long time to respond.
When a compaction summary or synchronous drop changes the model view, the outer
loop emits a fresh model-view estimate with the active tool definitions and
context window until a later request estimate replaces it. `ContextUsageSource`
retains the estimate source internally; the footer displays only the token
count and percentage.
The same local estimate is paired with usage-only SSE chunks
(`usage.prompt_tokens` or `usage.input_tokens`) from that request. Each request
gets a correlation ID; the optional
`YLVA_TOKEN_USAGE_DUMP_PATH` writer appends a metadata-only record to one JSONL
file, leaving estimate and reported usage distinct. Request timestamps are
captured before send; the agent loop passes its persisted session ID through the
engine/provider boundary, and subagent requests carry their parent's session ID.
Records without a session association use `null`. The writer never serializes
the request messages, credentials, or body. This is an accuracy-observation dump,
not session data or billed usage. See
[CLI options](./cli-options.md#debug) for the record contract.

`src/agent/compaction.rs` owns the policy: `plan_compaction` maps one usage
evaluation onto `StartSummary`/`DropFallback`, `safe_boundaries_from` keeps
tool-call/result pairs intact, and the compaction coordinator's
`CompactionRuntime::sweep` — called only
from the outer loop at safe boundaries via `sweep_compaction_and_report`, after
completed turns and before tool-loop follow-up requests —
applies a finished summary, honors a forced fallback, then re-evaluates the
thresholds read from the active engine (`CompactionBudget::from_engine`), so
a `/models` switch re-prices the budget on the next turn. The three ratios
are validated as `0 < target < soft < hard ≤ 1` when the profile loads
(`validate_compaction_settings` in `src/config/profile.rs`), and
`COMPACTION.md` resolves like `SOUL.md` where the factory wires each agent
type's compact engine. An explicit profile/agent summarizer uses an independent
copy of its named engine's original configuration; without either engine field,
the factory supplies that agent's
conversation Engine runtime itself, so model/engine switches affect subsequent
summaries. Per-agent engine and timeout precedence is independent. The summary
deadline is read from `[compaction]` after
resolving any agent-type override, not from the compact engine's own model settings. It
defaults to 60 seconds and is validated as positive. Scheduling after every tool
outcome uses only completed eligible prefixes and never applies snapshots inside
an open batch. The source includes the previous summary/recovery handoff.
`start_request` is the shared preflight for every root/subagent continuation: it
applies ready state, defers oversized requests for an existing summary's remaining
absolute deadline, then measures staged recovery. The outer select loop remains
responsive to commands. Generation and request identity are checked before
clearing in-flight state; each waiting continuation is consumed once.
Preflight notices are generation-checked and acknowledged by the outer loop
before provider dispatch, preventing stale notices and preserving notice order.

The summary task emits a scoped `UiUpdate::CompactionActivity` start update with
a unique request ID. The presenter retains in-flight IDs per root/subagent
conversation and projects only the visible conversation's activity into the
footer's `· Compacting` suffix. This does not change `AgentMode`, input
routing, or idle behavior. A failure or timeout clears activity as soon as
`CompactionRuntime::on_outcome` records it — there is nothing left to apply.
A success instead leaves activity active until `CompactionRuntime::sweep`
reaches a safe boundary and actually applies (or fails to apply) the pending
snapshot, so the indicator never implies completion before the model view has
changed. Stop and clear abort the task and clear activity immediately;
request IDs prevent a late completion from clearing a newer request's
indicator.

`AgentFactory` supplies startup capacity checks for every configured agent type
besides the selected root. `AgentLoop::run` polls these checks and the root's own
check alongside commands. Once all startup checks finish,
`group_capacity_warnings` combines identical warning bodies into one notice
listing the affected agent types, in deterministic root-first check order.
Warning bodies include both provider/model identities, effective windows,
the soft threshold, and estimated prompt/request overhead, so different
capacity conditions remain separate. Notices appear even in an idle TUI;
dropping the loop cancels pending checks. The first conversation request waits for all startup
checks so a fast CLI turn cannot exit before checking unused agent types; stop
and clear discard that waiting request. Explicit context lengths avoid catalog requests.
`RecordModelChanged` schedules an immediate capacity recheck, polled alongside
commands and events rather than blocking controls on catalog metadata. A newer
switch replaces a pending switch check; dropping the loop cancels it. The check
uses the live conversation engine and the resolved summarizer engine, evaluating
their Engine runtime settings without sending a conversation request.
`EngineRouter` forwards catalog hydration
to the active named engine so checks use its catalog-derived context window.
`CompactionRuntime::capacity_warning` hydrates metadata for unconfigured context
lengths at startup, after model switches, and before a request's
generation-checked preflight notice. It compares
the conversation engine's soft trigger plus the estimated token overhead of an
empty summary request against the independent summarizer window, suppressing
unchanged warnings per run. Startup and request checks share an asynchronous
lock with switch checks so they neither duplicate the root warning nor race its catalog lookup.
`generate_summary` separately estimates the actual immutable summary request
after summarizer metadata hydration and within the summary timeout,
including message framing and the shared fixed request-overhead allowance, and
emits a root/subagent-scoped `UiUpdate::Notice` if it exceeds the summarizer window.
Neither check clips source messages or changes summary/fallback policy.
`generate_summary` collects the summary in a `BoundedBuffer` of
`COMPACTION_SUMMARY_MAX_BYTES` (256 KiB); a runaway summary fails the attempt
like an empty one, so the existing history is kept and fallback policy applies.
If catalog hydration changes the conversation window during preflight,
`ContextWindowChanged` re-enters the outer loop's budget gate before dispatch.

The TUI's `EngineDisplayInfo` snapshot includes both conversation and
summarizer identities plus `follows_conversation`/`shares_conversation` flags.
The `/models` detail area asks `Engine::preview_model_selection` for the actual
router target and computes the highlighted choice's summarizer effect without
mutating Engine runtime state. After activation, the driver refreshes root and stored
subagent display snapshots; inherited subagent views project the parent's live
Engine runtime identity. These fields are view-only and do not extend session
provider/model records.

Hard-limit recovery is `Context::recover_model_view`, using shared reducers in
`src/agent/compaction/recovery.rs`. Its five measured stages clip older outcome
bodies, drop older safe groups with a real-content handoff/anchor, clip retained
outcomes, shorten summary/handoff, and finally shorten the request only when the
safe ceiling still cannot fit. The ceiling subtracts 10% reply reserve, 5% safety
margin and 512 estimated tokens of overhead. Marker costs are reserved before
truncation; estimates include result envelopes, schemas, and the shared
`context.rs::TOKEN_FRAMING_OVERHEAD` per message and embedded tool call. The
target is best-effort when protected continuity already
fits the ceiling. An irreducible valid continuation fails explicitly before
provider dispatch. Recognized provider context rejections can retry once with a
75% request-local ceiling, only before response text or tool-call accumulation.
No operations are replayed. `RecoveryProjection` contains an implicit-user
handoff (`<ylva_recovery_handoff>`, up to 24 actual removed tool outcomes with
120-character excerpts, folded prior handoff) and an explicit-user anchor.
A later summary reaching its boundary supersedes it.

Tool batches: the `ToolCall` handler marks positions at or beyond
`MAX_EXECUTED_TOOL_CALLS_PER_BATCH` as skipped (`resolve_skipped_tool_call`
pushes one `ToolError` and a `ToolAutoDenied`). `tool_output_token_limit`
divides the remaining allowance (less 192 estimated tokens of error framing per
queued call) among unresolved budgeted calls, covering every tool except
`agent_finish`. `advance_tool_queue` pauses only for an existing useful summary
or skips a call whose share is below `MINIMUM_USEFUL_RESULT_TOKENS` (256).
Approval recomputes the allowance before execution. A `BTreeMap` assembles calls
in original index order; unknown/invalid/denied slots are never backfilled.
The six-position cap is code-owned, with no user configuration or adaptation.
Result/error commits preserve canonical source output, then add model-only
overrides. Tool-local byte/line/network limits remain source limits; the harness
does not call execution-time output hooks to destructively enforce context pressure.
Binary payload/descriptors are kept whole or omitted together in the projection.
Subagent event projection keeps valid structured lifecycle JSON and reduces only
result/error text; immutable metadata can still force subsequent group recovery.

`Context::summary_boundary_for_target` counts removable tokens from
`model_view_start`, not from the retained transcript's oldest entry. Every
new summary boundary therefore advances past the already summarized or
synchronously dropped prefix. With no visible tail it returns `None`, avoiding
repeated summary requests whose snapshots cannot advance the model view.

`[agents.<type>.compaction]` overrides the profile engine and timeout
independently. `AgentFactory::build` resolves these settings for both roots and
subagents. A subagent that goes idle waits for its in-flight summary; after a
summary or synchronous drop changes its model view, the loop resumes the task
instead of returning an incomplete result.

Each summary application/recovery has one compact INFO reduction diagnostic with
trigger, safe boundary, request identity, stage, before/after estimates,
projected/clipped counts, admitted/skipped calls, cap, remaining allowance and
terminal reason. Summary effectiveness additionally includes signed savings and
percentage change; negative savings are valid. Lower-level sweeps/effectiveness
details remain DEBUG. These are advisory local estimates without message content,
not provider-reported input usage.

Each serialized-request estimate logs `estimated_input_tokens`,
`context_length`, and `usage_ratio` with the root/subagent `run_id`. Each enabled
compaction sweep logs its model-view estimate, active window, `target_tokens`,
the configured thresholds, whether a summary is in flight, and the selected
`CompactionAction` — including `None`. These separate DEBUG events show the
footer request estimate and the compaction budget evaluation. The post-change
model-view estimate is also sent to the footer after compaction.

Persistence: `StoreEvent::Compaction` carries the live `boundary` the context
restore uses plus the `transcript_boundary` the TUI positions its marker
from (system prompts excluded, since they are never persisted). The marker
and the footer readout travel the standard update path — `UiUpdate` →
`DomainEvent` → `AppState` → `render`/`tui_widgets` — with the marker
inserted either at replay (`AppState::replay_messages`) or live
(`DomainEvent::CompactionApplied`, deduplicated by boundary).
Subagent replay reads messages and the latest transcript-relative compaction
boundary together through `Store::read_session_state` on first drill-in.
`AppState::load_subagent_messages` uses the same boundary-aware projection as root
replay and caches the projected entries; snapshots without that boundary
continue to load without a marker.
`StoreEvent::ModelProjection` stores bounded overrides/handoff/anchor with
`projection_revision` and transcript-relative coordinates. Replay picks the
highest revision at unchanged or advancing boundaries, validates IDs/names and
safe cuts, and rebases against rebuilt systems. Clone copies the events. Dirty
projection state is recorded before dispatch and after sweeps; hydration never
duplicates synthetic messages in the canonical transcript.
The sweep reports background-summary starts, completion, summary failure, and
synchronous prefix drops as `UiUpdate::Notice`; the TUI displays each notice in
the transcript. Completion notices include conditional guidance for frequent
compaction and its potential detail loss; synchronous-drop notices distinguish
removed model context from the preserved transcript. A compaction update replaces the stale serialized-request
estimate with a model-view estimate scoped to the root or subagent run. The
OpenAI-compatible provider binds a request-scoped failure callback to that
request's response dump, allowing caller-side compaction timeouts to append
their reason even after the streaming future has been dropped.

Before each budgeted tool executes, the outer loop calculates admission from the
projected model view, schemas, window, and response/safety/error reserves. After
execution, common result/error handlers enforce the recalculated model-only
allowance without changing the canonical body. Tools retain their own call
arguments and source byte/line caps while collecting output.
The `read file` DEBUG event logs full-source byte/line counts separately from
raw selected-content counts after the byte cap; the later `read result size`
event reports returned-content counts after tool-local caps. Source
line count is unavailable for binary reads and partial byte ranges.

Subagent completion events are checked against the parent's current model view at
delivery time, after applying any ready parent compaction when idle. An oversized
subagent report gets a marked deterministic model-only reduction, not a blocking
LLM summary. Canonical parent/subagent records and drill-in bubbles keep the complete
report. The projected event sets `result_reduced=true` and
`result_summarized=false`; canonical flags remain false. Normal parent prefix
summarization remains independent of this report projection.

## Persistence ownership and session cloning

`store.rs` retains `Store`, `StoreInner`, the session lifecycle API and the
shared title snapshot. Private `store/event.rs` owns persisted event types,
versions and timestamps; `store/session_file.rs` owns lock/open/header
operations and append-open corruption repair; `store/writer.rs` owns the
single writer task, locked-file lifetime, flush barriers and atomic title
rewrite; `store/replay.rs` owns read-only queries and session/subagent-run
reconstruction. Explicit facade re-exports preserve existing paths.
Replay never owns or updates title-generation state. The writer retains its
lock after replacing the metadata inode and drains queued events when the
last shared handle is dropped.

`private_fs.rs` holds the crate's file-creation helpers. `replace_file(path,
contents, unix_mode)` is the single atomic and durable publish path: it writes
a create-new temporary file `<name>.<uuid>.tmp` beside the target, sets the
exact mode before publishing, `fsync`s it, `rename()`s it over the target and
`fsync`s the directory, removing the temporary file on failure so the old file
stays intact. The title rewrite (`store/writer.rs`, `0600`), archive and
decompression transitions (`store/archive.rs`, `0600`), the workspace
`.gitignore` update (`init_command.rs`, `0644`) and shim scripts
(`profile_shim.rs`, `0755`) all use it. `private_open_options` and
`create_private_dir_all` create new files `0600` and directories `0700` on Unix.

For finished subagents, the title snapshot correlates `agent_finish` call
arguments with matching `ToolResult` IDs and names. Its terminal result/error
uses `AgentFinishArgs::resolved_status`, shared with execution, and takes
precedence over intermediate assistant text. Root title eligibility remains
text-reply based. Subagent persistence and resume both reconstruct this snapshot;
utility generation still uses `TitleAttempt`, with no transcript mutation or
task-label substitution.
`generate_title` reads at most `TITLE_RESPONSE_MAX_BYTES` (4 KiB) of the
response, then stops and cancels the stream; the title is cut from that prefix.
The whole title request is limited to `TITLE_REQUEST_TIMEOUT` (60 s); on
expiry the future is dropped, which cancels the provider stream, and the
failure is logged without a fallback title.

The OpenAI-compatible provider client is built by
`http::provider_client_builder` with a 30 s connect timeout and a 300 s idle
read timeout (`CONNECT_TIMEOUT`, `READ_IDLE_TIMEOUT`). The idle limit applies
to response headers and between body chunks, so a stalled server or stream
fails (and is retried for headers) while long, steadily streaming responses are
unaffected. These limits are not configurable. Web fetch/search clients and
compaction keep their own configured timeouts.

`store/tests.rs` holds shared fixtures and cross-owner lifecycle assertions;
event, session-file, writer, replay, archive and maintenance suites live under
`store/tests/`. Archive tests inject interruptions at publication/removal
boundaries and verify complete-byte recovery. Command subprocess tests isolate
the launch directory, state home and provider environment.

`Store::clone_session` (`src/store.rs`) reads the source under a nonblocking
exclusive lock held only for the read, rejects a subagent transcript or a
missing header before creating anything, then writes a fresh header
(`cloned_from`, `parent_session = None`, copied title/provider/model/agent
type) followed by every event except the source header, rebasing
`subagent_start`/`subagent_result` records onto the new id. The source file is
never repaired, rewritten, or appended to; subagent transcript files stay where
they are and are reused as read-only history. `Cli::clone` requires
`--conversation-id` (clap `requires`), and `open_store` in `src/main.rs`
routes the run to the clone instead of a resume.

`Store::set_title` rewrites the first line via temporary file plus `rename()`.
If the writer cannot reopen the file afterwards, it fails closed: later events
are dropped (and logged) and later title writes error, rather than appending to
the replaced inode.

## Subagent delegation internals

`spawn_subagent` returns the tool's single admission result. The terminal
outcome arrives later as a hidden harness notice because the tool protocol
allows only one result per `tool_call_id`. The root receives one continuation
after all spawned work finishes. `AgentCommand::Stop` cancels running and
queued work without resuming the root; `/clear` cancels both states as well.
`SubagentManager` tracks every running subagent by `run_id`, plus a FIFO queue
for spawns beyond `max_concurrent_subagents`. The profile default is `4`, and
`0` or a negative value means unlimited. `None` is the scheduler's unlimited
representation. The profile-level cap applies across the current one-level
delegation tree; subagent-role tool registries do not expose `spawn_subagent`.
When a subagent finishes, the scheduler removes only that run and starts queued
requests in FIFO order until the cap is full.
The tool description and intermediate completion notices tell the root to
end its response when no independent work remains, rather than keep thinking
while waiting. This is model guidance, not an agent suspension or a change
to completion scheduling; ending a response leaves subagents running.
Both the tool description and completion notices require a self-contained
answer with relevant findings when `all_done` is true, without assuming tool
results or subagent responses are visible to the user. Completion notices retain
this guidance in both canonical and reduced model projections; omitted report
details must not be invented. These are model instructions, not output validation.

Pending tool-approval and clarify updates carry a `run_id`. Drivers wrap the
response in `AgentCommand::ForSubagent`, and the root loop forwards it to the
matching subagent. `AgentLoop` logs and drops a stale or unknown `run_id`.
Streamed text and thinking are tagged with their subagent's `run_id` in the
factory's forward task before entering the shared subagent UI channel, so
concurrent streams cannot be attributed to whichever subagent started most
recently. That channel is unbounded, so no update is dropped and the send never
blocks the root loop that drains it; scheduler lifecycle updates use synchronous
sends. Each subagent reports its finish on a private capacity-1 channel, which the
driver forwards to the shared finish channel only after the subagent's UI forwarder
has drained. A subagent's last UI update therefore always precedes its finish. The
root loop also drains queued subagent UI updates before handling a finish. Stop
and clear-context await every cancelled subagent task before proceeding.

The TUI keeps pending approval ids in request order and displays only the head
of that FIFO queue. Replies remain keyed by tool id and are wrapped in the
owning subagent's `run_id`; a subagent awaiting a later prompt stays blocked without
pausing its running siblings. `AppState::running_subagents` tracks every live
subagent by `run_id`, separately from the retained `subagent_conversations` map.

### Drill-in TUI conversation view internals

`SubagentStarted` associates a subagent's `run_id` with the exact
`spawn_subagent` bubble id. Drill-in resolves that id rather than choosing the
most recently started subagent.

Live `SubagentConversation::started_at` uses a monotonic clock initialized by
`SubagentStarted`; replayed conversations have no live timer. Render projections
key these start times by the originating bubble id, so the root bubble and drill-in
header share one elapsed timer. Queued and terminal bubbles never render it.
Each tool-bubble render samples elapsed whole seconds once for both its live
title and cache key. Subsecond redraws reuse unchanged timer chrome; spinner
frame changes still update independently. Completed durations retain their
millisecond precision.
Live Thinking timers also use truncated whole seconds in cached and legacy
rendering, with the cached title and animation key sharing one sample.
Completed `Thought for` titles retain millisecond precision.

`AppState` keeps `active_view`, `running_subagents`, and `subagent_conversations`
(keyed by `run_id`) separate. Finished conversations remain available while
other subagents run, and the view model retains independent mode and transcript
state for every run. `SubagentManager` forwards token updates but consumes
subagent stream lifecycle events so they cannot change the root TUI mode.

`CompactionApplied` carries `run_id` from the agent sweep through the driver
to the presenter. The presenter inserts and deduplicates the live marker in
the owning root `chat_entries` or subagent `SubagentConversation::messages`,
independent of `active_view`. An unknown subagent ID is logged and discarded,
never treated as root compaction.

For a pending tool or clarify update tagged with `run_id`, `AppState::
note_interactive_pause` always switches `active_view` to wherever that bubble
actually lives — the root transcript when `run_id` is `None`/unrecognised, or
that specific subagent's transcript otherwise — regardless of which conversation
(root or a different, possibly already-finished, subagent) is currently on
screen. It saves the current view into `pre_pause_view` before switching and
`resolve_possible_interactive_pause` restores it once the pause resolves. It
skips the switch if the owning view is already open, so a manual drill-in does
not trigger an automatic return.
Outstanding approvals are queued in `pending_approval_order` across root and
subagent transcripts. Committing a verdict locally transitions its bubble out of
`Pending` and resumes the next request with its owning view.
Overlapping pauses preserve the first `pending_input_snapshot`; restoration
waits until no approval owns the input.
See [features](./features.md#drill-in-tui-conversation-view) for the visible
behavior.

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

## TUI module layout

### Module responsibilities

The TUI renders the same headless `AgentLoop`; it does not own agent state.

- `src/tui_reducer_state.rs` retains `AppState`, construction, render revisions
  and cross-cutting coordination, with explicit compatibility re-exports.
  Its private `conversation.rs` child owns entries, tool states and shared
  turn buffers; `conversation/replay.rs`, `conversation/navigation.rs` and
  `conversation/tools.rs` separate transcript projection, root/subagent navigation
  and bubble mutations. `selection.rs` owns terminal-column selection,
  `model_picker.rs` owns slash/model choices, filtering and the
  `PickerRowLayout` that splits each narrow-terminal model into identity and
  details rows,
  `input_history.rs` owns history browsing, `layout.rs` owns hit-test geometry,
  and `events.rs` owns input/domain events, modes, focus and effects.
  Descendant impl blocks operate on the same parent-owned `AppState`; its
  public fields and original type/function paths are unchanged.
- `src/presenter.rs` retains `update`, `TuiEvent` and `TextareaContext`.
  Its private `keyboard.rs` and `mouse.rs` children own focus-prioritized input
  routing; `domain.rs` owns mode and subagent lifecycle transitions; and
  `tool_bubbles.rs` owns shared pending/clarify/progress/completion transitions.
  The reducer still emits ordered effects without terminal or channel I/O.
  Both state and presenter retain facade-level cross-owner tests, with
  responsibility-specific suites under their local `tests/` directories.
- `src/render.rs` adapts reducer state to the view and synchronizes draw geometry.
  `src/tui_widgets.rs` retains `draw`, `draw_borrowed`, frame composition,
  borrowed conversation inputs and `ButtonAreas`. Its private `chat.rs` child
  owns legacy conversation painting and scrollbars; `chat/cache.rs` owns cached
  entry/header layout and visible-row painting. `bubbles.rs` owns shared message
  chrome and prose; `tool_bubble.rs` coordinates tool chrome and body rendering
  through `tool_bubble/title.rs` and `tool_bubble/body.rs`. Cached, legacy and
  subagent-header paths share this implementation. `footer.rs` owns status,
  key hints and approval controls; `overlays.rs` owns slash/model panels,
  clarify banners and toasts; `help.rs` owns the help popup.
  Pure terminal-column selection (`chat/selection.rs`), hunk parsing
  (`tool_bubble/diff.rs`) stay covered alongside all rendering owners.
  Home-relative labels (`home_relative_path.rs`) are shared by every user-facing
  path: `display_path` renders the real home directory as `~` in CLI output,
  errors, the help popup, and the PATH hint, while logs, tracing fields, dump
  files, and agent tool results keep absolute paths. The module carries its own
  unit tests; there are no custom coverage exclusions.
  Widget tests follow their responsibility owners. Rendering-owner tests check
  cache invalidation, layout and selection-storage reuse, and painted-row
  bounds; external TUI tests check visible rendering, selection and public
  live/replayed buffer equivalence. Large highlighting inputs are generated
  deterministically, without depending on implementation file size or contents.
- `src/tui_driver.rs` retains `TuiApp`, `run_tui`, the select loop, frame
  orchestration and session-title task coordination. Its private
  `tui_driver/effects.rs` child owns command routing and `PendingRunIds`;
  `models.rs` owns cancellable catalog tasks, restart after approval pauses,
  typed selection and runtime display refresh; `input.rs` owns textarea,
  draft/overlay snapshots, mouse selection and history operations;
  `updates.rs` owns live UI-to-domain mapping, lazy subagent replay and the
  compatibility snapshot; and `terminal.rs` owns clipboard implementations,
  terminal/title guards and stop-signal relay. Clipboard types retain their
  public facade paths. Pending destinations stay driver-local and are
  recorded before reducer transitions, then consumed by approval/clarify
  commands. Catalog cancellation, agent shutdown, terminal teardown and
  title restoration keep their existing order. `src/cli_driver.rs` owns
  the headless loop.

Both conversation render paths publish `ChatScrollbarLayout` with the visible
scrollbar rect and the exact maximum offset after layout. The presenter maps
arrow/track clicks through that geometry rather than the selection-line count,
which can include a trailing spacer omitted from the painted conversation.
The rect is empty when content fits; help and slash-overlay hit-testing retain
priority over scrollbar clicks.

Input height and mouse hit-testing use ratatui-textarea's native wrapped screen
map rather than a separate `textwrap` approximation. Hit-testing adds the rendered
viewport offset and uses native cursor movements to resolve screen cells,
including tabs and wide characters. After rendering, an origin cursor clamped
with `CursorMove::InViewport` on a clone supplies the actual viewport top;
recalled or replaced drafts cannot inherit a stale offset. Input mouse-wheel
events scroll the textarea, and the next frame synchronizes the offset.

The footer readout follows the request path end to end: the OpenAI-compatible
provider calls the estimate callback before response headers arrive; the agent
loop maps it to `UiUpdate::ContextUsage` or
`UiUpdate::SubagentContextUsage { run_id, .. }`. `AgentLoop::run` also emits a
`ModelView` baseline estimate once at startup (before any prompt), so fresh and
resumed sessions populate the footer without waiting for a request. Handling
`AgentCommand::RecordModelChanged` re-emits the same `ModelView` estimate, so a
model switch that changes the context window refreshes the percentage. The driver maps these to
root/subagent domain events; the presenter stores root usage on
`AppState::context_usage` and subagent usage on the matching `SubagentConversation`.
`AppState::active_context_usage` selects the readout for the currently rendered
root or subagent transcript, and
`render`/`tui_widgets` format it as `64.2K (45%)` on the right end of the
key-hints row (or the engine label/usage row below 80 columns). Finished subagent
estimates stay with their transcripts for later
drill-in. Headless runs drop both usage update variants.

### Conversation layout cache

`AppState::render_cache` owns one active-view `ConversationRenderCache`, with
separate list/header row indexes and one current width per surface. Each
`EntryLayout` shares immutable styled rows through `Arc`, together with cached
selection text, chrome-prefix widths, and entry-relative hit ranges. Keys
include entry slot/revision, width, theme, collapse state, and rendering/title
variant. Tool title/argument/error rows are separate from expensive section/
result rows; streaming thinking separates its elapsed title from its body.
Spinner/elapsed chrome updates patch only their selection rows. Changed chrome
height repairs offsets without parsing the unchanged body.

Production `render::render` uses `ConversationSource` to borrow the active
transcript, turn buffer, queued steers, and root header bubble. The legacy
`TuiState` shell omits these payloads. Selection vectors move into that shell
and back, rather than being cloned per frame. Existing owned widget callers
and the driver's returned snapshots remain supported.

Expansion toggles record `pending_bubble_expansion`. Before its normal frame,
the driver flushes `render_expansion_feedback`, which borrows the same content
but uses the target's collapsed layout and overlays `Expanding…` on its title.
Only after that terminal draw succeeds does `BubbleExpansionFeedbackShown`
clear the pending target and allow expanded layout. The feedback frame preserves
the conversation's scroll state so a temporary collapsed bottom cannot
re-enable auto-scroll. Collapse, clear, and view navigation discard pending
feedback; the subagent single/double-click window remains unchanged.

The driver opts into revision tracking through `AppState::enable_render_cache`.
Tool mutation helpers advance slot revisions; ordinary progress metadata
advances title revisions only (multiline progress also invalidates the body).
Presenter stream events advance the owning conversation's stream revision.
Appends leave preceding slots valid; compaction insertion invalidates the
owning transcript's slots. Replay, lazy load, clear, and navigation release or
invalidate derived rows. Background subagent updates leave an unrelated active
transcript valid; its displayed root header has independent keys. Collapse is
a layout dependency, not a content revision.
Root queued steers and their footer indicator are omitted from a subagent's
view, just like the root transcript and stream.

Public `AppState::new` callers use a conservative rebuild path because their
public vectors and strings can be edited arbitrarily. A caller explicitly
enabling tracking must use mutation helpers or call
`AppState::invalidate_render_cache` after direct field changes. No warm lookup
hashes or compares source payloads.

`RowIndex` stores cumulative exact heights and resolves visible rows with
indexed lookups. Only these rows are cloned/highlighted, then painted at local
offset zero; absolute offsets stay `usize`. Ordered selection text is retained
for off-screen copying. Index updates splice changed selection segments;
structural changes rebuild ordering from already-derived entry data. Cached
base styles never receive selection overlays.
Selection extraction and overlays use terminal-width graphemes, rather than
mistaking a wide glyph's cells for separate character indices.

Optional expanded layouts retained after collapse share
`COLLAPSED_BODY_BUDGET_BYTE_SIZE` across the list/header. Accounting sums each
retained layout's struct size, row/span/vector allocation capacities, owned
span-string capacities, selection-string capacities, prefix widths, and hit
ranges once per shared allocation. Static borrowed span text is not charged.
This is a derived-layout allocation estimate, excluding allocator/cache
bookkeeping overhead, not resident memory or a total-process limit. Optional
layouts exceeding the allowance are not retained; others use least-recently-
retained eviction. Re-expanding refreshes recency. Required current layouts
remain resident even when oversized. Revision/width/theme changes prune
obsolete optional layouts; clear/view switches drop previous-view storage.

Deterministic coverage includes warm body-build counts, visible-row copy
counts, real Rust file highlighting, large Markdown/plain fixtures, full
selection/copy data, and absolute bottom offsets beyond `u16::MAX`.

## Slash commands

`src/slash_command.rs` owns shared command-name constants, the pure parser,
and the fixed command allowlist. Draft normalization and model-filter extraction
use those constants rather than repeating command strings. Suggestions and
the help popup derive selectable spellings from the allowlist, listing canonical
names first and then aliases; alias entries retain the same command action.
The TUI driver handles engine-dependent effects separately from reducer effects.
The `/models` request runs in a cancellable background task, so the driver can
continue handling input and agent updates.

The pending input snapshot stores the slash overlay with the draft. Restoring
it after tool approval or clarify preserves fetched model choices that the
draft alone cannot reconstruct. See
[features](./features.md#slash-commands) for picker behavior.

## Steer ordering

While an agent is busy, the presenter shows a new steer as queued and the loop
keeps it outside `Context`. After `SteerApplied`, the loop commits it after the
current round's emitted messages. The persisted order then matches what the
model saw. If the round is cancelled first, the loop does not send the steer.
