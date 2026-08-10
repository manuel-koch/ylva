# Ylva agent-harness features

## General

- Rust-based terminal UI application, runnable as a CLI too
- Implements agent-harness functionality

## Phase 1: Minimal headless build (the agent kernel)

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
- Deterministic `MockProvider` streaming tokens at a fixed interval
