# Make provider request handling resilient to HTTP 429 bursts (OpenRouter)

## Context

Ylva is a Rust TUI agent harness. Chat requests to the OpenRouter provider
(`https://openrouter.ai/api/v1/chat/completions`) fail with:

```text
chat completion failed with HTTP 429: {"error":{"message":"OpenRouter could not
verify available credits for this request in time. Retry shortly.","code":429,
"metadata":{"limit_source":"openrouter_admission_control","remedy_hint":"Slow
down the request rate for this account, then retry after the reset (see
X-RateLimit-Reset).","headers":{"Retry-After":"10"},"provider_name":null}}}
... after 3 retries
```

Diagnosis from a debug log (`tmp/ylva_1792834253.log`, session with the main
agent plus 4 concurrent subagents):

- The OpenRouter app-attribution headers are **not** the cause (they only
  identify the app).
- The account has enough credits; the 429 is a transient admission-control
  timeout (OpenRouter could not verify credits in time), triggered by bursts of
  parallel requests (main agent, up to 4 subagents, title generation).
- The real `Retry-After: 10` HTTP header **is** honored today (retries are about
  11-12 s apart). OpenRouter's docs only document the HTTP header, not a retry
  delay in the body, so body parsing is not needed.
- All parallel requests receive the same 429 with the same `Retry-After`, sleep
  the same 10 s and re-fire together, hit admission control again, and exhaust
  `MAX_RETRIES = 3` in lockstep (about 36 s), then surface the error.

Goal: parallel agents on one provider connection must survive a transient
429 burst without surfacing an error, by spreading retries apart and pausing
the whole connection after a 429 instead of retrying in lockstep.

Acceptance criteria:

- After one request receives a 429/503 carrying `Retry-After`, other requests of
  the same provider connection do not start new attempts before that delay has
  elapsed (shared cooldown), and resumed attempts are spread by jitter.
- A 429 with `Retry-After` is retried for a longer total budget than the current
  3 attempts so a 10 s admission-control hiccup does not fail the turn.
- Cancellation stays prompt during cooldown/backoff waits.
- Behavior for providers/status codes that do not send `Retry-After` is
  unchanged except where explicitly stated in the plan.
- Docs and tests updated; `make format lint` and `make test-coverage` pass.

Constraints:

- Keep the existing per-connection concurrency semaphore semantics (permit held
  across retries, released when the stream closes).
- Keep `main.rs` thin; shared transport logic stays in `src/http.rs` (binary +
  library split; items tests need must be re-exported via `src/lib.rs`).
- Full descriptive names, no abbreviations; license headers on new files; no
  `mod.rs` files.

## Implementation Plan

An implementing agent must flip `[ ]` to `[x]` immediately after completing
each step and rewrite this file in place, so progress is resumable across
sessions.

1. [ ] Reproduce and baseline: confirm in `src/http/tests/requests.rs` (pattern
   of `get_json_retries_rate_limit_and_honors_zero_retry_after`) how a local
   test server returns `429` with `Retry-After`; write a failing test for
   `send_with_retry` / `post_with_retry_and_limiter` proving that two concurrent
   callers on one limiter both retry at the same instant after a shared
   `Retry-After`.
2. [ ] Add a per-connection cooldown gate next to `RequestConcurrencyLimiter` in
   `src/http.rs` (for example a shared `Arc<Mutex<Option<Instant>>>` "not before"
   timestamp stored in the limiter, so it is shared exactly like the
   semaphore): `note_retry_after(delay)` extends the gate to the maximum of the
   current and new deadline; `wait_until_open(cancel)` sleeps (cancel-aware via
   `sleep_cancel_aware`) until the deadline.
3. [ ] Thread the limiter (or a cooldown handle) into `send_with_retry` so that
   (a) before every attempt it awaits `wait_until_open`, and (b) a retryable
   status with a parsed `Retry-After` calls `note_retry_after`. Keep the
   no-limiter call sites (`post_with_retry`, JSON GET/POST helpers) working by
   passing an inert/private gate.
4. [ ] Add jitter on top of `Retry-After` in `retry_delay_with_jitter`
   (`src/http.rs`): `retry_after + jitter * RETRY_AFTER_JITTER_SECONDS` with
   `RETRY_AFTER_JITTER_SECONDS = 3` (uniform `[0, 3s)`), still capped by
   `RETRY_MAX_SECONDS`; keep the pure-core injected-jitter test pattern and
   update `retry_delay_honors_retry_after` accordingly.
5. [ ] Give `429`/`503` responses that carry `Retry-After` a larger retry budget
   (`MAX_RETRIES_WITH_RETRY_AFTER = 6`, total waited time capped at about 90 s)
   while leaving all other retryable failures at `MAX_RETRIES = 3`. The final
   error message must state the retry count and total waited time (for example
   "after 6 retries over 74s"). Update the `retryable status; retrying` warning
   to log `delay_ms` and `retry_after_seconds`.
6. [ ] Keep `MAX_CONCURRENT_PROVIDER_REQUESTS = 4` and `provider_request_limiter`
   in `src/llm.rs` unchanged and not configurable; the gate applies to all
   providers using the shared limiter. No change to
   `src/llm/openrouter_provider.rs`.
7. [ ] Tests (per `docs/development-workflow.md`, TDD): unit tests for the gate
   (extends to the max deadline, cancel-aware), pure-core jitter bounds,
   integration-style test in `src/http/tests/requests.rs` with a local server
   returning `429` + `Retry-After` for N concurrent callers and then `200`,
   asserting no attempt lands before the cooldown deadline and all callers
   succeed. Read `docs/known-test-issues.md` before writing tests involving
   tracing or timing.
8. [ ] Documentation: update `docs/features.md` (outbound request retry and
   concurrency bullet near line 483: shared cooldown, jitter, larger
   `Retry-After` budget), `docs/implementation-notes.md` (concurrency/retry
   bullet near line 133), and `docs/provider-notes.md` (OpenRouter section:
   admission-control 429 behavior).
9. [ ] Align subagent-concurrency documentation: the provider cap of 4
   concurrent requests per connection is held for the whole streamed response,
   so it implicitly limits how many agents (root plus subagents, title
   generation, compaction) can stream or think at once, independent of
   `max_concurrent_subagents`. State this in `docs/configuration.md`
   (`max_concurrent_subagents` row near line 302 and the `can_spawn` row near
   line 691) and in `docs/features.md` (subagent sections near lines 816-870
   and 1066), including that excess agents wait for a free permit. Run `rumdl
   fmt` on all changed markdown files.
10. [ ] Verify: `make format lint`, then `make test-coverage`; remove this plan
   file once the implementation is committed (repo convention: "Remove
   implemented plan").

## Implementation Hints

- Retry loop: `send_with_retry` in `src/http.rs` (about lines 445-540).
  `MAX_RETRIES = 3`, `RETRY_BASE_SECONDS = 1`, `RETRY_MAX_SECONDS = 30`.
  `retry_delay` returns `Retry-After` verbatim (capped at 30 s) when present,
  otherwise full jitter in `[0, base * 2^(attempt-1)]`. `parse_retry_after`
  handles delta-seconds and HTTP-dates.
- `post_with_retry_and_limiter` acquires the semaphore permit, then calls
  `post_with_retry`; the permit is returned to the caller and moved into the
  streaming task in `src/llm/openai_compatible_provider.rs` (about lines
  817-870). Retries therefore sleep while holding the permit.
- `RequestConcurrencyLimiter` (`src/http.rs`, about line 97) is a cloneable
  `Arc<Semaphore>`; `provider_request_limiter(provider_connection)` in
  `src/llm.rs` returns one shared instance per connection name from a
  process-global map; `MAX_CONCURRENT_PROVIDER_REQUESTS = 4`, hard-coded and
  not configurable. Subagent concurrency is separate
  (`max_concurrent_subagents`, default 4, `src/agent/subagent_manager.rs`).
- OpenRouter docs (`/docs/api_reference/errors.md`, `/limits.md`) document only
  the HTTP `Retry-After` header on `429`/`503`; `X-RateLimit-*` headers describe
  platform limits; `metadata.limit_source` is the field to branch on, not
  `remedy_hint`. The body's `metadata.headers.Retry-After` is undocumented and
  mirrors the real header.
- The retry warning log line (`retryable status; retrying`) does not currently
  log the chosen delay or whether `Retry-After` was present.
- Decisions: applies to all providers; cap stays 4 and non-configurable; the
  gate triggers only on `Retry-After` (429 without it keeps per-request
  exponential backoff); a request keeps its permit while sleeping or waiting
  on the gate; waits are logged only (no TUI notice).
- The semaphore permit is released only when the streaming task spawned in
  `openai_compatible_provider.rs` ends (`[DONE]`, error or cancel), so the cap
  bounds concurrently open chat streams per connection, not just request
  starts. Model-list calls hold a permit until the body is read.
- Existing doc statements to keep consistent: `docs/features.md` (retry and
  concurrency bullet), `docs/implementation-notes.md` ("One semaphore per
  provider connection ... Retries ... stay under the same permit"),
  `docs/configuration.md` (`max_concurrent_subagents` row says provider request
  concurrency is separately limited per connection).
- No ADR in `docs/architecture-design-records/` covers HTTP retry or
  concurrency; none is required unless a new setting changes the contract.
- Debug artifacts: `tmp/ylva_1792834253.log` and `*_response.jsonl` files
  recording the 429s (`[transport] response error: ... after 3 retries`).
- First file to open: `src/http.rs`; first test file: `src/http/tests/requests.rs`.
