# Refactor generic methods into a shared HTTP/retry module

## Context

The crate currently ships a single concrete chat-completions provider,
`OpenAiProvider` (`src/llm/openai_provider.rs`). Alongside it, that file
defines six private free functions that implement generic HTTP `POST`-with-retry
behaviour:

- `send_with_retry` (`openai_provider.rs:228`)
- `should_retry` (`openai_provider.rs:301`)
- `response_body_detail` (`openai_provider.rs:309`)
- `retry_delay` (`openai_provider.rs:327`)
- `jitter_fraction` (`openai_provider.rs:340`)
- `sleep_cancel_aware` (`openai_provider.rs:354`)

plus the three backoff constants `MAX_RETRIES`, `RETRY_BASE_SECONDS`,
`RETRY_MAX_SECONDS` (`openai_provider.rs:27-31`).

None of these are OpenAI- or provider-specific: they implement cancel-aware
sleep, deterministic jitter, bounded exponential backoff honouring `Retry-After`,
HTTP status retry decision, response-body snippet extraction, and the
pre-stream `POST` + retry loop. They are all private to `openai_provider.rs`,
so a future provider cannot reuse them without copy/paste, which would
duplicate logic and drift the retry policy.

**Goal.** Extract these methods into a generic, provider-independent module so
that current and future providers share one implementation of the pre-stream
`POST`/retry mechanic and its supporting helpers. The behaviour must be
functionally unchanged.

**Motivation.** Reuse for additional provider implementations; single source of
truth for the retry policy (backoff bounds, jitter, `Retry-After` handling,
status retry decision) instead of per-provider copies.

**Scope / non-goals.**

- No change to the public `Provider` trait, `MockProvider`, `ToolSchema`, or any
  wire/streaming behaviour. Only code relocation and internal renaming.
- `parse_sse_payload` and the streaming `chat()` body remain in
  `openai_provider.rs` (SSE parsing is OpenAI-completions-specific).
- No new dependencies.

**Acceptance criteria / definition of done.**

1. Six helpers + three constants no longer live in `openai_provider.rs`; they
   live in a new generic module reachable by future providers.
2. `OpenAiProvider::chat` calls the moved `POST`-with-retry function and
   behaviour is unchanged (verified by the existing integration tests in
   `tests/http_provider.rs`, including `retry_after_500_then_succeeds` and
   `non_retryable_status_reports_error_with_backend_body`).
3. The `send_with_retry` name now reflects the `POST` mechanic and its
   `completions_url` parameter is renamed to `url`.
4. All unit tests move/arrive with the code; coverage stays ≥80% on the moved
   module (`make test-coverage` passes).
5. `make test`, `make lint`, `make format` all pass.
6. Affected docs (`docs/features.md`) are updated in the same change set.

## Implementation Plan

### 1. Create the generic module `src/llm/http.rs`

Create a new file-per-module `src/llm/http.rs` holding the generic pre-stream
HTTP helpers and backoff constants, all marked `pub(crate)` so sibling provider
modules can call them:

- Move `send_with_retry` in and rename it to `post_with_retry`, renaming its
  `completions_url` parameter to `url`. Replace the `api_key: &Option<String>`
  parameter with `headers: &reqwest::header::HeaderMap` so the auth policy stays
  with the provider, not the generic module. The signature becomes:

      (client: &reqwest::Client,
       url: &str,
       headers: &reqwest::header::HeaderMap,
       body: &serde_json::Value,
       cancel: &CancellationToken) -> anyhow::Result<reqwest::Response>

  Apply the headers (`request.headers(headers.clone())`) on each attempt. Keep
  the `429`/`5xx`/connect retry decision and the `Retry-After` handling identical.
- Move `should_retry`, `response_body_detail`, `retry_delay`, `jitter_fraction`,
  `sleep_cancel_aware`, and constants `MAX_RETRIES`, `RETRY_BASE_SECONDS`,
  `RETRY_MAX_SECONDS`.
- Preserve every doc comment, updating internal cross-references (e.g.
  `[`RETRY_MAX_SECONDS`]` in `retry_delay`) to resolve inside the new module.

### 2. Declare the module and rewire `openai_provider.rs`

- In `src/llm.rs`, add `pub(crate) mod http;` (sibling of the existing
  `pub mod openai_provider;`) so any provider can reuse it.
- In `openai_provider.rs`, import `reqwest::header::{HeaderMap, AUTHORIZATION}`.
  In `chat()`, build the auth headers once before the request

      let mut headers = HeaderMap::new();
      if let Some(key) = &self.api_key {
          headers.insert(AUTHORIZATION, format!("Bearer {key}").parse()?);
      }

  and replace the line 104 call with
  `super::http::post_with_retry(&client, &completions_url, &headers, &request_body, &cancel)`.
  Remove the now-moved functions and constants from the file. Drop `std::time` /
  `SystemTime` / `UNIX_EPOCH` imports if they become unused; keep only what
  `chat()` and `parse_sse_payload` still need.
- Confirm no remaining references to the removed identifiers in the file.

### 3. Move and extend the unit tests

- Move the `retry_delay_honors_retry_after`, `retry_delay_backs_off_bounded_by_max`,
  and `should_retry_marks_ratelimit_and_server_errors` tests from
  `openai_provider.rs`'s `tests` module into a `#[cfg(test)] mod tests` in
  `src/llm/http.rs`, using `use super::*;`.
- Add a unit test for `response_body_detail` proving a bounded (≤512 char)
  snippet is returned for a non-empty body and empty string for an unreadable/
  blank body (the truncation logic currently has no direct unit test).
- `post_with_retry` applies the (already-built) `headers` on every attempt, so
  no auth logic needs a unit test here; the bearer-path is covered end-to-end by
  `tests/http_provider.rs`.
- Keep the existing `tests/http_provider.rs` integration tests unchanged as the
  end-to-end guarantee that `post_with_retry` behaviour is identical.

### 4. Documentation step

- Update the pre-stream retry bullet in `docs/features.md`
  (`features.md:44-48`) to state that the retry/backoff helpers live in the new
  generic module (`src/llm/http.rs`), shared by providers, so the doc reflects
  the new location rather than implying the logic lives inside
  `openai_provider.rs`.
- No `architecture-design-records` entry is needed (the directory is empty and
  this change only relocates existing behaviour).

### 5. Verify

- Run `make format`, `make lint`, `make test`, and `make test-coverage`
  (threshold ≥80% on the moved module) and fix any findings before reporting
  done.

## Proposed Improvements

- Consider making `MAX_RETRIES`, `RETRY_BASE_SECONDS`, `RETRY_MAX_SECONDS`
  parameters (with defaults) of `post_with_retry` in a later change so different
  providers could tune their own retry budgets; not required now (YAGNI) — keep
  the current module-level constants.

## Implementation Hints

- Current `send_with_retry` applies auth via `.bearer_auth(key)` on the
  `reqwest::RequestBuilder`; moving to a `HeaderMap` param replaces this with
  building `AUTHORIZATION: "Bearer <key>"` in `OpenAiProvider` and
  `request.headers(...)` in `post_with_retry`. `HeaderMap` is re-used from
  `reqwest` (no new dependency).
- `reqwest::header::HeaderMap` must be entered via FQ paths or a `use
  reqwest::header::HeaderMap;` import; `reqwest` is already a dependency.
- All six helpers and three constants are currently private free functions in
  `src/llm/openai_provider.rs` (lines 27-31, 228-360); the only call site of
  `send_with_retry` is line 104.
- File-per-module layout is required (Rust 2018+, `foo.rs` not `foo/mod.rs`) and
  modules are declared in `src/llm.rs` (see `pub mod openai_provider;` at
  `llm.rs:26`).
- `tests/*.rs` cannot reach `pub(crate)` items and must use re-exports from
  `src/lib.rs`; these helpers are intentionally `pub(crate)`, so no `lib.rs`
  re-export is needed — `tests/http_provider.rs` (imports `OpenAiProvider` /
  `openai_provider_for`) already covers the retry path end-to-end without naming
  the helper functions.
- The three backoff constants use fixed values `MAX_RETRIES = 3`,
  `RETRY_BASE_SECONDS = 1`, `RETRY_MAX_SECONDS = 30`, mirrored verbatim in the
  "Pre-stream retry" bullet of `docs/features.md:44-48`.
- `response_body_detail` caps the snippet at 512 chars (`text.chars().take(512)`)
  and returns an empty string for blank/unreadable bodies.
- `retry_delay` uses `Duration::from_millis((capped as f64 * jitter_fraction() *
  1000.0) as u64)` with `jitter_fraction()` in `[0,1)` seeded from the wall
  clock (no dedicated RNG crate).
- `docs/class-diagram.mmd` only models high-level classes (`OpenAiProvider`,
  `Provider`) and does not list these helper functions, so it does not need a
  change.
