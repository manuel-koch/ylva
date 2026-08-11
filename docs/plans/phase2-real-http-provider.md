# Phase 2: Real HTTP Provider (Headless)

## Context

### System / component being changed

The **Ylva agent-harness** (Rust). Phase 1 is complete and committed: a headless
agent kernel (`AgentLoop` in `src/agent.rs`) driven by a **mock** provider
(`MockProvider` in `src/llm.rs`). This plan implements **Phase 2** of the
[blueprint](../blueprint.md) §7: replace the mock with a **real HTTP provider**
so the agent streams live tokens from an OpenAI chat-completions-compatible
backend (OpenRouter and Ollama) — still headless, no TUI.

### Goal and motivation

A working headless agent loop that streams real model output over HTTP. This
proves the `Provider` abstraction and the cancellation path against a live SSE
endpoint before the tools (Phase 3), TUI (Phase 4), and config/CLI (Phase 6/7)
layers are added. The unified provider serves both OpenRouter and Ollama via the
single OpenAI `/v1/chat/completions` spec — the architectural claim validated by
Spike 3 ([blueprint](../blueprint.md) §0 Spike 3, §4A, §5).

### Acceptance criteria (definition of done)

- A real `reqwest` + `eventsource-stream` provider implements the existing
  `Provider` trait unchanged (same `chat(messages, cancel)` signature, same
  `StreamEvent` enum) and streams `StreamEvent::Text` chunks over `mpsc`.
- The provider targets the OpenAI chat-completions spec and works for both
  OpenRouter (`base_url`, `api_key`) and local Ollama (`/v1`) by constructor
  configuration — Spike 3 PASS means no second `ollama_native` provider is needed.
- SSE parsing tolerates comment lines and non-content deltas (reasoning /
  `role`-only / `tool_calls`-only chunks) without emitting spurious `Text`.
  Tool-call **assembly** is deferred to Phase 3.
- Cancellation via `CancellationToken` closes the underlying TCP connection
  promptly (Spike 2 confirms the drop-the-request-future pattern; verified
  hermetically in tests via a mock server's live-connection counter).
- Pre-stream failures (`429`/`5xx`/connect) retry with bounded exponential
  backoff honoring `Retry-After`; mid-stream errors surface as
  `StreamEvent::Error` (never retried mid-stream).
- The headless stdout printer handles `StreamCancelled` (mark interrupted) and
  `StreamError` (print to stderr, return to `Idle`).
- No TUI. Still stdout/stderr only.
- `make lint`, `make test`, and `make test-coverage` (≥80% on testable files)
  pass.

### Key constraints

- **Keep `main.rs` thin** and the `Provider` trait signature stable: the existing
  `AgentLoop`/`Provider`/`StreamEvent` contract must not change in Phase 2
  (no tools, no `Delta`, no `ToolCall` — those are Phase 3).
- **`base_url` is a generic provider concept.** Its meaning — which endpoints and
  paths exist under a given base URL — is defined **solely inside the concrete
  provider implementation**, never in the `Provider` trait. The fact that Ylva
  currently ships an OpenAI-compatible provider must not leak the OpenAI path
  structure into the trait or into `base_url`'s meaning. The concrete
  `OpenAiProvider` interprets its `base_url` (e.g. `https://openrouter.ai/api/v1`
  or `http://localhost:11434/v1`) and appends `/chat/completions` internally.
  This leaves room for future
  provider-side queries (e.g. model listing) under the same `base_url`.
- **No config parsing, no TOML, no CLI** in Phase 2 (blueprint §7 Phase 1/2
  scope). The provider is configured by constructor arguments; the demo in
  `main.rs` selects the provider from environment variables (see step 5).
- **Hermetic tests.** Do not depend on a live OpenRouter or Ollama in CI.
  Use a local mock SSE server (pattern from
  `spikes/spike-http-stream-cancel/src/mock_server.rs`) that emits OpenAI-spec
  frames.
- **Context ownership (single-writer rule).** The inner streaming task receives a
  `Vec<Message>` snapshot and never touches `Context` (unchanged from Phase 1).
- **Native async trait** (Rust 1.75+), no `async-trait` crate — the trait returns
  a `BoxFuture` so `Arc<dyn Provider>` stays buildable (unchanged from Phase 1).

### Concrete starting point

Open `src/llm.rs`. It currently defines the `Provider` trait and `MockProvider`.
The new provider is a sibling implementation; `src/llm.rs` may grow or be split
into `src/llm/openai_provider.rs` (blueprint §3 layout) once it exceeds the
flat-file threshold. First edit: add the `reqwest`/`eventsource-stream`/`serde`
dependencies to `Cargo.toml` and write the OpenAI wire-serialization free
function.

## Implementation Plan

1. **Add dependencies** to `Cargo.toml`:
   `reqwest = { version = "0.12", features = ["json", "stream"] }`,
   `eventsource-stream = "0.2"`, `serde = { version = "1", features = ["derive"] }`.
   `serde_json`, `tokio`, `tokio-util`, `futures`, `anyhow`, `uuid` are already
   present. Run `cargo build` to lock the versions.

2. **Add the OpenAI wire-serialization free function** in the `llm` module
   (blueprint §4A Q19: `Message` stays decoupled from the wire format).
   `pub fn serialize_messages(messages: &[Message]) -> serde_json::Value` maps:
   - `Message::System { content }` → `{"role":"system","content":…}`
   - `Message::User { content }` → `{"role":"user","content":…}`
   - `Message::Assistant { content, .. }` → `{"role":"assistant","content":…}`
     (Phase 2 ignores `status`/`tool_calls`; tool-call serialization is Phase 3)
   - `Message::ToolResult` / `Message::ToolError` → **skipped silently** (not
     emitted). They cannot occur in Phase 2; Phase 3 adds their serialization
     when tools become reachable.
   Add unit tests for each variant and for the empty-slice case.

3. **Implement `OpenAiProvider`** in a new `src/llm/openai_provider.rs` (move the
   `Provider` trait and `MockProvider` into `src/llm/mod.rs` if splitting — keep
   the public API identical via re-exports in `src/lib.rs`). Struct:

   ```rust
   pub struct OpenAiProvider {
       base_url: String,   // e.g. "https://openrouter.ai/api/v1" or "http://localhost:11434/v1"
       model: String,
       api_key: Option<String>,
       client: reqwest::Client,
   }
   ```

   Constructor `OpenAiProvider::new(base_url, model, api_key)`. Implement
   `Provider::chat`:
   - Build the request body `{"model":…,"messages":serialize_messages(…),"stream":true}`.
   - POST to `base_url.trim_end_matches('/') + "/chat/completions"`. The
     concrete `OpenAiProvider` owns the `/chat/completions` path interpretation;
     `base_url` includes the backend origin and version prefix (e.g. Ollama
     `http://localhost:11434/v1`, OpenRouter `https://openrouter.ai/api/v1`).
     Document this convention in the constructor doc comment and test it.
   - Attach `Bearer` auth when `api_key` is `Some`.
   - **Pre-stream retry** (`429`/`5xx`/connect): bounded exponential backoff, up
     to 3 retries, base 1 s, max 30 s, with jitter, honoring `Retry-After` when
     present. Never retry once streaming begins.
   - On final pre-stream failure, return `Err` (the inner streaming task in
     `src/agent.rs` already converts it to `UiUpdate::Error` +
     `AgentEvent::StreamError`).
   - On success, wrap `response.bytes_stream().eventsource()`, create an
     `mpsc::channel`, `tokio::spawn` the receive loop (mirroring `MockProvider`),
     and return the receiver. The loop uses `tokio::select! { biased; … }` polling
     the `CancellationToken` against `stream.next()` (blueprint §4A; proven by
     Spike 2).

4. **Implement SSE parsing** in the provider's receive loop. For each
   `Ok(Event)`:
   - `[DONE]` sentinel → emit `StreamEvent::Done`, break.
   - Skip the frame entirely if it is a comment / has no `data` (tolerance for
     comment lines, per blueprint §4A note and Spike 3).
   - Parse `data` as JSON; read `choices[0].delta.content` — if it is a non-empty
     string, emit `StreamEvent::Text(content)`.
   - If the delta has no `content` (reasoning deltas, `role`-only first chunk,
     or `tool_calls`-only chunks), **skip** it — never emit `Text` for it. Phase
     2 does not emit `StreamEvent::Delta` (tool-call assembly is Phase 3).
   - `Err(e)` or a malformed/failed frame → emit `StreamEvent::Error`.
   - `cancel.cancelled()` fires → emit `StreamEvent::Cancelled`, break.
   Add comments reporting the parser-facing facts from Spike 3 (comment-line
   tolerance; non-content deltas must not surface as user-visible text). Extract
   the single-frame parse into a pure function (e.g. `parse_sse_payload(&str) ->
   Option<StreamEvent>`) so it is unit-testable without sockets.

5. **Wire the provider as a demo path in `src/main.rs`** (keep `main.rs` thin),
   selected from environment variables with **Ollama-first precedence**:
   - If `OLLAMA_BASE_URL` is set → `OpenAiProvider::new(OLLAMA_BASE_URL,
     OLLAMA_MODEL, OLLAMA_API_KEY)` (`OLLAMA_API_KEY` optional; `None` if unset).
   - Else if `OPENROUTER_BASE_URL` is set → `OpenAiProvider::new(
     OPENROUTER_BASE_URL, OPENROUTER_MODEL, OPENROUTER_API_KEY)`.
   - Else → `MockProvider` (the current demo).
   The existing headless printer already handles `UiUpdate::StreamCancelled`
   (" [interrupted]") and `UiUpdate::Error` — keep that behavior. No new CLI
   parsing.

6. **Add a hermetic mock SSE server integration-test helper** in
   `tests/common/` (alongside `mod.rs`/`collect_until`). Model it on
   `spikes/spike-http-stream-cancel/src/mock_server.rs` (it already tracks a
   live-connection counter and accepts a frame budget). Extend it to emit
   configurable OpenAI-spec SSE frames (one or more `data:` JSON chunks with
   `delta.content`, optional comment lines, optional reasoning/tool-call-only
   deltas, then `data: [DONE]`), and to respond with a configurable HTTP status
   (for retry tests).

   Also add an **opt-in manual verification target** — a separate binary (or an
   `#[ignore]`d test) that adapts the Spike 2 `verify` module's
   `lsof`/`netstat` socket checks to assert the real TCP connection closes
   promptly on cancellation. Kept out of CI because it shells out to
   `lsof`/`netstat`; run by hand (`cargo run`/`-- --ignored`) to confirm the at-
   the-socket close against a live or mock endpoint.

7. **Write the provider tests**, split per the repo convention:
   - **SSE fixtures**: commit a few small SSE frame files under `tests/fixtures/`
     — one plain `delta.content` stream, one containing comment lines, and one
     with a `reasoning`-only delta — and feed them to `parse_sse_payload` in the
     unit tests, so parser tolerance is regression-tested against realistic
     frames without a live backend (the Spike 3 captures in
     `spikes/spike-ollama-tool-calling/out/` are gitignored, so author small
     fixtures instead).
   - **In-module unit tests** in `src/llm/` for pure logic: `serialize_messages`
     mapping (step 2) and the extracted `parse_sse_payload` function — comment
     tolerance, reasoning-only delta skipped, `tool_calls`-only delta skipped,
     empty content skipped, `[DONE]` handling, malformed JSON → error.
   - **Integration tests in `tests/`** using the mock server:
     - Text streaming: multiple chunks emitted as `StreamEvent::Text` in order,
       then `Done`.
     - Cancellation: firing the `CancellationToken` mid-stream yields
       `StreamEvent::Cancelled` and the mock server's live-connection counter
       returns to 0 (reuses Spike 2's `token_cancel_releases_mock_connection`
       pattern).
     - Non-2xx response (mock returns 500 then 200) retries then succeeds; a
       persistent 500 eventually yields `StreamError`.
     - An `AgentLoop` + `OpenAiProvider` round-trip against the mock server,
       asserting the committed `Context` contains the streamed text (mirroring
       `tests/steering.rs`).

8. **Documentation step** (docs are part of the change set — update in the same
   commit):
   - `docs/features.md`: add a "Phase 2: Real HTTP provider (headless)" section
     describing the `OpenAiProvider`, unified OpenRouter/Ollama support, the
     `base_url` semantics, SSE tolerance, retry, and cancellation.

- `docs/blueprint.md`: check off the Phase 2 items in §7 that this plan
  completes (the `[ ]` boxes under "Phase 2: Real HTTP Provider (Still
  Headless)"), and update §4A's implementation note to record the two
  deviations from the literal text (tool-call assembly deferred to Phase 3;
  `base_url` semantics).
  - `docs/configuration.md`: only if a config key/format is introduced — Phase 2
    introduces none (constructor args + env vars only). Leave unchanged.
  - `docs/keybindings.md`: unaffected (headless). Leave unchanged.
  - `docs/development-workflow.md`: unaffected. Leave unchanged.

9. **Verify** with `make lint`, `make test`, and `make test-coverage` (≥80% on
   testable files; `main.rs` is excluded). Fix all findings before reporting
   done.

## Proposed Improvements

- **Add a `--format json`-style headless consumer** only when Phase 7 lands the
  CLI; do not build it in Phase 2.

## Implementation Hints

- **Current `Provider` trait** (`src/llm.rs:19`): `fn chat<'a>(&'a self, messages:
  Vec<Message>, cancel: CancellationToken) -> BoxFuture<'a, anyhow::Result<
  mpsc::Receiver<StreamEvent>>>`. Keep it unchanged. `MockProvider` (`src/llm.rs:35`)
  shows the channel + `tokio::spawn` + `tokio::select!` + `CancellationToken`
  pattern to mirror.
- **`StreamEvent` enum** (`src/agent.rs:40`): `Text(String)`, `Done`, `Cancelled`,
  `Error(anyhow::Error)`. No `Delta` variant in Phase 2.
- **`AgentLoop`** (`src/agent.rs:133`) and `start_request` (`src/agent.rs:259`)
  already convert provider `Err` / `StreamEvent::Error` / `StreamEvent::Cancelled`
  into `UiUpdate` and `AgentEvent`. No changes needed there for Phase 2.
- **Spike 2 client** (`spikes/spike-http-stream-cancel/src/client.rs`) is the
  reference implementation: `response.bytes_stream().eventsource()`,
  `tokio::select! { biased; _ = cancel.cancelled() => {…} item = stream.next() =>
  {…} }`, `data == "[DONE]"` sentinel, `Bearer` auth via `request.bearer_auth(key)`.
- **`base_url` values** (blueprint §4A / §5): OpenRouter
  `https://openrouter.ai/api/v1`, local Ollama `http://localhost:11434/v1` — both
  include the `/v1` prefix. The concrete `OpenAiProvider` appends `/chat/completions`.
  The `Provider` trait itself carries no `base_url` and no path semantics.
- **Spike 2 mock server** (`spikes/spike-http-stream-cancel/src/mock_server.rs`)
  already provides a live-connection counter and frame budget — reuse the pattern
  (not the crate; it is a standalone spike) for the Phase 2 `tests/common/` helper.
- **Spike 3 findings** (blueprint §4A note): OpenRouter emits comment lines
  (`: OPENROUTER PROCESSING`) mid-stream; reasoning models stream
  `delta.reasoning`/`delta.reasoning_details` (+ `content`, `role`) before a tool
  call. A chunk with neither `content` nor `tool_calls` must not be emitted as
  `Text`. Judge a stream by the presence of `data:` events, not its first line.
  Spike 3 PASS means the unified provider needs **no** `ollama_native` fallback.
- **`eventsource-stream` 0.2.3** provides `Eventsource` trait →
  `bytes_stream().eventsource()` yielding `Result<Event, EventSourceError>` with
  `Event { event, data, id, retry }`. Whether comment lines are surfaced as empty
  `data` events or dropped by the parser varies; the provider must skip any frame
  with no usable `data`/content regardless (tested in step 7).
- **Retry**: implement in the provider wrapping the initial `send().await` only.
  Use `reqwest::StatusCode` for `429`/`5xx` checks; read `Retry-After` from the
  response headers when present. No dedicated backoff crate is in the Phase 2
  dependency set — hand-roll the bounded exponential backoff with jitter.
- **`src/lib.rs`** re-exports (`src/lib.rs:1`) must expose any new public item
  (`OpenAiProvider`) so integration tests under `tests/` can reach it — `tests/*.rs`
  cannot `use` from `src/main.rs`.
- **Test conventions**: `tests/common/mod.rs` has `collect_until`; integration
  tests follow `tests/steering.rs` / `tests/interrupt.rs` (drive `AgentLoop`,
  `drop(agent)`, await `handle` for the committed `Context`). Real-HTTP tests
  cannot use `tokio::time::pause` — use a real runtime
  (`#[tokio::test]` without `start_paused`) against the mock server.
- **Coverage**: `make test-coverage` enforces ≥80% line coverage on testable
  files (`main.rs` excluded). The new provider and serializer are testable and
  count toward the threshold.
- **Docs are part of the change set** (AGENTS.md): a commit implementing this
  plan must bundle the `docs/features.md` and `docs/blueprint.md` updates from
  step 8 in the same commit.
- **Markdown lint**: run `rumdl` on edited markdown (config `.rumdl.toml`,
  line-length 100) and `rumdl fmt` to auto-fix.
