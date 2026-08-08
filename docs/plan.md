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
  "No channel backpressure" is measured as **steady-state queue depth ≤ 1 frame's worth (~20 tokens) at 1000+ msg/s**,
  i.e. the consumer never falls behind — not as the producer never waiting on a full channel.
  A bounded channel (`send().await` throttles the generator under overload) keeps memory flat regardless.
- Test rapid steering: send a mid-stream `AgentCommand::Prompt` — verify the stream is **not**
  cancelled (the steer is queued and injected at the next natural point) and the UI stays glitch-free.
- Test interruption: send a mid-stream `AgentCommand::Stop` — verify the stream cancels cleanly
  and a subsequent prompt restarts without UI glitches.

**Success criteria:**

- Frame time consistently ≤ 17 ms.
- Keyboard input (Enter, Ctrl+C, typing) responds instantly regardless of stream volume.
- Steady-state channel queue depth ≤ ~20 tokens at 1000+ msg/s (no consumer starvation / no effective backpressure).
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

1. **The TUI thread never blocks on I/O.** All LLM and tool I/O runs in a `tokio` task and communicates back via channels.
2. **The input box is never disabled.** The user can type and hit Enter at any moment — even while tokens are streaming.
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
├── ui.rs                # All ratatui widgets and layout math
├── events.rs            # Crossterm key → AppAction mapping (Ctrl+C, Enter)
├── config.rs            # Settings, API keys, model selection
├── db.rs                # SQLite persistence for chats
├── cli.rs               # Headless mode: arg parsing, fake TUI wrapper, exit codes
├── llm/
│   ├── mod.rs           # trait Provider { async fn chat(...) }
│   └── openai_provider.rs    # Raw reqwest + eventsource-stream targeting OpenAI spec
│                            # Works for both OpenRouter and Ollama via config
├── agent/
│   ├── mod.rs           # ReAct loop: select! { stream, cmd_rx }
│   ├── context.rs       # Message buffer, token counting, summarization,
│   │                    # PARTIAL message handling on abort
│   └── tools/
│       ├── mod.rs       # Tool trait + Registry
│       ├── fs.rs        # read_file, write_file, list_dir, edit_file
│       ├── shell.rs     # execute_command (with user confirmation!)
│       └── search.rs    # grep/ripgrep via std::process
└── widgets/
    ├── chat_bubble.rs   # Custom ratatui widget for assistant messages
    ├── code_block.rs    # Syntect-highlighted block
    └── file_tree.rs     # Left sidebar explorer
```

## 4. Core Implementation Patterns

**Unified provider decision:** Ollama exposes an OpenAI-compatible API at `/v1/chat/completions` (since v0.1.24).
Therefore, we do **not** need separate provider crates. A **single** raw `reqwest` + `eventsource-stream`
implementation targets the OpenAI chat-completions spec and works for both OpenRouter and Ollama via
configuration (`base_url`, `api_key`, `model`). This eliminates:

- Code duplication between two provider implementations.
- Hidden cancellation semantics inside convenience wrappers (`async-openai`, `ollama-rs`).
- The risk that one crate's stream adapter leaks connections when dropped.

All streaming logic — SSE parsing, `CancellationToken` polling, `ToolCallAccumulator` — lives in one place.

### A. Unified Provider Trait (with Abort)

The runtime abstraction is a **single** raw HTTP implementation targeting the OpenAI completions spec.
It supports **dropping the in-flight request** when the user interrupts or steers.

> **Terminology mapping:** The config calls this an **engine** (a named bundle of `base_url`, `model`,
> `temperature`, etc.). At startup the `Config` module builds a `HashMap<String, Arc<dyn Provider>>`
> from `Engine` definitions. The rest of the code only knows the `Provider` trait; "engine" is strictly
> a config concept.

```rust
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[async_trait]
trait Provider: Send + Sync {
    /// Stream back partial text + potential tool_calls.
    /// The implementation polls the CancellationToken; when it fires,
    /// the underlying HTTP connection is dropped.
    async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> anyhow::Result<mpsc::Receiver<StreamEvent>>;
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

**OpenRouter:** Configure `base_url = "https://openrouter.ai/api/v1"` and pass the OpenRouter API key.

**Ollama:** Configure `base_url = "http://localhost:11434/v1"`. No API key required for local Ollama.

**Tool call accumulation:** OpenAI streams tool calls incrementally across multiple
SSE chunks (`delta.tool_calls[0].function.arguments`). The **provider** does **not**
buffer or validate these fragments; it emits each chunk as `StreamEvent::Delta`.
The **agent's streaming task** owns the `ToolCallAccumulator`(s), buffering
fragmented name and argument deltas, validating JSON completeness, and only
emitting a `ToolCall` event (via `agent_tx`) once the full JSON is parseable —
the outer loop never sees raw deltas and stays the sole writer of `Context`.
If the stream ends with incomplete tool JSON, the accumulator emits an error
with the raw fragment for inspection.

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

This is the only way to own the state machine correctly. The inner streaming task
sends per-token updates to the TUI via `ui_tx`, but it **must** send a single
completion event back to the agent loop via `agent_tx` so the loop knows when to
transition `AgentMode` and trigger the next action (re-prompt, tool approval, etc.).

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
    ApproveTool { id: Uuid }, // Approve a pending tool
    SwitchModel(String),     // PLACEHOLDER: intentionally not yet wired anywhere in the
                             // select! match (only Prompt/Stop/ApproveTool are handled).
                             // Kept to document intent; wired to a /model slash command in
                             // the slash-command phase (Phase 6/7).
}

enum AgentEvent {
    StreamDone { partial: String },              // Natural completion
    StreamCancelled { partial: String },         // User aborted mid-stream
    ToolCall { name: String, arguments: Value, id: Uuid },
    ToolDone { id: Uuid, result: String },       // Tool executed successfully
    ToolError { id: Uuid, error: String },       // Tool failed or timed out
    ToolKilled { id: Uuid },                     // User cancelled tool via Stop
    StreamError(String),     // Provider or network error
}

enum UiUpdate {
    AppendText(String),
    ToolStart { name: String, id: Uuid },
    ToolPending { name: String, id: Uuid }, // waiting for approval
    ToolDone { id: Uuid, result: String },
    ToolError { id: Uuid, error: String },
    ToolKilled { id: Uuid },              // User cancelled tool via Ctrl+C
    StreamStarted,
    StreamDone,
    StreamCancelled,
    SteerQueued,        // A steer is queued; its bubble renders as pending until injected
    SteerApplied,       // Queued steers injected; their bubbles become normal user messages
    Error(String),
}

enum AgentMode {
    Idle,               // Waiting for user
    Streaming,          // LLM tokens in flight
    ToolPending,        // Waiting for user to approve/deny tool
    ToolExecuting,      // Tool running in background task
}

// Builds a request from an immutable snapshot and spawns the inner streaming task.
// The inner task owns only the `partial` buffer and tool-call accumulators; it never
// writes `Context`. Returns the fresh `CancellationToken` so the outer loop tracks it.
fn start_request(
    context: &Context,
    tools: &ToolRegistry,
    provider: Arc<dyn Provider>,
    mode: &AgentMode,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<AgentEvent>,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let ctx = context.messages().clone(); // read-only snapshot
    let schemas = tools.filter_for_mode(mode).schemas();
    tokio::spawn(async move {
        let mut stream = provider.chat(ctx, schemas, cancel.clone()).await?;
        let _ = ui_tx.send(UiUpdate::StreamStarted).await;
        let mut partial = String::new();
        let mut accums: HashMap<usize, ToolCallAccumulator> = HashMap::new();
        let mut done_calls: Vec<AssembledToolCall> = Vec::new();
        while let Some(event) = stream.recv().await {
            match event {
                StreamEvent::Text(s) => { partial.push_str(&s); ui_tx.send(UiUpdate::AppendText(s)).await.ok(); }
                StreamEvent::Delta(chunk) => { accums.entry(chunk.tool_call_index).or_default().feed(chunk); }
                StreamEvent::Done => {
                    if accums.values().any(|a| a.has_incomplete()) {
                        agent_tx.send(AgentEvent::StreamError("Incomplete tool call JSON".into())).await.ok();
                    } else {
                        ui_tx.send(UiUpdate::StreamDone).await.ok();
                        for a in accums.into_values() { done_calls.push(a.into_tool_call()); }
                        for call in &done_calls { agent_tx.send(AgentEvent::ToolCall(call.clone())).await.ok(); }
                        agent_tx.send(AgentEvent::StreamDone { partial }).await.ok();
                    }
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

// Spawn agent
let agent_handle = tokio::spawn(async move {
    let mut context = Context::new();
    let mut mode = AgentMode::Idle;
    let mut current_cancel: Option<CancellationToken> = None;
    let mut current_tool_cancel: Option<CancellationToken> = None;
    // Steers awaiting injection at the next natural point. Drawn as pending bubbles.
    let mut pending_steers: VecDeque<String> = VecDeque::new();
    // A single LLM message may request several tool calls. They are executed
    // SERIALLY, one after another, from this FIFO queue (never in parallel).
    let mut pending_tools: VecDeque<PendingTool> = VecDeque::new();

    loop {
        tokio::select! {
            // 1. User commands
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    AgentCommand::Prompt(text) => {
                        let is_internal_continuation = text.is_empty();

                        if mode == AgentMode::Idle && !is_internal_continuation {
                            // Fresh user turn while idle: append and start immediately.
                            context.push_user(text);
                            mode = AgentMode::Streaming;
                            current_cancel = Some(start_request(&context, &tools, provider.clone(),
                                &mode, ui_tx.clone(), agent_tx.clone()));
                        } else if is_internal_continuation {
                            // Internal deferred re-prompt (tool batch drained, or a natural
                            // point reached with queued steers): inject any steers, start fresh.
                            if inject_pending_steers(&mut pending_steers, &mut context) {
                                ui_tx.send(UiUpdate::SteerApplied).await.ok();
                            }
                            mode = AgentMode::Streaming;
                            current_cancel = Some(start_request(&context, &tools, provider.clone(),
                                &mode, ui_tx.clone(), agent_tx.clone()));
                        } else {
                            // === STEERING is cooperative: never cancel the live stream ===
                            // Queue the message; it is injected at the next natural point
                            // (model hand-off, completed tool call, or end of response).
                            // The TUI already drew the user bubble as "pending".
                            pending_steers.push_back(text);
                            ui_tx.send(UiUpdate::SteerQueued).await.ok();
                        }
                    }

                    AgentCommand::Stop => {
                        // INTERRUPT: hard-abort the current round; a steer never does this.
                        // Discard any queued steers (the redirection is abandoned).
                        pending_steers.clear();
                        // Cancel streaming LLM request if in flight
                        if let Some(ct) = current_cancel.take() {
                            ct.cancel();
                            // StreamCancelled handler will detect current_cancel is None
                            // and transition mode to Idle after appending partial
                        }
                        // Cancel running tool if in flight
                        if let Some(tool_ct) = current_tool_cancel.take() {
                            tool_ct.cancel();
                            // ToolKilled event will arrive via agent_rx
                        }
                    }

                    AgentCommand::ApproveTool { id } => {
                        if matches!(mode, AgentMode::ToolPending) {
                            // Approve/execute the HEAD of the serial queue (the one being presented).
                            if let Some(pending) = pending_tools.pop_front() {
                                if pending.id != id {
                                    // Out-of-order approval guard: put it back and ignore.
                                    pending_tools.push_front(pending);
                                } else {
                                    // Spawn tool execution in a separate task so the
                                    // agent loop stays responsive to Stop / steering.
                                let tool_cancel = CancellationToken::new();
                                current_tool_cancel = Some(tool_cancel.clone());
                                mode = AgentMode::ToolExecuting;
                                let ui_tx = ui_tx.clone();
                                let agent_tx = agent_tx.clone();

                                tokio::spawn(async move {
                                    let timeout = tokio::time::Duration::from_secs(
                                        pending.timeout_secs.unwrap_or(30)
                                    );
                                    let result = tokio::select! {
                                        biased;
                                        _ = tool_cancel.cancelled() => {
                                            ui_tx.send(UiUpdate::ToolKilled { id }).await.ok();
                                            agent_tx.send(AgentEvent::ToolKilled { id }).await.ok();
                                            return;
                                        }
                                        res = tokio::time::timeout(timeout, pending.execute()) => {
                                            match res {
                                                Ok(Ok(output)) => {
                                                    ui_tx.send(UiUpdate::ToolDone { id, result: output.clone() }).await.ok();
                                                    AgentEvent::ToolDone { id, result: output }
                                                }
                                                Ok(Err(e)) => {
                                                    ui_tx.send(UiUpdate::ToolError { id, error: e.clone() }).await.ok();
                                                    AgentEvent::ToolError { id, error: e }
                                                }
                                                Err(_) => {
                                                    let msg = format!("Tool timed out after {}s", timeout.as_secs());
                                                    ui_tx.send(UiUpdate::ToolError { id, error: msg.clone() }).await.ok();
                                                    AgentEvent::ToolError { id, error: msg }
                                                }
                                            }
                                        }
                                    };
                                    agent_tx.send(result).await.ok();
                                });
                                } // end else (id matched)
                            } // end if let Some(pending)
                        } // end if matches! ToolPending
                    } // end ApproveTool arm
                } // end match cmd
            }

            // 2. Lifecycle events from streaming task AND tool executor
            Some(evt) = agent_rx.recv() => {
                match evt {
                    AgentEvent::StreamDone { partial } => {
                        // Natural point: the current response ended.
                        context.push_assistant(partial);
                        if inject_pending_steers(&mut pending_steers, &mut context) {
                            // Steers queued during the response: inject and start a fresh turn.
                            ui_tx.send(UiUpdate::SteerApplied).await.ok();
                            mode = AgentMode::Streaming;
                            current_cancel = Some(start_request(&context, &tools, provider.clone(),
                                &mode, ui_tx.clone(), agent_tx.clone()));
                        } else {
                            mode = AgentMode::Idle;
                        }
                    }
                    AgentEvent::StreamCancelled { partial } => {
                        // Only reachable after a genuine interrupt (Stop) — a steer never aborts.
                        // If a new stream is already in flight, this is a stale cancellation.
                        if current_cancel.is_none() {
                            context.push_assistant_interrupted(partial);
                            mode = AgentMode::Idle;
                        }
                        // else: ignore; the new request already replaced current_cancel
                    }
                    AgentEvent::ToolCall(call) => {
                        if !pending_steers.is_empty() {
                            // STEER supersedes the pause (natural point): descope all queued
                            // tools, deliver the steers as the answer/redirect, re-prompt fresh.
                            pending_tools.clear();
                            context.descope_pending_tools();
                            inject_pending_steers(&mut pending_steers, &mut context);
                            ui_tx.send(UiUpdate::SteerApplied).await.ok();
                            mode = AgentMode::Streaming;
                            current_cancel = Some(start_request(&context, &tools, provider.clone(),
                                &mode, ui_tx.clone(), agent_tx.clone()));
                        } else {
                            // Enqueue; present for approval only if not already presenting one.
                            pending_tools.push_back(PendingTool {
                                name: call.name, arguments: call.arguments, id: call.id,
                            });
                            if mode != AgentMode::ToolPending {
                                mode = AgentMode::ToolPending;
                                if let Some(head) = pending_tools.front() {
                                    ui_tx.send(UiUpdate::ToolPending {
                                        name: head.name.clone(), id: head.id,
                                    }).await.ok();
                                }
                            }
                        }
                    }
                    AgentEvent::ToolDone { id, result } => {
                        // Natural point: a tool-call boundary was reached.
                        context.push_tool_result(id, result);
                        if inject_pending_steers(&mut pending_steers, &mut context) {
                            // STEER supersedes the rest of the batch; inject and re-prompt.
                            pending_tools.clear();
                            ui_tx.send(UiUpdate::SteerApplied).await.ok();
                            mode = AgentMode::Streaming;
                            current_cancel = Some(start_request(&context, &tools, provider.clone(),
                                &mode, ui_tx.clone(), agent_tx.clone()));
                        } else if let Some(next) = pending_tools.front() {
                            mode = AgentMode::ToolPending; // present next queued call for approval
                            ui_tx.send(UiUpdate::ToolPending {
                                name: next.name.clone(), id: next.id,
                            }).await.ok();
                        } else {
                            mode = AgentMode::Idle;
                            // Queue drained: DEFERRED re-prompt — all batch results are now in
                            // context, so a single follow-up call sees every result.
                            cmd_tx.send(AgentCommand::Prompt("".to_string())).await.ok();
                        }
                    }
                    AgentEvent::ToolError { id, error } => {
                        context.push_tool_error(id, error);
                        if inject_pending_steers(&mut pending_steers, &mut context) {
                            pending_tools.clear();
                            ui_tx.send(UiUpdate::SteerApplied).await.ok();
                            mode = AgentMode::Streaming;
                            current_cancel = Some(start_request(&context, &tools, provider.clone(),
                                &mode, ui_tx.clone(), agent_tx.clone()));
                        } else if let Some(next) = pending_tools.front() {
                            mode = AgentMode::ToolPending;
                            ui_tx.send(UiUpdate::ToolPending {
                                name: next.name.clone(), id: next.id,
                            }).await.ok();
                        } else {
                            mode = AgentMode::Idle;
                            // Re-prompt so LLM sees the error and can recover
                            cmd_tx.send(AgentCommand::Prompt("".to_string())).await.ok();
                        }
                    }
                    AgentEvent::ToolKilled { id } => {
                        context.push_tool_error(id, "Cancelled by user".into());
                        pending_tools.clear(); // user aborted the turn; drop the rest of the batch
                        pending_steers.clear(); // interrupt abandons any pending redirection
                        mode = AgentMode::Idle;
                    }
                    AgentEvent::StreamError(e) => {
                        pending_steers.clear(); // errored turn; abandon pending steers
                        mode = AgentMode::Idle;
                        // Error already sent to UI; wait for user
                    }
                }
            }
        }
    }
});

// In TUI thread (crossterm event loop)
loop {
    tokio::select! {
        // 1. UI updates from main agent
        Some(update) = ui_rx.recv() => {
            self.apply_main_ui_update(update);
            terminal.draw(|f| ui.draw(f, &self)).ok();
        }

        // 2. Live updates from sub-agent sidebar
        Some(sub_update) = subagent_ui_rx.recv() => {
            self.apply_subagent_ui_update(sub_update);
            terminal.draw(|f| ui.draw(f, &self)).ok();
        }

        // 3. Crossterm events (spawn_blocking bridged to async)
        Some(evt) = event_rx.recv() => {
            match evt {
                Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers: KeyModifiers::CONTROL }) => {
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
    messages: Vec<TieredMessage>,  // Ordered: [System, InitialContext, Compacted..., Recent...]
    buffer: String,                // Assistant message currently being streamed
    total_tokens: usize,           // Rolling token count (approximate)
    max_tokens: usize,             // From engine config (context_size × 0.9 safety margin)
}

struct TieredMessage {
    tier: MessageTier,
    msg: Message,
    token_count: usize,
}
```

**Insertion rules:**

- `push_system(text)` → `TieredMessage { tier: System, token_count: counted }`
- `push_initial_context(text)` → `TieredMessage { tier: InitialContext, token_count: counted }`
- `push_user(text)` → `flush_buffer()` + `TieredMessage { tier: Recent, ... }`
- `push_assistant(text)` → `TieredMessage { tier: Recent, ... }`
- `push_compacted(summary)` → `TieredMessage { tier: Compacted, ... }`

**Compaction (initial: drop only):**

```rust
impl Context {
    fn maybe_compact(&mut self) {
        while self.total_tokens > self.max_tokens {
            // Prefer dropping oversized tool results first, then oldest Recent/Compacted.
            if let Some(idx) = self.find_oversized_result().or_else(|| self.find_oldest_droppable()) {
                let removed = self.messages.remove(idx);
                self.total_tokens -= removed.token_count;
            } else {
                break; // Only System and InitialContext remain; cannot drop further
            }
        }
    }

    fn find_oldest_droppable(&self) -> Option<usize> {
        self.messages.iter()
            .position(|m| matches!(m.tier, MessageTier::Compacted | MessageTier::Recent))
    }

    // Q6c: a single oversized result must not crowd out many normal turns.
    fn find_oversized_result(&self) -> Option<usize> {
        self.messages.iter()
            .position(|m| m.is_tool_result && m.token_count > self.oversized_threshold)
    }
}
```

**Tool-result truncation:** Tool results are **truncated at insertion** to a
configurable cap (default ~8–16 KB / engine-tunable), with a clear marker such
as `… [truncated M of K chars]`. This bounds a single result regardless of tool
(`bash` output, large `read_file`, huge diffs). Combined with `maybe_compact`
preferring oversized results, a large result neither swallows the window nor
forces many older turns to be dropped. If the model needs more of a file, it
re-calls `read_file`/`edit_file` with `offset`/`limit` — the existing tool contract.

**Phase 6 (future):** Replace `find_oldest_droppable` + `remove` with a
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
rough estimate. The design tolerates this via the `context_size × 0.9` safety
margin; engine config can also override `context_size` as a lever if compaction
misbehaves in practice.

**Steering behavior:**  
When the user types "No, use `Arc` instead of `Rc`" while the model is still generating:

1. The message is drawn as a **pending bubble** and queued as a steer — the live stream
   is **not** cancelled.
2. At the next natural point — the current response ends (`StreamDone`), a tool call
   completes, or a hand-off pause is reached — `flush_buffer()` makes the (now complete)
   partial the last assistant message.
3. The queued steer(s) are appended as user messages.
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
the total exceeds the model limit. For a more advanced solution (Phase 6), summarise
dropped messages via a cheap summariser model call and prepend a system message
with the summary.

### D. Tool Registry + Dynamic Schemas

Never hand-write JSON schemas. Use `schemars` to derive them from structs.

```rust
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;

#[derive(JsonSchema, Deserialize)]
struct ReadFileArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
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
registry.register("read_file", schema_for!(ReadFileArgs), read_file_handler);
registry.register("bash", schema_for!(BashArgs), bash_handler);
registry.register("edit_file", schema_for!(EditFileArgs), edit_file_handler);
```

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
- **Sub-agents:** The `SubagentManager` passes the `allowed_tools` list from the
  subagent config; only those schemas are included in the request.

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
child runs in its own process group, and on timeout/cancel kill the **process group** (kill the group id,
e.g. via `nix::sys::signal::killpg`) to tear down the whole tree. Plain file tools need no
process-group treatment.

This pattern mirrors the LLM stream cancellation strategy: cooperative cancellation via `CancellationToken`,
no blocking in the main loop, and lifecycle events reported back via `agent_rx`.

**Tool timeouts:** Use `tokio::time::timeout` around `tokio::process::Command::output`.
Default: 30s for shell commands, 5s for file I/O. Display a TUI spinner while the
command runs.

**Tool errors:** Tool failures propagate back to the UI via a `ToolError` variant
in `UiUpdate` so the user sees what went wrong instead of a silent hang.

### E. Safety: Human-in-the-Loop

A terminal agent that can run `rm -rf` or `curl | sh` must ask for approval.

| Tool                      | Confirmation Level                                                      |
| ------------------------- | ----------------------------------------------------------------------- |
| `read_file`, `list_dir`   | `Auto`                                                                  |
| `write_file`, `edit_file` | `Ask` (show diff first)                                                 |
| `bash`, `git push`        | `Strict` (modal confirmation)                                           |
| `clarify`                 | `Interactive` (rendered as question banner; user answers via input box) |

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

**Headless mode safety:** Mutating tools (`write_file`, `edit_file`, `bash`) require
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

**Streaming markdown strategy:** Do not run `pulldown-cmark` on the raw partial
buffer every frame. Instead, render the entire partial assistant message as plain
text while streaming. Only apply `pulldown-cmark` + `syntect` **after** the stream
is `Done`. The user sees raw markdown during generation, which is acceptable for a
terminal agent and avoids re-parsing on every 16ms frame. Maintain a `Vec<RenderedBlock>`
in the UI state; when a complete code fence is detected, parse only that fence
with `syntect` and convert it to a `CodeBlock`.

**Syntect themes:** Ship three built-in themes — dark, light, and monokai — selectable
via config.

### G. Sub-Agent Delegation

The agent can spawn **sub-agents** — focused, short-lived ReAct agents that run
with curated context and return structured results. Sub-agents are treated as a
regular tool (`spawn_subagent`) in the main agent's `ToolRegistry`; when the LLM
calls it, the main loop pauses, executes the sub-agent, and resumes with the
result injected as a system message.

**One sub-agent at a time.** A sub-agent runs in parallel with the main agent,
but the main agent cannot spawn a second sub-agent while the first is still
running. This prevents resource contention and keeps the UX predictable. If the
main LLM calls while another sub-agent is active, the call is
queued or returns an error (configurable).

**No nesting.** Sub-agents are forbidden from calling `spawn_subagent`. The
maximimum delegation depth is one.

#### Spawn Mechanism

The LLM triggers a sub-agent via a standard tool call:

```rust
#[derive(JsonSchema, Deserialize)]
struct SpawnSubagentArgs {
    task: String,       // concise description of what to do
    briefing: String,   // curated context extracted from main conversation
    #[serde(default = "default_subagent_type")]
    subagent_type: String, // e.g. "research", "code_edit"
}
```

The main agent loop handles `ToolCall("spawn_subagent", ...)` like any other
pending tool: it emits `UiUpdate::ToolPending` and waits for user approval. Once
approved, the `SubagentManager` spawns a **separate** `AgentLoop` in a `tokio::spawn`
task. The main agent loop immediately returns to listening for user commands and
steering — it does **not** block on the sub-agent.

The sub-agent is a fully independent ReAct agent with:

- Its own `Context` (seeded from the briefing)
- Its own filtered `ToolRegistry` (whitelist from config)
- Its own `Provider` (from the engine assigned in the profile)
- Its own `CancellationToken` (linked to the parent's tool cancel)
- Its own `subagent_ui_tx` channel for live updates to the TUI sidebar

When the sub-agent finishes (via `finish` tool, timeout, or user cancellation),
it sends `AgentEvent::ToolDone` back to the main agent loop via `agent_rx`.
The main loop appends the result to its own `Context` and re-prompts.

**Channel isolation:** The sub-agent's live events flow through `subagent_ui_tx`
(a dedicated MPSC channel created by `SubagentManager`). The TUI's `tokio::select!`
listens to three sources: `ui_rx` (main agent), `subagent_ui_rx` (sub-agent),
and `event_rx` (crossterm).

#### Config-Driven Types

The model does **not** choose the provider, model, timeout, or iteration limit.
Those are defined in the user's config hierarchy.

##### Directory Layout

```text
~/.ylva/
├── subagents/                 # global, shared behavior definitions
│   ├── research.md            # .md with +++ frontmatter (seeded format)
│   └── code_edit.md
└── profiles/
    ├── default/
    │   └── config.toml        # default profile (always exists)
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
├── subagents/
│   ├── research.md
│   └── code_edit.md
└── profiles/
    └── default/
        └── config.toml
```

Themes are **not** files — they are compiled into the binary and
name-selectable (`dark | light | monokai`), not seeded into any scope (§4F).

**Three-layer fallback chain**, resolved per-file with **whole-layer replace**
(highest layer that defines a file wins; lower layers ignored for that file):

```text
project  .ylva/        (current project, cwd/git-root scope)
  → home  ~/.ylva/     (per-user, shared across projects)
  → built-in  config/  (embedded defaults — always the guaranteed floor)
```

- A file missing from a higher layer falls through to the next lower layer.
- Missing everywhere is **never a hard error** — built-in always supplies the
  standard default profile and subagents, so an empty `~/.ylva/` or a fresh
  project still runs out of the box.
- Project scope is discovered by walking up from cwd to the nearest ancestor
  containing a `.ylva/` (or the project's git root).

**Distribution — `ylva init`:**

- `ylva init` seeds the **full standard set** (default profile `config.toml`,
  `subagents/research.md`, and `subagents/code_edit.md`) into `~/.ylva/`.
- `ylva init --project` seeds **only** a minimal
  `.ylva/profiles/default/config.toml` with placeholder values into the
  project root. Subagents and additional profiles are added manually only when
  the project explicitly wants to override (inheritance covers the rest).
- Both are **write-if-missing** by default; `--force` reseeds a built-in file.

**Canonical paths, no synonyms:** there is exactly one location per file
(e.g. `.ylva/profiles/default/config.toml`); `.ylva/config.toml` is _not_ a
synonym for the default profile. Profile names are referenced by value
(e.g. `--profile default`), never by path.

The project `.ylva/` is meant to be **committed** to the project repo (shared,
team-consistent config).

**Secrets — never stored in config files.** API keys come only from the
environment. Optional per-scope `.env` files (e.g.
`~/.ylva/profiles/default/.env` or `.ylva/profiles/default/.env`) are
auto-loaded at startup, with an already-exported environment variable taking
precedence over the `.env` value. `ylva init --project` also deploys a
`.gitignore` inside the project's `.ylva/` (write-if-missing / `--force`) that
ignores `.env`, so secrets stay uncommitted while the rest of the config is
committed.

**Global subagents** (`~/.ylva/subagents/`) define _behavior_: tools,
prompts, max iterations, timeouts. They are fully self-contained and shared
across all profiles.

**Profiles** (`~/.ylva/profiles/<name>/`) define _infrastructure_:
which engine (provider/model/temperature/context-size) each subagent uses, and
can optionally override behavioral settings or disable subagents entirely.

##### Subagent File Formats

Each global subagent is defined by a file in `subagents/` named after its type.
The **built-in/seeded** subagents (research, code_edit) ship as Markdown with
frontmatter (format 2 below); pure TOML remains supported for hand-authored
files. Two formats are supported:

1. **Pure TOML** (`.toml`):

```toml
# subagents/research.toml
max_iterations = 10
timeout_secs = 120
allowed_tools = ["read_file", "search", "list_dir"]
system_prompt = """You are a deep research assistant.
Be thorough. Cite file paths when referencing code."""
```

2. **Markdown with TOML frontmatter** (`.md`):

```markdown
+++
max_iterations = 10
timeout_secs = 120
allowed_tools = ["read_file", "search", "list_dir"]
+++

You are a deep research assistant. Be thorough. Cite file paths when
referencing code. Always explain your reasoning before concluding.
```

Markdown frontmatter **must** be present; a `.md` file without a `+++` block is
a parse error. The body (after the closing `+++`) becomes the `system_prompt`.
This format is ideal for long, evolving prompts because it avoids escaped
quotes and indentation battles.

##### Engine Definitions (Model Plumbing)

An **engine** is the configuration name for a reusable bundle of LLM connection and sampling parameters.
At runtime, each engine definition is used to build a concrete `Provider` trait object. The terms are
separate concepts: **engine** = configuration data; **Provider** = runtime trait.

```toml
# profiles/work/config.toml
[engines.openrouter_fast]
provider_type = "openrouter"  # maps to Provider implementation (not the trait name)
model = "mistralai/mistral-small"
temperature = 0.2
context_size = 128000

[engines.ollama_local]
provider_type = "ollama"
model = "llama3.1:8b"
temperature = 0.7
context_size = 8192
```

Engines are named and referenced by string. They contain **only** model
specific properties (`provider_type`, `model`, `temperature`, `context_size`, `api_base`,
optionally `api_key`). They do **not** contain tools, prompts, or agent logic.

##### Profile Subagent Assignment & Overrides

Each profile declares which subagents are available and which engine runs them:

```toml
# profiles/work/config.toml
[agent]
engine = "openrouter_fast"   # main conversation agent engine

[subagents]
research = { engine = "openrouter_fast" }
code_edit = { engine = "ollama_local" }
```

Profiles can **adapt** global subagents by overriding specific fields:

```toml
[subagents.research]
engine = "openrouter_fast"
allowed_tools = ["read_file", "search"]      # override: removed "list_dir"
timeout_secs = 60                            # override from global 120
```

Profiles can **disable** a global subagent:

```toml
[subagents.research]
enabled = false
```

If a global subagent is not mentioned in a profile, it is available with its
global defaults.

Profiles can **add** local-only subagents by placing files in the profile's own
`subagents/` directory:

```text
profiles/work/
├── config.toml
└── subagents/
    └── compliance_audit.toml   # only visible to 'work' profile
```

A local subagent with the same name as a global one **shadows** the global
definition for that profile only.

##### Safety Inheritance

Safety settings (e.g., whether `bash` requires approval) are **inherited** from
the parent agent. In headless mode, `--auto-approve-shell` applies to
sub-agents as well.

#### Sub-Agent Lifecycle

1. **Briefing:** The `SubagentManager` constructs a new `Context` with a
   **hybrid** prompt: a system message containing the type's persona,
   constraints, available tools, and output format; and a user message
   containing the curated `briefing`.
2. **Spawn:** A reusable `AgentLoop` (extracted in Phase 3) is instantiated.
   It receives:
   - A filtered `ToolRegistry` (whitelist from subagent config)
   - The configured `Provider` (from the engine assigned in the profile)
   - A new `CancellationToken`
   - Its own `subagent_ui_tx` for live sidebar updates
   - Its own `agent_tx` for sending the final result back to the parent
     The sub-agent runs its own ReAct loop in a `tokio::spawn` task.
3. **Parallel execution:** The main agent loop does NOT wait. It continues to
   process `Prompt` (steering) and `Stop` commands from the user. The sub-agent
   and main agent run concurrently. If the main LLM produces new tokens while
   the sub-agent is working, both appear in the TUI (main chat + sidebar panel).
4. **Finish:** The sub-agent must call a special `finish` tool to return its
   result. The `finish` schema is:

   ```rust
   #[derive(JsonSchema, Deserialize)]
   struct FinishArgs {
       status: String, // "success" | "error"
       result: String,
       #[serde(default)]
       error: Option<String>,
       #[serde(default)]
       sources: Vec<String>,
   }
   ```

   The `finish` call is **exempt** from the iteration cap; it is the natural
   termination state. If the sub-agent never calls `finish` (timeout, max
   iterations, or user abort), the system extracts the last assistant message as
   raw text, wraps it in a synthetic `{"status": "unstructured", "result": "..."}`
   object, and returns that to the parent.
5. **Re-integration:** The `result` field of the structured output is injected
   into the main conversation as a `Message::System` (or a dedicated
   `Message::SubagentResult`) so the main LLM can use it on its next turn.

#### Steering While a Sub-Agent Runs

The input box is never disabled. If the user sends a new message while a
sub-agent is running, the message is **queued as a cooperative steer** — it is
never injected into a live main stream. It is appended to the main conversation
at the next natural point (a completed tool-call boundary / hand-off), and the
main LLM starts a fresh request with the updated context then.

The sub-agent continues to completion in the background. Its live events flow
through `subagent_ui_rx` to the sidebar panel. Its final result is appended to
the main history when it arrives via `agent_rx`.

**The main LLM then sees both the user's steering message and the sub-agent
result on its next turn**, and decides how to reconcile them. This is emergent
behavior — no special merge logic is required.

#### Persistence

Every sub-agent run is recorded in SQLite via a dedicated `subagent_runs` table:

```sql
CREATE TABLE subagent_runs (
    id INTEGER PRIMARY KEY,
    parent_conversation_id INTEGER REFERENCES conversations(id),
    parent_message_index INTEGER,
    subagent_type TEXT,
    task TEXT,
    result_json TEXT,
    status TEXT, -- success, timeout, max_iterations, error, aborted_by_user
    created_at TIMESTAMP,
    completed_at TIMESTAMP
);
```

The full sub-agent conversation is stored under a distinct `conversation_id`
with a parent linkage. The user can later inspect sub-agent reasoning via a
collapsible sidebar panel or by querying the database.

#### UI & Headless

- **TUI:** A dedicated collapsible sidebar panel shows **live** sub-agent
  activity: streaming tokens, tool calls, and status. It receives events via
  `subagent_ui_rx`, not by polling SQLite. The status bar shows
  `🔍 Sub-agent 'research' running... (14s / 120s)`. When finished, the panel
  shows the result summary and a link to view the full thread (queried from
  SQLite for audit).
- **Headless:** Sub-agents are **disabled by default**. Enable with
  `--allow-subagents`. When enabled, stderr prints `Spawned sub-agent
'research'...` and completion status. The `--auto-approve-*` flags inherited
  by the parent also apply to sub-agents.

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

### Architecture

Headless mode is **stateless by default**: each `ylva --prompt "..."` starts a
new conversation thread in SQLite, runs the ReAct loop, and exits.
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

# Use a specific profile (loads ~/.ylva/profiles/work/config.toml)
ylva --prompt "audit security" --profile work

# Resume existing conversation
ylva --prompt "what about error handling?" --conversation-id 42

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
Executing tool: read_file("src/main.rs") → 87 lines
Executing tool: write_file("src/lib.rs") → pending approval
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

Read-only tools (`read_file`, `list_dir`, `search`) execute automatically. Mutating tools require
explicit flags:

- `--auto-approve-write` → `write_file`, `edit_file`
- `--auto-approve-shell` → `bash`

The `clarify` tool is **not registered** in headless mode. The model never sees it;
if a decision is needed, the model must make a best-effort guess or error out.

If a mutating tool is invoked without the corresponding flag, the agent emits
exit code `4` and a descriptive error to stderr.

**Model selection:** Use `--profile <name>` to load a predefined engine configuration. Override explicitly
with `--provider` + `--model` flags (e.g., `--provider openrouter --model anthropic/claude-3.5-haiku`)
for one-off scripts. No hidden defaults.

**Commands:** Slash commands (`/model`, `/tools`, `/clear`, `/save`) are only recognised at the start
of a line to avoid triggering inside pasted code.

**Conversation titles:** Generated from `ID + timestamp` so headless runs are easy to find later in
the SQLite history.

**Persistence:** Always persists to SQLite; there is no `--no-persist` flag. Every run leaves a trace
for later review.

**Buffering strategy:** Buffer everything and emit at the end. For `--format text`, the user sees
status on stderr in real-time ("Streaming response...") but the actual LLM text is buffered and flushed
to stdout on completion. For `--format json`, the entire JSON object is emitted at the end.
This guarantees well-formed output for downstream tools and avoids partial output on interrupt.

## 7. Phased Build Plan

### Phase 0: Proof-of-Concept Spikes

See §0 for the two pre-build spikes (TUI responsiveness and HTTP stream cancellation).
These are standalone experiments, not part of the production code path.

### Phase 1: Minimal Headless Build (The Agent Kernel)

**Decision:** Build headless first. The TUI is deferred to Phase 4 and added as a rendering layer on
top of a proven agent loop.

- [ ] **Goal:** A working headless agent loop in ~300 lines that proves the core architecture:
      mock provider, dual channels, steering, cancellation, and context ownership.

- [ ] **No CLI surface yet.** `main.rs` hardcodes a prompt and a steering trigger. No `clap`,
      no config parsing, no TOML.

- [ ] **Mock provider** — a tokio task that emits artificial `StreamEvent::Text` tokens with
      `sleep` delays and respects `CancellationToken`. No `reqwest`, no SSE parsing, no API keys needed.

- [ ] **Minimal `Context`** — `Vec<Message>` enum (`System`, `User`, `Assistant`) with `push_user()`
      and `push_assistant_interrupted(partial)`. ~25 lines.

- [ ] **Three spawned tasks** (proving channel topology):
  1. `main()` coordinator: owns `Context`, `select!` on `agent_rx` + `cmd_rx`.
  2. Agent loop task: executes `provider.chat()`, sends `UiUpdate` to UI task and `AgentEvent` back to coordinator.
  3. UI task: `while let Some(update) = ui_rx.recv().await { println!(...) }`.

- [ ] **Dual channels from day one:**
  - `ui_tx` → `UiUpdate` (high-frequency per-token updates to UI task)
  - `agent_tx` → `AgentEvent` (lifecycle events: `StreamDone`, `StreamCancelled`, `ToolCall`)
  - `cmd_rx` ← `AgentCommand` (`Prompt`, `Stop` from main thread)

- [ ] **Single-writer rule enforced immediately:**
  - Inner streaming task receives `context.messages().clone()` (read-only snapshot).
  - It accumulates `partial` assistant response locally.
  - It returns the final/cancelled `partial` via `AgentEvent` to the outer loop.
  - Outer loop is the **sole writer** to `Context`.

- [ ] **Steering from first compile:** `main()` sends `AgentCommand::Prompt` mid-stream after a
      hardcoded `tokio::time::sleep`. Prove the cooperative handoff: the stream is **not** cancelled,
      the steer is queued, and at the next natural point (`StreamDone`) it is injected — flushed
      partial (now complete) → append steer → start fresh stream.

- [ ] **Interrupt test:** `main()` sends `AgentCommand::Stop`; verify `StreamCancelled` arrives,
      partial is flushed, pending steer queue is cleared, state returns to `Idle`.

- [ ] **Steer-supersedes-pause test:** send a `Prompt` while paused at a `ToolPending`/`clarify`;
      verify pending tools are descoped and the steer is injected as the next turn.

- [ ] Print final conversation history on exit to verify state transitions.

**Success criteria:**

- Steering never aborts a live stream and does not panic or deadlock.
- Final context (no interrupt) contains exactly: system message, first assistant (completed to its
  natural end), the queued steering user message(s).
- Interrupt path: system message, first partial assistant (interrupted), no leftover pending steers.
- No `Arc<Mutex<Context>>`, no shared mutable state.

**Key crates:** `tokio`, `tokio-util`, `futures`, `thiserror`, `anyhow`

### Phase 2: Real HTTP Provider (Still Headless)

- [ ] **Goal:** Replace the mock provider with a real `reqwest` + `eventsource-stream` implementation
      targeting the OpenAI chat-completions spec.

- [ ] Define unified `Provider` trait with `CancellationToken` parameter and `StreamEvent::Cancelled`

- [ ] Add `ToolCallAccumulator` inside the provider for fragmented `delta.tool_calls` chunks

- [ ] Implement one provider using raw `reqwest` + `eventsource-stream`

- [ ] Verify provider works for both OpenRouter (`base_url` override) and Ollama (`/v1` endpoint)
      via config switch

- [ ] Spawn chat in a `tokio::task`; send `StreamEvent::Text` chunks over `mpsc`

- [ ] **Cancellation verified with real TCP:** Drop the stream future and confirm via `netstat`/`lsof`
      that the connection closes immediately (no `CLOSE_WAIT` linger)

- [ ] Handle `StreamEvent::Cancelled` in the headless UI printer (mark message as interrupted in stdout)

- [ ] Handle `StreamEvent::Error` gracefully (print error to stderr, return to `Idle`)

- [ ] No TUI yet. Still stdout/stderr only.

**Key crates:** `tokio`, `tokio-util`, `reqwest`, `eventsource-stream`, `futures`, `serde`

### Phase 3: The Agent Loop with Tools (Still Headless)

- [ ] **Goal:** First working ReAct agent with file system tools and human approval.

- [ ] Add `schemars` and `ToolRegistry`

- [ ] Build 3 core tools with typed arguments:
  - `read_file`
  - `write_file`
  - `list_dir`

- [ ] Implement ReAct loop inside the `select!` block with explicit `AgentMode` state machine
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

- [ ] Show tool execution status in the headless UI (print to stderr)

- [ ] Add `ToolError` variant to `UiUpdate` for failed tool invocations

- [ ] Extract `AgentLoop` as a reusable struct (provider + registry + context)
      so Phase 5 sub-agents can instantiate it with a restricted tool whitelist

- [ ] Prevent infinite loops: max iterations = 15 (configurable per engine)

- [ ] No TUI yet. Approval flow is stdin-based or hardcoded for now.

**Key crates:** `schemars`, `serde_json`, `walkdir`, `tempfile`

### Phase 4: TUI Skeleton (Rendering Layer Added)

**Decision:** The TUI is added only after the agent loop is proven headless. The TUI is a rendering
layer, not a core architectural change.

- [ ] **Goal:** A responsive terminal interface where the input box is **never** disabled.

- [ ] Set up `ratatui` + `crossterm` event loop via `spawn_blocking` + `select!`

- [ ] Three UI areas: chat history (scrollable), sidebar (optional), input box

- [ ] Integrate `tui-textarea` for bottom input with history

- [ ] Echo user input back into the chat view to test scrolling/wrapping

- [ ] Status bar showing current mode / connection state

- [ ] Install `ctrlc` handler that sends `AgentCommand::Stop` via channel

- [ ] **Re-use the same `AgentLoop` struct** from Phase 3. The TUI is just another consumer of `UiUpdate`.

- [ ] **Steering in TUI:** `Enter` sends unconditionally, even while streaming — queued as a
      pending steer when busy. `Ctrl+C`/`Esc` sends `Stop` (interrupt; clears the steer queue).
- [ ] **Pending-steer rendering:** user bubbles for queued steers show a pending style until a
      `SteerApplied` `UiUpdate` converts them to normal; a `SteerApplied` clears the "steer queued"
      indicator.

**CRITICAL:** Ensure `Enter` and `Ctrl+C` are handled **regardless** of `is_streaming` flag.
The input widget must never be "disabled."

**Key crates:** `ratatui`, `crossterm`, `tui-textarea`, `textwrap`, `ctrlc`

### Phase 5: Terminal Superpowers

- [ ] **Goal:** Make it a true coding agent.

- [ ] `bash` tool wrapping `tokio::process::Command` with `tokio::time::timeout` (default 30s)

- [ ] Launch `bash` with `process_group(0)` and kill the process **group** on timeout/`Stop`
      (prevents orphaned grandchildren)

- [ ] Confirmation modal for `bash` and `write_file` (human-in-the-loop)

- [ ] `search` tool wrapping `ripgrep` via `std::process` with lazy binary check and graceful fallback

- [ ] `clarify` tool for interactive open-ended questions; render question banner in TUI, user answers
      via input box; not available in headless mode

- [ ] File tree sidebar using `ignore` + `walkdir`

- [ ] **Diff view:** When `write_file` is proposed, render a side-by-side or unified diff in chat
      before approval

- [ ] Add `edit_file` tool (line-based or search-and-replace) to reduce full-file rewrite tokens

- [ ] `spawn_subagent` tool in `ToolRegistry` with config-driven types and tool whitelists

- [ ] `SubagentManager` that owns sub-agent lifecycle (spawn, timeout, cancel via `CancellationToken`)

- [ ] `finish` tool available only inside sub-agent runs; fallback to unstructured text if never called

- [ ] `subagent_runs` SQLite table with parent conversation linkage

- [ ] Sub-agent inherits parent safety settings (`--auto-approve-*` in headless)

- [ ] TUI sidebar panel for active/past sub-agent runs (collapsible, shows runtime & status)

- [ ] Headless: `--allow-subagents` flag (disabled by default) + stderr status lines

**Key crates:** `which`, `ignore`, `tempfile`

### Phase 6: Polish & Persistence

- [ ] **Goal:** Usable daily driver.

- [ ] `syntect` highlighting for all Markdown code fences; render raw text while streaming,
      parse markdown only after `Done`

- [ ] Three built-in themes: dark, light, monokai

- [ ] SQLite persistence for conversations (`rusqlite`)

- [ ] Resume previous conversation on startup

- [ ] Config hierarchy (`~/.ylva/profiles/default/config.toml`) for
      default model, theme, keybindings, and subagent engine assignments

- [ ] Tiered context management: System (never drop), InitialContext (never drop initially),
      Recent (drop oldest first), Compacted (summarization placeholder)

- [ ] Token counting with cached counts per `TieredMessage` (re-count on insertion, not every request)

- [ ] Context window compaction: drop oldest `Recent`/`Compacted` messages when total > context_size × 0.9

- [ ] Phase 6 extension: replace "drop" with on-the-fly summarization via a cheap local model

- [ ] `/commands` in input box (e.g., `/model`, `/tools`, `/clear`, `/save`)

**Key crates:** `rusqlite`, `config`, `toml`, `chrono`, `tiktoken-rs`

### Phase 7: CLI Surface

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

- [ ] Print minimal status to stderr by default; `--quiet` to suppress

- [ ] Always persist to SQLite (no `--no-persist` flag)

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
rusqlite = { version = "0.32", features = ["bundled"] }
chrono = "0.4"
walkdir = "2"
ignore = "0.4"
which = "7"
tempfile = "3"
async-trait = "0.1"
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

- [ ] **Can type and send new messages while the model is still streaming** (queued and injected at the next natural point)

- [ ] Interrupted partial messages are preserved in conversation history; queued steers are shown as pending until injected

- [ ] Reads and proposes file edits with diff preview

- [ ] Executes shell commands only after explicit user confirmation

- [ ] **Can ask the user clarifying questions via the `clarify` tool when uncertain**

- [ ] **Can delegate focused tasks to sub-agents with curated briefing and config-driven types**

- [ ] **Sub-agent runs are persisted and auditable via the `subagent_runs` SQLite table**

- [ ] Remembers conversation history across restarts

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
| **Token counting**             | `tiktoken-rs` for OpenAI; byte heuristic for Ollama; cached per `TieredMessage`. Counts are **approximate/advisory** (encoding is model-family dependent); rely on `context_size × 0.9` margin | §4C Context State Machine                    |
| **Context tiers**              | `System` (never drop), `InitialContext` (never drop), `Recent` (drop oldest), `Compacted` (summarize in Phase 6)                                                                               | §4C Context State Machine                    |
| **Ollama tools**               | Unified OpenAI-compatible provider using raw `reqwest` to `/v1/chat/completions`; no separate `ollama-rs` crate                                                                                | §2 Crate Ecosystem, §5 OpenRouter vs Ollama  |
| **OS signals**                 | `ctrlc` handler mapped to `AgentCommand::Stop`                                                                                                                                                 | §1 Vision, §2 Crate Ecosystem                |
| **Tool binary checks**         | Lazy check at invocation with graceful fallback                                                                                                                                                | §4D Tool Registry                            |
| **Tool timeouts**              | `tokio::time::timeout` around `Command::output` with user-visible defaults                                                                                                                     | §4D Tool Registry                            |
| **Tool errors**                | `ToolError` variant in `UiUpdate` for visible failure reporting                                                                                                                                | §4B ReAct Loop, §4D Tool Registry            |
| **Headless mode**              | Full CLI with `--prompt`, `--format`, `--conversation-id`, granular `--auto-approve-*`, exit codes `0–4`                                                                                       | §7 CLI Surface                               |
| **Output streams**             | Split stdout (content) / stderr (status); `--quiet` flag; buffer and emit at end                                                                                                               | §7 CLI Surface                               |
| **Headless safety**            | Mutating tools require `--auto-approve-*`; exit code `4` if denied                                                                                                                             | §4E Safety, §7 CLI Surface                   |
| **Conversation titles**        | ID + timestamp                                                                                                                                                                                 | §7 CLI Surface                               |
| **Model switching**            | Supported mid-conversation; `SwitchModel` is a documented placeholder, not yet wired — arrives via `/model` in the slash-command phase                                                         | §4B ReAct Loop, §7 CLI Surface               |
| **App state**                  | Dumb view model; agent loop owns state machine                                                                                                                                                 | §4B ReAct Loop                               |
| **Config format**              | TOML for all config files; YAML explicitly rejected                                                                                                                                            | §2 Crate Ecosystem, §4G Sub-Agent Delegation |
| **Engine vs Provider mapping** | `Engine` = config data (static, named); `Provider` = runtime trait object built from Engine at startup                                                                                         | §4A Provider Trait, §4G Engine Definitions   |
| **Syntect themes**             | 3 built-in: dark, light, monokai                                                                                                                                                               | §4F Rich Chat Rendering                      |
| **edit_file tool**             | Both search-and-replace and line-range modes                                                                                                                                                   | §4D Tool Registry                            |
| **Slash commands**             | Only recognised at start of line                                                                                                                                                               | §7 CLI Surface                               |
| **Headless buffering**         | Buffer everything; emit at end                                                                                                                                                                 | §7 CLI Surface                               |
| **Clarify tool**               | Free-text only; no multiple-choice options                                                                                                                                                     | §4D Tool Registry                            |
| **Sub-agent delegation**       | Tool-based `spawn_subagent` using reusable `AgentLoop`                                                                                                                                         | §4G Sub-Agent Delegation                     |
| **Sub-agent parallelism**      | Sub-agent runs in parallel with main agent; main loop never blocks                                                                                                                             | §4G Sub-Agent Lifecycle                      |
| **Sub-agent channels**         | Dedicated `subagent_ui_tx`/`subagent_ui_rx` for live sidebar updates; SQLite only for audit                                                                                                    | §4G UI & Headless                            |
| **Sub-agent types**            | Profile-driven with global behavior + local engine assignment                                                                                                                                  | §4G Sub-Agent Delegation                     |
| **Sub-agent briefing**         | Hybrid system + user message; curated by main agent                                                                                                                                            | §4G Sub-Agent Delegation                     |
| **Sub-agent nesting**          | Forbidden; one level only                                                                                                                                                                      | §4G Sub-Agent Delegation                     |
| **Sub-agent execution**        | One sub-agent at a time; runs **in parallel** with the main agent, which never blocks                                                                                                          | §4G Sub-Agent Delegation                     |
| **Sub-agent steering**         | Main steer queues and injects at the next natural point; sub-agent finishes naturally in the background                                                                    | §4G Sub-Agent Delegation                     |
| **Sub-agent result**           | Tool-based `finish` with unstructured fallback                                                                                                                                                 | §4G Sub-Agent Delegation                     |
| **Sub-agent tools**            | Global subagent files define whitelist; profiles can override                                                                                                                                  | §4G Sub-Agent Delegation                     |
| **Sub-agent config format**    | `.toml` flat or `.md` with TOML frontmatter (body = system_prompt)                                                                                                                             | §4G Sub-Agent Delegation                     |
| **Sub-agent safety**           | Inherits parent safety settings                                                                                                                                                                | §4G Sub-Agent Delegation                     |
| **Sub-agent persistence**      | `subagent_runs` table linking parent conversation                                                                                                                                              | §4G Sub-Agent Delegation                     |
| **Sub-agent UI**               | Collapsible sidebar panel + status bar line                                                                                                                                                    | §4G Sub-Agent Delegation                     |
| **Sub-agent headless**         | `--allow-subagents` flag, disabled by default                                                                                                                                                  | §7 CLI Surface                               |
| **Build order**                | Headless first (Phase 1–3), TUI added as rendering layer in Phase 4                                                                                                                            | §7 Phased Build Plan                         |
| **Minimal build**              | No CLI surface, no config parsing, no TOML in Phase 1                                                                                                                                          | §7 Phase 1                                   |
| **Provider verification**      | Mock provider before real HTTP; real `reqwest` in Phase 2                                                                                                                                      | §7 Phase 1, §7 Phase 2                       |
| **Channel topology**           | Dual channels (`ui_tx` + `agent_tx`) proven from first compile                                                                                                                                 | §7 Phase 1                                   |
| **Steering**                   | Cooperative-inject at natural points included in early headless build to prove state-machine injection transitions                                                                             | §7 Phase 1                                   |
| **Context ownership**          | Single-writer rule enforced immediately; read-only clone passed to inner task                                                                                                                  | §7 Phase 1, §4C Context                      |
