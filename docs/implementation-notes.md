# Implementation notes

This is a contributor reference. Keep details only when code alone does not
explain them: cross-module interactions, constraints or workarounds, and design
decisions behind behavior described in [features](./features.md). Keep notes
brief and link to user-facing docs rather than repeat their contracts.

## Layered configuration and persona assembly

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
type's compact engine, and that framing instruction reaches every background
summary request; loops built outside the factory keep the built-in prompt. The
request places the framing instruction in a system message, followed by the
immutable prefix and a final user trigger. The built-in instruction calls out
those message boundaries so source conversation text is not mistaken for new
summarizer instructions.

## Engine and provider runtime configuration

`EngineImpl` and `OpenAiCompatibleProvider` share one
`EngineRuntimeConfigHandle`. The provider snapshots it before building a
request and releases the lock before network I/O. Updates therefore affect
later requests without changing settings in an in-flight request.

Catalog hydration runs once per engine before its first chat. A `/models`
selection supplies that model's metadata, so hydration skips another fetch.
The engine does not repeat a hydration lookup after its one attempt; transient
HTTP `429`/`5xx` responses inside that attempt use the shared bounded retry
policy below. If hydration rejects a profile reasoning value, the engine
queues a notice; `EngineRouter` drains it and `AgentLoop` forwards
`UiUpdate::Notice` before `StreamStarted`.

Each provider wrapper supplies a request-body customization function. The
shared transport leaves provider-specific wire fields to those wrappers.

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
rejects a new distinct index once `MAX_TOOL_CALLS_PER_RESPONSE` is reached,
cancels the provider stream, and sends a dedicated limit event to the outer
loop. The outer loop discards the partial batch, records a hidden harness notice
explaining the limit, charges the rejection as one tool-use iteration, and
re-prompts when the per-turn iteration budget permits. No tool execution,
approval bubble, or per-call failure bubble is created for the rejected batch.
The per-response limit is separate from `max_tool_iterations`, which counts
rounds after tool execution completes.

## Binary result projection

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

## Sandbox `cross_filesystem` rule

`resolve_device` reads Unix `st_dev` on each check and returns no device on
other platforms. A nonexistent target uses its deepest existing canonicalized
ancestor as the device anchor. No cache means remounts affect the next check.
See
[configuration](./configuration.md#sandbox-tool-approval-rules) for the rule's
contract.

## `bash` tool process-group cleanup

On Unix, `run_bash` kills the process group with `SIGKILL` on timeout or
cancellation, so grandchildren do not outlive the command. See
[features](./features.md#tools-to-be-used-by-models) for timeout and output
limits.

## CLI driver

The CLI builds a separate registry for tool display. It must receive the same
`[web_search]` and `[web_fetch]` configuration as the execution registry, or
the displayed backend can differ from the one the agent calls.

With `--auto-approve-spawn`, `cli_driver` keeps the process alive after the
root's `StreamDone` until every spawned call reaches a terminal outcome. The
immediate `spawn_subagent` result reports admission, not completion. See
[features](./features.md#cli-runtime) for the CLI behavior.

## Store and persistence

`Store` clones share one locked file and writer task. Record methods enqueue
without blocking; `flush` waits until the writer drains queued events. See
[persistence](./persistence.md) for message-commit, replay, and on-disk
contracts.

## Context management and compaction

`Context` in `src/agent.rs` holds both records at once: `messages` (the
transcript, never pruned) and the parallel `entries` vector that carries a
cached payload token count per message plus the current `view_start`/`compaction`
state. `model_messages()` builds the request from the leading system block,
the marked summary projection, and the tail after the boundary. Compaction
estimates this model view plus active tool definitions with `cl100k_base`.
The OpenAI-compatible transport separately applies `cl100k_base` to the
serialized JSON request body after provider-specific fields and tools have been
added. This treats the JSON representation as text; it does not simulate
provider-side request processing or the model's tokenizer. The transport calls
the agent's estimate callback before waiting for the response, so the TUI footer
follows the latest request even when the provider takes a long time to respond.
The same local estimate is paired with usage-only SSE chunks
(`usage.prompt_tokens` or `usage.input_tokens`) from that request. Each request
gets a correlation ID; the optional
`YLVA_TOKEN_USAGE_DUMP_PATH` writer appends a metadata-only record to one JSONL
file, leaving estimate and reported usage distinct. Request timestamps are
captured before send; the agent loop passes its persisted session ID through the
engine/provider boundary, and child requests carry their parent's session ID.
Records without a session association use `null`. The writer never serializes
the request messages, credentials, or body. This is an accuracy-observation dump,
not session data or billed usage. See
[CLI options](./cli-options.md#debug) for the record contract.

`src/agent/compaction.rs` owns the policy: `plan_compaction` maps one usage
evaluation onto `StartSummary`/`DropFallback`, `safe_boundaries_from` keeps
tool-call/result pairs intact, and `CompactionRuntime::sweep` — called only
from the outer loop at safe boundaries via `sweep_compaction_and_report`, after
completed turns and before tool-loop follow-up requests —
applies a finished summary, honors a forced fallback, then re-evaluates the
thresholds read from the active engine (`CompactionBudget::from_engine`), so
a `/models` switch re-prices the budget on the next turn. The three ratios
are validated as `0 < target < soft < hard ≤ 1` when the profile loads
(`validate_compaction_settings` in `src/config/profile.rs`), and
`COMPACTION.md` resolves like `SOUL.md` where the factory wires each agent
type's compact engine. The summary deadline is read from `[compaction]` after
resolving any agent-type override, not from the compact engine's own model settings. It
default to 60 seconds and is validated as positive.

`[agents.<type>.compaction]` overrides the profile engine and timeout
independently. `AgentFactory::build` resolves these settings for both roots and
children. A child that goes idle waits for its in-flight summary; after a
summary or synchronous drop changes its model view, the loop resumes the task
instead of returning an incomplete result.

Compaction lifecycle logs measure the model view immediately before and after
applying a summary or synchronously dropping old messages. They emit estimated
token counts, signed net savings, percentage change, and projected message
counts. A negative token saving is valid when the generated summary is larger
than the replaced estimate; a later sweep may need to trim further. These
values use the same `cl100k_base` estimate as budgeting, not provider-reported
input usage.

Each serialized-request estimate logs `estimated_input_tokens`,
`context_length`, and `usage_ratio` with the root/child `run_id`. Each enabled
compaction sweep logs its model-view estimate, active window, `target_tokens`,
the configured thresholds, whether a summary is in flight, and the selected
`CompactionAction` — including `None`. These separate DEBUG events show the
footer request estimate and the compaction budget evaluation.

Persistence: `StoreEvent::Compaction` carries the live `boundary` the context
restore uses plus the `transcript_boundary` the TUI positions its marker
from (system prompts excluded, since they are never persisted). The marker
and the footer readout travel the standard update path — `UiUpdate` →
`DomainEvent` → `AppState` → `TuiState` → `tui_widgets` — with the marker
inserted either at replay (`AppState::replay_messages`) or live
(`DomainEvent::CompactionApplied`, deduplicated by boundary).
The sweep reports background-summary starts, completion, summary failure, and
synchronous prefix drops as `UiUpdate::Notice`; the TUI displays each notice in
the transcript. A `ContextUsageCleared` update invalidates the latest request
estimate when a summary or drop changes the model view. The OpenAI-compatible
provider binds a request-scoped failure callback to that request's response
dump, allowing caller-side compaction timeouts to append their reason even
after the streaming future has been dropped.

## Session cloning

`Store::clone_session` (`src/store.rs`) reads the source under a nonblocking
exclusive lock held only for the read, rejects a child transcript or a
missing header before creating anything, then writes a fresh header
(`cloned_from`, `parent_session = None`, copied title/provider/model/agent
type) followed by every event except the source header, rebasing
`child_start`/`child_result` records onto the new id. The source file is
never repaired, rewritten, or appended to; child transcript files stay where
they are and are reused as read-only history. `Cli::clone` requires
`--conversation-id` (clap `requires`), and `open_store` in `src/main.rs`
routes the run to the clone instead of a resume.

## Child-agent delegation internals

`spawn_subagent` returns the tool's single admission result. The terminal
outcome arrives later as a hidden harness notice because the tool protocol
allows only one result per `tool_call_id`. The root receives one continuation
after all spawned work finishes. `AgentCommand::Stop` cancels running and
queued work without resuming the root.

Pending tool-approval and clarify updates carry a `run_id`. Drivers wrap the
response in `AgentCommand::ForChild`, and the root loop forwards it to the
matching subagent. `AgentLoop` logs and drops a stale or unknown `run_id`.

### Drill-in TUI conversation view internals

`ChildAgentStarted` associates a subagent's `run_id` with the exact
`spawn_subagent` bubble id. Drill-in resolves that id rather than choosing the
most recently started subagent.

`AppState` keeps `active_view`, `running_child`, and
`child_conversations` (keyed by `run_id`) separate. Finished conversations
remain available while another subagent runs, and the view model does not
assume execution is sequential. `ChildAgentManager` forwards token updates but
consumes subagent stream lifecycle events so they cannot change the root TUI
mode.

For a pending tool or clarify update tagged with the running subagent's
`run_id`, the TUI saves the current view before switching and restores it when
the pause resolves. It skips the switch if that subagent's view is already
open, so a manual drill-in does not trigger an automatic return.
See [features](./features.md#drill-in-tui-conversation-view) for the visible
behavior.

## TUI module layout

The TUI renders the same headless `AgentLoop`; it does not own agent state.

- `src/tui_reducer_state.rs` holds the view state, events, and effects.
- `src/presenter.rs` reduces input and domain events into effects without
  terminal or channel I/O.
- `src/render.rs` and `src/tui_widgets.rs` render the view and record geometry
  for mouse routing.
- `src/tui_driver.rs` owns the terminal loop, maps runtime updates to domain
  events, and executes effects. `src/cli_driver.rs` owns the headless loop.

The footer readout follows the request path end to end: the OpenAI-compatible
provider calls the estimate callback before response headers arrive; the agent
loop maps it to `UiUpdate::ContextUsage` or
`UiUpdate::ChildContextUsage { run_id, .. }`. The driver maps these to
root/child domain events; the presenter stores root usage on
`AppState::context_usage` and child usage on the matching `ChildConversation`.
`AppState::active_context_usage` selects the readout for the currently rendered
root or child transcript, and
`render`/`tui_widgets` format it as `64.2K (45%)` on the right end of the
key-hints row. Finished child estimates stay with their transcripts for later
drill-in. Headless runs drop both usage update variants.

## Slash commands

`src/slash_command.rs` owns the pure parser and fixed command allowlist. The
TUI driver handles engine-dependent effects separately from reducer effects.
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
