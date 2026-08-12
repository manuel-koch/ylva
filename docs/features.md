# Ylva agent-harness features

## General

- Rust-based terminal AI agent application, runnable as a CLI
- Implements agent-harness functionality

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
  (system/user/assistant; `ToolResult`/`ToolError` skipped, not reachable yet)
- Built-in `Message`/`ToolCallRequest` skeleton (tool execution is a later
  phase)
- SSE tolerance: `[DONE]` sentinel, comment lines and non-content deltas
  (`reasoning`/`role`-only/`tool_calls`-only chunks) skipped — never surfaced
  as user text; malformed payloads become `StreamEvent::Error`
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
  Setting `YLVA_VERBOSE=1` prints the selected provider, `base_url`, model, and
  key presence to stderr at startup for quick misconfiguration diagnosis
- Hermetic tests against an in-process mock SSE server (`tests/common/`) — no
  live OpenRouter/Ollama dependency in CI
