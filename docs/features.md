# Ylva agent-harness features

## General

- Rust-based terminal AI agent application, runnable as a CLI
- Implements agent-harness functionality

## Build Info

Use `ylva --version` to print build provenance (`Version`, `Commit`,
`Built`, `Profile`).

## Current: headless agent (the kernel + real HTTP provider)

- Headless agent loop (`AgentLoop`): outer `tokio::select!` task owning
  `Context` and `AgentMode`, inner streaming task per request
- Dual-ish channel topology (`cmd_tx`/`cmd_rx`, `ui_tx`/`ui_rx`,
  `agent_tx`/`agent_rx`) with a `Provider` abstraction over the streams
- Cooperative steering: a `Prompt` during a stream is queued and injected at
  the next `StreamDone`; it never aborts the live stream
- Interruption: `Stop` fires a `CancellationToken`, preserves the partial as an
  `Assistant` marked `Interrupted`, and clears pending steers
- `Context` ownership is single-writer (the outer loop); final history is
  returned on shutdown when the `AgentLoop` is dropped — no `Arc<Mutex>`
- Two `Provider` implementations:
  - `OpenAiProvider` (`src/llm/openai_provider.rs`): raw `reqwest` +
    `eventsource-stream` targeting the OpenAI chat-completions spec for
    **both** OpenRouter and local Ollama via constructor configuration
  - `MockProvider`: deterministic fallback streaming tokens at a fixed interval,
    used in tests and when no backend is configured
- `base_url` is a generic provider concept: the concrete provider receives a
  backend origin + version prefix (`https://openrouter.ai/api/v1`,
  `http://localhost:11434/v1`) and appends `/chat/completions` internally; the
  trait carries no `base_url`/path semantics
- `serialize_messages` maps the domain `Message` history onto the wire format
  (system/user/assistant with `tool_calls`, plus `role:"tool"` messages for
  `ToolResult`/`ToolError`)
- Built-in `Message` types cover system/user/assistant (`tool_calls`) and
  `ToolResult`/`ToolError`; tool execution landed in Phase 3
- SSE tolerance: `[DONE]` sentinel, comment lines and reasoning/`role`-only
  deltas (neither `content` nor `tool_calls`) skipped — never surfaced as user
  text; `delta.tool_calls` chunks become `StreamEvent::Delta`; malformed
  payloads become `StreamEvent::Error`
- Pre-stream retry (`429`/`5xx`/connect): bounded exponential backoff (up to 3
  retries, base 1 s, max 30 s, jitter), honoring `Retry-After`; never retried
  mid-stream. Terminal pre-stream errors include a bounded snippet of the
  backend's response body (e.g. Ollama's `model not found`), not just the
  status code.
- Cancellation: firing the `CancellationToken` drops the request future, closing
  the underlying TCP socket promptly (verified hermetically via the mock
  server's live-connection counter)
- Demo wiring (`main.rs`, no CLI/config): provider selected from env vars with
  Ollama-first precedence (`OLLAMA_*`, then `OPENROUTER_*`), else `MockProvider`.
  The initial user turn is resolved by precedence: `--prompt <text>`, else
  piped stdin read to EOF (only when stdin is not a terminal), else the
  hardcoded demo prompt. This lets real ReAct/tool round-trips be driven
  against a live provider per invocation (e.g. `ylva --prompt '…'` or
  `echo '…' | ylva`).
  Setting `YLVA_VERBOSE=1` prints the selected provider, `base_url`, model, and
  key presence to stderr at startup for quick misconfiguration diagnosis
- Hermetic tests against an in-process mock SSE server (`tests/common/`) — no
  live OpenRouter/Ollama dependency in CI

## Phase 3: agent loop with tools (headless, faked approval)

- `ToolRegistry` (`src/agent/tools.rs`) + `Tool` trait: schemas are derived
  from typed argument structs via `schemars` (`JsonSchema`), never hand-written
  JSON (blueprint §4D). A `default_registry()` builds the three built-ins.
- Three file-system tools (`src/agent/tools/fs.rs`): `read` (path + optional
  `mode`/`offset`/`limit`, where `mode` is `Bytes` (default)/`Lines`/`Chars` —
  UTF-8 code points via lossy decode; `offset` is 0-based — line 1 is
  `offset: 0`), `write` (path + content,
  `needs_approval() == true`), and `glob` (glob-wildcard dialect via the `glob`
  crate, CWD-relative; no regexp). File I/O uses `tokio::fs`.
- `Provider::chat` gained a `tools` parameter; the request body carries the
  `tools` array and `tool_choice: "auto"` (omitted when no tools are
  registered). `serialize_messages` now serializes `Assistant.tool_calls` and
  maps `ToolResult`/`ToolError` to `role:"tool"` messages with `tool_call_id`.
- **Uniform tool-result envelope** (SSOT): every `role:"tool"` message carries
  its `content` as JSON derived from the single `ToolEnvelope` struct —
  `{"ok":true,"result":...}` on success, `{"ok":false,"error":...}` on failure
  — so the outcome is structurally explicit rather than inferred from an
  arbitrary reply. The protocol is taught to the model via
  `TOOL_PROTOCOL_SYSTEM_PROMPT`, a second system message injected right after
  the persona prompt, whose embedded JSON schema is generated from the same
  `ToolEnvelope` type (never hand-written, blueprint §4D).
- `StreamEvent::Delta` + `ToolCallAccumulator`: the provider emits raw
  `delta.tool_calls` fragments (never assembling them); the agent's inner
  streaming task owns a `HashMap<index, ToolCallAccumulator>` that concatenates
  arguments fragments in order and emits a batch of `AssembledToolCall`s on
  `Done`. Incomplete tool JSON surfaces as an error carrying the raw fragment.
- ReAct loop runs inside the existing biased `select!`: `AgentMode` gains
  `ToolPending`. On assembled tool calls the loop appends an `Assistant`
  message with its `tool_calls`, queues each call as a `PendingTool`, pauses at
  `ToolPending`, and does **not** start a new request.
- **Uniform approval protocol across all tools:** every call pauses and waits
  for `AgentCommand::ApproveTool { id }` or `DenyTool { id }` (or hard `Stop`).
  The loop holds no approval policy — the driver decides.
- **CLI-only, no interactive approval:** the `main.rs` driver auto-approves
  `read`/`glob` (immediate `ApproveTool`) and always **denies** `write` (no
  `--auto-approve-write` in Phase 3); a denial is fed back to the model as a
  `ToolError` + re-prompt, not an `Idle` return.
- Multiple tool calls in one message are queued (`VecDeque<PendingTool>`),
  drained **serially** (each awaiting approval, executing under its own
  `CancellationToken` + `tokio::time::timeout`, default 5 s), and re-prompted
  only after the queue empties. Completion is channel-based via `agent_tx`.
- `UiUpdate::ToolError` (and `ToolStart`/`ToolPending`/`ToolDone`/`ToolKilled`)
  report tool status; `ToolStart` carries the tool-call arguments. The `Tool`
  trait stays **presentation-light**: it exposes tool data plus the argument
  display metadata a host needs — which arguments are defaulted
  (`is_default_argument`) and their display order (`argument_order`, lowest
  rank = primary/inline) — while rendering details (brackets, eliding long
  string values) remain the App's decision. The CLI host prints
  `[tool] Read docs/features.md [limit=32]` (primary `path`/`pattern` inline,
  remaining **non-default** arguments as `key=value`, long string values
  elided) using the tool's `title` as the label. `read`'s returned file
  contents are kept out of the console ([tool] `Read` done) to avoid flooding
  it, but are still committed to the final JSONL history.
- Infinite loops are prevented by a max-iteration guard (`MAX_ITERATIONS = 15`,
  module-level; per-engine configurability is Phase 5). It stops re-prompting
  within a single user turn once exceeded.
- Steering in `ToolPending` is deferred to Phase 4 (no interactive steering in
  CLI-only mode): a `Prompt` still queues into `pending_steers` and injects at
  the next natural point; `descope_pending_tools` is not yet built.
- `AgentLoop` is reusable (provider + `ToolRegistry` + context) so Phase 6
  child agents can instantiate it with a restricted tool whitelist.
