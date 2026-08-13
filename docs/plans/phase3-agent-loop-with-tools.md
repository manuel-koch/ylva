# Phase 3: The Agent Loop with Tools (Headless)

## Context

### System / component being changed

The **Ylva agent-harness** (Rust). Phases 1 and 2 are complete and committed:

- **Phase 1**: a headless agent kernel (`AgentLoop` in `src/agent.rs`) driven by
  a **mock** provider (`MockProvider`), proving the dual-channel topology
  (`cmd_tx/cmd_rx`, `ui_tx/ui_rx`, `agent_tx/agent_rx`), cooperative steering,
  cooperative cancellation, and single-writer `Context` ownership.
- **Phase 2**: a **real HTTP provider** (`OpenAiProvider` in
  `src/llm/openai_provider.rs`) streaming `StreamEvent::Text` over `mpsc`
  against the OpenAI chat-completions spec (serves both OpenRouter and Ollama),
  with pre-stream retry and SSE tolerance.

This plan implements **Phase 3** of the [blueprint](../blueprint.md) §7: make the
agent a **ReAct agent with file-system tools and faked approval** — still
headless, no TUI. In the CLI-only host the approval protocol is exercised
end-to-end but there is **no effective human approval**: the driver
auto-answers every tool call (see "Approval protocol is uniform across all
tools" below).

### Goal and motivation

A working ReAct loop where the model may call tools (`read`, `write`, `glob`),
pause for approval, execute the tool, append the result to the
conversation, and re-prompt the model — with guardrails against infinite loops.
This is the first "real agent" behavior. It is also the point where the
tool-call assembly pipeline deferred in Phase 2 (blueprint §4A Step 3 refinement
and §7 Phase 2 bullet) lands: the provider starts surfacing tool-call **deltas**
as `StreamEvent::Delta`, and the agent's inner streaming task owns the
`ToolCallAccumulator`s.

### Acceptance criteria (definition of done)

- `schemars` is added and a `ToolRegistry` exists; tool schemas are derived from
  typed argument structs, never hand-written JSON (blueprint §4D).
- Three core tools with typed arguments are implemented: `read`, `write`,
  `glob` (blueprint §7 Phase 3).
- The ReAct loop runs inside the existing `select!` with an explicit
  `AgentMode` state machine extended to `ToolPending`:
  1. Stream the LLM response.
  2. Detect `tool_calls` from the stream (assembled from `StreamEvent::Delta`
     fragments in the agent's inner streaming task).
  3. **Pause**: emit `UiUpdate::ToolPending`, do **not** start a new request.
  4. Wait for `AgentCommand::ApproveTool` (only valid in `ToolPending`).
  5. Execute the tool via the registry.
  6. Append the result to message history.
  7. Re-prompt the LLM (new stream).
- Multiple tool calls in one message are queued (`VecDeque<PendingTool>`),
  drained **serially**, and the model is re-prompted only after the queue
  empties (blueprint §4B). Tool calls are never executed in parallel.
- Tool execution status is shown in the headless UI (stderr).
- An `UiUpdate::ToolError` variant reports failed tool invocations.
- `AgentLoop` is reusable (provider + registry + context) so Phase 6 child
  agents can instantiate it with a restricted tool whitelist.
- Infinite loops are prevented: a max-iteration guard (default 15,
  configurable per engine) stops re-prompting once exceeded.
- **Approval protocol is uniform across all tools**: every tool call
  (`read`, `write`, `glob`) transitions to `ToolPending` and waits for an
  `AgentCommand::ApproveTool` (or `Stop`); the loop holds **no** auto-skip
  branch. Approval *policy* lives at the driver/tool boundary.
- **Host = CLI-only, with no interactive approval at all.** The driver
  (`main.rs`) answers `read`/`glob` with an immediate `ApproveTool` (normal
  protocol); a `write` tool call is **always denied** via `Stop`, because no
  `--auto-approve-*` flag exists in Phase 3.
- **A denied tool is fed back to the model**: the loop appends
  `Message::ToolError` for the denied call and **re-prompts**, letting the model
  react (it does **not** return to `Idle`). The denial is policy-driven, not an
  approve-then-fail flow.
- Tests drive approval/denial via `send_command(ApproveTool)` /
  `send_command(Stop)`.
- The **real `OpenAiProvider` works through the tool loop**: a live
  OpenRouter/Ollama session (`main.rs`, env-var-selected) calls tools, pauses
  at `ToolPending`, is approved/denied, appends the tool result, and
  re-prompts — proving the ReAct+tool flow on the production provider, not just
  the mock. Hermetic CI verifies the same loop via `MockProvider`.
- `src/lib.rs` re-exports every new public item so `tests/*.rs` can reach it.
- `make lint`, `make test`, and `make test-coverage` (≥80% on testable files)
  pass.

### Key constraints

- **The `Provider` trait signature changes.** Phase 2's `Provider::chat(messages,
  cancel)` becomes `chat(messages, tools, cancel)` (blueprint §4A), so the
  request body can carry the `tools` array. `MockProvider` and `OpenAiProvider`
  and every test call-site update accordingly. The request body is the
  `{"model":…,"messages":…,"tools":…,"tool_choice":"auto","stream":true}` shape.
- **`StreamEvent::Delta` appears in Phase 3.** Phase 2 deliberately skipped
  `tool_calls`-only chunks (`parse_sse_payload` returned `None`). Phase 3 adds a
  `StreamEvent::Delta(DeltaChunk)` variant and makes the provider emit it.
  Reasoning/`role`-only deltas (with no `content` and no `tool_calls`) must
  still be skipped (Spike 3 findings, blueprint §4A note).
- **Tool assembly lives in the agent's inner streaming task**, never in the
  provider (blueprint §4A Step 3 refinement and §7 Phase 2 bullet). The provider
  emits raw deltas; the inner task owns a `HashMap<usize, ToolCallAccumulator>`
  and emits a batch of assembled `ToolCall`s to the outer loop on `Done`. The
  outer loop stays the sole writer of `Context`.
- **Single-writer `Context`** (unchanged): the inner task receives a `Vec<Message>`
  snapshot and never holds `Context`.
- **Context ownership of assistant/tool messages**: the outer loop appends an
  `Assistant` message with its `tool_calls` and then a `ToolResult`/`ToolError`
  per executed call. `serialize_messages` must now serialize `Assistant`.
  `tool_calls` and `ToolResult`/`ToolError` (currently skipped in Phase 2).
- **Steering and interruption stay distinct.** A `Prompt` received while in
  `ToolPending` is a **steer**: queued in `pending_steers` and injected at the
  next natural point (per blueprint §4C, at a tool-approval hand-off the steer
  supersedes pending tools via `descope_pending_tools`).
  `Cmd → Stop` is the only abort: it cancels the current tool's
  `CancellationToken` and clears pending tools.
- **No config parsing, no TOML, no CLI** in Phase 3 (blueprint §7 scope). The
  max-iteration constant has a module-level default; per-engine configurability
  is deferred to the config/engine phase (Phase 5).
- **Hermetic tests; live provider is the demo path.** CI runs only the
  in-process `MockProvider` / mock SSE server (`tests/common/`) — no live
  OpenRouter/Ollama in CI. The **real** `OpenAiProvider` (Ollama and OpenRouter,
  selected via the same env-var wiring `main.rs` already uses) must **work**
  end-to-end in Phase 3: `main.rs` drives a live ReAct+tool session through it
  exactly as it drove Phase 2 streaming. Tool file tests use `tempfile` scratch
  directories, never real paths.
- **KISS / YAGNI.** Do not build `edit`, `bash`, `search`, `clarify`,
  `spawn_subagent`, diff views, or child agents — those are Phase 6. Only
  `read`, `write`, `glob` in Phase 3.

### Concrete starting point

Open `src/agent.rs`: it currently defines `AgentMode` (only `Idle`/`Streaming`),
`AgentCommand`, `AgentEvent`, `Message`, `Context`, and `AgentLoop`. Add
schemars and create a `src/agent/tools.rs` module + `src/agent/tools/fs.rs`
submodule (blueprint §3 layout, file-per-module style per `AGENTS.md`) with the
`Tool` trait and `ToolRegistry`. First edits: extend `AgentMode`, `AgentCommand`,
`AgentEvent`, `UiUpdate`, `StreamEvent`, and `Context` to the Phase 3 shapes
below, then add the `tools` module and the `Provider` signature change.

## Implementation Plan

1. **Add dependencies** to `Cargo.toml`: `schemars = "0.8"` (derive `JsonSchema`),
   `tempfile = "3"` (scratch dirs for `write`/`glob` tests), and `walkdir = "2"`
   (directory enumeration backing the `glob` tool). Keep `serde`'s `derive`
   feature (already present). Run `cargo build` to lock versions. If a more
   faithful glob is needed than a `walkdir`-based matcher, prefer the `glob`
   crate; do not add both.

2. **Shape the Phase 3 enums/types in `src/agent.rs`:**
   - `AgentMode`: add `ToolPending`. Do **not** add `ToolExecuting` (the tool
     task runs concurrently with the loop; awaiting-approval and running are
     distinguished by the `ToolStart`/`ToolDone`/`ToolError` `UiUpdate`s).
   - `AgentCommand`: add `ApproveTool { id: Uuid }` and
     `DenyTool { id: Uuid }` (a denial is fed back to the model as a
     `ToolError` + re-prompt, distinct from the hard-interrupt `Stop`).
   - `AgentEvent`: add `ToolCall { calls: Vec<AssembledToolCall> }`.
   - `UiUpdate`: add `ToolStart { name: String, id: Uuid, arguments: Value }`,
     `ToolPending { name: String, id: Uuid }`,
     `ToolDone { id: Uuid, name: String, result: String }`,
     `ToolError { id: Uuid, error: String }`, `ToolKilled { id: Uuid }`.
   - `StreamEvent`: add `Delta(DeltaChunk)` where `DeltaChunk {
     tool_call_index: usize, tool_call_id: Option<String>,
     function_name: Option<String>, arguments_fragment: Option<String> }`
     (blueprint §4A).
   - `Context`: add `push_tool_result(id, name, result)` and
     `push_tool_error(id, name, error)`, plus a method
     `push_assistant_tool_call(content, status, tool_calls)` (or extend
     `push_assistant` to take `tool_calls`). `descope_pending_tools` is **not**
     implemented in Phase 3 (deferred to Phase 4 steering; see step 6).
   - `AssembledToolCall { id: Uuid, name: String, arguments: Value }` and
     `PendingTool { call: AssembledToolCall, timeout_secs: Option<u64> }`
     (blueprint §4B). `timeout_secs` is set at queue time from a tool-defined
     default (5s read/glob/write). `PendingTool::execute` dispatches to the
     registry and completes via the `agent_tx` channel (see step 6) so the loop
     never awaits a tool task directly.
   Add/adjust unit tests for the new variants and the `Context` methods.

3. **Add the `tools` module** at `src/agent/tools.rs` (blueprint §3):
   - `tools.rs`: a `Tool` trait (native-async via `BoxFuture`, consistent with
     `Provider`) exposing `name()`, a `schemars`-derived parameter schema, a
     `needs_approval()` flag (with a **default of `true`**, safe-by-default;
     read-only tools override to `false`), and an async `run(&self, arguments:
     Value) -> Result<String, ToolError>`. `ToolRegistry` holds `HashMap<String,
     Arc<dyn Tool>>` with `register`, `get`, and `names`/`schemas` accessors so
     the `AgentLoop` builds the `tools` array by runtime filtering (§4D dynamic
     filtering; Phase 3 filters nothing yet, but keep the seam).
   - `fs.rs`:
      - `read`: args `{ path: String, mode: ReadMode, offset: Option<usize>,
        limit: Option<usize> }` where `ReadMode { Bytes, Lines, Chars }` (default
        `Bytes`). `offset`/`limit` count the selected unit (see Q2 resolved notes
        in Implementation Hints). `needs_approval() == false` — but see Q5: this
        is an **interim/fake** read-only marker; comment it as such, since the
        real policy (Phase 6 sandbox) is path-based and an out-of-workspace
        `read` may require approval.
      - `write`: args `{ path: String, content: String }`, `needs_approval() ==
        true`, `timeout_secs` per `WriteFileArgs` with
        `#[serde(default = "default_timeout")]`.
      - `glob`: args `{ pattern: String }`; `needs_approval() == false` (same
        interim-marker comment as `read`). Only the **glob dialect** is
        accepted (`*`, `?`, `**`, `[...]`), never regexp — document this in the
        tool `description`. Patterns resolve **CWD-relative**, and absolute
        path patterns are supported; a workspace root/base arg is **deferred to
        Phase 6** (see Q7).
      - Per-tool timeout override keeps the **`0 == use-default`** sentinel
        (`u64`); comment explicitly that a timeout is **always enforced — there
        is no "no-timeout" value**; `0` just selects `DEFAULT_TIMEOUT_SECS` (see
        Q6).
      - Use `tokio::fs` for I/O; wrap in `tokio::time::timeout` (default 5s for
        file I/O, §4D).
   - `ToolError`: a small `thiserror` enum mapping to the flat `String`-returning
     `PendingTool::execute` contract and to `UiUpdate::ToolError`/`Message::ToolError`.
   Unit test each tool against `tempfile` scratch dirs (create → read → glob),
   including the `read` `mode` selector (`Bytes`/`Lines`/`Chars`).

4. **Extend `Provider` and the providers** for tools:
   - `src/llm.rs`: change `Provider::chat` to take `tools: Vec<ToolSchema>`
     (define `ToolSchema` — the OpenAI `{"type":"function","function":{name,
     description, parameters}}` envelope wrapping a `schemars` root schema).
     Keep the native-async `BoxFuture` shape.
   - `MockProvider`: add an optional tool-call script so deterministic ReAct
     tests can emit `StreamEvent::Delta` fragments followed by `Done`. Design:
     extend `MockProvider::new(events, interval)` to accept a
     `Vec<StreamEvent>`-like script (Text + Delta), or add a distinct
     `ScriptedProvider` in `tests/common/`. Prefer extending `MockProvider` so
     both providers share one deterministic path.
   - `OpenAiProvider`: include `tools` (and `tool_choice: "auto"`) in the
     request body; in `parse_sse_payload`, parse `delta.tool_calls[i]` into
     `StreamEvent::Delta` and stop skipping tool-call chunks. Update the Phase 2
     unit test `parse_delta_without_content_is_skipped` — its `tool_calls`-only
     branch is now a `Delta`, not skipped; reasoning/`role`-only chunks stay
     skipped.
   - `serialize_messages`: serialize `Assistant.tool_calls` (per-call
     `{id, type:"function", function:{name, arguments}}`) and map
     `Message::ToolResult`/`ToolError` to `role:"tool"` messages carrying
     `tool_call_id` and `content` (instead of skipping them). Update the Phase 2
     unit test `serialize_messages_skips_tool_messages_silently`.

5. **Rework `start_request`** (the inner streaming task in `src/agent.rs`) to
   assemble tool calls from `StreamEvent::Delta`:
   - Build `tools` from the registry and pass them to
     `provider.chat(messages, tools, cancel)`.
   - Maintain `HashMap<usize, ToolCallAccumulator>`. For each `Delta`, create or
     update the accumulator at `tool_call_index` (name, arguments-fragment
     buffer, id). `ToolCallAccumulator{ id, name: Option<String>, arguments:
     String }` with `into_tool_call()` (blueprint §4A). Multi-chunk argument
     deltas must concatenate in order.
   - On `[DONE]`: if there are accumulated calls, finalize each
     (`arguments` parsed as `serde_json::Value`) into `AssembledToolCall`s and
     send `AgentEvent::ToolCall { calls }`; otherwise send
     `AgentEvent::StreamDone { partial }` as today. If the stream ends with
     **incomplete** tool JSON, emit an error carrying the raw fragment
     (blueprint §4A note).
   - On `Cancelled`, tool accumulators are dropped (blueprint §12 Q14).
   - The inner task still never writes `Context`.

6. **Rework the outer `AgentLoop` loop** to add the `ToolPending` mode, the
   serial pending-tool queue, approval, and the iteration guard:
   - Maintain `pending_tools: VecDeque<PendingTool>` and a
     `current_tool_cancel: Option<CancellationToken>`.
   - On `AgentEvent::ToolCall { calls }`: append one `Assistant` message carrying
     the `tool_calls`, queue each call as a `PendingTool`, transition to
     `ToolPending`, send `UiUpdate::ToolPending`, and do **not** start a new
     request. This path is identical for every tool — the loop never decides
     approval scope.
   - Drain the queue **serially**: for each pending tool, stay in `ToolPending`
     and wait for `AgentCommand::ApproveTool { id }` (only handled in
     `ToolPending`); then **spawn** the tool in its own task under
     `tokio::time::timeout` with the pending tool's `timeout_secs` (default 5s)
     and a per-tool `CancellationToken` (`current_tool_cancel`). **Completion is
     channel-based**: the tool task sends a `ToolDone`/`ToolError` via
     `agent_tx`; the loop receives it on the next `select!` iteration and stays
     non-blocking (so `Stop`/`DenyTool` remain responsive mid-tool). On result
     the loop appends `ToolResult`/`ToolError` and emits
     `ToolDone`/`ToolError`/`ToolKilled`. Move to the next queue entry. When the
     queue empties, **re-prompt** the model (new stream) — this is the
     "internal deferred re-prompt" the blueprint mentions at the empty-prompt
     site (§7 Phase 3, Q26).
   - **Denied tool (policy-driven, CLI no-interactive):** the driver sends
     `AgentCommand::DenyTool { id }` (not `Stop`) to reject a pending `write`
     call. The loop appends `Message::ToolError { name: "write", error:
     "<denial message>" }` to `Context` and **re-prompts** (a new stream) so the
     model sees the denial and can react — it does **not** return to `Idle`
     after a denial. The denial is indistinguishable from a failed tool in the
     loop's eyes; the iteration guard still applies.
   - **Iteration guard**: count ReAct iterations (requests) within the current
     user turn; when `> 15` (module-level `MAX_ITERATIONS` const), stop
     re-prompting, return to `Idle`, and emit `UiUpdate::Error` (or a
     `ToolError`) explaining the guard tripped.
   - `AgentCommand::Stop` in `ToolPending` (the `Ctrl+C`-style hard interrupt,
     deliberately distinct from the per-tool denial above): cancel
     `current_tool_cancel`, clear `pending_tools`, emit `ToolKilled`, return to
     `Idle`.
   - Steering in `ToolPending`: **deferred to Phase 4** (no interactive
     steering exists in CLI-only mode). Keep the existing Phase 1/2 behavior — a
     `Prompt` while `ToolPending` queues into `pending_steers` and injects at
     the next natural point; do **not** implement `descope_pending_tools` yet.
   - Keep `CurrentAgentCommand`/`current_cancel.take()` semantics so a stale
     `StreamCancelled` from a replaced stream is ignored.

7. **Headless demo wiring in `src/main.rs`** (keep `main.rs` thin): the
   existing `ui_rx.recv()` printer gains handlers for `ToolStart`/`ToolPending`
   (print "Tool: {name} …" to **stderr**), and for `ToolDone`/`ToolError`/
   `ToolKilled` (print result/error). **No interactive approval in CLI mode:**
   - On `ToolPending` for `read`/`glob` → the driver sends an immediate
     `ApproveTool { id }` (normal protocol; no special-casing in the loop).
   - On `ToolPending` for `write` → the driver **denies** via
     `DenyTool { id }`: it has no `--auto-approve-write` flag in Phase 3; the
     denial is fed back to the model as a `ToolError` and the agent re-prompts.
   (Blueprint: "No TUI yet. Approval flow is stdin-based or hardcoded for now." —
   this plan resolves it to **no interactive approval at all in the CLI-only
   phase**.) For live ReAct/tool demos, accept a **minimal `--prompt <text>` CLI
   argument** in `main.rs` (keep it thin — a single optional arg, not a full CLI
   parse; no clap in Phase 3); when absent, read **all of stdio piped into the
   process** (only when stdin is not a terminal) to fill the prompt; when there
   is no piped input either, fall back to the hardcoded
   demo prompt. This makes a real `write`/`glob`/`read` tool round-trip runnable
   per-invocation against Ollama/OpenRouter instead of only the fixed demo
   string. Construct the `AgentLoop` with a `ToolRegistry` containing
   `read`/`write`/`glob`, and wire it to the **same provider selection `main.rs`
   already uses**: Ollama-first (`OLLAMA_*`), then OpenRouter (`OPENROUTER_*`),
   else `MockProvider`. The live `OpenAiProvider` must drive a real ReAct+tool
   round-trip (approve the auto-approved `read`/`glob`, deny `write`, see the
   result, re-prompt) — that live path is the Phase 3 demo, not just the mock.
   Add a `tools_reusable_check` manual smoke: run `main.rs` against Ollama and
   OpenRouter at least once and confirm `ToolPending` → `ToolDone` → re-prompt.

8. **Write the Phase 3 tests** (TDD; split per the repo's unit/integration
   convention):
   - `src/agent/tools/` unit tests: registry register/lookup/unknown-name;
     `schemars` schema for each tool (correct `name`, `parameters.required`);
     `read` (existing + missing file, `offset`/`limit`), `write` (creates file;
     `needs_approval()` true), `glob` (pattern matches in a scratch dir).
   - `src/agent.rs` in-module tests: `ToolCallAccumulator` assembly (single chunk
     vs fragmented arguments across indices), incomplete-JSON error,
     `Context::push_tool_result`/`push_tool_error`/`descope_pending_tools`, and
     the new enum-variant construction.
   - `src/llm/` unit tests: `parse_sse_payload` now yields `Delta` for
     `tool_calls`-only chunks and still skips reasoning/`role`-only; update the
     existing Phase 2 assertions accordingly; `serialize_messages` with
     assistant `tool_calls` and `ToolResult`/`ToolError`.
   - `tests/` integration tests (drive via `send_command` + `collect_until`,
     using an extended `MockProvider` that emits a tool-call script):
     - A model stream with a tool call → `ToolPending`, model is **not**
       re-prompted; `send_command(ApproveTool)`. ReAct → `ToolDone`, result
       appended, and a **new** `StreamStarted` (re-prompt) follows.
     - Two tool calls in one message → both queued and drained **serially**
       (second does not start until first `ToolDone`), single re-prompt after.
     - A tool that errors → `UiUpdate::ToolError` and a `Message::ToolError` in
       the final context; the loop still re-prompts.
     - **A denied tool** (an auto-`read` approved via `ApproveTool`, then a `write`
       denied via `DenyTool { id }`) → `Message::ToolError` appended, a **new**
       `StreamStarted` re-prompt follows (the model gets the denial), and the
       loop does **not** return to `Idle` immediately.
     - `Stop` during `ToolPending` (hard interrupt, the `Ctrl+C` analog) →
       `ToolKilled`, pending tools cleared, back to `Idle`.
     - A tool that never finishes → hard `Stop` mid-tool → `ToolKilled`, returns
       to `Idle`, no orphaned task.
     - Iteration guard: a scripted provider that always asks for a denied tool →
       exactly `MAX_ITERATIONS` re-prompts then stops with the guard error.
   - Keep the existing `tests/steering.rs`, `tests/interrupt.rs`,
     `tests/concurrent_steers.rs`, `tests/http_provider.rs`, `tests/empty_prompt.rs`
     green (their `MockProvider::new(tokens, interval)` and `Provider::chat`
     call-sites change to the new signature; mechanical updates).

9. **Documentation step** (docs are part of the change set — update in the same
   commit):
   - `docs/features.md`: add a "Phase 3: Agent loop with tools (headless)"
     section describing the `ToolRegistry`, the three tools, the
     `StreamEvent::Delta` + `ToolCallAccumulator` assembly, `ToolPending` mode,
     serial execution, stdin approval, `UiUpdate::ToolError`, and the iteration
     guard.
    - `docs/blueprint.md`: check off the Phase 3 `[ ]` boxes in §7, update the
      Phase 3 Goal line to "faked approval (no effective human approval in
      CLI-only mode)" to match the locked approval model, and update §4A's note
      to record the final `Provider::chat` `tools` parameter and the
      `StreamEvent::Delta` return-to-provider outcome (reconciling the "provider
      does not emit Delta" Phase 2 refinement bullet with Phase 3 now emitting
      deltas while the **assembly** stays in the agent's task).
   - `docs/configuration.md`, `docs/keybindings.md`: unaffected (headless, no
     config/keys landed in Phase 3). Verify and leave unchanged.
   - `docs/plans/.gitkeep`: unaffected.
   - Architecture decision records: `docs/architecture-design-records/` is empty
     (only `.gitkeep`); no ADR covers tools yet. Before coding, re-check this
     directory; if a relevant ADR exists by then, keep the implementation
     consistent with it.

10. **Verify** with `make lint`, `make test`, and `make test-coverage` (≥80% on
    testable files; `main.rs` excluded). Run `rumdl` on the touched markdown.
    Fix all findings before reporting done.

## Proposed Improvements

- **Workspace/sandbox containment (Phase 6+):** replace the Phase 3
  read/glob-auto / write-denied approximation with real **path** containment —
  auto-approve only inside the current workspace root and its subdirs, deny
  out-of-workspace paths. This makes approval **path-dependent**: an
  out-of-workspace `read`/`glob` would then require approval (so `read`/`glob`
  `needs_approval() == false` is only an interim marker, not the final policy —
  see Q5). It also introduces the workspace **root/base directory** that
  `glob` currently resolves from CWD (see Q7). Defines what counts as
  "inside the workspace" and supplies a base for relative file-tool paths.
  This is a refinement of the same `approval_scope()`/permission predicate;
  Phase 3 does not build it.
- **Keep `ToolSchema`/`ToolRegistry` behind a small seam** (an `allowed_tools`
  filter on registry read) so Phase 6 child agents and Phase 5's runtime
  filtering plug in without changing `AgentLoop`.
- **Fail fast on unknown tool names**: an `AssembledToolCall` naming an
  unregistered tool should surface `ToolError` rather than be silently ignored.

## Consistency Issues

- [high] **`Provider::chat` signature change ripples through every caller and
  test.** The Phase 2 `Provider` trait (`chat(messages, cancel)`), its two
  implementations, and ~half a dozen integration/unit tests all call it. Step 4+5
  must update every call-site in the same commit, or `make test` breaks
  mid-implementation. This is not a design conflict — it is the intended §4A
  contract — but it must be done atomically with the `tools` parameter added.

## Implementation Hints

- **`Provider` trait** (`src/llm.rs:23`) is currently `fn chat<'a>(&'a self,
  messages: Vec<Message>, cancel: CancellationToken) ->
  BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>>`. The `tools: Vec<
  ToolSchema>` parameter replaces `messages: Vec<Message>`'s position or is added
  after it — agree on argument order and update all callers. `MockProvider`
  (`src/llm.rs:39`) and `OpenAiProvider` (`src/llm/openai_provider.rs:63`)
  both implement it.
- **`serialize_messages`** (`src/llm.rs:92`) currently skips
  `ToolResult`/`ToolError` and serializes `Assistant` content only. Phase 3 moves
  tool messages into the wire format as `role:"tool"` with `tool_call_id`;
  `Assistant` gains `tool_calls`. The OpenAI chat-completions shape for a tool
  result is `{"role":"tool","tool_call_id":"<uuid>","content":"<result>"}`.
- **`parse_sse_payload`** (`src/llm/openai_provider.rs:144`) currently returns
  `None` for the `tool_calls`-only case (`parse_delta_without_content_is_skipped`
  at `src/llm/openai_provider.rs:338`). Phase 3 must turn that branch into a
  `StreamEvent::Delta`. Keep skipping reasoning/`role`-only deltas.
- **`start_request`** (`src/agent.rs:259`) is the inner streaming task and is
  where `ToolCallAccumulator`s live; `stream.recv()` loop is the correct place to
  match `StreamEvent::Delta`. It already forwards `Done`/`Cancelled`/`Error` to
  `agent_tx`; add the `ToolCall` branch on `Done`.
- **Outer loop** (`src/agent.rs:175`, the spawned `tokio::task` in
  `AgentLoop::run`) owns `context`, `mode`, `current_cancel`, `pending_steers`.
  Add `pending_tools`, `current_tool_cancel`, and the tool-`cmd` arms beside the
  existing `agent_rx`/`cmd_rx` arms. `select!` is `biased` today
  (`src/agent.rs:183`) — preserve that property for deterministic tool/token
  ordering.
- **`Context`** (`src/agent.rs:90`) has `new()`, `push_user`, `push_assistant
  (content, status)` (hard-codes empty `tool_calls` — needs to accept tool
  calls), `messages()`. `Message::ToolResult`/`ToolError` and
  `Message::Assistant.tool_calls` already exist as data shapes but are unproduced
  in Phase 1/2. Modularize `src/agent.rs` into `src/agent/tools/` once the agent
  file exceeds the repo's ~150-line flat-file threshold (blueprint Q24), keeping
  `src/lib.rs` re-exports stable.
- **Re-exports** (`src/lib.rs`): add `DeltaChunk`, `AssembledToolCall`,
  `PendingTool`, `ToolSchema`, and the `tools` module to the `pub use`/`pub mod`
  so `tests/` can reach them.
- **`Token call deltas arrive in fragments** (Spike 3): `function.arguments`
  arrive split across chunks for some models (e.g. deepseek streamed over 7
  chunks / 5 fragments). The accumulator must concatenate `arguments_fragment`
  in order and only parse JSON once complete; do not parse the name/arguments on
  every fragment.
- **`schemars` 0.8** derives `JsonSchema` from `#[derive(JsonSchema,
  Deserialize)]` structs (blueprint §4D). `schema_for!(ReadFileArgs)` yields a
  `RootSchema`; wrap its `schema` into the OpenAI `{"type":"function","function":
  {name, description, parameters}}` envelope when building `ToolSchema`.
- **Existing `MockProvider` script** (`src/llm.rs:44`): to keep tests
  deterministic under `tokio::time::pause`, the extended script must sleep on a
  fixed interval between events and still be cancel-aware (`select!` the send
  against `CancellationToken`), matching the current implementation.
- **Approval model (locked):** the loop enforces one protocol for all tools —
  `ToolPending` → wait for `ApproveTool` (approved) or `DenyTool` (denied,
  fed back as `ToolError` + re-prompt) or hard `Stop` (abort) → execute. The
  loop holds **no** approval policy; the driver decides. Add an
  `approval_scope()` (or equivalent `needs_approval()`/`is_auto_approved()`)
  method on the `Tool` trait that the driver reads to choose
  immediate-`ApproveTool` (`read`/`glob`) vs `DenyTool` (`write`). `main.rs` and
  tests both answer via commands — never by a loop-side bypass. CLI-only means
  **no interactive approval**; `write` is always denied in Phase 3 (no
  `--auto-approve-write`), and the denial is fed back as a `ToolError` +
  re-prompt, not an `Idle` return.
- **Iteration guard**: a single `MAX_ITERATIONS: usize = 15` const suffices for
  Phase 3 (per-engine configurability is Phase 5's `EngineConfig`). Reset the
  counter when a fresh user turn (rather than a tool re-prompt) starts the loop.
- **Test helpers**: `tests/common.rs` provides `collect_until`; the mock SSE
  server `tests/common/mock_llm_server.rs` already supports custom `frames`
  (via `content_frame`) and a status list — extend it, if needed, with a
  `tool_call_frame` helper that builds a `delta.tool_calls` SSE frame so a real
  `OpenAiProvider` ReAct round-trip can be integration-tested.
- **Steering in `ToolPending` is deferred to Phase 4** (no interactive
  steering in CLI-only mode). Phase 3 keeps the existing Phase 1/2 steer
  behavior (`Prompt` → `pending_steers` → inject at next natural point) and
  does not build `descope_pending_tools`; note this in `docs/features.md`.
- **`read` `mode`/`unit` selector** (`src/agent/tools/fs.rs` `ReadFileArgs`):
  add `mode: ReadMode { Bytes, Lines, Chars }` defaulting to `Bytes`
  (`#[serde(default)]`). `Bytes` = raw byte slice; `Lines` = split on `\n`
  (lossy decode then line-slice); `Chars` = count UTF-8 code points via
  `char_indices()`/lossy decode. Only decode when the selected mode needs it
  (`Lines`/`Chars`), not for `Bytes` (binary-safe). The tool `description`
  must state the units so the model chooses correctly.
- **`glob` (`src/agent/tools/fs.rs` `GlobArgs`)** uses the `glob` crate
  (Cargo.toml already adds `glob = "0.3"`), so only wildcard patterns are
  valid; a leading `[` is a `PatternError` (the existing
  `glob_invalid_pattern_is_error` test). Patterns are CWD-relative; the test
  `glob_matches_pattern_in_scratch_dir` passes an absolute
  `"{dir.path()}/*.txt"` pattern, confirming absolute-path support.
- **`needs_approval()`** (`src/agent/tools.rs` `Tool` trait) currently has no
  default and all three tools implement it (`read`/`glob` → `false`,
  `write` → `true`). Change the trait to provide a default of `true` and have
  `read`/`glob` override with `false` plus a comment labeling it an
  interim/fake read-only marker pending Phase 6 path-based approval (Q4/Q5).
- **`timeout_secs`** (`src/agent/tools/fs.rs`): each tool struct carries a
  `timeout_secs: u64` field where `0` selects `DEFAULT_TIMEOUT_SECS` (the
  `ReadTool::with_timeout`/`timeout_secs()` pattern). This is the agreed
  `0 == use-default` sentinel; add a doc comment stating a timeout is always
  enforced and there is no "no-timeout" value (Q6).

## Resolved Post-Implementation Questions

These were raised after the first Phase 3 implementation pass and are now
resolved. The resolutions are folded into Implementation Plan steps 3 and 7,
the sandbox note in Proposed Improvements, and the Implementation Hints below.

- **Q1 — Live-provider testing / setting the prompt from the CLI.**
  Add a minimal `--prompt <text>` CLI argument to `main.rs` (a single optional
  arg; no full CLI/clap parse in Phase 3). When absent, read **all of stdin**
  (piped input, i.e. when stdin is not a terminal) to fill the prompt; when
  neither `--prompt` nor piped stdin provides input, fall back to the hardcoded
  demo prompt. This lets a real `write`/`glob`/`read` ReAct round-trip be
  driven against Ollama/OpenRouter per invocation instead of only the fixed
  "sky looks blue" demo string. (See step 7.)
- **Q2 — `ReadFileArgs` `offset`/`limit`: bytes or lines?**
  A `mode`/`unit` selector `{ Bytes, Lines, Chars }` (default `Bytes`) is added
  so the model chooses the unit. `Chars` counts **UTF-8 code points** via lossy
  decode — one can only know what a "char" is given an encoding, and Rust
  `str`/`char` semantics are defined for UTF-8 only; for binary files only
  `Bytes` is meaningful. No encoding *detection* is added (out of scope). The
  tool description tells the model the units. (See step 3.)
- **Q3 — `GlobArgs` `pattern`: glob chars or regexp?**
  Glob (wildcard) dialect only (`*`, `?`, `**`, `[...]`), never regexp, matching
  the `glob` crate and the blueprint example `src/**/*.rs`. Documented in the
  tool description. (See step 3.)
- **Q4 — default `Tool::needs_approval()` should be `true`.**
  Safe-by-default: any tool requires approval unless it explicitly opts out with
  `false` (read-only marker). A future tool like `clarify` overrides as needed;
  a forgotten mutating tool is denied, never silently auto-approved. (See
  step 3.)
- **Q5 — `read`/`glob` returning `false` is the *interim* (fake) flow.**
  The loop protocol is uniform — every tool pauses at `ToolPending` and receives
  `ApproveTool`/`DenyTool`/`Stop`. `needs_approval() == false` on `read`/`glob`
  is only the *driver hint* that makes it auto-approve today; it is **not** the
  final policy. Once the Phase 6 sandbox lands, an out-of-workspace `read`/
  `glob` will require approval, so even read-only tools can report
  `needs_approval() == true` per-path. Mark the `false` with a comment stating
  it is interim/fake and path-independent-for-now. (See step 3 + sandbox note.)
- **Q6 — `timeout_secs` keeps `0 == use-default` (u64), not `Option<u64>`.**
  `0` is the "use `DEFAULT_TIMEOUT_SECS`" sentinel. Comment explicitly that a
  timeout is **always enforced — there is no "no-timeout" value**; `0` merely
  selects the module default. `#[serde(default = "default_timeout")]` gives the
  same clean default for parsed args without an `Option`. (See step 3.)
- **Q7 — `glob` base directory is CWD-relative.**
  Patterns resolve relative to the process CWD (`src/**/*.rs`); absolute-path
  patterns (`/abs/path/*.rs`) search outside the workspace. Phase 3 has **no
  workspace root** concept yet — a base directory arg and the notion of
  "in/out of workspace" are deferred to the Phase 6 sandbox
  (see Proposed Improvements). Document the CWD-relative behavior in the tool
  description. (See step 3.)
