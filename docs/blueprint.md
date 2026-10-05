# Terminal AI Agent (Rust) — Full Build Plan

A practical blueprint for building a **terminal-first AI coding agent** in Rust
(similar to OpenCode or Hermes by Nous Research). It runs as a TUI application,
supports OpenRouter and local Ollama models, executes tools with user approval,
and allows real-time steering/interruption.

## 0. Proof-of-Concept Spikes

Before building the full architecture, validate the riskiest assumptions through isolated spikes.

### Spike 1: TUI Responsiveness Under Load

Create a minimal `ratatui` + `tokio` application that simulates an LLM stream.

- A tokio task generates artificial `StreamEvent::Text` tokens at **1000+ messages/sec**.
- The TUI event loop (`spawn_blocking` for crossterm + `select!`) receives these via `mpsc` and appends
  them to a scrolling buffer. The draw loop **batch-drains** all available tokens per frame via
  `try_recv()` (bounded per-frame cap) rather than one at a time, so it sustains ~60 FPS at high
  injection rates.
- Draw at ~60 FPS. Log frame times to verify no dropped frames, no input latency, and no consumer starvation.
  "No channel backpressure" is measured as **steady-state queue depth ≤ 1 frame's worth
  (~20 tokens) at 1000+ msg/s**,
  i.e. the consumer never falls behind — not as the producer never waiting on a full channel.
  A bounded channel (`send().await` throttles the generator under overload) keeps memory flat regardless.
- Demonstrate the spike generator's prompt-triggered cancel/restart path. This is a spike control,
  not a test of production cooperative steering; `tests/steering.rs` verifies that behavior.
- Test interruption: send a mid-stream `AgentCommand::Stop` — verify the stream cancels cleanly
  and a subsequent prompt restarts without UI glitches.

**Success criteria:**

- Frame time consistently ≤ 17 ms.
- Keyboard input (Enter, Ctrl+C, typing) responds instantly regardless of stream volume.
- Steady-state channel queue depth ≤ ~20 tokens at 1000+ msg/s
  (no consumer starvation / no effective backpressure).

The ten-minute memory soak is not an acceptance criterion. Peak buffered memory remains an
informational metric, and the ring buffer is capacity-bounded.

#### Spike 1 Outcome

Implemented as a standalone crate at `spikes/spike-tui-responsiveness`
(binary `ylva-spike-tui-responsiveness`), with run instructions and controls in
[`spikes/spike-tui-responsiveness/README.md`](../spikes/spike-tui-responsiveness/README.md).
The core pattern (bounded `mpsc` + `spawn_blocking` crossterm bridge +
`tokio::select!` + batch-drain per frame) works and validates the §4B design.

Measured on the default 60 s short run at 1000 msg/s:

- **Frame time:** avg ~1.3 ms (min ~0.1 ms, max ~2.5 ms), **0** frames above
  17 ms, histogram entirely in the `<8 ms` bucket → "≤ 17 ms" criterion **met**.
- **Queue depth:** peak ≤ 1 token (target ~20) → no consumer starvation, the
  "no effective backpressure" criterion **met** at 1000 msg/s.
- **Memory:** the scrolling ring is capacity-bounded and peak buffered bytes
  stayed flat on the short run. The ten-minute soak is not required.
- **Prompt demo:** an auto `Prompt` fires ~3 s into the run and cancels/restarts
  the generator cleanly. This does not test Ylva's non-cancelling steer
  behavior; production steering is covered by `tests/steering.rs`.
- **Implementation notes:** an initial bug (the generator's `send().await`
  was not cancel-aware) could deadlock shutdown/steering when the bounded
  channel was full; fixed by `select!`-ing the send against the
  `CancellationToken`. Frames are counted only on event-driven redraws, so
  the window tracks actual draw cadence, not a fixed timer.

### Spike 2: HTTP Stream Cancellation

Build a minimal raw `reqwest` + `eventsource-stream` client.

- Connect to an actual SSE endpoint (OpenRouter streaming completion or a local mock server).
- Start receiving chunks. At a random mid-stream point, cancel by aborting the request
  future (`Response`, which drops the byte stream). Because dropping **only** the SSE
  `eventsource` adapter does not reliably close the underlying socket (§8 Pitfall #8),
  the spike verifies **three distinct mechanisms** — adapter-only drop, adapter + `Response`
  drop, and `CancellationToken` abort — and reports which actually closes the connection.
- Verify via `netstat`/`lsof` that the TCP connection closes immediately.
- Verify no zombie tokio tasks remain by inspecting the runtime task count.
- Repeat for both OpenRouter and Ollama (`/v1/chat/completions`) endpoints.

**Success criteria:**

- Aborting the **request future** closes the underlying HTTP connection immediately (no
  `CLOSE_WAIT` or `ESTABLISHED` linger). The SSE-**adapter-only** drop case is measured
  separately so the §4A cancellation path is proven rather than assumed (see §8 Pitfall #8).
- The tokio runtime shows zero leaked tasks.
- `CancellationToken` integration works reliably (no race between `cancelled()` and `stream.next()`).

**Spike 2 outcome** (verified against the hermetic mock in
`spikes/spike-http-stream-cancel/`, `cargo run --release`):

- **Pass — immediate close.** All three mechanisms (`drop:adapter-only`,
  `drop:adapter+response`, `token`) close the connection: the mock server's
  live-connection counter returns to `0` and `lsof`/`netstat` show no
  `ESTABLISHED`/`CLOSE_WAIT` linger. Measured server-side close latency is
  ~2 ms (the earlier ~100 ms reading was `lsof` subprocess spawn overhead, not
  connection linger).
- **Pass — zero leaked tasks.** `num_alive_tasks()` returns to the pre-run
  baseline after a 339-connection sweep, and `shutdown_timeout` completes.
- **Pass — no cancel race.** A 300-iteration `biased`-select token sweep fired
  the token at a tight mid-stream boundary with **zero** deliveries of more
  than the tolerated single in-flight chunk after the token fired.
- **Pitfall #8 adjudication:** with `reqwest`, `Response::bytes_stream(self)`
  **consumes** the `Response`, so the SSE adapter owns the byte stream and a
  bare adapter-only drop already drops the whole request future — all three
  mechanisms behave identically and never leak the socket for this exact stack.
  The §4A cancellation pattern (§4A note lines ~287-311) is therefore sound.
- Default/CI runs are hermetic; OpenRouter (`OPENROUTER_API_KEY`) and Ollama
  (`/v1/chat/completions`) remain opt-in real-endpoint targets via `--endpoint`.

### Spike 3: Ollama OpenAI-Compatible Tool Calling

Before Phase 2 commits to a **single** unified provider targeting the OpenAI
chat-completions spec, validate that Ollama's `/v1/chat/completions` endpoint
actually supports the `tools` parameter and streams `delta.tool_calls` chunks
identically to OpenRouter. If it does not, Phase 2 must add a second
`ollama_native` provider (POST to `/api/chat` with manual tool-injection
prompting) and the "unified provider" claim must be revised.

- Send a raw `curl` POST to `http://localhost:11434/v1/chat/completions` with
  a `tools` array and a `tool_choice` of `auto`.
- Use a tool-capable local model (e.g. `llama3.1:8b`, `qwen2.5`, or
  `mistral-nemo`). Verify the model is pulled and running before the test.
- Inspect the SSE stream for `delta.tool_calls` fragments (the same shape
  OpenRouter returns: `tool_calls[0].function.name` and `.arguments`).
- If the endpoint rejects the request (HTTP 400/422) or ignores `tools` and
  returns plain text, the unified-provider assumption is **invalidated**.

**Success criteria:**

- Ollama `/v1/chat/completions` accepts a `tools` array in the request body
  and returns SSE chunks containing `delta.tool_calls` with incremental
  `function.name` and `function.arguments` fields.
- The streaming shape is **byte-for-byte compatible** with OpenRouter's
  `delta.tool_calls` format (no custom Ollama field names or nesting).
- If **any** criterion fails: document the failure mode, add a
  `ollama_native` provider to the Phase 2 plan, and remove the
  "single provider serves both backends" claim from §4A and §5.

**Spike 3 outcome:** **PASS** — the unified-provider assumption is validated; keep the
single provider serving both backends for Ollama and OpenRouter via the OpenAI
`/v1/chat/completions` spec. The comparison was recorded in
[`spikes/spike-ollama-tool-calling/`](../spikes/spike-ollama-tool-calling/);
raw SSE captures are not retained in the repository.

- **Criterion A (accepts `tools`, streams `delta.tool_calls`) — PASS.** HTTP 200 with at
  least one chunk carrying `choices[0].delta.tool_calls` with `function.name` and
  `function.arguments`:
  - Ollama `gemma4:e4b-mlx` (ID `e98b54fe1913`): `get_weather` →
    `{"location":"Berlin","unit":"fahrenheit"}`. All in a single chunk.
  - Ollama `gpt-oss:20b` (ID `17052f91a42e`): `get_weather` →
    `{"location":"Berlin, Germany","unit":"Fahrenheit"}`. Single chunk.
  - OpenRouter `deepseek/deepseek-v4-flash-0731`: `get_weather` →
    `{"location": "Berlin, Germany", "unit": "fahrenheit"}`. Streamed over 7 chunks with
    incremental arguments (5 fragments) — the direct evidence of incremental
    `function.arguments`. The model emits `reasoning`/`reasoning_details` deltas before the
    tool call.
  - OpenRouter `moonshotai/kimi-k2.6`: `get_weather` →
    `{"location":"Berlin, Germany","unit":"fahrenheit"}`. Streamed over 14 chunks with
    incremental arguments (12 fragments).
- **Criterion B (byte-for-byte OpenRouter-compatible shape) — PASS (live diff).**
  `inspect_sse.py --shape` of the Ollama and live OpenRouter captures yields an **empty
  diff** for **both** OpenRouter models (`deepseek/deepseek-v4-flash-0731` and
  `moonshotai/kimi-k2.6`): all produce
  `tool_calls[].{id,index,type,function}.function.{name,arguments}`
  with no backend-specific keys. The OpenRouter captures also confirm the incremental
  arguments accumulate identically to the documented shape. Note: an SSE stream may begin
  with comment lines (OpenRouter emits `: OPENROUTER PROCESSING`), so a stream must be
  judged by the presence of `data:` events, not by its first non-empty line. The
  parser-facing consequences of these runs (comment-line tolerance, reasoning deltas
  without `tool_calls`) are captured as an implementation note in §4A.
- **Architectural consequence:** keep the shared streaming transport (§2, §4A, §5). No
  `ollama_native` chat provider is required. **Base URL / HTTP constraint to note:**
  Ollama receives the server origin (`http://localhost:11434`) and derives
  `/v1/chat/completions` for chat plus native `/api` catalog requests, while
  OpenRouter receives `https://openrouter.ai` and derives `/api/v1`; both still share one client
  and one streaming request/parse path. Local Ollama holds the connection until
  the stream ends; unload via `ollama stop <model>` between model switches.
- **Models actually used:** `gemma4:e4b-mlx` (primary) and `gpt-oss:20b` (confirmation);
  the models named in §Spike 3 text (`llama3.1:8b`, `qwen2.5`, `mistral-nemo`) were not
  pulled and were not used. Raw capture files were not retained in the repository.

---

## 1. Vision & Architecture

> **Agent-harness naming decision: Ylva.** The agent-harness is named **Ylva** (see
> [naming-research.md](./naming-research.md)). Default config files live under
> `~/.ylva/`. The term "Ylva" refers to the harness itself; "agent" is the runtime
> loop that Ylva hosts.

The application is a **ReAct agent** embedded inside a **TUI event loop**.
The LLM streams responses asynchronously while the UI thread remains responsive
at ~60 FPS. The user can **interrupt** the model or **send steering messages**
at any time without waiting for the stream to finish.

**Steering and interruption are distinct:**

- **Steer** (user sends a new message while the agent is busy): never aborts a live
  generation. The message is drawn as a pending bubble and queued; it is injected at
  the next safe point — after the live response ends or after every issued tool call
  has its matching result. A normal steer cannot replace a pending approval or
  `clarify` result. Queued
  steers are appended in order (after flushing any partial assistant text to history)
  and then the agent starts a fresh request. Verified: no OpenAI-compatible SSE provider
  supports true mid-stream message injection, so a steer can only *wait for a natural
  point* or *abort-and-restart* — never splice into a live completion.
- **Interrupt** (`Ctrl+C` / `Esc`): hard-aborts the current round via `CancellationToken`,
  discards any pending steer queue, flushes the partial response to history, and returns
  to idle.

The outer agent loop owns the `Context`; the inner streaming task sends events back via a
dedicated channel and is never a direct writer to the message buffer. This single-writer
design eliminates lock contention on every token and prevents race conditions.

For OS-level interruption, install a `ctrlc` handler that sends `AgentCommand::Stop`
via the same `cmd_tx` channel used for keyboard events. Double-fire is safe because
the channel deduplicates; the handler gives the agent a clean shutdown path and
works even if crossterm's event queue is momentarily stuck.

```text
┌──────────────────────────────────────────────────────────────┐
│  TUI Thread (ratatui) — ALWAYS RESPONSIVE                    │
│  ┌───────────────────────────────────────────────────────┐   │
│  │ Chat / Code / Diff / Partial Stream (Scrollable)      │   │
│  └───────────────────────────────────────────────────────┘   │
│  ┌────────────────────────────────────────────────────────┐  │
│  │ Input Box  [Always active — type + Enter anytime]      │  │
│  └────────────────────────────────────────────────────────┘  │
│  [Ctrl+C] Stop Stream  |  [Enter] Send / Steer  |  Status    │
└──────────────────────┬──────────────────────▲────────────────┘
                       │                      │
           ┌───────────┴──────────────────────┴───────────┐
           │         Bidirectional Channels               │
           │  ui_tx ───────► UI updates (tokens, status)  │
           │  cmd_rx ◄────── Commands (prompt, stop,      │
           │                tool_approve, model_switch)   │
           └──────────────────────┬───────────────────────┘
                                  │
                    ┌─────────────▼─────────────────┐
                    │   Agent Task (tokio)          │
                    │                               │
                    │  loop {                       │
                    │    select! {                  │
                    │      // 1. Stream tokens      │
                    │      token = stream.next() => │
                    │        ui_tx.send(token)      │
                    │                               │
                    │      // 2. Handle commands    │
                    │      cmd = cmd_rx.recv() =>   │
                    │        match cmd {            │
                    │          Prompt(text) => {    │
                    │            // STEER: queue    │
                    │            // user msg,        │
                    │            // inject at next   │
                    │            // natural point    │
                    │          }                    │
                    │          Stop => {            │
                    │            // INTERRUPT:      │
                    │            // cancel token,   │
                    │            // keep partial    │
                    │          }                    │
                    │          ApproveTool(id) =>   │
                    │            // resume paused   │
                    │            // agent loop      │
                    │        }                      │
                    │    }                          │
                    │  }                            │
                    └───────────────────────────────┘
```

**Key principles:**

1. **The TUI thread never blocks on I/O.** All LLM and tool I/O runs in a `tokio` task
   and communicates back via channels.
2. **The input box is never disabled.** The user can type and hit Enter at any moment
   — even while tokens are streaming.
3. **Interruption is cooperative.** `Ctrl+C` / `Esc` fires the `CancellationToken` to abort the HTTP
   stream and any running tool, preserve whatever partial response was received, and return to idle.
4. **Steering never aborts.** A new user message sent while the agent is busy is queued and injected
   after the live response ends or after every issued tool call has its matching result — never by
   cancelling a live generation or replacing a pending tool response.

## 2. Crate Ecosystem

### TUI & Terminal

| Crate            | Role                                                     |
| ---------------- | -------------------------------------------------------- |
| `ratatui`        | Modern standard for Rust TUIs: layouts, widgets, buffers |
| `crossterm`      | Event handling, backend for ratatui, clear/flush         |
| `tui-textarea`   | Multi-line input with history and vim-like keybindings   |
| `syntect`        | Syntax highlighting for code blocks in chat bubbles      |
| `pulldown-cmark` | Parse markdown to render headers/code blocks distinctly  |
| `textwrap`       | Intelligent wrapping of LLM output to terminal width     |
| `unicode-width`  | Correct width measurement for CJK/emoji                  |

### Async & LLM

| Crate                            | Role                                                                                                       |
| -------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `tokio`                          | Async runtime. Agent runs in a task; TUI on main thread                                                    |
| `tokio-util`                     | `CancellationToken` for cooperative cancellation of streams                                                |
| `futures`                        | `StreamExt::next()` inside `tokio::select!` for the agent loop                                             |
| `async-openai`                   | ~~Primary for OpenRouter~~ **REMOVED** — see unified provider decision below                               |
| `ollama-rs`                      | ~~Native Ollama client~~ **REMOVED** — Ollama supports OpenAI-compatible API                               |
| `reqwest` + `eventsource-stream` | **Primary.** Custom unified streaming client for all backends. Direct HTTP control guarantees cancellation |
| `serde` / `serde_json`           | Messages, tool schemas, config serialization                                                               |
| `schemars`                       | **Critical.** Derive JSON Schemas from structs for LLM function calling                                    |
| `tiktoken-rs`                    | Token counting for OpenAI-compatible models                                                                |
| `ctrlc`                          | SIGINT handler that forwards `AgentCommand::Stop` into the agent channel                                   |

### Tooling & Agent Logic

| Crate                  | Role                                                                |
| ---------------------- | ------------------------------------------------------------------- |
| `thiserror` / `anyhow` | Error handling across tools and LLM calls                           |
| `tokio::sync::mpsc`    | Send tokens & tool results from agent task to UI                    |
| `ignore`               | Respect `.gitignore` when searching                                |
| `tempfile`             | Safe scratchpads for agent drafts before disk writes                |

### Persistence & Config

| Crate                  | Role                                              |
| ---------------------- | ------------------------------------------------- |
| `dirs`                 | Find `~/.ylva/` cross-platform                    |
| `config`               | Hierarchical configuration (default < file < env) |
| `toml`                 | Config serialization                              |
| `serde` / `serde_json` | Messages, tool schemas, config serialization      |
| `chrono`               | Message timestamps                                |
| `clap`                 | CLI argument parsing for headless mode            |

## 3. Project Structure

```text
src/
├── main.rs              # CLI args, mode dispatch (TUI vs headless), rt.block_on wrapper
├── app.rs               # App state (Mode, streaming_state, partial_msg)
├── tui_widgets.rs        # All ratatui widgets and layout math
├── events.rs            # Crossterm key → AppAction mapping (Ctrl+C, Enter)
├── config.rs            # Settings, API keys, model selection
├── store.rs             # JSONL persistence for chats (see §4H)
├── cli.rs               # Headless mode: arg parsing, fake TUI wrapper, exit codes
├── llm.rs               # trait Provider { async fn chat(...) }
├── llm/
│   └── openai_compatible_provider.rs  # Raw reqwest + eventsource-stream targeting OpenAI spec
│                           # Works for both OpenRouter and Ollama via config
├── agent.rs             # ReAct loop: select! { stream, cmd_rx }
├── agent/
│   ├── context.rs       # Message buffer, token counting, summarization,
│   │                    # PARTIAL message handling on abort
│   ├── tools.rs         # Tool trait + Registry
│   └── tools/
│       ├── fs.rs        # read, write, glob, edit
│       ├── shell.rs     # execute_command (with user confirmation!)
│       └── search.rs    # Built-in ignore + regex content search
├── widgets.rs           # module root (re-exports, shared widget state)
└── widgets/
    ├── chat_bubble.rs   # Custom ratatui widget for assistant messages
    ├── code_block.rs    # Syntect-highlighted block
```

## 4. Core Implementation Patterns

**Unified provider decision:** Ollama exposes an OpenAI-compatible API at
`/v1/chat/completions` (since v0.1.24).
Therefore, we do **not** need separate provider crates. A **single** raw `reqwest` + `eventsource-stream`
implementation targets the OpenAI chat-completions spec and works for both OpenRouter and Ollama via
configuration (`base_url`, `api_key`, `model`). This eliminates:

- Code duplication between two provider implementations.
- Hidden cancellation semantics inside convenience wrappers (`async-openai`, `ollama-rs`).
- The risk that one crate's stream adapter leaks connections when dropped.

All streaming logic — SSE parsing, `CancellationToken` polling, `ToolCallAccumulator`
— lives in one place.

### A. Unified Provider & Engine Traits (with Abort)

The runtime exposes **two** layers (see [class-diagram.mmd](./class-diagram.mmd)):

- **`Provider`** — low-level backend trait. A single raw HTTP implementation targets the OpenAI
  completions spec and supports **dropping the in-flight request** when the user interrupts or steers.
  Also exposes backend discovery (`list_models`).
- **`Engine`** — the runtime abstraction an `AgentLoop` depends on. A thin `EngineImpl` wraps a
  `Provider` plus the `EngineConfig` (model, temperature, `context_length`, ...), so the agent loop
  talks to one seam and can read sampling parameters without separately holding config data.

> **Terminology mapping:** the config calls this an **engine** (a named bundle of `base_url`, `model`,
> `temperature`, etc.). At startup the `Config` module builds a `HashMap<String, Arc<dyn Engine>>`
> from `EngineConfig` definitions + the resolved `Provider` per engine. The rest of the code only knows
> the `Engine` trait; "engine" is the config concept, `Engine` the runtime trait, `Provider` the backend.
>
> **`base_url` semantics (Phase 2 refinement):** `base_url` is a **generic provider concept**, not an
> OpenAI-endpoint string. Its meaning — which endpoints and request paths exist under that base — is
> defined **only inside the concrete provider implementation**, never in the `Provider` trait. The fact
> that Ylva currently ships an OpenAI-compatible provider must not leak the OpenAI path structure into
> the trait or into `base_url`'s meaning. The concrete
> `OpenAiCompatibleProvider` receives a `base_url` that includes the backend
> origin and version prefix for the OpenAI-compatible chat endpoint. OpenRouter
> instead receives its origin (`https://openrouter.ai`) and derives `/api/v1`.
> Ollama instead receives its server origin
> (`http://localhost:11434`) and derives `/v1/chat/completions` for chat plus
> native `/api` catalog endpoints. This leaves room for future provider-side
> queries under the provider's own paths.

```rust
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Native async trait (Rust 1.75+). The Provider creates an internal
/// `mpsc::channel` and returns the receiver, so retry logic and SSE
/// parsing live inside the implementation and never leak to the caller.
trait Provider: Send + Sync {
    /// Stream back partial text + potential tool_calls.
    /// The implementation polls the CancellationToken; when it fires,
    /// the underlying HTTP connection is dropped.
    fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> impl std::future::Future<Output = anyhow::Result<mpsc::Receiver<StreamEvent>>> + Send;

    /// Query the backend for available models (OpenRouter `/models`, Ollama `/api/tags`).
    /// Used by a future `/models` slash command and config auto-completion.
    fn list_models(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<ModelInfo>>> + Send;
}

/// Backend-reported model descriptor. All fields except `id` are optional because
/// backends return inconsistent metadata. The catalog's `context_length` is
/// shown in `/models` and supplies the engine's context limit when the profile
/// does not set `EngineConfig.context_length` explicitly.
struct ModelInfo {
    id: String,
    context_length: Option<usize>,
    /// `None` = unknown, `Some([])` = known text-only, otherwise the
    /// provider-advertised supported input modalities.
    input_modalities: Option<Vec<InputModality>>,
}

enum InputModality {
    Image,
    File,
}

/// Runtime abstraction an AgentLoop depends on. A thin `EngineImpl` wraps a
/// `Provider` plus the `EngineConfig` so the agent reads sampling parameters
/// (`engine.config().context_length`) without holding config data separately.
trait Engine: Send + Sync {
    fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> impl std::future::Future<Output = anyhow::Result<mpsc::Receiver<StreamEvent>>> + Send;

    fn config(&self) -> &EngineConfig;
}

/// Concrete `Engine` impl: a Provider + the config the engine was built from.
struct EngineImpl {
    provider: Arc<dyn Provider>,
    config: EngineConfig,
}

enum StreamEvent {
    Text(String),              // Regular text token
    Delta(DeltaChunk),         // Raw tool-call fragment (not yet assembled)
    Done,                      // SSE stream ended naturally
    Cancelled,                 // Emitted when the stream was aborted by user
    Error(String),             // HTTP or parse error
}

struct DeltaChunk {
    tool_call_index: usize,    // Which tool call in the array this chunk belongs to
    tool_call_id: Option<String>,
    function_name: Option<String>, // May be None if this chunk only contains arguments
    arguments_fragment: Option<String>,
}
```

**HTTP retry:** Pre-stream failures only (`429`/`5xx`/connect) with bounded exponential
backoff (max 3 retries, base 1 s, max 30 s, with jitter) honoring `Retry-After`;
never retry mid-stream. Retry logic lives inside the `Provider` implementation
wrapping the initial `send().await` before SSE begins; mid-stream errors emit
`StreamEvent::Error` and the agent loop surfaces them to the user.

**Implementation note for raw `reqwest`:**
The client builds a `reqwest::Request`, starts the SSE stream, and wraps the chunk polling inside `tokio::select!`:

```rust
let response = client.post(&self.base_url)
    .json(&request_body)
    .send()
    .await?;

let mut stream = response.bytes_stream().eventsource();

loop {
    tokio::select! {
        biased; // check cancel first
        _ = cancel.cancelled() => {
            // Dropping `stream` and `response` kills the TCP connection
            let _ = tx.send(StreamEvent::Cancelled).await;
            break;
        }
        item = stream.next() => {
            match item { ... }
        }
    }
}
```

**OpenRouter:** Configure `base_url = "https://openrouter.ai"` and pass the
OpenRouter API key. The provider derives `/api/v1`.

**Ollama:** Configure `base_url = "http://localhost:11434"`. The provider
derives `/v1/chat/completions` for chat and `/api/tags` plus `/api/show` for
model discovery. No API key is required for local Ollama.

**Tool call accumulation:** OpenAI streams tool calls incrementally across multiple
SSE chunks (`delta.tool_calls[0].function.arguments`). The **provider** does **not**
buffer or validate these fragments; it emits each chunk as `StreamEvent::Delta`.
The **agent's streaming task** owns the `ToolCallAccumulator`(s), buffering
fragmented name and argument deltas, validating JSON completeness, and only
emitting a `ToolCall` event (via `agent_tx`) once the full JSON is parseable —
the outer loop never sees raw deltas and stays the sole writer of `Context`.
If the stream ends with incomplete tool JSON, the accumulator emits an error
with the raw fragment for inspection.

> **Phase 3 outcome (reconciles the Phase 2 "provider does not emit Delta"
> refinement):** `Provider::chat` now takes a `tools: Vec<ToolSchema>` parameter
> and the request body sends `tools` (+ `tool_choice: "auto"`). The provider
> **returns to emitting `StreamEvent::Delta`** for `delta.tool_calls` chunks
> (raw fragments only, never assembled) — superseding the Phase 2 bullet — while
> the `ToolCallAccumulator` and assembly stay in the **agent's streaming task**,
> exactly as this §4A refinement and §7 Phase 2 Step 3 refinement describe.
> Chunks with neither `content` nor `tool_calls` are still skipped.

**SSE robustness & non-tool-call deltas (Spike 3):** local validation against Ollama
and live OpenRouter (`deepseek/deepseek-v4-flash-0731`, `moonshotai/kimi-k2.6`) showed
two parser-facing facts the unified provider must respect:

- **Comment lines.** OpenRouter emits SSE comment lines (`: OPENROUTER PROCESSING` text) —
  including mid-stream, not only at the head — so a stream must be judged by the presence
  of `data:` events, and any non-`data:` line (comments, keepalives, malformed frames) must
  be skipped, never treated as a payload.
- **Reasoning deltas.** Reasoning-capable models stream `delta.reasoning` /
  `delta.reasoning_details` (+ `content`, `role`) **before** the tool call. These arrive as
  deltas with **no** `tool_calls`. Consequence for the provider: a chunk with neither
  `content` nor `tool_calls` must not be emitted as `StreamEvent::Text` (it is not
  user-facing content) nor as `StreamEvent::Delta` (it carries no tool-call fragment); the
  accumulator ignores deltas without `tool_calls`. Whether to surface reasoning to the user
  later is a separate product decision (see §7 CLI); the provider keeps tolerance for
  unknown `delta` keys, consistent with §4H's unknown-fields handling.

```rust
struct ToolCallAccumulator {
    id: String,
    name: Option<String>,
    arguments: String, // partial JSON buffer
}
```

A single LLM message may request several tool calls, each identified by
`tool_call_index`. The streaming task therefore holds a **map of accumulators**
keyed by index (`HashMap<usize, ToolCallAccumulator>`), one per requested call.
Parallel call deltas are **not** executed in parallel: all assembled calls from
one message are queued to the outer loop and executed **serially, one after
another** (see §4B). Tool calls requested by the model are always executed
sequentially, never concurrently. When a stream completes, each accumulator is
turned into an `AssembledToolCall { name, arguments, id }` and the whole batch is
emitted to the outer loop, which queues and drains it serially.

```rust
/// A fully assembled, validated tool call, ready for the outer loop to queue
/// for serial (one-at-a-time) execution.
struct AssembledToolCall {
    id: Uuid,
    name: String,
    arguments: Value, // parsed JSON arguments
}

/// Finalize ONE accumulator once its JSON is complete/parseable.
impl ToolCallAccumulator {
    fn into_tool_call(self) -> AssembledToolCall {
        AssembledToolCall {
            id: self.id,
            name: self.name.unwrap_or_default(),
            arguments: serde_json::from_str(&self.arguments)
                .unwrap_or(Value::Null), // caller validates completeness before calling
        }
    }
}
```

### B. Non-Blocking ReAct Loop with Interruption & Steering

This is the heart of the application. The agent loop is a `tokio::select!` that
simultaneously listens for LLM tokens **and** user commands.

The outer agent loop is a `tokio::select!` on **two** inputs:

1. `cmd_rx.recv()` — user commands (Prompt, Stop, ApproveTool)
2. `agent_rx.recv()` — lifecycle events from the inner streaming task (Done, Cancelled, ToolCall, Error)

This is the only way to own the state machine correctly.

```rust
enum AssistantStatus {
    Complete,
    Interrupted,
}

/// In-memory conversation message.  Persistence uses a separate `StoreEvent`
/// enum (§4H) so the domain model never carries disk-format metadata.
enum Message {
    System { content: String },
    User   { content: String },
    Assistant {
        content: String,
        status: AssistantStatus,
        tool_calls: Vec<ToolCallRequest>,
    },
    ToolResult {
        id: uuid::Uuid,
        name: String,
        result: String,
        /// Provider-neutral descriptors that reference `result`; they do not
        /// own another copy of the Base64 payload.
        attachments: Vec<ToolAttachment>,
    },
    ToolError {
        id: uuid::Uuid,
        name: String,
        error: String,
    },
}

struct ToolCallRequest {
    id: uuid::Uuid,
    name: String,
    arguments: serde_json::Value,
}
```

```rust
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// Channels
let (ui_tx, mut ui_rx) = mpsc::channel::<UiUpdate>(100);     // → TUI thread
let (agent_tx, mut agent_rx) = mpsc::channel::<AgentEvent>(10); // → outer agent loop
let (cmd_tx, mut cmd_rx) = mpsc::channel::<AgentCommand>(10);   // ← TUI / ctrlc

enum AgentCommand {
    Prompt(String),          // New user message (steer)
    Stop,                    // Interrupt current generation
    // The following variants are deferred to Phase 3 (tools):
    // ApproveTool { id: Uuid },
    // Phase 7 instead switches the model on the shared `Engine`
    // (`Engine::switch_model`) from the `/models` slash command, and records
    // the confirmation with `AgentCommand::RecordNotice`; no `SwitchModel`
    // command variant was needed.
}

enum AgentEvent {
    StreamDone { partial: String },              // Natural completion
    StreamCancelled { partial: String },         // User aborted mid-stream
    // The following variants are deferred to Phase 3 (tools):
    // ToolCall { name: String, arguments: Value, id: Uuid },
    // ToolDone { id: Uuid, result: String },
    // ToolError { id: Uuid, error: String },
    // ToolKilled { id: Uuid },
    StreamError(String),     // Provider or network error
}

/// Phase 1 `UiUpdate` is minimal: AppendText, StreamStarted, StreamDone,
/// StreamCancelled, Error.  Tool and steering variants are added in later phases.
enum UiUpdate {
    AppendText(String),
    // ToolStart { name: String, id: Uuid },        // Phase 3
    // ToolPending { name: String, id: Uuid },      // Phase 3
    // ToolDone { id: Uuid, result: String },       // Phase 3
    // ToolError { id: Uuid, error: String },       // Phase 3
    // ToolKilled { id: Uuid },                     // Phase 3
    StreamStarted,
    StreamDone,
    StreamCancelled,
    // SteerQueued,                                 // Phase 3+
    // SteerApplied,                                // Phase 3+
    Error(String),
}

enum AgentMode {
    Idle,               // Waiting for user
    Streaming,          // LLM tokens in flight
    // ToolPending,     // Phase 3
    // ToolExecuting,   // Phase 3
}

/// A fully assembled, validated tool call, ready for the outer loop to queue
/// for serial (one-at-a-time) execution.
struct AssembledToolCall {
    id: uuid::Uuid,
    name: String,
    arguments: serde_json::Value, // parsed JSON arguments
}

/// Finalize ONE accumulator once its JSON is complete/parseable.
impl ToolCallAccumulator {
    fn into_tool_call(self) -> AssembledToolCall {
        AssembledToolCall {
            id: self.id,
            name: self.name.unwrap_or_default(),
            arguments: serde_json::from_str(&self.arguments)
                .unwrap_or(serde_json::Value::Null), // caller validates completeness before calling
        }
    }
}

enum ToolAttachment {
    Image { media_type: String },
    File { media_type: String, filename: String },
}

/// Pending tool awaiting approval and execution.
///
/// A per-tool timeout is **always enforced** — there is no "no-timeout" value.
/// `timeout_secs == 0` selects the tool/module default (5s for file I/O), never
/// "unbounded" (Q6). Parsed args use `#[serde(default = "default_timeout")]`
/// rather than `Option<u64>`, consistent with `BashArgs.timeout_secs`.
struct PendingTool {
    call: AssembledToolCall,
    timeout_secs: u64,
}

impl PendingTool {
    async fn execute(&self) -> Result<String, String> {
        // Dispatches to ToolRegistry (Phase 3)
        todo!()
    }
}

// Builds a request from an immutable snapshot and spawns the inner streaming task.
// Phase 1: no tools, no tool-call accumulation.  Phase 3 adds ToolRegistry + accumulators.
fn start_request(
    context: &Context,
    provider: Arc<dyn Provider>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<AgentEvent>,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let ctx = context.messages().clone(); // read-only snapshot
    tokio::spawn(async move {
        let mut stream = provider.chat(ctx, vec![], cancel.clone()).await?;
        let _ = ui_tx.send(UiUpdate::StreamStarted).await;
        let mut partial = String::new();
        // Phase 3: HashMap<usize, ToolCallAccumulator> and done_calls go here
        while let Some(event) = stream.recv().await {
            match event {
                StreamEvent::Text(s) => {
                    partial.push_str(&s);
                    ui_tx.send(UiUpdate::AppendText(s)).await.ok();
                }
                // Phase 3: StreamEvent::Delta => tool-call accumulation
                StreamEvent::Done => {
                    ui_tx.send(UiUpdate::StreamDone).await.ok();
                    // Phase 3: assemble tool calls, emit AgentEvent::ToolCall
                    agent_tx.send(AgentEvent::StreamDone { partial }).await.ok();
                    break;
                }
                StreamEvent::Cancelled => {
                    ui_tx.send(UiUpdate::StreamCancelled).await.ok();
                    agent_tx.send(AgentEvent::StreamCancelled { partial }).await.ok();
                    break;
                }
                StreamEvent::Error(e) => {
                    ui_tx.send(UiUpdate::Error(e.clone())).await.ok();
                    agent_tx.send(AgentEvent::StreamError(e)).await.ok();
                    break;
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    });
    cancel
}

// Drains queued steers into context as user messages; returns true if any were injected.
// Injecting is the ONLY way a steer reaches the LLM — it never cancels a live stream.
fn inject_pending_steers(pending_steers: &mut VecDeque<String>, context: &mut Context) -> bool {
    if pending_steers.is_empty() { return false; }
    while let Some(msg) = pending_steers.pop_front() {
        context.push_user(msg); // flushes any partial assistant buffer first
    }
    true
}

// AgentLoop constructor and channel topology (settled in Q9/Q12/Q18)
//
//   let (agent, mut ui_rx) = AgentLoop::new(provider);
//   agent.send_command(AgentCommand::Prompt("hello".into())).await;
//   let handle = agent.run();  // spawns outer task, returns JoinHandle
//
// `AgentLoop` internally creates `cmd_tx`/`cmd_rx`, `ui_tx`/`ui_rx`, and
// `agent_tx`/`agent_rx`.  Tests drive it via `send_command()` and consume
// `UiUpdate` events from `ui_rx`.  `main.rs` does the same and awaits `handle`.
//
// NOTE — target seams (see class-diagram.mmd and §4A): Phase 1's `AgentLoop`
// depends on `Provider` directly. The target architecture introduces three
// seams above it:
//   - `Engine` trait: `AgentLoop` will hold `Arc<dyn Engine>` instead of
//     `Arc<dyn Provider>`, gaining `config()` access to sampling parameters.
//   - `AgentFactory` struct: owns the resolved `HashMap<String, Arc<dyn Engine>>`
//     and exposes `create(profile, agent_type, role) -> AgentLoop`. Child agents
//     are built by the same factory in a `Role::Child` (§4G).
//   - `App` trait: `async fn run(self) -> Result<u8>`; `TuiApp` and `ConsoleApp`
//     implement it (§6). `main.rs` dispatches on CLI mode.
// Phase 1's `AgentLoop::new(provider)` is the baseline; the refactor onto
// these seams lands with the config/profile phase.

pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    cmd_tx: mpsc::Sender<AgentCommand>,
    ui_rx: mpsc::Receiver<UiUpdate>,
}

impl AgentLoop {
    pub fn new(provider: Arc<dyn Provider>) -> (Self, mpsc::Receiver<UiUpdate>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>(10);
        let (ui_tx, ui_rx) = mpsc::channel::<UiUpdate>(100);
        let agent = AgentLoop { provider, cmd_tx, ui_rx };
        (agent, ui_rx)
    }

    pub async fn send_command(&self, cmd: AgentCommand) {
        let _ = self.cmd_tx.send(cmd).await;
    }

    pub fn ui_updates(&self) -> mpsc::Receiver<UiUpdate> {
        self.ui_rx  // ownership transferred to caller
    }

    pub fn run(self) -> tokio::task::JoinHandle<anyhow::Result<Context>> {
        tokio::spawn(async move {
            let mut context = Context::new();
            let mut mode = AgentMode::Idle;
            let mut current_cancel: Option<CancellationToken> = None;
            let mut pending_steers: VecDeque<String> = VecDeque::new();
            // Phase 3 adds: ToolRegistry, pending_tools, current_tool_cancel

            loop {
                tokio::select! {
                    // 1. User commands
                    cmd = cmd_rx.recv() => {
                        let Some(cmd) = cmd else {
                            // AgentLoop dropped → clean shutdown: return the
                            // final committed Context (read-only final history).
                            return Ok(context);
                        };
                        match cmd {
                            AgentCommand::Prompt(text) => {
                                if text.is_empty() {
                                    // Phase 1: ignore empty prompts (Q26).
                                    // Phase 3: internal deferred re-prompt after tool batch.
                                    continue;
                                }

                                if mode == AgentMode::Idle {
                                    // Fresh user turn while idle: append and start immediately.
                                    context.push_user(text);
                                    mode = AgentMode::Streaming;
                                    current_cancel = Some(start_request(
                                        &context, provider.clone(),
                                        ui_tx.clone(), agent_tx.clone(),
                                    ));
                                } else {
                                    // === STEERING is cooperative: never cancel the live stream ===
                                    // Queue the message; it is injected at the next natural point.
                                    pending_steers.push_back(text);
                                    // Phase 3: ui_tx.send(UiUpdate::SteerQueued).await.ok();
                                }
                            }

                            AgentCommand::Stop => {
                                // INTERRUPT: hard-abort the current round; a steer never does this.
                                pending_steers.clear();
                                if let Some(ct) = current_cancel.take() {
                                    ct.cancel();
                                }
                                // Phase 3: also cancel current_tool_cancel
                            }

                            // Phase 3: AgentCommand::ApproveTool arm removed
                        }
                    }

                    // 2. Lifecycle events from inner streaming task
                    Some(evt) = agent_rx.recv() => {
                        match evt {
                            AgentEvent::StreamDone { partial } => {
                                // Natural point: the current response ended.
                                context.push_assistant(partial, AssistantStatus::Complete, vec![]);
                                if inject_pending_steers(&mut pending_steers, &mut context) {
                                    // Phase 3: ui_tx.send(UiUpdate::SteerApplied).await.ok();
                                    mode = AgentMode::Streaming;
                                    current_cancel = Some(start_request(
                                        &context, provider.clone(),
                                        ui_tx.clone(), agent_tx.clone(),
                                    ));
                                } else {
                                    mode = AgentMode::Idle;
                                }
                            }
                            AgentEvent::StreamCancelled { partial } => {
                                // Only reachable after a genuine interrupt (Stop).
                                if current_cancel.is_none() {
                                    context.push_assistant(partial, AssistantStatus::Interrupted, vec![]);
                                    mode = AgentMode::Idle;
                                }
                                // else: stale cancellation from a replaced stream; ignore
                            }
                            AgentEvent::StreamError(e) => {
                                pending_steers.clear();
                                mode = AgentMode::Idle;
                                ui_tx.send(UiUpdate::Error(e)).await.ok();
                            }
                            // Phase 3: ToolCall, ToolDone, ToolError, ToolKilled arms
                        }
                    }
                }
            }
        })
    }
}

// In TUI thread (crossterm event loop)
loop {
    tokio::select! {
        // 1. UI updates from root agent
        Some(update) = ui_rx.recv() => {
            self.apply_root_ui_update(update);
            terminal.draw(|f| ui.draw(f, &self)).ok();
        }

        // 2. Live updates from child-agent conversations
        Some(sub_update) = subagent_ui_rx.recv() => {
            self.apply_child_ui_update(sub_update);
            terminal.draw(|f| ui.draw(f, &self)).ok();
        }

        // 3. Crossterm events (spawn_blocking bridged to async)
        Some(evt) = event_rx.recv() => {
            match evt {
                Event::Key(KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                }) => {
                    cmd_tx.send(AgentCommand::Stop).await.ok();
                }
                Event::Key(KeyEvent { code: KeyCode::Enter, .. }) => {
                    let text = self.input.drain();
                    if !text.is_empty() {
                        self.messages.push(Message::user(text.clone()));
                        cmd_tx.send(AgentCommand::Prompt(text)).await.ok();
                    }
                }
                // ... other keys
            }
        }
    }
}
```

**Key observations:**

- `Enter` works **unconditionally**. There is no "busy state" that disables input.
- `Ctrl+C` / `Esc` sends `Stop` which fires the `CancellationToken`. The provider drops the HTTP connection.
- Steering (`Prompt` while busy) **never cancels** the live stream. The message is queued as a pending
  steer and injected at the next natural point (model hand-off, completed tool call, or end of the
  current response); only then are the queued steers appended and a fresh request started.
- The streaming task is fire-and-forget **but sends a single completion event** (`AgentEvent`) back
  to the outer agent loop. The loop is the sole owner of the `AgentMode` state machine and decides all
  transitions (Idle → Streaming, Streaming → ToolPending, etc.).
- The TUI thread is a **dumb view model**: it receives `UiUpdate` and renders, but never decides when
  to start or stop streams. No logic lives in the draw loop.

**TUI event loop:** For a fully async TUI, use `spawn_blocking` for crossterm events,
then `select!` on `ui_rx` + `event_rx` in the main TUI thread. Redraw only when
either channel fires:

```rust
let (event_tx, mut event_rx) = mpsc::channel(32);
tokio::task::spawn_blocking(move || {
    loop {
        if let Ok(evt) = crossterm::event::read() {
            if event_tx.blocking_send(evt).is_err() { break; }
        }
    }
});

// Main TUI loop
loop {
    tokio::select! {
        Some(update) = ui_rx.recv() => { /* apply UI update */ }
        Some(evt) = event_rx.recv() => { /* handle key/mouse */ }
    }
    terminal.draw(|f| ui.draw(f, &self))?;
}
```

**Async context:** The TUI loop must be inside the Tokio runtime. Structure `main.rs` as:

```rust
fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut terminal = ratatui::init();
        run_tui(&mut terminal).await?;
        ratatui::restore();
        Ok(())
    })
}
```

**State machine & rapid steering:** Track explicit `AgentMode` states (`Idle`, `Streaming`,
`ToolPending`) to eliminate overlap bugs: `ApproveTool` is only processed
in `ToolPending`. Because a steer never aborts or restarts a live stream, it introduces
no overlap risk: multiple `Prompt`s in `cmd_rx` are simply queued into `pending_steers`
and injected together at the next natural point (never cancelling anything). The only
abort path is `Stop` (interrupt), which uses `current_cancel.take()` so a stale
`StreamCancelled` from the old task is ignored once the round has been aborted.
No explicit `Restarting` state is needed: injection only ever happens at a natural
point when the loop is not mid-flight, and it always starts exactly one fresh request.

### C. Tiered Context State Machine

The conversation history is organized into **priority tiers** to ensure the most
valuable context is preserved when the window is full. This mirrors real-world
agent harnesses (Hermes, Claude Code) that cannot afford to drop the system prompt
or recent turns when the conversation grows long.

```rust
enum MessageTier {
    System,         // Tier 0: Core identity prompt — NEVER dropped or summarized
    InitialContext, // Tier 1: Stable workspace instructions — NEVER dropped initially
    Recent,         // Tier 2: Last N user/assistant turns — dropped LAST
    Compacted,      // Tier 3: Previously summarized buckets — dropped before Recent
}

struct Context {
    messages: Vec<Message>,  // Phase 1: simple Vec.  Phase 12 adds TieredMessage + token_count.
}

impl Context {
    fn new() -> Self {
        Context { messages: vec![] }
    }
    fn push_user(&mut self, text: String) {
        self.messages.push(Message::User { content: text });
    }
    fn push_assistant(
        &mut self, content: String, status: AssistantStatus,
        tool_calls: Vec<ToolCallRequest>,
    ) {
        self.messages.push(Message::Assistant { content, status, tool_calls });
    }
    fn messages(&self) -> &[Message] {
        &self.messages
    }
    // Phase 3 adds: push_tool_result and push_tool_error
    // Phase 12 adds: push_system, push_initial_context, maybe_compact, token counting
}
```

**Insertion rules (Phase 1 subset):**

- `Context::new()` seeds the initial system message `Message::System { content:
  "You are Ylva, a helpful agent with tool capabilities." }` from the
  code-owned global SOUL value; Phase 10 allows project and home
  **SOUL.md** files to replace it, see §4G.
- The resolved global SOUL is inherited exactly once by root and child
  contexts. An agent type then contributes its own `persona` and
  file-authored `system_prompt` body; the tool protocol follows, and a child
  adds its briefing and runtime constraints after that shared prompt.
- `push_user(text)` → `Message::User { content: text }`
- `push_assistant(content, status, tool_calls)` →
  `Message::Assistant { content, status, tool_calls }`

**Phase 12:** Keep the complete transcript separate from the model-facing
context view. Add tiered messages, cached token estimates, and an append-only
compaction snapshot. The transcript remains intact when the model view drops or
summarizes old messages.

**Context ownership rule (HARD):** The outer agent loop is the **sole writer** to `Context`.

**Compaction thresholds and target:** Start a background summary after a
completed turn at 70% of the active context length. Apply it at the next safe
turn boundary. At 90%, use a synchronous drop fallback if the summary has not
been applied. The default target is `context_length × 0.5`; configure
`compact_target_ratio` per engine, and make the soft (70%) and hard (90%)
triggers configurable per engine with those defaults.

Use cached, approximate `cl100k_base` token counts for all models and
re-evaluate after `/models` changes the active context length. The estimate is
advisory. When `YLVA_TOKEN_USAGE_DUMP_PATH` is set, record a metadata-only
comparison between the estimate of each exact serialized request and provider-
reported input usage for that request, when available; never treat either as
billed usage. See [CLI options — Debug](./cli-options.md#debug).

**Summary generation:** Use the configured `[compaction].engine` without tools. It
summarizes an immutable prefix snapshot; the outer agent loop applies the
result. If summary generation fails or times out, log the failure and drop
old complete turns or oversized old results to reach the target. Do not cut a
tool-call/result pair or compact while the serial tool queue is non-empty.

**Persistence and replay:** Append a compaction event to the current session
JSONL file. Each event stores a monotonic message boundary and the summary for
the prefix through that boundary. A later event supersedes the previous model
snapshot. Replay uses the latest event, retains the original transcript, and
keeps messages after the boundary as the verbatim tail. The TUI displays the
complete transcript with a compaction marker; the model receives a marked,
provider-safe summary followed by the tail. Do not insert a mid-conversation
`system` message.

**Scope:** Compaction is automatic for root and child agent contexts. Child
contexts use their agent type's compaction overrides and may compact repeatedly
before reporting their result. A manual `/compact` command is deferred. The
per-result 512 KiB wire guard and bounded `glob` are also deferred to
`docs/ideas/tool-result-size-guard.md`. The earlier 8–16 KiB insertion-time
truncation proposal is not selected.

**Context ownership rule (HARD):** The outer agent loop is the **sole writer** to `Context`.
The inner streaming task never holds a mutable reference, `Arc`, or lock to `Context`.
Instead, it receives a `Vec<Message>` clone (`context.messages().clone()`) at stream start,
builds the assistant's `partial` response locally, and returns it via `AgentEvent`
(`StreamDone { partial }`, `StreamCancelled { partial }`, `ToolCall`).
The outer loop appends messages to history. This eliminates race conditions during
steering and avoids lock contention on the hot path entirely.

**Token counting:** Use `tiktoken-rs` for OpenAI-compatible models and a byte-length
heuristic (4 chars ≈ 1 token) for Ollama. Count at insertion time and cache in
`TieredMessage.token_count`. Re-counting on every request is O(N) and wasteful.
Counts are **approximate and advisory**, not exact: `tiktoken` encoding is
model-family dependent (one fixed encoding is only exact for the corresponding
OpenAI models; OpenRouter spans several families), and the byte heuristic is a
rough estimate. The design tolerates this via the `context_length × 0.9` safety
margin; engine config can also override `context_length` as a lever if compaction
misbehaves in practice.

**Steering behavior:**
When the user types "No, use `Arc` instead of `Rc`" while the model is still generating:

1. The message is drawn as a **pending bubble** and queued as a steer — the live stream
   is **not** cancelled.
2. At the next safe point — the current response ends (`StreamDone`), or every
   tool call in the current response has its matching result — the outer loop calls
   `push_assistant(partial, Complete, tool_calls)` to make the partial response a
   committed message in `Context`.
3. The queued steer(s) are appended as user messages via `push_user`.
4. A new `provider.chat()` starts with the full updated context.

The model sees (the assistant turn is complete rather than mid-word cut off):

```text
[User]: Write a thread-safe counter.
[Assistant]: Here is a simple counter using `Rc`... (completed turn)
[User]: No, use `Arc` instead of `Rc`
```

If the agent is paused for tool approval or is processing a multi-tool response, a new user
message remains queued until every issued tool call has its matching result. It cannot descope
pending calls or be inserted into the middle of that tool-call sequence because some providers
require exactly one result for each `tool_call_id` before they accept later conversation content.
The user can interrupt the current conversation with `Esc`/`Ctrl+C`; steering does not cancel the
pending tool sequence.

The earlier rolling-window sketch is superseded by the Phase 12 design above:
cached estimates, background summarization through `[compaction].engine`, a hard
drop fallback, and an append-only compaction event. The summary is projected
with a provider-safe marked message, not a mid-conversation `system` role.

### D. Tool Registry + Dynamic Schemas

Never hand-write JSON schemas. Use `schemars` to derive them from structs.

```rust
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;

#[derive(JsonSchema, Deserialize)]
enum ReadMode {
    Lines, // explicit; strict UTF-8 line offset/limit
    Bytes, // explicit byte ranges/binary data
    Chars, // explicit character counts; strict UTF-8 code points
}
// `mode` is omitted (None) by default: autodetect reads UTF-8 text as
// lines, returns a whole recognized media file as Base64 (typed
// attachment), and fails other binary content with a retry hint.

#[derive(JsonSchema, Deserialize)]
enum FileEncoding {
    Utf8,   // default; text behavior (bytes mode remains lossy for invalid bytes)
    Base64, // standard padded Base64 transport for exact bytes
}

impl Default for FileEncoding {
    fn default() -> Self {
        Self::Utf8
    }
}

#[derive(JsonSchema, Deserialize)]
struct ReadFileArgs {
    path: String,
    #[serde(default)]
    mode: Option<ReadMode>,     // None = autodetect (text→lines, whole media→Base64)
    #[serde(default)]
    encoding: FileEncoding,  // defaults to Utf8; Base64 requires explicit Bytes mode
    #[serde(default)]
    offset: Option<usize>,   // 0-based index of the selected unit; line 1 is offset 0 in Lines mode
    #[serde(default)]
    limit: Option<usize>,    // number of selected units
}

// `encoding: Base64` is valid only with `mode: Bytes`; byte ranges are sliced
// before encoding, and the model receives the complete Base64 result text.
// For the user-facing tool bubble, valid UTF-8 decoded Base64 is presented
// like text (including text metadata and a decoded preview); opaque bytes are
// presented with size metadata only.

The binary `read`/`write` implementation remains an exact transport and
UI-safety layer. Complete explicit Base64 `read` results are also inspected
with magic-number detection for PNG, JPEG, WebP, GIF, and PDF. Recognized
complete results carry a provider-neutral `ToolAttachment` descriptor that
references the existing `ToolResult.result`; partial ranges and unknown
formats remain ordinary Base64. Provider serialization projects only
modalities advertised by the selected model: contiguous tool responses use a
short marker followed by a wire-only user content array with text before an
`image_url` or PDF `file` part. Unknown or incompatible models retain the
complete Base64 tool text. The synthetic user message is never persisted or
rendered.

#[derive(JsonSchema, Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

#[derive(JsonSchema, Deserialize)]
struct EditFileArgs {
    path: String,
    // Supports both modes:
    #[serde(flatten)]
    mode: EditMode,
}

#[derive(JsonSchema, Deserialize)]
#[serde(untagged)]
enum EditMode {
    SearchReplace {
        old_string: String,
        new_string: String,
    },
    LineRange {
        start_line: usize,
        end_line: usize,
        new_string: String,
    },
}

// Register at startup:
registry.register("read", schema_for!(ReadFileArgs), read_file_handler);
registry.register("bash", schema_for!(BashArgs), bash_handler);
registry.register("edit", schema_for!(EditFileArgs), edit_file_handler);
```

**Glob dialect and base directory:** the `glob` tool accepts only the glob
(wildcard) dialect — `*`, `?`, `**`, `[...]` — never regexp, matching the `glob`
crate and the example `src/**/*.rs` (Q3). Patterns resolve **relative to the
process CWD**; absolute-path patterns (`/abs/path/*.rs`) reach outside the
workspace. Phase 3 defines **no workspace root** — a base-directory argument and
the notion of "in/out of workspace" arrive with the sandbox in Phase 6 (Q7).

**`clarify` tool:** An interactive tool that lets the model ask the user an
open-ended question when it is uncertain. The agent loop pauses, the TUI
renders the question prominently, and the user types an answer in the input box.

The `clarify` tool can ask an open-ended question or provide up to five
numbered choices. Selecting a choice copies it into the input box; the user
submits it with `Enter`. Headless mode does not register `clarify`.

```rust
#[derive(JsonSchema, Deserialize)]
struct ClarifyArgs {
    question: String,
    #[serde(default)]
    context: Option<String>, // additional context explaining why the question is being asked
}
```

In **TUI mode**, the `clarify` tool is registered and behaves like a pending tool:
the loop pauses, the UI shows the question in a distinct style (e.g., yellow banner),
and the user answers via the normal input box.
The answer is returned as the tool result.

In **headless mode**, the `clarify` tool is **excluded** from the active tool set.
The model never sees it in the `tools` array. If a headless script needs a decision,
the model must make a best-effort guess or return an error.
This prevents a non-interactive process from hanging indefinitely waiting for
human input.

**Dynamic tool filtering:** The `ToolRegistry` holds all compiled schemas at startup,
but the `AgentLoop` builds the `tools` array sent to the LLM by **runtime filtering**
based on active mode and configuration:

- **TUI mode:** All registered tools (including `clarify`) are available unless
  a profile explicitly restricts them.
- **Headless mode:** `clarify` is automatically excluded.
- **Child agents:** the `ChildAgentManager` passes the `allowed_tools` list from
  the agent type's config; only those schemas are included in the request.

This is deterministic: if a tool is not in the filtered list, the model literally
cannot call it. No prompt-engineering required.

```rust
// AgentLoop builds the request
tool_schemas = registry.filter(|name| active_tool_names.contains(name));
```

**Search implementation:** The `search` tool uses the built-in `ignore` walker
and `regex` crate. It respects `.gitignore` and does not require an external
`rg` executable.

**Tool execution strategy:** Every tool invocation runs in a dedicated `tokio::spawn` task with its
own `CancellationToken` (`current_tool_cancel`). The agent loop remains responsive to `Stop`
commands and steering messages while the tool executes. A `tokio::time::timeout` bounds the execution
(default 30s for shell, 5s for file I/O). If the user hits `Ctrl+C` while a tool runs, the `Stop`
handler fires the tool's `CancellationToken`, killing the process. The task then sends
`AgentEvent::ToolKilled` back to the loop.

**Process-group termination:** For shell/spawning tools (`bash`, and any tool that runs subprocesses
such as `curl | sh` or a script with backgrounded `&` jobs), `tokio::process::Command::kill()` alone
only kills the direct child, leaving orphaned grandchildren. Use `Command::process_group(0)` so the
child runs in its own process group, and on timeout/cancel kill the **process group**
(kill the group id,
e.g. via `nix::sys::signal::killpg`) to tear down the whole tree. Plain file tools need no
process-group treatment.

This pattern mirrors the LLM stream cancellation strategy: cooperative cancellation via `CancellationToken`,
no blocking in the root loop, and lifecycle events reported back via `agent_rx`.

**Tool timeouts:** Use `tokio::time::timeout` around `tokio::process::Command::output`.
Default: 30s for shell commands, 5s for file I/O. Display a TUI spinner while the
command runs.

**Tool errors:** Tool failures propagate back to the UI via a `ToolError` variant
in `UiUpdate` so the user sees what went wrong instead of a silent hang.

### E. Safety: Human-in-the-Loop

A terminal agent that can run `rm -rf` or `curl | sh` must ask for approval.

| Tool                      | Confirmation Level                                                      |
| ------------------------- | ----------------------------------------------------------------------- |
| `read`, `glob`   | `Auto`                                                                  |
| `write`, `edit` | `Ask` (show diff first)                                                 |
| `bash`, `git push`        | `Strict` (modal confirmation)                                           |
| `clarify`                 | `Interactive` (rendered as question banner; user answers via input box) |

**Path-based approval via sandbox:** `read` and `glob` are auto-approved by
default, but approval is **path-dependent** — each tool queries
`SandboxContext::check_path`, which returns `None` (sandbox abstains; tool
applies its own default) or `Some(verdict)` (explicit rule; tool honours it
unconditionally). All rule evaluation runs against the **canonical** (symlink-
resolved) path so symlinks cannot hide their real location, name, or extension.
For non-existent paths (e.g. a write target), lexical normalization is the
fallback.

**Tool approval rules (shipped TOML config):** each tool's path-based policy
is configurable via a `[tools.<name>]` block's ordered `rules` list
(last-match-wins; a configured list fully replaces the default floor):

```toml
[tools.read]
rules = [
  { rule = "outside_workspace", verdict = "user_approval_required" },  # default floor rule, restated
  { rule = "dot_path", verdict = "user_approval_required" },           # default floor rule, restated
  # cross_filesystem omitted — opt-in only; add it to restrict cross-device reads:
  # { rule = "cross_filesystem", verdict = "user_approval_required" },
  { pattern = "**/.*", verdict = "auto_approved" },         # re-allow dot-files inside workspace
]

[tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { rule = "dot_path", verdict = "auto_denied" },
  # cross_filesystem omitted — opt-in only; add it to restrict cross-device writes
]
```

Named rules:

- `outside_workspace` — canonicalized path is not under the workspace root
- `dot_path` — any component of the workspace-relative path starts with `.`
- `cross_filesystem` — canonicalized path is on a different filesystem (device)
  than the workspace root; catches writes into mounted shares or volumes.
  **Not configured by default** (sandbox abstains). Set its `verdict` to
  `auto_denied` or `user_approval_required` to restrict cross-device access.

**Flow:**

1. Agent emits `ToolCall("bash", { command: "..." })` or `ToolCall("clarify", { question: "..." })`
2. Agent loop **pauses** (does not start a new LLM request)
3. TUI renders modal or question banner: `Execute: git commit -m "..." ? [Y/n/e(edit)]`
   or `Clarify: Which API version do you want to use?`
4. User sends approval or answer via `cmd_tx`
5. Agent resumes: executes tool, appends result, re-prompts LLM

**Important:** A pending tool approval cannot be replaced by a normal steer.
The tool call must receive its approval/denial result; a pending `clarify` call
must receive its answer or cancellation result. A user message queued while the
agent is busy is injected only after all outstanding tool calls have matching
results. `Esc`/`Ctrl+C` remains the interrupt path.

**Headless mode safety:** Mutating tools (`write`, `edit`, `bash`) require
explicit `--auto-approve-write`, `--auto-approve-edit`, or `--auto-approve-bash`
flags (implemented; see `docs/cli-options.md`/`docs/configuration.md`'s
"`bash` command rules" — the `--auto-approve-shell` name below is this
blueprint's original placeholder for what shipped as `--auto-approve-bash`).
If invoked without the corresponding flag, the headless driver denies the tool
and returns a descriptive tool error. The blueprint's proposed exit code `4`
for this case is deferred to `docs/ideas/cli-exit-codes.md`; the current CLI
does not implement that exit-code contract.

### F. Rich Chat Rendering

Parse the markdown stream with `pulldown-cmark` and render distinctly:

- **Normal text:** wrapped with `textwrap`
- **Code blocks:** wrapped in `Block` borders, highlighted via `syntect`
- **Tool proposals:** distinct color block (e.g., cyan background)
- **Partial / interrupted messages:** subtle indicator (e.g., `…` or faded text)
  to show the response was cut off

Use `ratatui::widgets::Paragraph` with `Line`/`Span` composition.
Cache highlighted lines to avoid re-tokenizing every frame.

**Streaming markdown strategy:** Render the partial assistant message live,
the same way a completed message is rendered — `pulldown-cmark` re-parses the
growing buffer every frame, and `syntect` highlighting for code fences is
served from the content-addressed cache (`src/highlight.rs`'s
`highlighted_lines_cached`, keyed by content + file extension), so a fence
whose content hasn't changed since the last frame is never re-tokenized.
Shipped and measured to work at normal token-streaming rates without
dropped frames; deferring parsing until the stream is `Done` (rendering raw
markdown syntax while streaming) was considered as a precaution against
per-frame parsing cost, but that cost was never actually measured as a
problem (Spike 1 §0 only measured raw token throughput, not
`pulldown-cmark`/`syntect` cost) and the tradeoff — visible `**`/`` ``` ``
syntax mid-stream — is a worse experience for no confirmed benefit. Revisit
only if a real, measured frame-time regression appears during streaming
(see `docs/ideas/` for a fallback note).

**Syntect themes:** Ship two built-in themes — dark and light — selectable
via config. A third ("monokai") theme is not bundled by `syntect` and is
tracked separately as a future idea, not part of this phase.

### G. Agent Types & Delegation

Ylva models a **single pool of typed agents**. An agent type (e.g. `research`,
`code_edit`, `default`) is a named bundle of behavior — persona, tool whitelist,
engine override, timeout, and `max_tool_iterations` — declared once in config
and usable in **any role**.
There is no separate "main agent" vs "sub-agent" category: a `research` agent
can run as the root conversation (the user types `ylva --agent research`) or as
a child spawned by a coding root. The same config entry, the same factory call.

What differs between those two cases is the agent's **role** — its position in
the spawn tree. Role carries the runtime plumbing that the type config cannot
hold (live channels, a parent's `CancellationToken`, a curated briefing):

| Concern | Decided by | Why |
|---|---|---|
| persona, tool whitelist, engine, timeout, iteration limit | **agent type** | static, profile/file configured |
| `parent_cancel: CancellationToken` | **role** | only a child has a parent |
| output channels (root UI vs. child view + parent) | **role** | depends on caller |
| initial `Context` (user prompt vs. curated briefing) | **role** | briefing comes from parent |
| `spawn_subagent` tool available? | **role ∩ type** | see "No nesting" below |

Two roles exist:

- **`Role::Root`** — the top of a spawn tree. Constructed by the `App`
  (`TuiApp` / `ConsoleApp`); owns its own `CancellationToken`; writes to the
  App's UI channel; its `ToolRegistry` includes `spawn_subagent` (when its type
  permits).
- **`Role::Child { parent_cancel, subagent_ui_tx, agent_tx, briefing }`** —
  constructed by a parent `AgentLoop`; its cancellation is linked to the
  parent's tool cancel; writes live updates to `subagent_ui_tx` and its final
  result to `agent_tx`; its `Context` is seeded from `briefing`; its
  `ToolRegistry` never includes `spawn_subagent`.

Both are built by a single factory method (see
[class-diagram.mmd](./class-diagram.mmd)):

```rust
impl AgentFactory {
    pub fn create(&self, profile: &ProfileConfig,
                  agent_type: &str, role: Role) -> AgentLoop { ... }
}
```

**Current scheduling constraint.** The current scheduler runs a root in
parallel with at most one child and queues additional child requests. This
keeps the present UX predictable, but it is not part of the model-facing
lifecycle contract; a later scheduler may run several children concurrently
and deliver their terminal events out of order.

**No nesting.** Children are forbidden from calling `spawn_subagent`. The
maximum delegation depth is one. This is enforced **structurally**: a `Child`'s
`ToolRegistry` is built without the `spawn_subagent` tool, regardless of the
type's `can_spawn` flag. `can_spawn` on `AgentConfig` only governs whether a
`Root` of that type may spawn children at all.

#### Spawn Mechanism

The LLM triggers a child agent via a standard tool call:

```rust
#[derive(JsonSchema, Deserialize)]
struct SpawnSubagentArgs {
    label: String,        // short display/correlation label
    task: String,       // concise description of what to do
    briefing: Option<String>, // curated context extracted from the root conversation
    #[serde(default = "default_agent_type")]
    agent_type: String, // e.g. "research", "code_edit"
}
```

The root agent loop handles `ToolCall("spawn_subagent", ...)` like any other
pending tool: it emits `UiUpdate::ToolPending` and waits for user approval.
Once approved, the `ChildAgentManager` builds a `Role::Child` and calls
`AgentFactory::create(profile, agent_type, role)`, which spawns a **separate**
`AgentLoop` in a `tokio::spawn` task. The root loop immediately returns to
listening for user commands and steering — it does **not** block on the child.

The child is a fully independent ReAct agent with:

- Its own `Context` (seeded from the briefing)
- A filtered `ToolRegistry` (whitelist from the type's config; `spawn_subagent`
  stripped because role = `Child`)
- Its own `Provider` (from the engine assigned to the type in the profile)
- Its own `CancellationToken` (linked to the parent's tool cancel)
- Its own `subagent_ui_tx` channel for live updates to the child conversation view

When the child finishes (via `agent_finish` tool, timeout, or user cancellation),
it sends a terminal completion event to the root agent loop. The event carries
the child `run_id`, originating `tool_call_id`, `label`, terminal status,
result/error/sources, and `outstanding_count`/`all_done` aggregation metadata.
The root loop appends each event to its own `Context` and re-prompts once the
final outstanding child result is available, rather than forcing an
acknowledgement turn for every intermediate completion.

**Channel isolation:** the child's live events flow through `subagent_ui_tx`
(a dedicated MPSC channel created by `ChildAgentManager`). The TUI's
`tokio::select!` listens to three sources: `ui_rx` (root agent),
`subagent_ui_rx` (child agent), and `event_rx` (crossterm).

#### Config-Driven Types

The model does **not** choose the provider, model, timeout, or iteration limit.
Those are defined in the user's config hierarchy.

##### Directory Layout

```text
~/.ylva/
├── SOUL.md                     # optional persona / initial system message (code default)
├── agents/                     # global, shared agent-type definitions
│   ├── research.md             # .md with +++ TOML or --- YAML frontmatter
│   └── code_edit.md
└── profiles/
    ├── default/
    │   └── config.toml         # optional file; code defaults apply if absent
    ├── local/
    │   └── config.toml
    └── work/
        └── config.toml
```

##### Config Source of Truth & Distribution

The `~/.ylva/` layout above is the **runtime** home-directory tree. Its
configuration files are user-authored and may be committed to a project or
kept in the user's home directory. Ylva owns only the code-backed built-ins:
the global SOUL text and `ProfileConfig::default()`. Richer profile and
agent-type examples belong in this documentation and are not repository
configuration templates.

Themes are **not** files — they are compiled into the binary and
name-selectable (`dark | light | monokai`), not seeded into any scope (§4F).

**Persona / initial system message (SOUL.md):** the initial system message of
a chat follows the same scopes, expressed as a **`SOUL.md`** in the project
`.ylva/` and home `~/.ylva/` layers with a code-owned built-in default. It
resolves through the same three-layer fallback chain below. The default
message (`"You are Ylva, a helpful agent with tool capabilities."`) is one
SSOT value used by the loader, `Context::new()`, and `ylva init`.

**Three-layer fallback chain**, resolved per-file with **whole-layer replace**
(highest layer that defines a file wins; lower layers ignored for that file):

```text
project  .ylva/        (current project, cwd/git-root scope)
  → home  ~/.ylva/     (per-user, shared across projects)
  → built-in  code     (compiled defaults — always the guaranteed floor)
```

- A file missing from a higher layer falls through to the next lower layer.
- The implicit `default` profile falls back to `ProfileConfig::default()` when
  both disk layers are absent; an explicitly requested missing profile is a
  hard error.
- The built-in SOUL always supplies a persona, so an empty `~/.ylva/` or a
  fresh project still has an identity out of the box.
- Workspace scope is discovered by walking up from cwd to the nearest ancestor
  containing a `.ylva/` or `.git` marker.

**Distribution — `ylva init`:**

- `ylva init` writes only code-owned starters (`SOUL.md`, the default profile
  `config.toml`, and an annotated default profile `.env`) into `~/.ylva/`; it
  does not seed agent-type files.
- `ylva init --workspace` seeds **only** the default profile `config.toml` and
  annotated `.env` starters plus `.ylva/.gitignore` into the workspace root.
  Agent types and additional profiles are added manually only when the project
  explicitly wants to override (inheritance covers the rest).
- Both are **write-if-missing** by default; `--force` reseeds a built-in file.

**Canonical paths, no synonyms:** there is exactly one location per file
(e.g. `.ylva/profiles/default/config.toml`); `.ylva/config.toml` is *not* a
synonym for the default profile. Profile names are referenced by value
(e.g. `--profile default`), never by path.

The project `.ylva/` is meant to be **committed** to the project repo (shared,
team-consistent config).

The home profile's optional `.env` file
(`~/.ylva/profiles/default/.env`) and, when a workspace is discovered, the
matching project file (`.ylva/profiles/default/.env`) are auto-loaded at
startup. Project values override home values, and an already-exported
environment variable takes precedence over both. TOML string values can
reference the merged environment with `${ENV_VAR}`. Numeric and boolean
fields accept expanded strings when the result parses as the target type.
Missing variables are startup errors, secret values are never logged, and the
workspace `.gitignore` is seeded by `ylva init --workspace`.

**Agent types** define *behavior*: tools, prompts, max iterations, timeouts,
and `can_spawn`. Global (`~/.ylva/agents/`), profile
(`~/.ylva/profiles/<name>/agents/`), and project (`.ylva/agents/`) files are
resolved by project > profile > global precedence. The selected file is the
complete base definition; inline `[agents.<type>]` profile values are applied
afterward as field-level overrides. The same type is usable as root or child,
and the global SOUL remains a separate identity baseline rather than being
copied into agent files.

**Profiles** (`~/.ylva/profiles/<name>/`) define *infrastructure*:
which engine (provider-connection/model/temperature/context-size) each agent
type uses, and can optionally override behavioral settings or disable a type
entirely.

##### Agent-Type File Formats

Each global agent type is defined by a file in `agents/` named after its type.
The example types (research, code_edit) use Markdown with frontmatter (format
2 below); pure TOML remains supported for hand-authored files. Two formats are
supported:

1. **Pure TOML** (`.toml`):

```toml
# agents/research.toml
max_tool_iterations = 10
timeout_secs = 120
allowed_tools = ["read", "search", "glob"]
can_spawn = false
system_prompt = """You are a deep research assistant.
Be thorough. Cite file paths when referencing code."""
```

2. **Markdown with TOML frontmatter** (`.md`, `+++` delimiters):

```markdown
+++
max_tool_iterations = 10
timeout_secs = 120
allowed_tools = ["read", "search", "glob"]
can_spawn = false
+++

You are a deep research assistant. Be thorough. Cite file paths when
referencing code. Always explain your reasoning before concluding.
```

Markdown frontmatter **must** be present; a `.md` file without a `+++` or `---`
block is a parse error. The body (after the closing delimiter) becomes the
`system_prompt`.

3. **Markdown with YAML frontmatter** (`.md`, `---` delimiters):

```markdown
---
allowed_tools:
  - read
  - search
  - glob
timeout_secs: 120
can_spawn: false
---

Locate relevant files and explain the evidence.
```

YAML frontmatter uses the same body convention: everything after the closing
`---` becomes the `system_prompt`. A Markdown file must use one of these
frontmatter formats; a plain Markdown file is a parse error.
This format is ideal for long, evolving prompts because it avoids escaped
quotes and indentation battles.

The **default** agent type is always available from the code-owned profile
defaults and is overridable like any other type. It carries a full tool
whitelist and `can_spawn = true`, so a bare `ylva` invocation runs the
default agent as a `Root` that can delegate. This is the single out-of-the-box
experience. `ylva init` does not seed the example agent files.

##### Engine Definitions (Model Plumbing)

An **engine** is the configuration name for a reusable bundle of model and
sampling parameters. A named provider connection owns the endpoint and
credentials. At runtime, each engine definition is built into a concrete
`Engine` trait object (a thin wrapper around a `Provider` plus the
`EngineConfig`). The terms are separate concepts: **engine** = model/runtime
configuration; **provider connection** = named endpoint and credentials;
**provider** = implementation kind; **Engine** = runtime trait the agent
depends on; **Provider** = low-level backend trait (see
[class-diagram.mmd](./class-diagram.mmd)).

```toml
# profiles/work/config.toml
default_engine = "ollama/local"

[provider_connections.ollama-local]
provider = "ollama"  # implementation kind
api_base_url = "http://localhost:11434"

[provider_connections.ollama-cloud]
provider = "ollama"
api_base_url = "https://ollama.com"
api_key = "${OLLAMA_CLOUD_API_KEY}"

[engines."ollama/local"]
provider_connection = "ollama-local"
model = "llama3.1:8b"
temperature = 0.7
context_length = 8192

[engines."ollama/cloud"]
provider_connection = "ollama-cloud"
model = "llama3.1:70b"
temperature = 0.2
```

Provider connections are named and own endpoint and credential settings.
Engines are named and reference a connection with `provider_connection`; this
allows multiple connections to use the same implementation kind. Engine-level
`api_base_url` and `api_key` values override the connection values. Engines
contain model-specific properties (`provider_connection`, `model`,
`temperature`, `context_length`, and optional connection overrides). They do
not contain tools, prompts, or agent logic.

**Quoted keys (optional):** TOML allows quoted keys containing `/` or `:` for
human-readable engine-name hints. The dotted form (`openrouter_fast`) is the
recommended default and works with all TOML tooling; quoted keys are an opt-in
for readability and carry no extra semantics:

```toml
[engines."ollama/local"]
provider_connection = "ollama-local"
model = "llama3.1:8b"
```

##### Default Engine & Role-Based Engine Resolution

Each profile declares a top-level `default_engine`. Any agent type that
omits its `engine` field inherits `default_engine`. This lets a user change one
engine and have every agent type that did not explicitly override follow along.

```toml
# profiles/work/config.toml
default_engine = "openrouter_fast"

[agents.default]
# engine omitted → uses default_engine

[agents.research]
engine = "ollama_local"  # explicit override
```

Resolution order for any agent type's `engine` field:
explicit `engine` → `default_engine` → hard error at startup.

**Role-based engine fields:** specialized background engines may be declared as
ad-hoc top-level `<role>_engine` fields as the features that need them land
(YAGNI — add only when the feature ships). Each resolves the same way:
explicit `<role>_engine` → `default_engine` → hard error.

```toml
# Future roles, added when the corresponding feature is implemented:
utility_engine = "openrouter_small"  # session title generator (Phase 9)
[compaction]
engine = "openrouter_small"            # context compaction summarizer (Phase 12)
timeout_sec = 120
# curator_engine = "openrouter_small" # reserved for a future curator child agent
```

##### Profile Agent-Type Assignment & Overrides

Each profile declares which agent types are available and which engine runs them:

```toml
# profiles/work/config.toml
[agents.default]
engine = "openrouter_fast"   # root conversation agent engine (omit to use default_engine)

[agents.research]
engine = "openrouter_fast"

[agents.code_edit]
engine = "ollama_local"
```

Profiles can **adapt** global agent types by overriding specific fields:

```toml
[agents.research]
engine = "openrouter_fast"
allowed_tools = ["read", "search"]      # override: removed "glob"
timeout_secs = 60                            # override from global 120
```

Profiles can **disable** a global agent type:

```toml
[agents.research]
enabled = false
```

If a global agent type is not mentioned in a profile, it is available with its
global defaults.

Profiles can **add** local-only agent types by placing files in the profile's own
`agents/` directory:

```text
profiles/work/
├── config.toml
└── agents/
    └── compliance_audit.toml   # only visible to 'work' profile
```

A local agent type with the same name as a global one **shadows** the global
definition for that profile only.

##### Safety Inheritance

Safety settings (e.g., whether `bash` requires approval) are **inherited** from
the parent agent. In headless mode, `--auto-approve-bash` (shipped under this
name; see the note near "Headless mode safety" above) applies to
child agents as well.

#### Child-Agent Lifecycle

1. **Briefing:** the `ChildAgentManager` constructs a new `Context` with a
   **hybrid** prompt: the inherited global SOUL followed by the type's
   persona, constraints, available tools, and output format; and a user
   message containing the curated `briefing`.
2. **Spawn:** the `ChildAgentManager` builds a `Role::Child` and calls
   `AgentFactory::create(profile, agent_type, role)`. The returned
   `AgentLoop` receives:
   - A filtered `ToolRegistry` (whitelist from the type's config;
     `spawn_subagent` stripped because role = `Child`)
   - The configured `Provider` (from the engine assigned to the type in the
     profile)
   - A new `CancellationToken`
   - Its own `subagent_ui_tx` for live child-conversation updates
   - Its own `agent_tx` for sending the final result back to the parent
     The child runs its own ReAct loop in a `tokio::spawn` task.
3. **Parallel execution:** the root agent loop does NOT wait. It continues to
   process `Prompt` (steering) and `Stop` commands from the user. The child
   and root run concurrently. If the root LLM produces new tokens while the
   child is working, the root and child conversations continue independently
   in the TUI.
4. **Finish:** the child must call a special `agent_finish` tool to return its
   result. The `agent_finish` schema is:

   ```rust
   #[derive(JsonSchema, Deserialize)]
   struct AgentFinishArgs {
       status: String, // "success" | "error"
       result: String,
       #[serde(default)]
       error: Option<String>,
       #[serde(default)]
       sources: Vec<String>,
   }
   ```

   The `agent_finish` call is **exempt** from the iteration cap; it is the natural
   termination state. If the child goes idle without ever calling `agent_finish`
   (final assistant text with no tool call, or a killed tool), the system extracts
   the last assistant message as raw text, wraps it in a synthetic
   `{"status": "unstructured", "result": "..."}` object, and returns that to the
   parent. If the child instead hits the max-tool-iterations cap before ever
   calling `agent_finish`, there is no genuine final answer to salvage — the
   child loop returns an `Err` instead of its `Context`, and the parent receives
   a `{"status": "failure", "error": "...", "result": null}` object explaining
   the cap was hit, rather than mistaking the cap's boilerplate message for the
   child's own conclusion. Ahead of its briefing, a child also receives a
   heads-up system message stating its tool-use iteration budget, so a
   capable model can pace its tool calls and call `agent_finish` before it
   runs out. The child should make its handoff self-contained, concise, and
   non-redundant, proportional to the task, without process narration.
 1. **Re-integration:** the structured child result is added to the root
    conversation as a marked harness notice so the root LLM can use it on its
    next turn. It is not inserted as a mid-conversation `system` message.

#### Steering While a Child Agent Runs

The input box is never disabled. If the user sends a new message while a
child is running, the message is **queued as a cooperative steer** — it is
never injected into a live root stream. It is appended to the root conversation
at the next natural point (a completed tool-call boundary / hand-off), and the
root LLM starts a fresh request with the updated context then.

The child continues to completion in the background. Its live events flow
through `subagent_ui_rx` to the child conversation view. Its final result is
appended to the root history when it arrives via `agent_rx`.

**The root LLM then sees both the user's steering message and the child
result on its next turn**, and decides how to reconcile them. This is emergent
behavior — no special merge logic is required.

#### Persistence

Every child-agent run is recorded as a structured record in the global chat
store (see §4H). Two JSONL record shapes are written per parent session file:

```json
{"type":"child_start","v":1,"id":"<run-uuid>","parent_session":"<uuid>","agent_type":"research","task":"…","created_at":"…"}
{"type":"child_result","v":1,"id":"<run-uuid>","parent_session":"<uuid>","status":"success|timeout|max_iterations|error|aborted_by_user","result":"…","sources":["…"],"origin":"agent|runtime","completed_at":"…"}
```

- The parent-session link is explicit (`parent_session`, `parent_message_index`).
- The full child conversation is stored as its **own session file** under
  the same `project_key` (its own `<sub-session_id>.jsonl`) with its own
  `meta` header whose title is generated from the child's task and first
  reply by the Phase 9 utility flow, mirroring §4H layout —
  so child reasoning stays inspectable and audit-able without a
  relational table. Per §4H decision 9, this sub-session is written **once at
  completion**, not streamed live. Its own `meta` header also carries
  `parent_session` (the parent's `session_id`), so the link is discoverable
  bottom-up from the child file alone, not only top-down via the parent's
  `child_start`/`child_result` records.
- Querying by run id / parent is a scan of the project directory's session
  files (the `meta`/`child_*` lines), which is cheap at the single-user,
  per-project scale involved here.

#### UI & Headless

- **TUI:** a full-screen drill-in view shows the child's own transcript while
  the parent continues in the background. Live updates arrive through
  `subagent_ui_rx`; completed child transcripts remain browsable. The previous
  collapsible-sidebar design was superseded.
- **Headless:** child agents are **disabled by default** — without
  `--auto-approve-spawn`, `spawn_subagent` is denied outright. With the flag,
  the one-shot process stays alive until every spawned child has reported a
  terminal outcome, and the `--auto-approve-*` flags inherited by the parent
  also apply to child agents.

Child-agent persistence is recorded via the unified store (§4H); there is **no**
separate SQLite table.

### H. Persistence & Chat History

This section replaces the earlier "SQLite persistence" mentions. It is the
single source of truth for how chats, child-agent runs, and audit data are
stored.

**Design decisions settled so far (Q&A):**

- **Memory vs disk:** `Message` is the in-memory domain model (§4B, Q1).
  Persistence uses a separate `StoreEvent` enum with `Meta`, `MessageEvent
  { msg: Message }`, `ChildStart`, and `ChildResult` variants. The `Store`
  module (Phase 7) owns the JSONL schema, `meta` header, and append-only writer;
  it converts `Message` → `MessageEvent` at commit time.

1. **Scope — one global store, not per-project:**
   History is cross-project user data, so it is stored once, globally, under
   `~/.ylva/chats/`. Project-scoped `.ylva/` stays config-only (and committed);
   chat data is never mixed into it. The `~/.ylva/` root is resolved from the
   dedicated `YLVA_HOME` environment variable when set and non-empty — used
   verbatim as the root, with no further `.ylva` nesting, matching
   `CARGO_HOME`/`RUSTUP_HOME`-style conventions — and otherwise from
   `<operating-system-home>/.ylva`.

2. **Project grouping — hash of the raw CWD, not git root:**

   ```text
   ~/.ylva/chats/<project_key>/
   └── <session_id>.jsonl        # one file per conversation
   ```

   - `<project_key>` = a stable content-hash (e.g. SHA-256, hex) of the
     **canonicalized CWD** at session start — symlinks resolved and `..`
     collapsed via the same canonicalization `Sandbox` already applies to its
     workspace root (§4E, `SandboxContext::canonicalize_best_effort`), reused
     rather than re-implemented, so the same physical directory always maps
     to one `project_key` regardless of which symlink or OS path quirk (e.g.
     macOS's `/var` → `/private/var`) it was reached through. Hashed (not
     path-encoded) so it is opaque but collision-free and robust to any
     characters; keying on CWD (rather than the plan's git-root/`.ylva`
     discovery) means **non-git projects are first-class** — a plain
     directory still gets its own history.
   - `<session_id>` = a UUID per conversation.
   - A project directory holds **many** session files; there is **no
     per-session subdirectory**.
   - Consequence (accepted): launching from a *different* cwd within the same
     repo yields a different `project_key`, so history is grouped per launch
     directory, not per repository. Mirrors Claude Code's per-cwd session
     grouping.

3. **Per-conversation format — append-only JSONL, one file per session:**
   - Directory shape:

     ```text
     ~/.ylva/chats/<project_key>/
     ├── <session_a>.jsonl
     ├── <session_b>.jsonl
     └── ...
     ```

   - The **first line** of every file is a version/metadata header record;
     every subsequent line is one committed event, appended immediately (with
     `flush`) when it is posted to history. Never rewritten, never a
     whole-document rewrite. This gives **crash-safe incremental durability**
     mid-chat (a killed process leaves every already-committed line intact)
     and is grep-able/human-inspectable by design.
   - Only **committed** events are written (user message,
     completed/interrupted assistant response, tool call + result, child-agent
     record). Live streaming `partial`s are ephemeral UI state and never touch
     disk until committed.

4. **Schema versioning — tolerant events + header metadata (A+B):**
   Every JSON line is a discriminated union with a `type` tag and a per-event
   `v` field; the **first line of every session file is the version/metadata
   header**:

   ```json
    {"type":"meta","version":1,"schema_version":1,"app_version":"0.7.0","created_at":"…","project_key":"…","provider_connection":"openrouter","model":"gpt-4o","session_id":"…","title":"…","agent_type":"default"}
    {"type":"model_changed","version":1,"changed_at":"…","provider_connection":"openrouter","model":"gpt-4o"}
    {"type":"message","version":1,"timestamp":"…","message":{"role":"user","content":"…","kind":"explicit"}}
    {"type":"agent_type_changed","version":1,"changed_at":"…","agent_type":"research"}
   ```

   - **Reader tolerance (A):** `#[serde(tag = "type")]`, *never*
     `#[serde(deny_unknown_fields)]`. Unknown/extra fields are ignored;
     unknown `type` values are skipped or rendered as raw. Old readers load
     new files and vice versa. Additive field additions are one-line diffs
     with a `#[serde(default)]`.
   - **Idempotent upgraders (B):** the `meta` header's `schema_version` is the
     file's version marker. On load, a chain of small idempotent upgrader
     functions rewrites the file once if a true semantic break requires it;
     Per-line `v` lets an old reader take an event-type-specific path even
     without the header.
   - **Coherency invariant:** the `meta` header line governs the whole file;
     all subsequent lines are assumed coherent with it.
   - **Upgrade writes are atomic:** if an upgrader rewrites a file, it writes
     a temp file and `rename()`s it over the original, so a crash mid-upgrade
     can never corrupt a session.

5. **Resume model — explicit id only, no auto-resume, no startup picker:**
   - The **TUI always starts a fresh session** on launch: never auto-resumes,
     never shows a startup session picker.
   - Re-attaching an existing session is **explicit** via `--conversation-id
     <session_id>` (available in both headless and TUI modes). Headless
     without a `--conversation-id` is a new session by default.
   - **Browsing is not ruled out** (see decision 8): discoverability is
     grep-based today; a future `/sessions`-style feature can layer on
     without layout change since the `meta` header already carries
     `created_at`.
    - **Consequence:** "remembers conversation history across restarts" is
      scoped to explicit resume. A user who wants continuity passes
      `--conversation-id`; otherwise each launch is a new session.
    - **Root agent type:** store the selected `agent_type` in the session
      header. On resume, use the latest `agent_type` selection unless the user
      supplies `--agent`; append an `agent_type_changed` event for an explicit
      override so later resumes keep it. Older sessions without this metadata
      use `default`. If the effective type is no longer available, fail startup
      unless an explicit `--agent` selects an available type.
    - **Cloned sessions:** add `cloned_from` to the new session's `meta`
      header. Omit it for sessions that were not cloned.
    - **Clone without changing the source:** `--conversation-id <id> --clone`
      copies a top-level session's full logical event stream into a new session with a new UUID,
      new creation time, copied title/provider/model/initial-agent metadata,
      and `cloned_from` source ID. The clone copies messages, compaction
      snapshots, model/agent selection events, and child-run lifecycle records.
      Child transcript files remain shared read-only references. All new
      messages append to the clone; the source file is never modified. Cloning
      fails if another Ylva holds the source session's write lock. Child-session
      IDs are not valid clone sources.

   > **Future tracing note (log correlation per session):** once the TUI can
   > clear/reset/continue a session in-process (not just restart with an explicit
   > `--conversation-id`), the agent must carry the current `session_id` as a
   > field on its root `tracing` span, so it propagates into every JSONL log
   > line's `spans` chain and all logs of one session stay correlatable. Today
   > each launch is a distinct process and `tool_call_id` (agent.rs, `tool_execute`
   > span) is the only per-run correlation key — adequate because logs never mix
   > sessions in one process. In-process session switching would break that
   > assumption, hence the future span argument.

6. **Trailing-corruption recovery — repair-on-open, read-only reads:**
   - **Cause:** even though only full committed messages are written, a single
     `write()`/`flush()` syscall can be cut short by a kill/power-loss,
     leaving a truncated final JSON line. Rare, but must never block loading.
   - **Rule:** pure reads (listing sessions) **never mutate** files. When a
     session file is opened and its final line fails JSON parse, the invalid
     tail is **truncated before append-only resume** so the repaired file has
     no mid-file corruption going forward. A session opened read-only skips
     the bad line in memory without touching disk; a session opened for
     resume+append repairs first, then appends.

7. **Write serialization — flock every open, fail-fast on conflict:**
   - A per-session advisory lock (`flock`, exclusive) is taken on **every**
     session-file open — fresh creation *and* resume — because a fresh
     session's meta header is already discoverable on disk, so a second
     process could otherwise list it and collide with the live writer.
   - Taking the lock: non-blocking `try_lock`-style; on conflict the opener
     gets a clear error ("session `<uuid>` is open in another Ylva process")
     rather than silently interleaving.
   - A listing scan never blocks the active writer: it attempts the lock
     non-blockingly and only marks a session as "currently active" if the
     lock is held; locked-out sessions appear in listings but cannot be
     resumed until the holder releases.

8. **Findability — no index file; the session abstract is the grep surface:**
   - **No separate per-project index/`.index` file** and no runtime browsing
     built-in.
   - Every session's `meta` header carries an **auto-generated title** produced
     by a dedicated utility (`utility_engine`) conversation. The utility
     conversation receives the first explicit user message and the first
     completed assistant reply, then returns a concise title rather than
     copying either source verbatim. The header itself is written to disk
     immediately (empty title) when the session is created, so no event is
     ever held only in memory waiting on title generation; the generated title
     is persisted once by an atomic rewrite of just that first line after the
     first conversation turn completes.
   - The **sidecar — a file that is the session's abstract**: a small,
     human-readable, always-plaintext summary record per session that carries
     the session's discoverable essence (title, meta fields, status, size,
     counts). For a **raw** session the abstract lives as the `meta` first
     line of the `.jsonl` (no separate file — SSOT, no duplicate write). For
     a **compressed/archived** session the abstract is materialized as a
     **`.meta.json` sidecar** (`<uuid>.jsonl.gz` + `<uuid>.meta.json`) so it
     stays greppable after compression; the sidecar is generated *from* the
     meta line, never hand-maintained.
   - **Discovery invariant:** every session — raw or archived — is findable
     via plain grep:
     `rg '"type":"meta"' ~/.ylva/chats/ -g '*.jsonl' -g '*.meta.json'`.
     One tool, one command, no binary reading.
   - Archive write order (crash-safe): write the `.meta.json` sidecar
     **first**, then gzip, then delete the original `.jsonl`. An interrupt
     after the sidecar leaves a benign duplicate (raw JSONL still source of
     truth); both never diverge because the sidecar is derived, never edited.
   - A future interactive `/sessions` picker can be built later from the same
     grep-able abstracts; nothing here blocks it.

9. **Child-agent recording — at-completion, not live:**
   - The parent session file gets a `child_start` line **before** the run
     begins (crash evidence even if the run dies), then a single
     `child_result` line at completion — see §4G `Persistence`.
   - The full child-agent transcript is written **once, at completion**, to its
     own `<sub-session>.jsonl` (same `project_key`, with its `meta` title
     generated from the child's task and first reply by the Phase 9 utility
     flow). Not streamed live.
   - Rationale: child agents are short and loss-tolerant (their result is already
     summarized into the parent context by `agent_finish`); avoiding a second live
     flocked append stream per child agent keeps the machinery simple. A
     mid-run killed child agent loses only its reasoning transcript; its start
     line still marks that a run occurred.

10. **Durability & I/O — flush per line via a dedicated writer:**
    - **Durability bar = process-crash-safe, not power-cut-safe.** Each
      committed line is appended and followed by `flush()` (pushes the writer's
      buffer to the OS page cache), but **not** `fsync` per line. A process
      crash (panic/SIGKILL) loses nothing already flushed; a machine **power
      loss** can drop only the un-flushed tail, which the trailing-corruption
      repair (decision 6) already handles. This matches agent-harness practice
      and keeps the cost off the stream's critical path.
    - **Non-blocking I/O:** all store writes go through a **dedicated writer
      (an `mpsc` channel to a writer task)** owned/invoked by the agent loop,
      so the streaming and TUI threads never block on disk. The single-writer
      ordering of the agent loop is preserved: the loop enqueues committed
      events in order; the writer appends them serially.

11. **Compression lifecycle — raw when new, auto-compress old, manual override:**
    - **Newer sessions stay raw JSONL** (byte-addressable, tail-visible,
      `tail`-able, tail-repairable per decisions 3/6).
    - **Older sessions are automatically compressed** beyond an age/size
      threshold (config-driven, e.g. `compress_after_days`). On compression a
      session's **abstract is materialized** as `<uuid>.meta.json` (decision 8)
      and the body becomes `<uuid>.jsonl.gz`.
    - **Never compress a locked session:** the compressor (decision 7's flock
      rules) skips any session currently held by an active writer — it
      attempts the lock non-blockingly, and only compresses sessions it can
      exclusively lock. This guarantees no live writer ever has its file
      gzipped under it.
    - **Manual override:** the user can `ylva session compress` /
      `ylva session decompress --all | --before <date> | <session-id>`.
      Decompress restores a
      `.jsonl.gz` to raw `.jsonl` (dropping the now-redundant sidecar or
      demoting it back to the in-file meta line).
    - The archive write order (decision 8) is followed: write sidecar first,
      then gzip, then remove the raw `.jsonl`.
    - **Retention/deletion:** no automatic deletion; history is kept forever.
      Old sessions are compacted in place by auto-compression (decision 11),
      and deletion is always an explicit user act via the CLI (decision 12).

12. **Deletion — explicit CLI only, with dry-run:**
    - History is never auto-deleted. Deleting a session is an explicit,
      user-triggered act: `ylva session delete <session-id>…` (or
      `ylva session delete --before <date>` / `--all`), supported by a **`--dry-run`
      flag** that lists exactly which `<uuid>.jsonl[.gz]` (+ `.meta.json`)
      files *would* be removed without touching anything.
    - Deletion respects the lock (decision 7): a session held by an active
      writer is refused or skipped, never deleted from under it.
    - Deletion removes the session's body and its sidecar together; the
      session is then gone (only `--before`/`--all`/explicit id make it
      destructive — no background sweep).

## 5. OpenRouter vs Ollama

| Feature              | OpenRouter                          | Ollama                                         |
| -------------------- | ----------------------------------- | ---------------------------------------------- |
| **Setup**            | `OPENROUTER_API_KEY` env var        | Ollama server running locally                  |
| **Models**           | 100+ (Claude, GPT-4, Mistral, etc.) | Downloaded GGUFs                               |
| **Function Calling** | Native OpenAI `tools` spec          | Native since v0.3+ (needs tool-trained models) |
| **Streaming**        | SSE with `delta.tool_calls` chunks  | SSE via `/v1/chat/completions`                 |
| **Abort / Cancel**   | Drop HTTP request body              | Drop HTTP request body                         |
| **Cost**             | Pay-per-token                       | Free (your hardware)                           |

**Strategy:** Start with OpenRouter + `mistralai/mistral-small` or `anthropic/claude-3.5-haiku`
for cheap, reliable tool use while debugging the TUI. Once solid, test locally with Ollama + `llama3.1:8b`.

**Spike 3 validation:** The live Ollama/OpenRouter shape comparison passed for
`gemma4:e4b-mlx`, `gpt-oss:20b`, `deepseek/deepseek-v4-flash-0731`, and
`moonshotai/kimi-k2.6`. Both OpenRouter shape diffs were empty. Keep one shared
OpenAI-compatible chat stream at `/v1/chat/completions`; Ollama's model
discovery still uses native `/api` endpoints. No `ollama_native` chat provider
is required. See the Spike 3 outcome in §0 and its README.

## 6. Headless / CLI Mode

The agent supports a **non-interactive CLI mode** for automation, CI/CD,
and scripting. It reuses the same `Provider` trait, `ToolRegistry`,
and `AgentState` machine as the TUI, but drives them through a minimal
"fake TUI" wrapper that prints to stdout/stderr instead of rendering a
terminal UI.

The shipped headless contract is documented in `docs/features.md` and
`docs/cli-options.md`. The JSON output, quiet mode, output-stream separation,
buffering, and exit-code redesign described below are deferred to the
individual notes listed in Phase 13.

**Mode dispatch:** `main.rs` selects an `App` implementation (see
[class-diagram.mmd](./class-diagram.mmd)). The `App` trait is a single seam:

```rust
trait App {
    async fn run(self) -> anyhow::Result<u8>;
}
```

`TuiApp` owns the ratatui terminal + crossterm event bridge; `ConsoleApp` owns
the CLI args + stdout/stderr buffering. Each constructs its root `AgentLoop`
via `AgentFactory::create(profile, agent_type, Role::Root)`, drives it, and
returns an exit code. Constructors are not on the trait — the two modes take
different inputs (terminal handle vs CLI flags). The root `agent_type`
defaults to `default` (the code-owned fallback type, see §4G) and is overridable
via `--agent <type>` in both modes.

```rust
let agent_type = cli_args.agent_type().unwrap_or("default");
match mode {
    Mode::Tui => TuiApp::new(factory, profile, agent_type).run().await,
    Mode::Cli => ConsoleApp::new(factory, profile, agent_type, cli_args).run().await,
}
```

### Architecture

Headless mode is **stateless by default**: each `ylva --prompt "..."` starts a
new conversation thread as a fresh JSONL session file (see §4H), runs the
ReAct loop, and exits.
Use `--conversation-id <id>` to resume an existing thread and append new
exchanges to it.

```text
┌────────────────────────────────────────────┐
│  CLI Entry (main.rs)                       │
│  ┌──────────────────────────────────────┐  │
│  │ ConsoleApp                           │  │
│  │ • Receives UiUpdate events           │  │
│  │ • Streams assistant text to stdout   │  │
│  │ • Prints session hint to stderr      │  │
│  │ • JSON output is deferred            │  │
│  └──────────────────────────────────────┘  │
│  ┌──────────────────────────────────────┐  │
│  │ Shared Agent Task (tokio)            │  │
│  │ • Same ReAct loop as TUI             │  │
│  │ • Same channels: ui_tx, cmd_rx       │  │
│  └──────────────────────────────────────┘  │
└────────────────────────────────────────────┘
```

### CLI Interface

```bash
# Default: stateless, text output to stdout, status to stderr
ylva --prompt "summarize src/main.rs"

# Run a specific agent type as the root (default agent type is "default")
ylva --prompt "audit security" --agent research

# Use a specific profile (loads ~/.ylva/profiles/work/config.toml)
ylva --prompt "audit security" --profile work

# Resume existing conversation (UUID session id, §4H)
ylva --prompt "what about error handling?" --conversation-id 3f2a9c81-9b1e-4a7d-8c44-9b9c63d7f2b1

# Clone an existing conversation and continue without appending to its source
ylva --prompt "try a different approach" --conversation-id 3f2a9c81-9b1e-4a7d-8c44-9b9c63d7f2b1 --clone

# JSON output (buffered, emitted at end)
ylva --prompt "generate tests" --format json

# Auto-approve by tool category
ylva --prompt "refactor everything" \
  --auto-approve-read \
  --auto-approve-write \
  --auto-approve-bash

# Suppress stderr progress indicators
ylva --prompt "quick check" --quiet
```

### Output Streams

| Stream     | Content                                                        | Destination                  |
| ---------- | -------------------------------------------------------------- | ---------------------------- |
| **stdout** | LLM response text (text mode) or final JSON object (json mode) | Piped to files / other tools |
| **stderr** | Status messages, tool annotations, progress indicators         | User-facing diagnostics      |

**Example stderr output:**

```text
Streaming response...
Executing tool: read("src/main.rs") → 87 lines
Executing tool: write("src/lib.rs") → pending approval
Error: Tool 'bash' requires --auto-approve-bash or interactive mode
Done.
```

### Exit Codes

| Code | Meaning                                                                         |
| ---- | ------------------------------------------------------------------------------- |
| `0`  | Success — LLM completed, all tools executed                                     |
| `1`  | General error — API failure, network error, panic                               |
| `2`  | User interruption (Ctrl+C / SIGINT)                                             |
| `3`  | Tool failure — tool execution returned error                                    |
| `4`  | Approval required but denied — mutating tool invoked without `--auto-approve-*` |

### Tool Approval in Headless Mode

Read-only tools (`read`, `glob`, `search`) execute automatically. Mutating tools require
explicit flags:

- `--auto-approve-write` → `write`, `edit`
- `--auto-approve-bash` → `bash` (implemented; see `docs/cli-options.md`/
  `docs/configuration.md`'s "`bash` command rules" — this superseded the
  `--auto-approve-shell` name originally sketched in this blueprint)

The `clarify` tool is **not registered** in headless mode. The model never sees it;
if a decision is needed, the model must make a best-effort guess or error out.

If a mutating tool is invoked without the corresponding flag, the headless
driver denies the tool and returns a descriptive tool error. The proposed
exit code `4` is deferred to `docs/ideas/cli-exit-codes.md`.

**Model selection:** Use `--profile <name>` to load a predefined engine configuration. Override explicitly
with `--provider` + `--model` flags (e.g., `--provider openrouter --model anthropic/claude-3.5-haiku`)
for one-off scripts. No hidden defaults.

**Commands:** Slash commands are only recognised as the first character of the
draft input, to avoid triggering inside pasted code. Phase 7 ships `/models`,
`/tools`, and `/clear` (alias `/reset`) in the TUI; `/save` was dropped
(persistence is unconditional, see §4H) and the headless CLI has no
interactive command surface. Anything outside the allowlist is ordinary chat
text.

**Conversation titles:** The `meta` header carries an auto-generated title
returned by a dedicated utility (`utility_engine`) conversation. That
temporary conversation receives the first user message and the first assistant
reply and returns a concise, single-line title (see §4H decision 8), so
headless runs are easy to find later via
`rg '"type":"meta"' ~/.ylva/chats/`.

**Persistence:** Always persists to a JSONL session file (§4H); there is no `--no-persist` flag.
Every run leaves a trace
for later review.

**Buffering strategy:** Buffer everything and emit at the end. For `--format text`, the user sees
status on stderr in real-time ("Streaming response...") but the actual LLM text is buffered and flushed
to stdout on completion. For `--format json`, the entire JSON object is emitted at the end.
This guarantees well-formed output for downstream tools and avoids partial output on interrupt.

## 7. Phased Build Plan

### Phase 0: Proof-of-Concept Spikes

See §0 for the three pre-build spikes: TUI responsiveness, HTTP stream
cancellation, and Ollama tool calling. They are standalone experiments, not
part of the production code path.

### Phase 1: Minimal Headless Build (The Agent Kernel)

**Decision:** Build headless first. The TUI is deferred to Phase 4 and added as a rendering layer on
top of a proven agent loop.

- [x] **Goal:** A working headless agent loop in ~300 lines that proves the core architecture:
      mock provider, dual channels, steering, cancellation, and context ownership.
      No CLI surface, no config parsing, no TOML.

- [x] **`lib.rs` exports:** `pub mod agent; pub mod llm;` with `AgentLoop` struct
      (`new()`, `run()`, `send_command()`, `ui_updates()`).
      `main.rs` is a thin wrapper: `rt.block_on(AgentLoop::new(mock_provider).run())`.

- [x] **`Message` enum (Q1):** `System`, `User`, `Assistant { content, status, tool_calls }`,
      `ToolResult`, `ToolError`. Phase 1 only uses `System`, `User`, `Assistant`.

- [x] **`Context` (Q2, Q13):** Simple `Vec<Message>` owned by `AgentLoop`. No `buffer` field,
      no `TieredMessage`, no token counting.

- [x] **`AgentLoop` channels (Q9, Q18, Q22):** Internally creates `cmd_tx`/`cmd_rx`,
      `ui_tx`/`ui_rx`, `agent_tx`/`agent_rx`. Two spawned tasks:
      1. Outer task: `select!` on `cmd_rx` + `agent_rx`, owns `Context` and `AgentMode`.
      2. Inner streaming task: `provider.chat()` → `stream.recv()` loop, sends `UiUpdate`
      and `AgentEvent` back to outer task.

- [x] **Mock provider (Q8, Q12, Q21):** `MockProvider::new(tokens: Vec<String>, interval: Duration)`
      emits `Text`, `Done` (the `UiUpdate::StreamStarted` marker is emitted by the inner
      streaming task, per the Phase 1 plan). Tests use `tokio::time::pause` + `advance`
      for deterministic, instant verification.

- [x] **`Provider` trait (Q12):** Native async trait (no `async-trait` crate). The Provider
      creates its own internal `mpsc::channel` and returns the receiver.

- [x] **Cancellation flow (Q20):** Outer loop creates `CancellationToken`, passes clone to
      Provider. On `Stop`, `current_cancel.take()?.cancel()`. Provider detects and emits
      `StreamEvent::Cancelled`. Inner task forwards as `AgentEvent::StreamCancelled`.
      Outer loop ignores stale cancellation when `current_cancel` is `Some`.

- [x] **Empty `Prompt` ignored (Q26):** `AgentCommand::Prompt("")` is a no-op in Phase 1.

- [x] **Steering test:** Send `Prompt` mid-stream → verify cooperative handoff (stream not
      cancelled, steer queued, injected at `StreamDone` → new request starts).

- [x] **Interrupt test:** Send `Stop` mid-stream → verify `StreamCancelled`, partial
      flushed as `Assistant { status: Interrupted, … }`, pending steers cleared.

- [x] **Persist committed conversation history** in the session store; the CLI
      does not dump it again when the process exits.

- [x] **Build provenance surfaced via `--version`:** `ylva --version` prints
      `Version` / `Commit` / `Built` / `Profile` (from the `BUILD_INFO` const in
      `src/build_info.rs`, compiled in via `build.rs`) to stdout and exits `0`
      before the runtime or provider are touched. The TUI "About" popup that renders
      the same values is Phase 4.

**Success criteria:**

- Steering never aborts a live stream and does not panic or deadlock.
- Final context (no interrupt) contains exactly: the seeded system message
  (`Context::new()`, "You are Ylva, a helpful agent with tool capabilities."), first assistant
  (completed), the queued steering user message(s).
- Interrupt path: system message, first partial assistant (interrupted), no leftover pending steers.
- No `Arc<Mutex<Context>>`, no shared mutable state.

**Key crates (Q23):** `tokio`, `tokio-util`, `futures`, `thiserror`, `anyhow`,
`uuid` (`v4`), `serde_json` — the extra two carry the full `Message`/
`ToolCallRequest` skeleton (no placeholders, no `todo!()`)

### Phase 2: Real HTTP Provider (Still Headless)

- [x] **Goal:** Replace the mock provider with a real `reqwest` + `eventsource-stream` implementation
      targeting the OpenAI chat-completions spec.

- [x] The unified `Provider` trait (already defined in Phase 1 with the `CancellationToken` parameter
      and `StreamEvent::Cancelled`) stays **unchanged** — no `Delta` variant, no tool-call pipeline
      (see Step 3 refinement below).

- [x] **Step 3 refinement (deferred tool-call assembly):** implement **one**
      provider (`OpenAiCompatibleProvider`) using raw `reqwest` +
      `eventsource-stream`. The provider emits only
      `StreamEvent::Text` for `delta.content`; it **skips** non-content deltas (`reasoning`/`role`-only
      and `tool_calls`-only chunks). It does **not** emit `StreamEvent::Delta`. The
      `ToolCallAccumulator` and tool-call assembly move to **Phase 3** and live in the **agent's
      streaming task** per §4A — never "inside the provider" (this supersedes the earlier Phase 2
      bullet that placed the accumulator in the provider).

- [x] Implement the `OpenAiCompatibleProvider` struct (`base_url`, `model`,
      `api_key`, `reqwest::Client`) with the shared
      `serialize_messages_with_policy` projection. It preserves the ordinary
      text-only projection and, when runtime model capabilities allow it,
      expands supported tool attachments into a wire-only user content array.
      Append `/chat/completions` to `base_url` internally per the §4A
      `base_url` semantics note.

- [x] Verify provider works for both OpenRouter (`base_url` override) and Ollama
      (server-origin `base_url`, with derived `/v1` chat and `/api` catalog endpoints)
      via config switch. **Blocked until Spike 3 (§0) validates Ollama tool-calling support.**
      If Spike 3 fails, replace this step with: implement a second `ollama_native`
      provider targeting `/api/chat` with manual tool-injection prompting, and
      remove the "unified provider" claim from §4A and §5.
      Spike 3 has **passed** — the shared streaming transport is kept; Ollama's
      model catalog uses a dedicated provider wrapper rather than a second
      streaming transport.

- [x] Spawn chat in a `tokio::task`; send `StreamEvent::Text` chunks over `mpsc`

- [x] **Cancellation verified with real TCP:** Drop the stream future and confirm via `netstat`/`lsof`
      that the connection closes immediately (no `CLOSE_WAIT` linger); verified hermetically in tests
      via a mock server's live-connection counter (Spike 2 pattern).

- [x] Handle `StreamEvent::Cancelled` in the headless UI printer (mark message as interrupted in stdout)

- [x] Handle `StreamEvent::Error` gracefully (print error to stderr, return to `Idle`)

- [x] **Pre-stream HTTP retry** (`429`/`5xx`/connect): bounded exponential backoff, up to 3 retries,
      base 1 s, max 30 s, jitter, honoring `Retry-After`; never retry mid-stream (§4A).

- [x] **Demo wiring (Phase 2, no CLI/config):** `main.rs` discovers every
      configured supported provider from the process environment and the
      selected profile's optional `.env`, with process values taking
      precedence. Ollama is initially selected when configured; OpenRouter
      uses `https://openrouter.ai` when its base URL is unset, and the
      `MockProvider` remains the fallback when no backend is configured.

- [x] No TUI yet. Still stdout/stderr only.

**Key crates:** `tokio`, `tokio-util`, `reqwest`, `eventsource-stream`, `futures`, `serde`

### Phase 3: The Agent Loop with Tools (Still Headless)

- [x] **Goal:** first working ReAct agent with file-system tools and **faked
      approval** (no effective human approval in CLI-only mode; see plan).

- [x] **Minimal `--prompt` (Q1):** `main.rs` accepts a single optional
      `--prompt <text>` argument (no full CLI/clap parse in Phase 3) so a real
      `write`/`glob`/`read` ReAct round-trip can be driven against Ollama/
      OpenRouter per invocation; it falls back to a hardcoded demo prompt when
      absent. This refines the broader "no CLI in Phase 3" stance to
      "no CLI *surface/config* — a single `--prompt` arg is allowed for live
      tool demos"; full `--provider`/`--auto-approve-*`/`--format` arrive with
      the Phase 5/7 CLI.

- [x] Add `schemars` and `ToolRegistry`

- [x] Build 3 core tools with typed arguments:
  - `read`
  - `write`
  - `glob`

- [x] Implement ReAct loop inside the `select!` block with explicit `AgentMode` state machine
      (`Idle`, `Streaming`, `ToolPending`):

  1. Stream LLM response
  2. Detect `tool_calls` in stream or final message
  3. **Pause:** emit `UiUpdate::ToolPending`, do NOT start a new request
  4. Wait for `AgentCommand::ApproveTool` via `cmd_rx` (only valid in `ToolPending`)
  5. Execute tool via registry
  6. Append result to message history
  7. Re-prompt LLM (new stream)

  Multiple tool calls in one message are queued (`VecDeque<PendingTool>`), drained
  **serially**, and re-prompt only after the queue empties (see §4B).

- [x] Show tool execution status in the headless UI (print to stderr)

- [x] Add `ToolError` variant to `UiUpdate` for failed tool invocations

- [x] Extract `AgentLoop` as a reusable struct (provider + registry + context)
      so Phase 6 child agents can instantiate it with a restricted tool whitelist

- [x] Prevent infinite loops with the fixed 50-request guard in `src/agent.rs`.
      Per-agent `max_tool_iterations` configuration with the same default is a
      remaining agent-controls task in Phase 13.

- [x] No TUI yet. Approval flow is **hardcoded, not interactive** (CLI-only): every
      tool call pauses for `ApproveTool`/`DenyTool`/`Stop`; the driver auto-approves
      `read`/`glob` and denies `write`, denying fed back to the model as a `ToolError`.

**Key crates:** `schemars`, `serde_json`, `glob`, `tempfile`

### Phase 4: TUI Skeleton (Rendering Layer Added)

**Decision:** The TUI is added only after the agent loop is proven headless. The TUI is a rendering
layer, not a core architectural change.

- [x] **Goal:** A responsive terminal interface where the input box is **never** disabled.

- [x] Set up `ratatui` + `crossterm` event loop via `spawn_blocking` + `select!`

- [x] Two UI areas: chat history (scrollable) and input box. Sidebar is deferred to Phase 6.

- [x] Integrate `ratatui-textarea` (renamed from `tui-textarea`) for bottom input with history
      navigation (`↑`/`↓` on boundary lines loads previous/next sent message; draft is saved).

- [x] Echo user input back into the chat view; user messages rendered as a block with a
      colored left border and soft background — no text prefix. Border color encodes
      `UserMessageState`: Green = Sent, Yellow = Queued (pending steer), LightGreen = Steered
      (applied steer), DarkGray = Cancelled.
      Colors are named constants, ready to be extracted into a `Theme` struct.

- [x] 2-row footer: mode indicator plus compact active running/queued subagent
      and steer counts (row 1); key hints (row 2). Finished subagents are
      excluded.

- [x] Install `ctrlc` handler that sends `AgentCommand::Stop` via channel immediately — no
      double-press required for OS-level SIGINT.

- [x] **Re-use the same `AgentLoop` struct** from Phase 3. The TUI is just another consumer
      of `UiUpdate`.

- [x] **Steering in TUI:** `Enter` sends unconditionally, even while streaming — queued as a
      pending steer when busy and shown with a `Queued` (yellow) border style.

- [x] **Interrupt — double-press confirm with 2-second window:**
      `Ctrl+C` and `Esc` use a key-agnostic double-press: the first press of either key sets
      confirm state; the second press of either key within `CONFIRM_TIMEOUT` (2 s) triggers the
      action. A natural transition to `Idle` (via `StreamDone`, `StreamCancelled`, or `Error`)
      clears confirm state immediately.
      - While streaming / `ToolPending`: first press → footer hint `"Press again to cancel"`;
      second press ≤ 2 s → `AgentCommand::Stop`.
      - While idle: `Ctrl+C` first press → hint `"Press again to quit"`; second press ≤ 2 s →
      quit TUI. `Esc` while idle → clear input field only (no confirm, no quit).
      - Help popup open → `Esc` closes popup immediately; confirm state is unchanged.

- [x] **Pending-steer rendering:** `Queued` bubbles convert to `Sent` (blue) when `SteerApplied`
      arrives. Interrupting marks all `Queued` bubbles as `Cancelled` (grey) with a dimmed
      `"Not sent."` annotation; cancelled texts are retained in `input_history` so the user
      can recover them via `↑`.

- [x] **Tool approval sub-mode:** when `UiUpdate::ToolPending` arrives, the footer switches to
      `[ ✓ Approve (Enter) ]  [ ✗ Deny (Esc) ]` with mouse-clickable buttons. In this sub-mode
      `Enter` sends `ApproveTool`, `Esc` sends `DenyTool` immediately (no double-press). `Ctrl+C`
      still follows the double-press confirm → `AgentCommand::Stop`. Clicking outside the buttons
      is a no-op.

- [x] **F1 Help popup:** opens/closes a centered popup (60 % × 70 %) showing the full keybinding
      table and `BuildInfo` values (version, commit, build timestamp, profile).

- [x] **Mode dispatch:** `std::io::stdout().is_terminal()` in `main.rs` selects between
      `tui_driver::run_tui_on_stdout` and `headless_runner::run_headless`. No `App` trait yet
      (deferred to Phase 5).

**CRITICAL:** Ensure `Enter`, `Ctrl+C`, and `Esc` are handled **regardless** of streaming state.
The input widget must never be "disabled."

**Key crates:** `ratatui 0.30`, `crossterm 0.29`, `ratatui-textarea 0.9`, `textwrap`, `ctrlc`

#### Phase 4 outcome

Decisions recorded during implementation:

- **Double-press confirm is key-agnostic.** A single `confirm_state: Option<Instant>` in the TUI
  driver (not in `TuiState`) tracks the first-press timestamp; the current `agent_mode` decides
  the action when the second press fires within `CONFIRM_TIMEOUT` (2 s). No `ConfirmKind` enum.
- **OS SIGINT → immediate `Stop`.** The `ctrlc` handler runs on a dedicated OS thread and forwards
  to tokio as `AppAction::SigInt` into the *action* channel (so the relay holds no `cmd_tx` clone
  and cannot keep the agent loop alive after a quit). A `spawn_blocking` watcher polls
  `recv_timeout` and exits as soon as the action receiver closes, so it never leaks a thread that
  hangs runtime shutdown. `block_on` is never called inside the handler. No double-press for
  OS-level SIGINT.
- **Natural transition to Idle clears confirm state.** `StreamDone`/`StreamCancelled`/`Error`
  (observed via `UiUpdate`) reset the driver's confirm state as soon as the agent returns to
  `Idle`.
- **Tool approval sub-mode.** `Enter` = `ApproveTool`, `Esc` = `DenyTool` (immediate); the footer
  renders mouse-clickable buttons whose rects are captured from the frame for hit-testing;
  clicking outside is a no-op. `Ctrl+C` still follows the double-press confirm → `Stop`.
- **User message visual design.** User messages render as blocks with a left-only border and a
  soft background shade (`USER_MESSAGE_BG_COLOR`), with **no text prefix**. Border color encodes
  `UserMessageState` (Green = Sent, Yellow = Queued, LightGreen = Steered, DarkGray = Cancelled);
  colours are named constants ready to be extracted into a `Theme` struct.
- **Cancelled steers.** Marked `Cancelled` in `ChatEntry` (ephemeral view state, never persisted)
  and retained in `input_history` for recovery via `↑`.
- **`SteerQueued`/`SteerApplied` are pure mode signals** — no payload; the TUI driver creates the
  bubbles itself.
- **Terminal restoration is RAII.** `run_tui_on_stdout` restores the terminal (raw mode +
  alternate screen) via a guard on every exit path, including panics.
- **Mouse capture is enabled.** `EnableMouseCapture`/`DisableMouseCapture` are part of the
  enter/leave sequences; without them the wheel and clickable approval buttons would never receive
  events.
- **Modified Enter compatibility.** `run_tui_on_stdout` pushes
  `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES` (and pops it in the terminal guard) so
  supporting terminals (iTerm2, kitty, WezTerm) report `Shift+Enter`/`Alt+Enter` as
  distinguishable CSI-u sequences. Terminals that encode `Shift+Enter` as a line feed, such as
  Zed's terminal, are normalized before routing; unsupported terminals may still fall back to a
  bare `Enter`.
- **Mouse click/drag in the input box.** A left-click places the cursor at the clicked character
  (word-wrapping accounted for); a left-drag selects text. The input box's on-screen rect is
  written to `TuiState` each frame for hit-testing.
- **Input box is a word-wrapped message.** The input renders as a left-border bubble (Cyan), no
  full frame and no title, and long text word-wraps inside the box (no horizontal scrollbar). Its
  height grows with the wrapped content, capped at 1/3 of the terminal.
- **`Sending…` state.** A `TuiState::submitted` flag is set when a prompt is sent from idle and
  cleared on `StreamStarted`, so the footer shows `"Sending…"` (not `"Idle"`) while waiting for
  the first stream event.
- **TUI starts empty.** The hardcoded demo prompt is headless-only; the TUI launches with an
  empty input box unless an explicit `--prompt` or piped stdin was given.
- **Scrollbar thumb reaches the track end.** The chat scrollbar uses `content_length =
  max_offset + 1` (not the raw line count), because ratatui's thumb proportion only reaches the
  very bottom/top when `content_length` is the number of scrollable positions.
- **Scroll offset is clamped and direction-correct.** `conversation.scroll_offset` is a row
  offset from the top: `PageUp`/wheel-up decrease it (toward earlier content), `PageDown`/
  wheel-down increase it, and `tui_widgets::draw` clamps it to the rendered content height so
  over-scrolling cannot silently drift. Scrolling back to the bottom re-enables auto-scroll.
- **Wordmark.** The chat list always opens with a centered ASCII-art "Ylva" wordmark so the
  initial frame is not empty while the first response streams.
- **`run_tui` returns the final `TuiState`** so integration tests (`tests/tui_render_state.rs`) can
  assert on the resulting view state; the production entry point discards it.

### Phase 4 Reducer Refactor (TUI state handling)

**Decision:** Replace the imperative, mixed-concern `handle_action` in `tui_runner.rs` with a
pure reducer pattern (`update(state, event) → Vec<Effect>`) and a clean MVP module split.
See reducer-state-handling-plan.md.

- [x] **`src/tui_reducer_state.rs` (Model):** pure view-model; defines `AppState`, `TuiMode`, `Focus`,
      `AppLayout`, `Effect`, `InputEvent`, `DomainEvent`. No ratatui backend, no channel types.
- [x] **`src/presenter.rs` (Presenter):** `update()` + routing helpers (`route_keyboard`,
      `route_mouse`, `route_domain`); no I/O. All state-machine logic lives here; fully unit-tested
      without a live terminal.
- [x] **`src/render.rs` (View):** `render(frame, &mut AppState)` entry point extracted from
      `src/tui_widgets.rs`; writes geometry back to `state.layout.*`; excluded from coverage.
- [x] **`src/tui_driver.rs` (Driver, renamed from `tui_runner.rs`):** thin async event loop;
      owns `TextArea` and channel handles; converts `AppAction → TuiEvent` and
      `UiUpdate → DomainEvent`; executes `Vec<Effect>` via `execute_effects()`.
- [x] **`TuiMode`** replaces the legacy `agent_mode: AgentMode` field for TUI control flow;
      `Focus` replaces `show_help: bool` and the `pending_approval` sub-mode flag.
- [x] **`TuiMode::DoublePressAbort`** folds the driver-local `confirm_state: Option<Instant>` into
      typed state; `DomainEvent::ConfirmExpired` drives the expiry transition.
- [x] **State diagram:** `docs/tui-reducer-state-diagram.mmd` (rendered SVG alongside) documents
      all `TuiMode × Focus` transitions.

#### Phase 4 Reducer Refactor outcome

- **MVP boundary is compiler-enforced.** `presenter.rs` has no `use ratatui` / channel imports;
  clippy catches violations.
- **Unit tests without a terminal.** Each handler in `presenter.rs` is tested by setting
  `state.mode`/`state.focus` and asserting `Vec<Effect>` — no channel, no crossterm needed.
- **Integration tests unchanged.** `tests/tui_render_state.rs` exercises the full Driver +
  Presenter + View composition; type imports updated to new names (`AppState`, `TuiMode`,
  `Focus`).

### Phase 5: Interface Seams

- [x] **Goal:** Introduce the runtime interface layer (§4A, [class-diagram.mmd](./class-diagram.mmd))
      so Phase 6 child agents build against `AgentFactory` and `Engine` from the start,
      not against the Phase 1 `Provider`-direct seam.

- [x] **`Engine` trait + `EngineImpl`:** thin wrapper holding `Arc<dyn Provider>` +
      `EngineConfig`; exposes `chat()` and `config()`. `AgentLoop` migrates from
      `Arc<dyn Provider>` to `Arc<dyn Engine>`.

- [x] **`AgentFactory` struct:** owns the resolved `HashMap<String, Arc<dyn Engine>>`
      built from `EngineConfig` definitions + profile resolution. Exposes
      `create(profile, agent_type, role) -> AgentLoop` (single method; §4G).

- [x] **`App` trait:** `async fn run(self) -> anyhow::Result<u8>`. `TuiApp` and
      `ConsoleApp` implement it (§6). `main.rs` dispatches on CLI mode.

- [x] **`Provider::list_models()`:** add to the `Provider` trait; `OpenAiCompatibleProvider`
      queries `/models` (OpenRouter) or `/api/tags` (Ollama). Returns `Vec<ModelInfo>`
      for a future `/models` slash command.

- [x] **Config refactor:** rename `provider_type` → `provider` in `EngineConfig`;
      add profile-level `default_engine` (required) and `[compaction].engine` (optional,
      resolves to `default_engine`); document quoted-key engine names
      (`[engines."ollama/local"]`) as opt-in.

- [x] **Child-agent construction** (§4G) uses
      `AgentFactory::create(profile, agent_type, Role::Child { ... })` from
      the start — no `AgentLoop::new()` direct calls for child agents.

**Key crates:** none new (uses existing `tokio`, `reqwest`, `serde`).

### Phase 6: Terminal Superpowers

- [x] **Goal:** Make it a true coding agent.

- [x] `bash` tool wrapping `tokio::process::Command` with `tokio::time::timeout` (default 30s)

- [x] Launch `bash` with `process_group(0)` and kill the process **group** on timeout/`Stop`
      (prevents orphaned grandchildren)

- [x] Confirmation modal for `bash` and `write` (human-in-the-loop)

- [x] Built-in `search` tool using `ignore` + `regex`; it respects `.gitignore`
      and does not depend on external `rg`.

- [x] `clarify` tool for interactive open-ended questions; render question banner in TUI, user answers
      via input box; not available in headless mode

- [-] File-tree sidebar using `ignore` + `walkdir` — **dropped**. The TUI has
  no sidebar; file discovery uses `search` and `glob`.

- [x] **Diff view:** When `write` is proposed, render a side-by-side or unified diff in chat
      before approval

- [x] Add `edit` tool (line-based or search-and-replace) to reduce full-file rewrite tokens

- [x] `spawn_subagent` tool in `ToolRegistry` with config-driven types and tool whitelists

- [x] `ChildAgentManager` that owns child-agent lifecycle (spawn, timeout, cancel via `CancellationToken`)

- [x] `agent_finish` tool available only inside child-agent runs; fallback to unstructured
      text if never called

- [x] Child-agent persistence records (`child_start` / `child_result`) with
      parent session linkage (§4H)

- [x] Child agent inherits parent safety settings (`--auto-approve-*` in headless)

- [x] Full-screen child-conversation drill-in view, which supersedes the
      originally planned child-agent sidebar panel.

- [x] Headless: `--auto-approve-spawn` flag (child agents denied without it) + stderr status lines

**Key crates:** `ignore`, `regex`, `tempfile`

### Phase 7: Polish & Persistence

- [x] **Goal:** Usable daily driver.

- [x] `syntect` highlighting for all Markdown code fences (already shipped in
      Phase 6, §4F); live rendering during streaming/thinking already matches
      the completed-message rendering path — no separate "raw text while
      streaming" behavior is planned (see §4F "Streaming markdown strategy")

- [x] Two built-in themes: dark, light (a third "monokai" theme is deferred — no
      bundled `syntect` default theme is named "monokai"; sourcing/embedding a
      third theme asset is tracked as a standalone future idea, not a phase task —
      see `docs/ideas/monokai-theme.md`)

- [x] JSONL persistence for conversations, child agents, and schema-version handling (§4H)

- [x] Implement the `Store` (`src/store.rs`, §4H) as the single persistence path for
      **both** main-conversation messages and child-agent records — do not build a
      child-only persistence layer. Append `child_start` / `child_result` JSONL
      records (shapes in §4G *Persistence* / §4H) to the parent session file via the
      same writer used for conversation messages, keyed by `parent_session` +
      `run_id`. This supersedes the child-persistence task originally planned as
      Phase 6.12, dropped because it
      had no main-session persistence to attach to.

- [x] Resume an existing session via explicit `--conversation-id` (no auto-resume, no
      picker); an id that does not match an existing session file is a hard error,
      never a silent new-session fallback. Phase 7 resume **appends** to the session
      file; replaying its earlier turns back into the in-memory `Context` is
      Phase 8 (Session Replay on Resume, below).

- [x] Config hierarchy — **narrow scope**: load a single flat
      `~/.ylva/profiles/<name>/config.toml` (default profile at
      `~/.ylva/profiles/default/config.toml`) into the existing
      `ProfileConfig`/`AgentConfig`/`EngineConfig` structs for default model,
      theme, keybindings, and agent-type engine assignments. The full `§4G`
      three-layer fallback chain (project `.ylva/` → home `~/.ylva/` → code-owned
      built-ins), `ylva init`/`ylva init --workspace` distribution, `SOUL.md`
      persona loading, and global `agents/*.md` type files are **out of scope
      for Phase 7** — see Phase 10 below. Profile-scoped `.env` loading is
      implemented for the provider environment.

- [x] `/commands` in input box, recognised only when `/` is the first character
      of the draft message (never mid-paste): `/models` (list available models
      from every configured provider via `Provider::list_models()`, §4A, and
      switch the active engine and model on selection — supersedes the
      standalone `SwitchModel` placeholder), `/tools` (show the active agent's
      allowed tool list), `/clear`/`/reset` (aliases; reset `Context` and start
      a fresh `Store` session). `/save` is dropped — it had no behavior
      distinct from the `Store`'s existing automatic per-line flush, §4H
      decision 10. A recognised command may open a small overlay above the
      conversation list while typing (e.g. `/models`' candidate list) and
      always produces a result bubble in the conversation on confirm. `/agent`
      (overlay listing (sub-)agent runs and switching the view to one) is **not**
      in Phase 7 scope — see `docs/ideas/slash-commands.md`.

**Key crates:** `serde_json`, `sha2`, `toml`, `chrono`

### Phase 8: Session Replay on Resume

- [x] **Goal:** Resuming a session (`--conversation-id`) reconstructs its prior
      turns instead of starting blank, so both the model and the user see
      continuous history — closing the gap Phase 7 explicitly deferred (§4H
      decision 5's resume model **appends** to the file but does not replay
      it back into `Context` or the TUI).

- [x] `Context::set_store` reads `Store::read_messages` and hydrates a new
      context without re-`commit`-ing persisted messages. Hydration is
      unconditional for fresh and resumed stores; a fresh file simply has no
      messages to append. The two leading system prompts stay sourced fresh
      from the running binary (per §4H decision 4's schema-tolerance note).

- [x] Headless (`ConsoleApp`): a resumed session's hydrated `Context` is
      visible to the model on the very next turn (prior turns are part of
      the request payload), matching how a live session behaves.

- [x] TUI (`TuiApp`/`AppState`) projects hydrated messages into `chat_entries`
      before the first frame and keeps the existing bottom-lock behavior.
      Explicit user messages, assistant text, completed tool results, and tool
      errors replay with their persisted state. `Message::ToolResult` and
      `Message::ToolError` additively persist summary/content-section metadata
      and tool duration. Assistant tool calls persist their tool-specific title
      arguments, and tool results persist optional specialized result-section
      identifiers; all new fields use `#[serde(default)]`, so no schema-version
      bump is required and older sessions degrade silently. Orphaned generic
      tool calls become in-memory terminal errors rather than pending/running
      bubbles.

- [x] Child-agent replay is included. `ChildStart`/`ChildResult` carry
      `call_id` (and `ChildStart` carries `label`) with serde defaults. The
      root `spawn_subagent` bubble is fixed up from lifecycle records to its
      real completed or interrupted state, and replay-discovered child
      metadata is made drillable without opening child files eagerly. The
      child's own transcript is loaded lazily on first drill-in.

- [x] Tests cover schema compatibility, hydration without duplicate writes,
      orphaned tool recovery, child lifecycle pairing, full-fidelity TUI
      projection, lazy child loading, and real-binary headless replay.

- [x] Documentation: `docs/persistence.md`, `docs/features.md`, and
      `docs/implementation-notes.md` describe the replay contract and
      implementation.

**Key crates:** none new (uses the existing `Store::read_messages`, §4H).

### Phase 9: Automatic Session Title Generation

- [x] **Goal:** Make persisted sessions discoverable by generating a concise
      title from the first explicit user message and the first completed LLM
      reply, using a dedicated utility engine rather than copying the user's
      message into the `meta` header.

- [x] Add the profile-level `utility_engine` setting alongside `default_engine`
      and `[compaction].engine`; when unset, resolve `utility_engine` to
      `default_engine`. Resolve it independently of the active conversation
      engine and the context-compaction `[compaction].engine`. The utility request
      must not change the active engine or its runtime model settings.

- [x] After the first conversation turn completes, capture the first explicit
      user message and the first completed assistant reply. Do not trigger title
      generation for implicit harness messages, incomplete/interrupted replies,
      or sessions that never commit a user turn and a reply.

- [x] Start an explicit temporary utility conversation with no session store,
      no visible UI updates, and no tools. Send a strict title-generation
      prompt containing both source messages in clearly delimited sections:
      return only a concise title, with no explanation, markdown, or quotation
      marks.

- [x] Validate and normalize the utility response to one non-empty line within
      the session-title length limit. A failed, empty, or malformed utility
      response must not fall back to copying the first user message; report the
      failure and leave the title pending for a later retry.

- [x] Persist the generated title to the current session through an explicit
      store operation that atomically rewrites only the `meta` header line.
      Keep the user and assistant events durable in their original order and
      ensure the temporary utility conversation never appears in the current
      session transcript or session listing.

- [x] On resume, detect a completed first turn whose title is still empty and
      retry the same utility conversation from the persisted user/assistant
      messages. Apply the same flow to child transcripts, using the child's
      first task message and first reply as its title inputs.

- [x] Add seam-level tests for utility-engine selection, the exact prompt
      inputs, isolation from the current store/UI, validation and retry
      behavior, atomic title persistence, resume recovery, and metadata-only
      session discovery.

**Key crates:** none new (extends the existing `Engine` abstraction and JSONL
`Store`; add the configured `utility_engine` slot alongside
`[compaction].engine`).

### Phase 10: Full Configuration & Persona System

- [x] **Goal:** Replace Phase 7's single flat config file with the complete `§4G`
      config-driven-types system, so behavior (agent types, persona) and
      infrastructure (engines/profiles) are both fully file-driven and
      shareable across projects/users.

- [x] Three-layer fallback chain: project `.ylva/` (cwd/git-root scope) → home
      `~/.ylva/` (per-user) → code-owned built-ins, resolved per-file with
      whole-layer replace. The implicit default profile and global SOUL have
      code-backed floors; an explicitly requested missing profile is a hard
      error.

- [x] `ylva init` (writes SOUL/profile starters, including `.env`, into
      `~/.ylva/`) and
      `ylva init --workspace` (seeds the default profile `config.toml` and
      `.env` starters into the workspace root);
      both write-if-missing by default, `--force` reseeds. Agent-type files are
      user-authored and richer examples live in documentation.

- [x] `SOUL.md` persona / initial system message loader (project `.ylva/` and
      home `~/.ylva/`, built-in default), replacing the Phase 1 SSOT constant's
      source per §4C/§4G

- [x] Global agent-type definitions (`~/.ylva/agents/*.md` with `+++` TOML or
      `---` YAML frontmatter, e.g. `research.md`, `code_edit.md`), shared across
      profiles
      and usable as root or child agent types

- [x] Profile-scoped agent-type definitions
      (`~/.ylva/profiles/<name>/agents/*.md` with `+++` TOML or `---` YAML
      frontmatter, e.g. `research.md`, `code_edit.md`), usable as root or child
      agent types

- [x] Project-scoped agent-type definitions (`.ylva/agents/*.md` with `+++` TOML
      or `---` YAML frontmatter, e.g. `research.md`, `code_edit.md`), usable as
      root or child agent types

- [x] Profile-scoped `.env` auto-loading
      (`~/.ylva/profiles/<name>/.env`), with an already-exported environment
      variable taking precedence.

- [x] Project-scoped `.env` auto-loading (e.g. `.ylva/profiles/<name>/.env`);
      `ylva init --workspace` also deploys a `.gitignore` inside the project's
      `.ylva/` ignoring `.env`

- [x] Per-tool sandbox rule configuration: load configurable `[tools.*]` rule
      lists (including `outside_workspace`/`dot_path` overrides and the
      opt-in `cross_filesystem` rule, §4E) from the
      layered config, replacing Phase 6's hardcoded two-rule default

**Key crates:** `toml` (already added in Phase 7); evaluate `dirs`/`directories`
for home-directory resolution when this phase starts.

### Phase 11: CLI Option Parsing (clap)

**Decision:** Replace the handwritten, per-flag scans of the raw argument
vector in `main.rs` with a single `clap` derive model in lib `src/cli.rs`,
accepting clap's stricter uniform errors (unknown flags and missing values are
hard usage errors: stderr, exit 2).

- [x] **Goal:** Make CLI option parsing a maintained library concern instead of
      brittle handwritten scanning with per-flag-family semantics.

- [x] Add `clap` (derive) as a direct dependency; parse via
      `Cli::try_parse_from` in `main()` with errors routed through the binary's
      usage-error path (stderr, exit 2) so exit/flush behavior stays under our
      control; `--version` keeps the rich `BUILD_INFO` rendering via a
      pre-parse scan (`disable_version_flag`)

- [x] Move the parser into lib `src/cli.rs` (`Cli`, `CliCommand`), re-exported
      from `lib.rs`; `main.rs` keeps only dispatch (version, `init`
      subcommand, mode detection, prompt precedence)

- [x] Cover the existing flag surface: `init [--workspace] [--force]`,
      `--no-tui`, `--color=<auto|no|yes>`, `--prompt`, `--profile`,
      `--log-file`, `--log-level`, `--log-format`, `--conversation-id`, nine
      `--auto-approve-*` booleans; add clap's `--help`

- [x] Deliberate strictness: unknown flags rejected; missing values hard-error
      (including `--profile`, previously silently ignored); duplicate
      `--color`/`--prompt` are last-wins; `init` accepted after global flags;
      `--no-color` keeps its migration hint via a hidden flag

- [x] Migrate parsing unit tests to `src/cli.rs`, add process-level tests
      (`--help` exit 0, unknown flag exit 2, migration hint exit 2), and
      update `docs/cli-options.md`, `docs/features.md`,
      `docs/implementation-notes.md`

**Key crates:** `clap`

### Phase 12: Context Compaction & Token Budgeting

- [x] **Goal:** Bound the model-facing context in long-running root
      conversations while preserving the full session transcript.

- [x] Separate the complete transcript from the model-facing context view.
      Keep the outer `AgentLoop` as the sole writer; background work receives
      immutable snapshots only.

- [x] Add tiered context entries and cached token estimates. Never compact the
      leading system prompt; keep recent messages verbatim. Use `tiktoken-rs`
      for OpenAI-compatible models and a byte-length estimate (4 characters ≈
      1 token) for Ollama. Counts are approximate and used for budgeting, not
      billing.

- [x] Configure `compact_target_ratio` per engine, default `0.5`, plus the
      `soft_compaction_threshold` (default `0.7`) and `hard_compaction_threshold`
      (default `0.9`) triggers per engine. Start a background summary after a
      completed turn at the soft threshold of the active `context_length`. Apply
      the summary at the next safe turn boundary. If the hard threshold is
      reached before the summary applies, synchronously drop old complete turns
      or oversized old results until the target is met.

- [x] Summarize the old prefix with the configured `[compaction].engine`. The
      summary must preserve decisions, open work, file paths, identifiers, user
      constraints, and the boundary before the verbatim tail. On summarizer
      failure or timeout, use the synchronous drop fallback and log the
      failure.

- [x] Persist a versioned compaction event in the existing session JSONL file.
      Store a monotonic message boundary and the prefix summary. A later event
      supersedes the previous model-context snapshot. Do not rewrite or delete
      original messages. Replay uses the latest event to rebuild the model view
      and keeps the raw transcript intact.

- [x] Keep tool-call/result groups indivisible during compaction. A boundary
      must never separate a `tool_call_id` from its one matching result, and
      compaction runs only when the serial tool queue is empty.

- [x] Keep the full transcript visible in the TUI and show a compaction marker
      at each boundary. The model receives a provider-safe marked summary
      followed by the verbatim tail. Do not insert a mid-conversation `system`
      message.

- [x] Re-evaluate the active budget when `/models` switches to a model with a
      different context length. Root and child contexts use the compaction
      settings resolved for their agent type. No manual `/compact` command is
      included.

- [x] Keep per-result wire-size protection and a default `glob` ceiling out of
      this phase. They are deferred to
      `docs/ideas/tool-result-size-guard.md`. The blueprint's earlier 8–16 KiB
      insertion-time truncation proposal is not selected.

- [x] Add unit and integration coverage for cached estimates, threshold
      behavior, summary application, fallback, tool-call/result pairing,
      append-only persistence, resume replay, model switching, and the visible
      TUI marker. Update `README.md`, `CONTEXT.md`, and the named runtime docs
      in the finalization plan.

**Key crates:** `tiktoken-rs`

### Phase 13: CLI Surface

**Decision:** Headless mode already works and clap-backed argument parsing
landed in Phase 11. This phase tracks the remaining agent/session CLI
controls selected for implementation. Structured output, quiet mode, stream
policy, buffering, exit-code changes, one-off provider/model overrides, and
session-maintenance commands are deferred to their idea notes.

- [x] **Goal:** Finish the selected CLI and agent controls. JSON/quiet output,
      output stream policy, buffering, exit-code changes, one-off provider/model
      overrides, and session maintenance are deferred to individual notes in
      `docs/ideas/` and `docs/ideas/session.md`.

- [x] Implement `ConsoleApp`, the headless driver that receives `UiUpdate`
      events and writes CLI output.

- [ ] Buffer stdout output; emit final JSON object on `--format json`
      (deferred to `docs/ideas/cli-output-buffering.md` and
      `docs/ideas/cli-json-output.md`)

- [x] Implement granular `--auto-approve-read`/`--auto-approve-write`/
      `--auto-approve-glob`/`--auto-approve-bash`/`--auto-approve-search`/
      `--auto-approve-edit`/`--auto-approve-web-search`/
      `--auto-approve-web-fetch`/`--auto-approve-spawn` flags (all nine implemented
      ahead of this phase — see `docs/cli-options.md`)

- [ ] Define exit codes: `0` success, `1` error, `2` interrupt, `3` tool failure, `4` approval denied
      (deferred to `docs/ideas/cli-exit-codes.md`)

- [x] Support `--conversation-id <id>` to resume and append to existing threads
      (implemented ahead of this phase — see `docs/cli-options.md`)

- [ ] Store-management subcommands for compression, decompression, and deletion
      (deferred to `docs/ideas/session.md`)

- [ ] Print minimal status to stderr by default; `--quiet` to suppress
      (deferred to `docs/ideas/cli-quiet-mode.md` and
      `docs/ideas/cli-output-streams.md`)

- [x] Always persist to a JSONL session file (no `--no-persist` flag)

- [x] Add `--agent <type>` for selecting the root agent in both TUI and
      headless modes. Store the selected type in new-session metadata and
      restore the latest selection on resume. Persist an explicit resume
      override as an `agent_type_changed` event. Legacy sessions without this
      metadata use `default`. A stored or explicitly requested type that is no
      longer available is a startup error unless the user explicitly selects
      an available type.

- [x] Add per-agent `max_tool_iterations`, default `50`, for root and child
      loops. Reject zero. This replaces the original per-engine limit of 15.

- [x] Add `--clone` with `--conversation-id <id>` in both TUI and headless
      modes. Clone a top-level session's full logical event stream into a new
      UUID with a `cloned_from` source link and copied title. Refuse to clone a
      child transcript or a session held by another Ylva process; all new
      messages append only to the clone.

- [x] Keep `--provider` and `--model` one-off overrides deferred to
      `docs/ideas/cli-provider-override.md` and
      `docs/ideas/cli-model-override.md`.

## 8. Critical Pitfalls to Avoid

1. **Never block the TUI thread.** Always use `tokio::spawn` + `mpsc`. If the agent blocks, the terminal
   freezes and `Ctrl+C` feels broken.
2. **Tool schemas must stay in sync.** Use `schemars` derive. Do not hand-edit JSON schemas when
   adding a tool.
3. **Context window overflow.** Count tokens (or estimate) and implement a sliding window or
   summary strategy before the LLM truncates from the wrong end.
4. **Untrusted bash commands.** Always require confirmation for shell execution. Never auto-execute
   `rm`, `curl | sh`, `git push`, etc.
5. **Streaming parse errors.** LLMs may emit malformed JSON for tool arguments. Be ready to catch
   parse errors, show the raw output, and ask the user or retry.
6. **Race on steer/injection.** A steer never aborts, so it cannot overlap a live stream by
   construction — queued `Prompt`s accumulate in `pending_steers` and are injected only at a
   natural point, and injection always starts exactly one fresh request. The remaining race is
   on **interrupt**: `current_cancel.take()` owns the in-flight stream so a stale `StreamCancelled`
   from the aborted task is ignored once `current_cancel` is `None`. Do not conflate the two jobs:
   the steer queue is for *cooperative redirection*; `current_cancel.take()` is only for the
   *abort* path.
7. **Lost partial messages on abort.** When cancelling a stream, always capture the `partial` string
   from the streaming task and append it to history before starting the next request. If you drop it,
   the conversation becomes incoherent.
8. **HTTP connection leaks.** Dropping the SSE stream adapter does not necessarily drop the underlying
   TCP socket. Abort the **request future** to ensure the connection is actually closed.
9. **Double-append on Cancelled → Done race.** If the cancel fires just as the stream naturally ends,
   ensure you don't append the assistant message twice. Use a `stream_finished: AtomicBool` inside
   the streaming task.
10. **Headless mode must never auto-approve dangerous tools by default.** If `--auto-approve-bash`
    (implemented; superseded the `--auto-approve-shell` placeholder name) is not passed, `bash`
    is denied and returns a clear tool error. The proposed exit code `4` is deferred to
    `docs/ideas/cli-exit-codes.md`. A script that forgets the flag must fail loudly, not
    silently execute `rm -rf /`.
11. **Context ownership confusion.** The inner streaming task must never hold a mutable reference to
    `Context`. Send completion events back to the outer agent loop via channel so the outer loop
    remains the sole writer.

## 9. Starter `Cargo.toml`

**Phase 1 minimal (Q23):**

**Phase 1 adds `uuid` (v4) and `serde_json` to the minimal set** so the full
§4B `Message`/`ToolCallRequest` skeleton is real typed API from first compile
(decision — no placeholders, dead-code, or `todo!()`):

```toml
[package]
name = "ylva"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
futures = "0.3"
thiserror = "1"
anyhow = "1"
uuid = { version = "1", features = ["v4"] }
serde_json = "1"
```

**Full dependency list (all phases):**

```toml
[dependencies]
tokio = { version = "1", features = ["full", "process"] }
tokio-util = "0.7"
futures = "0.3"
crossterm = "0.28"
ratatui = "0.29"
tui-textarea = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "0.8"
thiserror = "1"
anyhow = "1"
reqwest = { version = "0.12", features = ["json", "stream"] }
eventsource-stream = "0.2"
syntect = "5"
pulldown-cmark = "0.12"
textwrap = "0.16"
unicode-width = "0.2"
dirs = "5"
config = "0.14"
toml = "0.8"
sha2 = "0.10"
chrono = "0.4"
ignore = "0.4"
tempfile = "3"
uuid = { version = "1", features = ["v4"] }
nix = { version = "0.29", features = ["process", "signal"] }
clap = { version = "4", features = ["derive"] }
ctrlc = "3"
tiktoken-rs = "0.5"
```

## 10. Reference Implementations

Study these for patterns:

- `aichat` (Rust): Mature terminal chat client. See input handling, provider switching, and config.
- `rig` (Rust by Archetype): Higher-level LLM framework. Borrow its tool/structure abstractions.

## 11. Success Criteria

- [x] Runs in any terminal with zero GUI dependencies

- [x] Supports OpenRouter and Ollama with a single config change

- [x] Streams tokens in real-time without UI freezes

- [x] **Can interrupt model generation at any time with** `Ctrl+C`

- [x] **Can type and send new messages while the model is still streaming**
  (queued and injected at the next natural point)

- [x] Interrupted partial messages are preserved in conversation history;
  queued steers are shown as pending until injected

- [x] Reads and proposes file edits with diff preview

- [x] Executes shell commands only after explicit user confirmation

- [x] **Can ask the user clarifying questions via the `clarify` tool when uncertain**

- [x] **Can delegate focused tasks to child agents with curated briefing and config-driven types**

- [x] **Child-agent runs are persisted and auditable via the unified JSONL store (§4H)**

- [x] Conversation history is retained across restarts; resuming an old session works via explicit `--conversation-id`

- [x] Search respects `.gitignore`. The file-tree sidebar was dropped.

- [ ] **Headless mode uses the proposed structured exit-code contract** (deferred to `docs/ideas/cli-exit-codes.md`)

- [ ] **Headless mode supports `--format json` for programmatic output** (deferred to `docs/ideas/cli-json-output.md`)

- [x] **Headless mode supports `--conversation-id` to resume existing threads**

- [x] **Headless mode requires explicit `--auto-approve-*` flags for mutating tools**

- [x] Long conversations compact the model-facing context while preserving the
      full transcript (Phase 12).

- [x] Root agent types are selectable with `--agent` and restored on resume
      unless explicitly overridden.

- [x] A top-level session can be cloned with
      `--conversation-id <id> --clone`; new messages append to the clone and
      the source remains unchanged.

- [x] Root and child agents support per-agent `max_tool_iterations` settings.

## 12. Design Decisions

The following table lists the major design decisions made during planning and
where they are documented in the body of this document.

| Area                           | Decision                                                                                                                                                                                       | Location                                     |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------- |
| **Tool schema compilation**    | `schemars` derive at compile time; `ToolRegistry` holds all schemas                                                                                                                            | §4D Tool Registry                            |
| **Tool visibility**            | **Dynamic filtering:** runtime subset built per request based on mode/profile/whitelist                                                                                                        | §4D Tool Registry                            |
| **Agent loop architecture**    | Outer `tokio::select!` on `cmd_rx` + `agent_rx`; inner streaming task sends `AgentEvent` back to loop for state transitions                                                                    | §4B ReAct Loop                               |
| **Context ownership**          | Outer agent loop owns `Context`; inner task sends events via channel                                                                                                                           | §1 Vision, §4B ReAct Loop, §4C Context       |
| **Multi-line input**           | `Enter` sends; `Shift+Enter` inserts newline                                                                                                                                                   | §4B ReAct Loop                               |
| **Markdown parsing**           | Render Markdown while streaming; syntax highlighting uses a content-addressed cache                                                                                                            | §4F Rich Chat Rendering                      |
| **Tool call streaming**        | Provider emits raw `Delta`s; the agent's streaming task owns a `ToolCallAccumulator` per index, assembles calls, emits batch on `Done`                                                         | §4A Unified Provider Trait, §4B ReAct Loop   |
| **HTTP cancellation**          | Drop the **request future** to abort the TCP connection                                                                                                                                        | §4A Unified Provider Trait                   |
| **HTTP retry**                 | Pre-stream failures only (`429`/`5xx`/connect) with bounded exponential backoff honoring `Retry-After`; never retry mid-stream                                                                 | §4A Unified Provider Trait                   |
| **Steering semantics**        | A steer never aborts a live generation. During a tool-call sequence, wait for every issued call's matching result before injecting it. `Stop` is the interrupt path. | §1 Vision, §4B ReAct Loop, §8 Pitfalls |
| **State overlap**              | Explicit `AgentMode` enum (`Idle`, `Streaming`, `ToolPending`)                                                                                                                                 | §4B ReAct Loop                               |
| **Multi tool call execution**  | One message's calls accumulate by `tool_call_index`, queued, drained **serially** (approval inline at each call); deferred single re-prompt on empty queue; never parallel                     | §4A, §4B ReAct Loop                          |
| **Tool process cleanup**       | `process_group(0)` + kill the process group (`killpg`) for shell tools; `kill()` alone leaks grandchildren                                                                                     | §4D Tool Registry                            |
| **Tool result size**           | Aggregate context compaction is Phase 12; the per-result 512 KiB wire guard and bounded `glob` are deferred to `docs/ideas/tool-result-size-guard.md`                                          | §4C Context, Phase 12                        |
| **TUI event loop**             | `spawn_blocking` for crossterm + `select!` on `ui_rx` + `subagent_ui_rx` + `event_rx`                                                                                                          | §4B ReAct Loop                               |
| **TUI async context**          | `main.rs` uses `rt.block_on` for the TUI loop                                                                                                                                                  | §4B ReAct Loop                               |
| **Token counting**             | `tiktoken-rs` for OpenAI; byte heuristic for Ollama; cached per `TieredMessage`. Counts are **approximate/advisory** (encoding is model-family dependent); rely on `context_length × 0.9` margin | §4C Context State Machine                    |
| **Context tiers**              | `System` (never drop), `InitialContext` (never drop), `Recent` (drop oldest), `Compacted` (summarize in Phase 12)                                                                               | §4C Context State Machine                    |
| **Ollama tools**               | Dedicated Ollama provider reusing the unified raw `reqwest` streaming transport for `/v1/chat/completions`; model discovery uses native `/api/tags` and `/api/show`; no separate `ollama-rs` crate | §2 Crate Ecosystem, §5 OpenRouter vs Ollama  |
| **OS signals**                 | `ctrlc` handler mapped to `AgentCommand::Stop`                                                                                                                                                 | §1 Vision, §2 Crate Ecosystem                |
| **Tool binary checks**         | Lazy check at invocation with graceful fallback                                                                                                                                                | §4D Tool Registry                            |
| **Tool timeouts**              | `tokio::time::timeout` around `Command::output` with user-visible defaults                                                                                                                     | §4D Tool Registry                            |
| **Tool errors**                | `ToolError` variant in `UiUpdate` for visible failure reporting                                                                                                                                | §4B ReAct Loop, §4D Tool Registry            |
| **Headless mode**              | Shipped CLI supports prompts, resume, and granular `--auto-approve-*`; root `--agent` and `--clone` are planned in Phase 13; JSON output and exit-code changes are deferred                    | §7 CLI Surface                               |
| **Output streams**             | Separate stdout/stderr policy, `--quiet`, and buffering are deferred to `docs/ideas/cli-output-streams.md`, `cli-quiet-mode.md`, and `cli-output-buffering.md`                               | §7 CLI Surface                               |
| **Headless safety**            | Mutating tools require `--auto-approve-*`; proposed exit code `4` is deferred                                                                                                                 | §4E Safety, §7 CLI Surface                   |
| **Session cloning**            | `--conversation-id <id> --clone` forks a top-level session into a new UUID with copied events and `cloned_from`; the source is read-only                                                         | §4H Persistence, §7 CLI Surface               |
| **Conversation titles**        | Utility-engine conversation generates a concise title from the first user message and first assistant reply, then atomically persists a single-line value in the `meta` header (§4H decision 8) | §7 Phase 9, §4H decision 8                   |
| **Model switching**            | Supported mid-conversation via `/models`, which queries each configured provider connection once and switches the selected connection/model through `EngineRouter`; the `SwitchModel` command placeholder was dropped | §4B ReAct Loop, §6 Commands |
| **App state**                  | Dumb view model; agent loop owns state machine                                                                                                                                                 | §4B ReAct Loop                               |
| **Config format**              | Profiles and `.toml` agent files use TOML; `.md` agent files accept `+++` TOML or `---` YAML frontmatter                                                                                         | §4G Agent-Type File Formats                  |
| **Engine vs Provider mapping** | `Engine` = named model/runtime configuration; `provider_connection` = named endpoint/credential connection; `provider` = implementation kind; `Provider` = runtime trait object built from a connection at startup | §4A Provider Trait, §4G Engine Definitions |
| **Syntect themes**             | 2 built-in and selectable: dark, light; "monokai" is not bundled with `syntect` and stays an idea (`docs/ideas/monokai-theme.md`)                                                                | §4F Rich Chat Rendering                      |
| **edit tool**             | Both search-and-replace and line-range modes                                                                                                                                                   | §4D Tool Registry                            |
| **Slash commands**             | Only recognised at start of line                                                                                                                                                               | §7 CLI Surface                               |
| **Headless buffering**         | Buffering policy is deferred to `docs/ideas/cli-output-buffering.md`                                                                                                                           | §7 CLI Surface                               |
| **Clarify tool**               | Supports free-text answers and up to five selectable choices                                                                                                                                  | §4D Tool Registry                            |
| **Child-agent delegation**      | Tool-based `spawn_subagent` using reusable `AgentLoop`                                                                                                                                         | §4G Agent Types & Delegation                  |
| **Child-agent parallelism**     | Child agent runs in parallel with root agent; root loop never blocks                                                                                                                          | §4G Child-Agent Lifecycle                    |
| **Child-agent channels**        | Dedicated `subagent_ui_tx`/`subagent_ui_rx` for live child-conversation updates; store for audit                                                                                              | §4G UI & Headless                            |
| **Agent types**                 | Single pool; profile-driven with global behavior + local engine assignment                                                                                                                     | §4G Agent Types & Delegation                  |
| **Child-agent briefing**        | Hybrid system + user message; curated by root agent                                                                                                                                           | §4G Agent Types & Delegation                  |
| **Child-agent nesting**         | Forbidden; one level only (structural: `Child` role strips `spawn_subagent`)                                                                                                                   | §4G Agent Types & Delegation                  |
| **Child-agent execution**       | Current scheduler runs one child at a time in parallel with the root; lifecycle protocol supports future concurrent, out-of-order completion                                                                                                               | §4G Agent Types & Delegation                  |
| **Child-agent steering**        | Root steer queues and injects at the next natural point; child finishes naturally in the background                                                                                            | §4G Agent Types & Delegation                  |
| **Child-agent result**          | Tool-based `agent_finish` with unstructured fallback                                                                                                                                                | §4G Child-Agent Lifecycle                    |
| **Agent-type tools**            | Global agent files define whitelist; profiles can override                                                                                                                                      | §4G Agent-Type File Formats                  |
| **Agent-type config format**    | `.toml` flat or `.md` with `+++` TOML or `---` YAML frontmatter (body = system_prompt)                                                                                                          | §4G Agent-Type File Formats                   |
| **Child-agent safety**          | Inherits parent safety settings                                                                                                                                                                | §4G Safety Inheritance                        |
| **Child-agent persistence**     | `child_start` / `child_result` JSONL records in the parent session + dedicated sub-session file (§4H)                                                                                          | §4G Agent Types & Delegation, §4H            |
| **Child-agent UI**              | Full-screen child-conversation drill-in; no child sidebar                                                                                                                                      | §4G Agent Types & Delegation                  |
| **Child-agent headless**        | `--auto-approve-spawn` opt-in; `spawn_subagent` denied headless without it                                                                                                                      | §4G Agent Types & Delegation                  |
| **Build order**                | Headless first (Phase 1–3), TUI added as rendering layer in Phase 4                                                                                                                            | §7 Phased Build Plan                         |
| **Minimal build**              | No CLI surface, no config parsing, no TOML in Phase 1                                                                                                                                          | §7 Phase 1                                   |
| **Provider verification**      | Mock provider before real HTTP; real `reqwest` in Phase 2                                                                                                                                      | §7 Phase 1, §7 Phase 2                       |
| **Channel topology**           | Dual channels (`ui_tx` + `agent_tx`) proven from first compile                                                                                                                                 | §7 Phase 1                                   |
| **Steering**                   | Cooperative-inject at natural points included in early headless build to prove state-machine injection transitions                                                                             | §7 Phase 1                                   |
| **Context ownership**          | Single-writer rule enforced immediately; read-only clone passed to inner task                                                                                                                  | §7 Phase 1, §4C Context                      |
| **Persistence store**          | Plain append-only JSONL (no SQLite): global `~/.ylva/chats/<hash-of-cwd>/<uuid>.jsonl`; raw-rich, archive-compressible                                                                          | §4H decisions 1–3                            |
| **Schema evolution**           | Tolerant typed events + `meta` header `schema_version` + idempotent atomic upgraders; unknown fields/`type` ignored                                                                             | §4H decision 4                               |
| **Resume/session model**       | Explicit `--conversation-id` resumes in place; `--conversation-id --clone` forks a top-level session to a new UUID without changing the source                                                   | §4H decision 5                               |
| **Corruption recovery**        | Repair-on-open (truncate invalid tail before resume+append); reads never mutate                                                                                                                 | §4H decision 6                               |
| **Concurrent writers**         | `flock` every session open; fail-fast on conflict; listings never disturb active writer                                                                                                        | §4H decision 7                               |
| **Session findability**        | Sidecar = session abstract, greppable: in-file `meta` line when raw, `.meta.json` when compressed; `rg '"type":"meta"'`                                                                       | §4H decision 8                               |
| **Compression**                | Raw when new; auto-compress old unlocked sessions; `ylva session compress`/`decompress` manual override                                                                                        | §4H decision 11                              |
| **Durability**                 | `flush` per line, no per-line `fsync`; dedicated writer channel so loops never block on disk                                                                                                    | §4H decision 10                              |
| **Retention/deletion**         | History kept forever (no auto-delete); explicit `ylva session delete … --dry-run`; auto-compression bounds disk                                                                               | §4H decisions 11–12                          |
| **Message enum**               | `System`, `User`, `Assistant { content, status, tool_calls }`, `ToolResult`, `ToolError` (Q1)                                                                                               | §4B ReAct Loop                               |
| **Context Phase 1**            | Simple `Vec<Message>`; no `buffer`, no `TieredMessage` (Q2, Q13)                                                                                                                              | §4C Context                                  |
| **Persona / system message**   | `Context::new()` uses the code-owned global SOUL value; project `.ylva/SOUL.md` and home `~/.ylva/SOUL.md` replace it through the three-layer fallback                                                                                      | §4C Context, §4G Config, §7 Phase 1/6        |
| **Ollama tools validation**    | Spike required before Phase 2; fallback to native provider if `/v1` lacks tool support (Q3). **Spike 3 PASSED** — unified provider kept, no `ollama_native` fallback                                                                                                | §5 OpenRouter vs Ollama, §0 Spike 3                      |
| **`base_url` semantics**       | `base_url` is a generic provider concept: endpoint/path interpretation lives only in the concrete provider; OpenAI compatibility never enters the `Provider` trait. `OpenAiCompatibleProvider` receives a base incl. the origin+version prefix and internally appends `/chat/completions` (Phase 2 refinement) | §4A Unified Provider Trait |
| **HTTP retry**                 | Inside Provider; 3 retries, base 1 s, max 30 s, exponential with jitter, honoring `Retry-After`; pre-stream only (Q4)                                                                         | §4A Unified Provider Trait                   |
| **lib.rs API**                 | `AgentLoop::new(provider) -> (AgentLoop, Receiver<UiUpdate>)`; `run()` spawns outer task, returns `JoinHandle<Result<Context>>` (final history on shutdown via drop of `AgentLoop` → closed cmd channel); `send_command()` + `ui_updates()` (Q5, Q9)                                                | §4B ReAct Loop, §7 Phase 1                   |
| **PendingTool**                | `AssembledToolCall` + `timeout_secs`, with `execute()` method dispatching to `ToolRegistry` (Q6)                                                                                           | §4D Tool Registry                            |
| **Persistence**                | Separate `StoreEvent` enum (`Meta`, `MessageEvent`, `ChildStart`, `ChildResult`) (Q7)                                                                                                         | §4H Persistence                              |
| **Mock determinism**           | `tokio::time::pause` + `advance` in tests; `MockProvider::new(tokens, interval)` (Q8, Q21)                                                                                                    | §7 Phase 1                                   |
| **Native async trait**         | Rust 1.75+ native async trait; no `async-trait` crate (Q12)                                                                                                                                   | §4A Unified Provider Trait                   |
| **Interrupted assistant**      | `push_assistant(partial, Interrupted, vec![])` — drops incomplete tool calls (Q14)                                                                                                             | §4C Context                                  |
| **Agent loop shutdown**        | Infinite loop; drop `AgentLoop` to exit; `StreamError` → `Idle` (Q15)                                                                                                                         | §4B ReAct Loop                               |
| **Phase 1 scope**              | No tools, no registry, no schemars, no TUI (Q16)                                                                                                                                             | §7 Phase 1                                   |
| **Task structure**             | Two tasks: outer loop (select! on cmd_rx + agent_rx) + inner streaming task (Q18, Q22)                                                                                                      | §4B ReAct Loop                               |
| **OpenAI serialization**       | Free function in provider module; `Message` decoupled from wire format (Q19)                                                                                                               | §4A Unified Provider Trait                   |
| **CancellationToken**          | Outer loop creates it; passes clone to Provider; `take().cancel()` on `Stop` (Q20)                                                                                                         | §4B ReAct Loop                               |
| **Phase 1 Cargo.toml**         | `tokio`, `tokio-util`, `futures`, `thiserror`, `anyhow` plus `uuid` (`v4`) and `serde_json` for the full `Message` skeleton (Q23)                                                | §7 Phase 1, §9 Cargo.toml                    |
| **File layout**                | `src/agent.rs` remains a top-level module; the LLM package is split into `src/llm.rs` plus focused `src/llm/*.rs` modules as implementations grow (Q24)                                      | §7 Phase 1                                   |
| **Shared mock**                | `src/llm/mock_provider.rs` with the deterministic `MockProvider` and `MockEvent` script (Q25)                                                                                                  | §7 Phase 1                                   |
| **Empty Prompt**               | Ignored in Phase 1; `is_internal_continuation` deferred to Phase 3 (Q26)                                                                                                                       | §4B ReAct Loop                               |
