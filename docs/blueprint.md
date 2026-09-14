# Terminal AI Agent (Rust) — Full Build Plan

A practical blueprint for building a **terminal-first AI coding agent** in Rust
(similar to OpenCode or Hermes by Nous Research). It runs as a TUI application,
supports OpenRouter and local Ollama models, executes tools with user approval,
and allows real-time steering/interruption.

## 0. Proof-of-Concept Spikes

Before building the full architecture, validate the two riskiest assumptions through isolated spikes.

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
- Test rapid steering: send a mid-stream `AgentCommand::Prompt` — verify the stream is **not**
  cancelled (the steer is queued and injected at the next natural point) and the UI stays glitch-free.
- Test interruption: send a mid-stream `AgentCommand::Stop` — verify the stream cancels cleanly
  and a subsequent prompt restarts without UI glitches.

**Success criteria:**

- Frame time consistently ≤ 17 ms.
- Keyboard input (Enter, Ctrl+C, typing) responds instantly regardless of stream volume.
- Steady-state channel queue depth ≤ ~20 tokens at 1000+ msg/s
  (no consumer starvation / no effective backpressure).
- No memory growth (flat peak buffered memory) after 10 minutes of continuous token injection.

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
- **Memory:** scrolling ring is capacity-bounded and peak buffered bytes stay
  flat on the short run → criterion is consistent with the design; the
  mandated 10-minute (`--duration 600`) soak with a flat peak is still to be
  confirmed manually/CI.
- **Steering:** an auto `Prompt` fires ~3 s into the run and cancels/restarts
  the generator cleanly (verified via the exit summary); live visuals for
  "no UI glitches" require an interactive terminal.
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
`/v1/chat/completions` spec. Recorded from
[`spikes/spike-ollama-tool-calling/`](../spikes/spike-ollama-tool-calling/) (SSE logs in
`out/`).

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
  pulled and were not used. Raw capture: `spikes/spike-ollama-tool-calling/out/sse_*.log`.

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
  the next natural point — a model hand-off pause (`clarify` / tool-approval), a
  completed tool-call boundary, or the end of the current assistant response. Queued
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
│  ┌────────────┐  ┌──────────────────────────────────────┐    │
│  │ Sidebar    │  │ Chat / Code / Diff / Partial Stream  │    │
│  │ (Files)    │  │ (Scrollable)                         │    │
│  └────────────┘  └──────────────────────────────────────┘    │
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
   at the next natural point (model hand-off, completed tool call, or response end) — never by
   cancelling a live generation.

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
| `which`                | Validate that `bash`, `git`, `rg` exist before offering shell tools |
| `ignore`               | Respect `.gitignore` when building file tree or searching           |
| `tempfile`             | Safe scratchpads for agent drafts before disk writes                |
| `walkdir`              | Recursive directory traversal for the file explorer                 |

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
│       └── search.rs    # grep/ripgrep via std::process
├── widgets.rs           # module root (re-exports, shared widget state)
└── widgets/
    ├── chat_bubble.rs   # Custom ratatui widget for assistant messages
    ├── code_block.rs    # Syntect-highlighted block
    └── file_tree.rs     # Left sidebar explorer
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
/// backends return inconsistent metadata. `context_length` here is **advisory,
/// display-only** (e.g. a future `/models` slash command shows it); the
/// `EngineConfig.context_length` from TOML is the SSOT for engine construction.
struct ModelInfo {
    id: String,
    context_length: Option<usize>,
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

        // 2. Live updates from child-agent sidebar
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
    InitialContext, // Tier 1: Project root, file tree, workspace notes — NEVER dropped in initial implementation
    Recent,         // Tier 2: Last N user/assistant turns — dropped LAST
    Compacted,      // Tier 3: Previously summarized buckets — dropped before Recent
}

struct Context {
    messages: Vec<Message>,  // Phase 1: simple Vec.  Phase 10 adds TieredMessage + token_count.
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
    // Phase 3 adds: push_tool_result, push_tool_error, descope_pending_tools
    // Phase 10 adds: push_system, push_initial_context, maybe_compact, token counting
}
```

**Insertion rules (Phase 1 subset):**

- `Context::new()` seeds the initial system message `Message::System { content:
  "You are Ylva, a helpful agent with tool capabilities." }` (SSOT constant for
  Phase 1; Phase 9 sources this from a customizable **SOUL.md**,
  see §4G).
- `push_user(text)` → `Message::User { content: text }`
- `push_assistant(content, status, tool_calls)` →
  `Message::Assistant { content, status, tool_calls }`

**Phase 10 (future):** Replace `Vec<Message>` with `Vec<TieredMessage>`, add
`push_system`, `push_initial_context`, `push_compacted`, `maybe_compact`,
`find_oldest_droppable`, and `find_oversized_result`.

**Context ownership rule (HARD):** The outer agent loop is the **sole writer** to `Context`.

**Compaction (Phase 10 future):**

Phase 1 does not implement compaction or token counting. The full algorithm
(`maybe_compact`, `find_oldest_droppable`, `find_oversized_result`) is deferred
until Phase 10 when `TieredMessage` and `tiktoken-rs` are introduced.

**Compaction trigger and target:** `maybe_compact` uses a **hysteresis** model
with a single control, `compact_target_ratio` (fraction of `context_length`):

- **Trigger (high-water mark):** when `total_tokens > context_length × 0.9`.
- **Target (low-water mark):** compact down to **≤ `context_length × compact_target_ratio`**.
  Default `compact_target_ratio = 0.5` → ~0.4L headroom after compaction,
  preventing thrash on the next turn.
- The target is an **upper bound**: the result may end up smaller (e.g. one
  oversized tool result dropped can overshoot), but the algorithm performs
  **minimal intervention** — drops/summarizes the least amount needed to reach
  the target. It never aims smaller than the target.
- The ratio (not an absolute token count) auto-scales across engines: an 8 KB
  Ollama model and a 200 KB OpenRouter model use the same parameter.
- Both `context_length` and `compact_target_ratio` come from `EngineConfig`
  (per-engine), since a small-window model may want a tighter ratio.
- A "minimum drop ratio" guardrail is **not** added now (YAGNI); if compaction
  in practice over-discards, add `max_drop_ratio` ("never drop more than X% of
  current in one pass") then.

**Tool-result truncation (Phase 10 future):** Tool results are **truncated at
insertion** to a configurable cap (default ~8–16 KB / engine-tunable), with a
clear marker such as `… [truncated M of K chars]`. Combined with
`maybe_compact` preferring oversized results, a large result neither swallows
the window nor forces many older turns to be dropped. If the model needs more
of a file, it re-calls `read`/`edit` with `offset`/`limit` — the
existing tool contract.

**Phase 10 (future):** Replace `find_oldest_droppable` + `remove` with a
summarization pass: collect K oldest `Recent` messages, spawn a background
summarizer model, and replace them with one `Compacted` system message.

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
2. At the next natural point — the current response ends (`StreamDone`), a tool call
   completes, or a hand-off pause is reached — the outer loop calls
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

If the agent is stuck at a hand-off (tool approval or `clarify`) the steer supersedes it:
pending tools are descoped, and the message is delivered as the redirect (or the `clarify`
answer). This works remarkably well with modern instruction-tuned models. Aborting a
generation is reserved for the explicit interrupt (`Ctrl+C` / `Esc`).

**Token counting:** Use `tiktoken-rs` for OpenAI-compatible models and a byte-length
heuristic (4 chars ≈ 1 token) for Ollama as a cheap fallback. Maintain a rolling
window that trims the oldest user/assistant pairs (never the system prompt) when
the total exceeds the model limit. For a more advanced solution (Phase 10), summarise
dropped messages via a cheap summariser model call and prepend a system message
with the summary.

### D. Tool Registry + Dynamic Schemas

Never hand-write JSON schemas. Use `schemars` to derive them from structs.

```rust
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;

#[derive(JsonSchema, Deserialize)]
enum ReadMode {
    Lines, // default for all reads; line offset/limit
    Bytes, // explicit byte ranges/binary data
    Chars, // explicit character counts; UTF-8 code points (lossy decode)
}

impl Default for ReadMode {
    fn default() -> Self {
        Self::Lines
    }
}

#[derive(JsonSchema, Deserialize)]
struct ReadFileArgs {
    path: String,
    #[serde(default)]
    mode: ReadMode,          // defaults to Lines for every call
    #[serde(default)]
    offset: Option<usize>,   // 0-based index of the selected unit; line 1 is offset 0 in Lines mode
    #[serde(default)]
    limit: Option<usize>,    // number of selected units
}

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

The `clarify` tool is **free-text only**. The model poses an open-ended question
and the user answers via the normal input box. No multiple-choice options are
supported; if the model wants the user to pick between alternatives, it can
express them in the question text (e.g. "Should I use A or B?"). This keeps
the TUI rendering simple — a single question banner — and avoids modal
navigation logic for option selection.

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

**Tool binary checks:** Re-check tool availability **lazily** at invocation time,
not just at startup. If `ripgrep` is missing when `search` is invoked, catch the
error and show a helpful TUI message. Gracefully degrade: if `rg` is missing,
fall back to a slower `walkdir` + `regex` search.

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

**Tool approval rules (planned TOML config):** each tool's path-based policy is
configurable via a `[tool.<name>]` block. Named rules act as early defaults;
glob rules (last-match-wins) can override them:

```toml
[tool.read]
path_outside_workspace = deny   # default: deny
dot_path               = deny   # default: deny
# cross_filesystem not set — sandbox abstains; configure to deny|user when needed
globs = [
    "allow:**/.*",              # re-allow dot-files inside workspace
    "user:/**/.env",            # escalate .env files to user approval
]

[tool.write]
path_outside_workspace = deny   # default: deny
dot_path               = deny   # default: deny
# cross_filesystem not set — sandbox abstains; configure to deny|user when needed
```

Named rules:

- `path_outside_workspace` — canonicalized path is not under the workspace root
- `dot_path` — any component of the workspace-relative path starts with `.`
- `cross_filesystem` — canonicalized path is on a different filesystem (device)
  than the workspace root; catches writes into mounted shares or volumes.
  **Not configured by default** (sandbox abstains). Set to `deny` or `user`
  to restrict cross-device access.

**Flow:**

1. Agent emits `ToolCall("bash", { command: "..." })` or `ToolCall("clarify", { question: "..." })`
2. Agent loop **pauses** (does not start a new LLM request)
3. TUI renders modal or question banner: `Execute: git commit -m "..." ? [Y/n/e(edit)]`
   or `Clarify: Which API version do you want to use?`
4. User sends approval or answer via `cmd_tx`
5. Agent resumes: executes tool, appends result, re-prompts LLM

**Important:** The input box remains active during tool approval. The user can
even steer ("change the commit message to...") by typing and hitting Enter, which
queues a steer and injects it at the next natural point rather than cancelling
anything.

**Headless mode safety:** Mutating tools (`write`, `edit`, `bash`) require
explicit `--auto-approve-write` or `--auto-approve-shell` flags. If invoked without
the corresponding flag, the agent emits exit code `4` and a descriptive error to
stderr. A script that forgets the flag fails loudly, never silently executes `rm -rf /`.

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
engine override, timeout — declared once in config and usable in **any role**.
There is no separate "main agent" vs "sub-agent" category: a `research` agent
can run as the root conversation (the user types `ylva --agent research`) or as
a child spawned by a coding root. The same config entry, the same factory call.

What differs between those two cases is the agent's **role** — its position in
the spawn tree. Role carries the runtime plumbing that the type config cannot
hold (live channels, a parent's `CancellationToken`, a curated briefing):

| Concern | Decided by | Why |
|---|---|---|
| persona, tool whitelist, engine, timeout | **agent type** | static, TOML-deserialized |
| `parent_cancel: CancellationToken` | **role** | only a child has a parent |
| output channels (App UI vs. sidebar + parent) | **role** | depends on caller |
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
- Its own `subagent_ui_tx` channel for live updates to the TUI sidebar

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
├── SOUL.md                     # optional persona / initial system message (built-in default)
├── agents/                     # global, shared agent-type definitions
│   ├── research.md             # .md with +++ frontmatter (seeded format)
│   └── code_edit.md
└── profiles/
    ├── default/
    │   └── config.toml         # default profile (always exists)
    ├── local/
    │   └── config.toml
    └── work/
        └── config.toml
```

##### Config Source of Truth & Distribution

The `~/.ylva/` layout above is the **runtime** home-directory tree. Its
canonical contents are **maintained in the repo** as a `config/` tree that
**mirrors this layout exactly**, and embedded into the binary via
`include_str!`:

```text
config/                        # source of truth (versioned, embedded)
├── agents/
│   ├── research.md
│   └── code_edit.md
└── profiles/
    └── default/
        └── config.toml
```

Themes are **not** files — they are compiled into the binary and
name-selectable (`dark | light | monokai`), not seeded into any scope (§4F).

**Persona / initial system message (SOUL.md):** the initial system message of
a chat (the root agent's persona) follows the same scopes, expressed as a
**`SOUL.md`** in the project `.ylva/` and home `~/.ylva/` layers with a
built-in default. It resolves through the same three-layer fallback chain
below. Phase 1 hard-codes the default message (`"You are Ylva, a helpful agent
with tool capabilities."`) as an SSOT constant; the SOUL.md loader replaces
that constant's source in the config phase (§4C, §7 Phase 9).

**Three-layer fallback chain**, resolved per-file with **whole-layer replace**
(highest layer that defines a file wins; lower layers ignored for that file):

```text
project  .ylva/        (current project, cwd/git-root scope)
  → home  ~/.ylva/     (per-user, shared across projects)
  → built-in  config/  (embedded defaults — always the guaranteed floor)
```

- A file missing from a higher layer falls through to the next lower layer.
- Missing everywhere is **never a hard error** — built-in always supplies the
  standard default profile and agent types, so an empty `~/.ylva/` or a fresh
  project still runs out of the box.
- Project scope is discovered by walking up from cwd to the nearest ancestor
  containing a `.ylva/` (or the project's git root).

**Distribution — `ylva init`:**

- `ylva init` seeds the **full standard set** (default profile `config.toml`,
  `agents/research.md`, and `agents/code_edit.md`) into `~/.ylva/`.
- `ylva init --project` seeds **only** a minimal
  `.ylva/profiles/default/config.toml` with placeholder values into the
  project root. Agent types and additional profiles are added manually only
  when the project explicitly wants to override (inheritance covers the rest).
- Both are **write-if-missing** by default; `--force` reseeds a built-in file.

**Canonical paths, no synonyms:** there is exactly one location per file
(e.g. `.ylva/profiles/default/config.toml`); `.ylva/config.toml` is *not* a
synonym for the default profile. Profile names are referenced by value
(e.g. `--profile default`), never by path.

The project `.ylva/` is meant to be **committed** to the project repo (shared,
team-consistent config).

The home profile's optional `.env` file
(`~/.ylva/profiles/default/.env`) is auto-loaded at startup, with an
already-exported environment variable taking precedence over the `.env`
value. TOML string values can reference the merged environment with
`${ENV_VAR}`. Missing variables are startup errors, and secret values are
never logged. Project-scoped `.env` loading and the `ylva init --project`-
managed `.gitignore` remain Phase 9 work.

**Global agent types** (`~/.ylva/agents/`) define *behavior*: tools,
prompts, max iterations, timeouts, `can_spawn`. They are fully self-contained
and shared across all profiles, and the same entries are usable as root or
child.

**Profiles** (`~/.ylva/profiles/<name>/`) define *infrastructure*:
which engine (provider-connection/model/temperature/context-size) each agent
type uses, and can optionally override behavioral settings or disable a type
entirely.

##### Agent-Type File Formats

Each global agent type is defined by a file in `agents/` named after its type.
The **built-in/seeded** types (research, code_edit) ship as Markdown with
frontmatter (format 2 below); pure TOML remains supported for hand-authored
files. Two formats are supported:

1. **Pure TOML** (`.toml`):

```toml
# agents/research.toml
max_iterations = 10
timeout_secs = 120
allowed_tools = ["read", "search", "glob"]
can_spawn = false
system_prompt = """You are a deep research assistant.
Be thorough. Cite file paths when referencing code."""
```

2. **Markdown with TOML frontmatter** (`.md`):

```markdown
+++
max_iterations = 10
timeout_secs = 120
allowed_tools = ["read", "search", "glob"]
can_spawn = false
+++

You are a deep research assistant. Be thorough. Cite file paths when
referencing code. Always explain your reasoning before concluding.
```

Markdown frontmatter **must** be present; a `.md` file without a `+++` block is
a parse error. The body (after the closing `+++`) becomes the `system_prompt`.
This format is ideal for long, evolving prompts because it avoids escaped
quotes and indentation battles.

The **default** agent type is always available (seeded under
`config/agents/default.md`, overridable like any other type). It carries a
full tool whitelist and `can_spawn = true`, so a bare `ylva` invocation runs
the default agent as a `Root` that can delegate. This is the single
out-of-the-box experience.

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
compact_engine = "openrouter_small"   # context compaction summarizer (Phase 10)
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
the parent agent. In headless mode, `--auto-approve-shell` applies to
child agents as well.

#### Child-Agent Lifecycle

1. **Briefing:** the `ChildAgentManager` constructs a new `Context` with a
   **hybrid** prompt: a system message containing the type's persona,
   constraints, available tools, and output format; and a user message
   containing the curated `briefing`.
2. **Spawn:** the `ChildAgentManager` builds a `Role::Child` and calls
   `AgentFactory::create(profile, agent_type, role)`. The returned
   `AgentLoop` receives:
   - A filtered `ToolRegistry` (whitelist from the type's config;
     `spawn_subagent` stripped because role = `Child`)
   - The configured `Provider` (from the engine assigned to the type in the
     profile)
   - A new `CancellationToken`
   - Its own `subagent_ui_tx` for live sidebar updates
   - Its own `agent_tx` for sending the final result back to the parent
     The child runs its own ReAct loop in a `tokio::spawn` task.
3. **Parallel execution:** the root agent loop does NOT wait. It continues to
   process `Prompt` (steering) and `Stop` commands from the user. The child
   and root run concurrently. If the root LLM produces new tokens while the
   child is working, both appear in the TUI (main chat + sidebar panel).
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
   runs out.
5. **Re-integration:** the `result` field of the structured output is injected
   into the root conversation as a `Message::System` (or a dedicated
   `Message::ChildResult`) so the root LLM can use it on its next turn.

#### Steering While a Child Agent Runs

The input box is never disabled. If the user sends a new message while a
child is running, the message is **queued as a cooperative steer** — it is
never injected into a live root stream. It is appended to the root conversation
at the next natural point (a completed tool-call boundary / hand-off), and the
root LLM starts a fresh request with the updated context then.

The child continues to completion in the background. Its live events flow
through `subagent_ui_rx` to the sidebar panel. Its final result is appended to
the root history when it arrives via `agent_rx`.

**The root LLM then sees both the user's steering message and the child
result on its next turn**, and decides how to reconcile them. This is emergent
behavior — no special merge logic is required.

#### Persistence

Every child-agent run is recorded as a structured record in the global chat
store (see §4H). Two JSONL record shapes are written per parent session file:

```json
{"type":"child_start","v":1,"id":"<run-uuid>","parent_session":"<uuid>","agent_type":"research","task":"…","created_at":"…"}
{"type":"child_result","v":1,"id":"<run-uuid>","parent_session":"<uuid>","status":"success|timeout|max_iterations|error|aborted_by_user","result":"…","sources":["…"],"completed_at":"…"}
```

- The parent-session link is explicit (`parent_session`, `parent_message_index`).
- The full child conversation is stored as its **own session file** under
  the same `project_key` (its own `<sub-session_id>.jsonl`) with its own
  `meta` header (auto-title = the child `task`), mirroring §4H layout —
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

- **TUI:** a dedicated collapsible sidebar panel shows **live** child-agent
  activity: streaming tokens, tool calls, and status. It receives events via
  `subagent_ui_rx`, not by polling disk. The status bar shows
  `🔍 Child agent 'research' running... (14s / 120s)`. When finished, the panel
  shows the result summary and a link to view the full thread (loaded from the
  sub-session file for audit).
- **Headless:** child agents are **disabled by default**. Enable with
  `--allow-child-agents`. When enabled, stderr prints `Spawned child agent
  'research'...` and completion status. The `--auto-approve-*` flags inherited
  by the parent also apply to child agents.

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
   {"type":"meta","v":1,"schema_version":3,"app_version":"0.6.2","created_at":"…","cwd_hash":"…","engine":"…"}
   {"type": "user", "v": 1, "text": "…"}
   {"type": "assistant", "v": 2, "text": "…"}
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
   - Every session's `meta` header carries an **auto-generated title**: the
     first explicit user message, truncated to a configurable length (e.g.
     first 40–80 chars, on one line). The header itself is written to disk
     immediately (empty title) when the session is created, so no event is
     ever held only in memory waiting on it; the title is filled in once,
     by an atomic rewrite of just that first line, when the first
     conversation turn completes (the first assistant reply) — never by
     delaying the durability of anything committed before then.
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
     own `<sub-session>.jsonl` (same `project_key`, own `meta` header with an
     auto-title = child-agent `task`). Not streamed live.
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
    - **Manual override:** the user can `ylva compress` / `ylva decompress
      --all | --before <date> | <session-id>`. Decompress restores a
      `.jsonl.gz` to raw `.jsonl` (dropping the now-redundant sidecar or
      demoting it back to the in-file meta line).
    - The archive write order (decision 8) is followed: write sidecar first,
      then gzip, then remove the raw `.jsonl`.
    - **Retention/deletion:** no automatic deletion; history is kept forever.
      Old sessions are compacted in place by auto-compression (decision 11),
      and deletion is always an explicit user act via the CLI (decision 12).

12. **Deletion — explicit CLI only, with dry-run:**
    - History is never auto-deleted. Deleting a session is an explicit,
      user-triggered act: `ylva delete <session-id>…` (or
      `ylva delete --before <date>` / `--all`), supported by a **`--dry-run`
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
| **Streaming**        | SSE with `delta.tool_calls` chunks  | SSE via `/api/chat`                            |
| **Abort / Cancel**   | Drop HTTP request body              | Drop HTTP request body                         |
| **Cost**             | Pay-per-token                       | Free (your hardware)                           |

**Strategy:** Start with OpenRouter + `mistralai/mistral-small` or `anthropic/claude-3.5-haiku`
for cheap, reliable tool use while debugging the TUI. Once solid, test locally with Ollama + `llama3.1:8b`.

> **Assumption check — Spike 3 (§0):** The unified-provider architecture assumes
> Ollama's `/v1/chat/completions` endpoint supports the OpenAI `tools` spec
> identically to OpenRouter. This is **unverified**. Run Spike 3 before Phase 2
> begins; if it fails, the plan must add a second `ollama_native` provider
> targeting `/api/chat` and the "single provider serves both backends" claim
> in §4A must be revised.

**Ollama implementation note:** If you need full control over the request body and streaming response
parsing, you can also implement the Ollama provider using raw `reqwest` + `serde_json` to POST to
`/api/chat`. This avoids any abstraction mismatch with tool calling, though `ollama-rs` remains a
convenient choice for model listing and discovery.

## 6. Headless / CLI Mode

The agent supports a **non-interactive CLI mode** for automation, CI/CD,
and scripting. It reuses the same `Provider` trait, `ToolRegistry`,
and `AgentState` machine as the TUI, but drives them through a minimal
"fake TUI" wrapper that prints to stdout/stderr instead of rendering a
terminal UI.

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
defaults to `default` (the built-in seeded type, see §4G) and is overridable
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
│  │ FakeTuiWrapper                       │  │
│  │ • Receives UiUpdate events           │  │
│  │ • Buffers tokens to stdout           │  │
│  │ • Prints status to stderr            │  │
│  │ • Emits JSON at end (--format json)  │  │
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

# JSON output (buffered, emitted at end)
ylva --prompt "generate tests" --format json

# Auto-approve by tool category
ylva --prompt "refactor everything" \
  --auto-approve-read \
  --auto-approve-write \
  --auto-approve-shell

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
Error: Tool 'bash' requires --auto-approve-shell or interactive mode
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
- `--auto-approve-shell` → `bash`

The `clarify` tool is **not registered** in headless mode. The model never sees it;
if a decision is needed, the model must make a best-effort guess or error out.

If a mutating tool is invoked without the corresponding flag, the agent emits
exit code `4` and a descriptive error to stderr.

**Model selection:** Use `--profile <name>` to load a predefined engine configuration. Override explicitly
with `--provider` + `--model` flags (e.g., `--provider openrouter --model anthropic/claude-3.5-haiku`)
for one-off scripts. No hidden defaults.

**Commands:** Slash commands are only recognised as the first character of the
draft input, to avoid triggering inside pasted code. Phase 7 ships `/models`,
`/tools`, and `/clear` (alias `/reset`) in the TUI; `/save` was dropped
(persistence is unconditional, see §4H) and the headless CLI has no
interactive command surface. Anything outside the allowlist is ordinary chat
text.

**Conversation titles:** The `meta` header carries an auto-generated title —
the first user message, truncated and single-line (see §4H decision 8) — so
headless runs are easy to find later via `rg '"type":"meta"' ~/.ylva/chats/`.

**Persistence:** Always persists to a JSONL session file (§4H); there is no `--no-persist` flag.
Every run leaves a trace
for later review.

**Buffering strategy:** Buffer everything and emit at the end. For `--format text`, the user sees
status on stderr in real-time ("Streaming response...") but the actual LLM text is buffered and flushed
to stdout on completion. For `--format json`, the entire JSON object is emitted at the end.
This guarantees well-formed output for downstream tools and avoids partial output on interrupt.

## 7. Phased Build Plan

### Phase 0: Proof-of-Concept Spikes

See §0 for the three pre-build spikes (TUI responsiveness and HTTP stream cancellation).
These are standalone experiments, not part of the production code path.

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
      `api_key`, `reqwest::Client`) with
      helper function `serialize_messages(&[Message]) -> Value` (skip `ToolResult`/`ToolError`
      silently — unreachable in Phase 2). Append `/chat/completions` to `base_url` internally per the
      §4A `base_url` semantics note.

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

- [x] Prevent infinite loops: max iterations = 15 (configurable per engine)

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
      add profile-level `default_engine` (required) and `compact_engine` (optional,
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

- [x] `search` tool wrapping `ripgrep` via `std::process` with lazy binary check and graceful fallback

- [x] `clarify` tool for interactive open-ended questions; render question banner in TUI, user answers
      via input box; not available in headless mode

- [-] File tree sidebar using `ignore` + `walkdir`

- [x] **Diff view:** When `write` is proposed, render a side-by-side or unified diff in chat
      before approval

- [x] Add `edit` tool (line-based or search-and-replace) to reduce full-file rewrite tokens

- [x] `spawn_subagent` tool in `ToolRegistry` with config-driven types and tool whitelists

- [x] `ChildAgentManager` that owns child-agent lifecycle (spawn, timeout, cancel via `CancellationToken`)

- [x] `agent_finish` tool available only inside child-agent runs; fallback to unstructured
      text if never called

- [-] Child-agent persistence records (`child_start` / `child_result`) with parent session linkage (§4H)

- [x] Child agent inherits parent safety settings (`--auto-approve-*` in headless)

- [x] TUI sidebar panel for active/past child-agent runs (collapsible, shows runtime & status)

- [x] Headless: `--allow-child-agents` flag (disabled by default) + stderr status lines

**Key crates:** `which`, `ignore`, `tempfile`

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
      Phase 6.12 in `docs/plans/phase6-terminal-superpowers.md`, dropped because it
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
      three-layer fallback chain (project `.ylva/` → home `~/.ylva/` → embedded
      `config/`), `ylva init`/`ylva init --project` distribution, `SOUL.md`
      persona loading, and global `agents/*.md` type files are **out of scope
      for Phase 7** — see Phase 9 below. Profile-scoped `.env` loading is
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

### Phase 9: Full Configuration & Persona System

- [ ] **Goal:** Replace Phase 7's single flat config file with the complete `§4G`
      config-driven-types system, so behavior (agent types, persona) and
      infrastructure (engines/profiles) are both fully file-driven and
      shareable across projects/users.

- [ ] Three-layer fallback chain: project `.ylva/` (cwd/git-root scope) → home
      `~/.ylva/` (per-user) → embedded `config/` (built-in floor), resolved
      per-file with whole-layer replace; missing everywhere is never a hard
      error

- [ ] `ylva init` (seeds the full standard set into `~/.ylva/`) and
      `ylva init --project` (seeds a minimal `.ylva/profiles/default/config.toml`
      into the project root); both write-if-missing by default, `--force` reseeds.
      What standard config should be used for engine/provider/agent ?

- [ ] `SOUL.md` persona / initial system message loader (project `.ylva/` and
      home `~/.ylva/`, built-in default), replacing the Phase 1 SSOT constant's
      source per §4C/§4G

- [ ] Global agent-type definitions (`~/.ylva/agents/*.md` with `+++`
      frontmatter, e.g. `research.md`, `code_edit.md`), shared across profiles
      and usable as root or child agent types

- [ ] Profile-scoped agent-type definitions (`~/.ylva/profiles/<name>/agents/*.md`
      with `+++` frontmatter, e.g. `research.md`, `code_edit.md`), usable as root
      or child agent types

- [ ] Project-scoped agent-type definitions (`.ylva/agents/*.md`
      with `+++` frontmatter, e.g. `research.md`, `code_edit.md`), usable as root
      or child agent types

- [x] Profile-scoped `.env` auto-loading
      (`~/.ylva/profiles/<name>/.env`), with an already-exported environment
      variable taking precedence.

- [ ] Project-scoped `.env` auto-loading (e.g. `.ylva/profiles/<name>/.env`);
      `ylva init --project` also deploys a `.gitignore` inside the project's
      `.ylva/` ignoring `.env`

- [ ] Per-tool sandbox rule configuration: load configurable `[tool.*]` rule
      lists (including `outside_workspace`/`dot_path` overrides, §4E) from the
      layered config, replacing Phase 6's hardcoded two-rule default

**Key crates:** `toml` (already added in Phase 7); evaluate `dirs`/`directories`
for home-directory resolution when this phase starts.

### Phase 10: Context Compaction & Token Budgeting

- [ ] **Goal:** Keep the context window bounded for long-running conversations
      without losing the most valuable context (§4C).

- [ ] Tiered context management: replace `Context`'s `Vec<Message>` with
      `Vec<TieredMessage>` tagged `System` (never drop), `InitialContext` (never
      drop initially), `Recent` (drop oldest first), `Compacted` (drop before
      `Recent`)

- [ ] Token counting with cached counts per `TieredMessage` (re-count on
      insertion, not every request); `tiktoken-rs` for OpenAI-compatible
      engines, a byte-length heuristic (4 chars ≈ 1 token) for Ollama

- [ ] Context window compaction: when total > `context_length × 0.9`, compact to
      ≤ `context_length × compact_target_ratio` (default 0.5, per `EngineConfig`);
      minimal intervention, drops/summarizes oldest `Recent`/`Compacted` messages
      (`maybe_compact`), preferring an oversized tool result
      (`find_oversized_result`) over dropping whole turns

- [ ] Tool-result truncation at insertion (default ~8–16 KB, engine-tunable),
      with a `"… [truncated M of K chars]"` marker

- [ ] Phase 10 extension: replace "drop" with on-the-fly summarization via a
      cheap local model (engine = `compact_engine`, §4G/§4G config, wired in
      Phase 9)

**Key crates:** `tiktoken-rs`

### Phase 11: CLI Surface

**Decision:** Headless mode already works from Phase 1. This phase adds the full `clap` CLI interface
for scripting and automation.

- [ ] **Goal:** Automation and scripting support.

- [ ] Add `clap` for CLI argument parsing (`--prompt`, `--profile`, `--format`, `--conversation-id`,
      `--provider`, `--model`, `--auto-approve-*`, `--quiet`)

- [ ] Implement `FakeTuiWrapper` that receives `UiUpdate` events and prints to stdout/stderr

- [ ] Buffer stdout output; emit final JSON object on `--format json`

- [ ] Implement granular `--auto-approve-read`, `--auto-approve-write`, `--auto-approve-shell` flags

- [ ] Define exit codes: `0` success, `1` error, `2` interrupt, `3` tool failure, `4` approval denied

- [ ] Support `--conversation-id <id>` to resume and append to existing threads

- [ ] Store-management subcommands: `ylva compress` / `ylva decompress [--all|--before <date>|<session-id>]`,
      `ylva delete [<session-id>…|--before <date>|--all] --dry-run` (§4H decisions 11–12)

- [ ] Print minimal status to stderr by default; `--quiet` to suppress

- [ ] Always persist to a JSONL session file (no `--no-persist` flag)

**Key crates:** `clap`

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
10. **Headless mode must never auto-approve dangerous tools by default.** If `--auto-approve-shell`
    is not passed, `bash` must exit with code `4` and a clear error. A script that forgets the flag
    should fail loudly, not silently execute `rm -rf /`.
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
walkdir = "2"
ignore = "0.4"
which = "7"
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

- [ ] Runs in any terminal with zero GUI dependencies

- [ ] Supports OpenRouter and Ollama with a single config change

- [ ] Streams tokens in real-time without UI freezes

- [ ] **Can interrupt model generation at any time with** `Ctrl+C`

- [ ] **Can type and send new messages while the model is still streaming**
  (queued and injected at the next natural point)

- [ ] Interrupted partial messages are preserved in conversation history;
  queued steers are shown as pending until injected

- [ ] Reads and proposes file edits with diff preview

- [ ] Executes shell commands only after explicit user confirmation

- [ ] **Can ask the user clarifying questions via the `clarify` tool when uncertain**

- [ ] **Can delegate focused tasks to child agents with curated briefing and config-driven types**

- [ ] **Child-agent runs are persisted and auditable via the unified JSONL store (§4H)**

- [ ] Conversation history is retained across restarts; resuming an old session works via explicit `--conversation-id`

- [ ] Respects `.gitignore` when showing file trees and searching

- [ ] **Headless mode runs non-interactively with structured exit codes**

- [ ] **Headless mode supports `--format json` for programmatic output**

- [ ] **Headless mode supports `--conversation-id` to resume existing threads**

- [ ] **Headless mode requires explicit `--auto-approve-*` flags for mutating tools**

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
| **Markdown parsing**           | Render raw text while streaming; parse markdown only after `Done`                                                                                                                              | §4F Rich Chat Rendering                      |
| **Tool call streaming**        | Provider emits raw `Delta`s; the agent's streaming task owns a `ToolCallAccumulator` per index, assembles calls, emits batch on `Done`                                                         | §4A Unified Provider Trait, §4B ReAct Loop   |
| **HTTP cancellation**          | Drop the **request future** to abort the TCP connection                                                                                                                                        | §4A Unified Provider Trait                   |
| **HTTP retry**                 | Pre-stream failures only (`429`/`5xx`/connect) with bounded exponential backoff honoring `Retry-After`; never retry mid-stream                                                                 | §4A Unified Provider Trait                   |
| **Steering semantics**        | Cooperative: a steer never aborts a live generation; queued steers inject at the next natural point (hand-off, completed tool call, end of response). Interrupt (`Stop`) is the only abort path. `current_cancel.take()` ignores stale `StreamCancelled` | §1 Vision, §4B ReAct Loop, §8 Pitfalls |
| **State overlap**              | Explicit `AgentMode` enum (`Idle`, `Streaming`, `ToolPending`)                                                                                                                                 | §4B ReAct Loop                               |
| **Multi tool call execution**  | One message's calls accumulate by `tool_call_index`, queued, drained **serially** (approval inline at each call); deferred single re-prompt on empty queue; never parallel                     | §4A, §4B ReAct Loop                          |
| **Tool process cleanup**       | `process_group(0)` + kill the process group (`killpg`) for shell tools; `kill()` alone leaks grandchildren                                                                                     | §4D Tool Registry                            |
| **Tool result size**           | Truncate at insertion (tunable cap + marker); `maybe_compact` prefers oversized results                                                                                                        | §4C Context, §4D Tool Registry               |
| **TUI event loop**             | `spawn_blocking` for crossterm + `select!` on `ui_rx` + `subagent_ui_rx` + `event_rx`                                                                                                          | §4B ReAct Loop                               |
| **TUI async context**          | `main.rs` uses `rt.block_on` for the TUI loop                                                                                                                                                  | §4B ReAct Loop                               |
| **Token counting**             | `tiktoken-rs` for OpenAI; byte heuristic for Ollama; cached per `TieredMessage`. Counts are **approximate/advisory** (encoding is model-family dependent); rely on `context_length × 0.9` margin | §4C Context State Machine                    |
| **Context tiers**              | `System` (never drop), `InitialContext` (never drop), `Recent` (drop oldest), `Compacted` (summarize in Phase 10)                                                                               | §4C Context State Machine                    |
| **Ollama tools**               | Dedicated Ollama provider reusing the unified raw `reqwest` streaming transport for `/v1/chat/completions`; model discovery uses native `/api/tags` and `/api/show`; no separate `ollama-rs` crate | §2 Crate Ecosystem, §5 OpenRouter vs Ollama  |
| **OS signals**                 | `ctrlc` handler mapped to `AgentCommand::Stop`                                                                                                                                                 | §1 Vision, §2 Crate Ecosystem                |
| **Tool binary checks**         | Lazy check at invocation with graceful fallback                                                                                                                                                | §4D Tool Registry                            |
| **Tool timeouts**              | `tokio::time::timeout` around `Command::output` with user-visible defaults                                                                                                                     | §4D Tool Registry                            |
| **Tool errors**                | `ToolError` variant in `UiUpdate` for visible failure reporting                                                                                                                                | §4B ReAct Loop, §4D Tool Registry            |
| **Headless mode**              | Full CLI with `--prompt`, `--format`, `--conversation-id`, granular `--auto-approve-*`, exit codes `0–4`                                                                                       | §7 CLI Surface                               |
| **Output streams**             | Split stdout (content) / stderr (status); `--quiet` flag; buffer and emit at end                                                                                                               | §7 CLI Surface                               |
| **Headless safety**            | Mutating tools require `--auto-approve-*`; exit code `4` if denied                                                                                                                             | §4E Safety, §7 CLI Surface                   |
| **Conversation titles**        | Auto-generated from first user message, truncated single-line in the `meta` header (§4H decision 8)                                                                                                     | §7 CLI Surface, §4H decision 8               |
| **Model switching**            | Supported mid-conversation via `/models`, which queries each configured provider connection once and switches the selected connection/model through `EngineRouter`; the `SwitchModel` command placeholder was dropped | §4B ReAct Loop, §6 Commands |
| **App state**                  | Dumb view model; agent loop owns state machine                                                                                                                                                 | §4B ReAct Loop                               |
| **Config format**              | TOML for all config files; YAML explicitly rejected                                                                                                                                            | §2 Crate Ecosystem, §4G Agent Types & Delegation |
| **Engine vs Provider mapping** | `Engine` = named model/runtime configuration; `provider_connection` = named endpoint/credential connection; `provider` = implementation kind; `Provider` = runtime trait object built from a connection at startup | §4A Provider Trait, §4G Engine Definitions |
| **Syntect themes**             | 2 built-in and selectable: dark, light; "monokai" is not bundled with `syntect` and stays an idea (`docs/ideas/monokai-theme.md`)                                                                | §4F Rich Chat Rendering                      |
| **edit tool**             | Both search-and-replace and line-range modes                                                                                                                                                   | §4D Tool Registry                            |
| **Slash commands**             | Only recognised at start of line                                                                                                                                                               | §7 CLI Surface                               |
| **Headless buffering**         | Buffer everything; emit at end                                                                                                                                                                 | §7 CLI Surface                               |
| **Clarify tool**               | Free-text only; no multiple-choice options                                                                                                                                                     | §4D Tool Registry                            |
| **Child-agent delegation**      | Tool-based `spawn_subagent` using reusable `AgentLoop`                                                                                                                                         | §4G Agent Types & Delegation                  |
| **Child-agent parallelism**     | Child agent runs in parallel with root agent; root loop never blocks                                                                                                                          | §4G Child-Agent Lifecycle                    |
| **Child-agent channels**        | Dedicated `subagent_ui_tx`/`subagent_ui_rx` for live sidebar updates; store only for audit                                                                                                    | §4G UI & Headless                            |
| **Agent types**                 | Single pool; profile-driven with global behavior + local engine assignment                                                                                                                     | §4G Agent Types & Delegation                  |
| **Child-agent briefing**        | Hybrid system + user message; curated by root agent                                                                                                                                           | §4G Agent Types & Delegation                  |
| **Child-agent nesting**         | Forbidden; one level only (structural: `Child` role strips `spawn_subagent`)                                                                                                                   | §4G Agent Types & Delegation                  |
| **Child-agent execution**       | Current scheduler runs one child at a time in parallel with the root; lifecycle protocol supports future concurrent, out-of-order completion                                                                                                               | §4G Agent Types & Delegation                  |
| **Child-agent steering**        | Root steer queues and injects at the next natural point; child finishes naturally in the background                                                                                            | §4G Agent Types & Delegation                  |
| **Child-agent result**          | Tool-based `agent_finish` with unstructured fallback                                                                                                                                                | §4G Child-Agent Lifecycle                    |
| **Agent-type tools**            | Global agent files define whitelist; profiles can override                                                                                                                                      | §4G Agent-Type File Formats                  |
| **Agent-type config format**    | `.toml` flat or `.md` with TOML frontmatter (body = system_prompt)                                                                                                                              | §4G Agent-Type File Formats                   |
| **Child-agent safety**          | Inherits parent safety settings                                                                                                                                                                | §4G Safety Inheritance                        |
| **Child-agent persistence**     | `child_start` / `child_result` JSONL records in the parent session + dedicated sub-session file (§4H)                                                                                          | §4G Agent Types & Delegation, §4H            |
| **Child-agent UI**              | Collapsible sidebar panel + status bar line                                                                                                                                                    | §4G Agent Types & Delegation                  |
| **Child-agent headless**        | `--allow-child-agents` flag, disabled by default                                                                                                                                              | §7 CLI Surface                               |
| **Build order**                | Headless first (Phase 1–3), TUI added as rendering layer in Phase 4                                                                                                                            | §7 Phased Build Plan                         |
| **Minimal build**              | No CLI surface, no config parsing, no TOML in Phase 1                                                                                                                                          | §7 Phase 1                                   |
| **Provider verification**      | Mock provider before real HTTP; real `reqwest` in Phase 2                                                                                                                                      | §7 Phase 1, §7 Phase 2                       |
| **Channel topology**           | Dual channels (`ui_tx` + `agent_tx`) proven from first compile                                                                                                                                 | §7 Phase 1                                   |
| **Steering**                   | Cooperative-inject at natural points included in early headless build to prove state-machine injection transitions                                                                             | §7 Phase 1                                   |
| **Context ownership**          | Single-writer rule enforced immediately; read-only clone passed to inner task                                                                                                                  | §7 Phase 1, §4C Context                      |
| **Persistence store**          | Plain append-only JSONL (no SQLite): global `~/.ylva/chats/<hash-of-cwd>/<uuid>.jsonl`; raw-rich, archive-compressible                                                                          | §4H decisions 1–3                            |
| **Schema evolution**           | Tolerant typed events + `meta` header `schema_version` + idempotent atomic upgraders; unknown fields/`type` ignored                                                                             | §4H decision 4                               |
| **Resume/session model**       | Explicit `--conversation-id` (UUID) only; no auto-resume, no startup picker                                                                                                                    | §4H decision 5                               |
| **Corruption recovery**        | Repair-on-open (truncate invalid tail before resume+append); reads never mutate                                                                                                                 | §4H decision 6                               |
| **Concurrent writers**         | `flock` every session open; fail-fast on conflict; listings never disturb active writer                                                                                                        | §4H decision 7                               |
| **Session findability**        | Sidecar = session abstract, greppable: in-file `meta` line when raw, `.meta.json` when compressed; `rg '"type":"meta"'`                                                                       | §4H decision 8                               |
| **Compression**                | Raw when new; auto-compress old unlocked sessions; `ylva compress`/`decompress` manual override                                                                                                 | §4H decision 11                              |
| **Durability**                 | `flush` per line, no per-line `fsync`; dedicated writer channel so loops never block on disk                                                                                                    | §4H decision 10                              |
| **Retention/deletion**         | History kept forever (no auto-delete); explicit `ylva delete … --dry-run`; auto-compression bounds disk                                                                                        | §4H decisions 11–12                          |
| **Message enum**               | `System`, `User`, `Assistant { content, status, tool_calls }`, `ToolResult`, `ToolError` (Q1)                                                                                               | §4B ReAct Loop                               |
| **Context Phase 1**            | Simple `Vec<Message>`; no `buffer`, no `TieredMessage` (Q2, Q13)                                                                                                                              | §4C Context                                  |
| **Persona / system message**   | `Context::new()` seeds `Message::System` = "You are Ylva, a helpful agent with tool capabilities." (SSOT constant); a config phase loads it from a `SOUL.md` (project `.ylva/` / home `~/.ylva/`, three-layer fallback)                                                                                      | §4C Context, §4G Config, §7 Phase 1/6        |
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
