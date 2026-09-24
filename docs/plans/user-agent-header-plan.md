# Add dedicated Ylva User-Agent header to outbound HTTP requests

## Context

Ylva makes outbound HTTP requests exclusively through one shared `reqwest::Client` held by
`OpenAiCompatibleProvider` (all providers — OpenRouter, Ollama — go through it). Today the client
is created with `reqwest::Client::new()`, which sends **no** `User-Agent` header at all (reqwest
only adds one via `ClientBuilder::user_agent`; Ylva never calls it).

Goal: every outbound HTTP request from Ylva carries a dedicated, identifying `User-Agent` string
so remote servers can recognize the client (per MDN guidance on library/net-tool UA strings:
identify product and version, avoid impersonating browsers).

Acceptance criteria:

- All outbound requests (model listing, chat/SSE, Ollama `api/tags` + `api/show`) send
  `User-Agent: ylva/<version>` (e.g. `ylva/0.10.7`, version from `CARGO_PKG_VERSION`).
- The UA is set in exactly one place — the shared client construction — so no request path can
  omit it.
- An integration test asserts the UA at the wire level against the mock server.
- Docs affected by the behavior change are updated in the same change set
  (per `AGENTS.md`: docs are part of the change set).
- `make format lint` and `make test-coverage` pass.

Key constraints:

- Single client construction site: `src/llm/openai_compatible_provider.rs` (`Client::new()` at
  line 260); three send helpers in `src/llm/http.rs` (`get_json`, `post_json`,
  `post_with_retry`) are the only request paths.
- Client-level default headers do **not** appear in `YLVA_REQUEST_DUMP_PATH` dumps: `dump_request`
  dumps the per-request `HeaderMap` built by the caller (only `AUTHORIZATION` today), not the
  wire-level merged headers.
- Version SSOT: `crate::build_info::BUILD_INFO.version` (from `CARGO_PKG_VERSION`), already
  re-exported via `src/lib.rs`.
- No mock-server crates exist; tests use the hand-rolled `tests/common/mock_llm_server.rs`, which
  reads the full request head but currently discards headers.
- No config knob for HTTP client behavior exists; no ADR covers HTTP/networking.

The implementing agent must flip `[ ]` to `[x]` on each step immediately after completing it and
rewrite this file in place, so progress is resumable across sessions.

## Implementation Plan

1. [ ] Set the User-Agent on the shared HTTP client in
   `src/llm/openai_compatible_provider.rs` (construction at line 260): replace
   `reqwest::Client::new()` with `reqwest::Client::builder().user_agent(<ua-string>).build()`,
   handling the `Result` consistently with existing panic-on-construction style (the current
   `Client::new()` panics internally on failure anyway). Compose the string once as a named
   constant (full descriptive name per `AGENTS.md`) with the settled format `ylva/<version>` —
   e.g. `concat!("ylva/", env!("CARGO_PKG_VERSION"))` or via
   `crate::build_info::BUILD_INFO.version`. Set client-level only; do not also insert into the
   per-request `HeaderMap` (decision: dumps stay unchanged).

2. [ ] Extend `tests/common/mock_llm_server.rs` to capture request headers: `read_request_head`
   (line 513) already reads the full head; parse the header lines and record them alongside the
   existing request-body capture, exposing them via an accessor following the
   `request_bodies()` pattern (line 180).

3. [ ] Add one integration test in `tests/http_provider.rs` asserting that an outbound request
   (e.g. `list_models`) carries `User-Agent: ylva/<version>` at the wire level, using the
   header-capturing mock server from step 2 and comparing against the same expected string the
   source uses (derive from `ylva::BUILD_INFO.version` or `env!("CARGO_PKG_VERSION")` — note
   `cfg(test)` does not reach library code from integration tests, see
   `docs/known-test-issues.md`). No unit-level test and no dump-behavior test required.

4. [ ] Update documentation in the same change set:
   - `docs/features.md` — add a bullet under the "Model Provider" section (lines 144–176)
     stating outbound requests send a dedicated `ylva/<version>` User-Agent.
   - `docs/implementation-notes.md` — note in the "Engine/provider runtime configuration"
     section (lines 55–110) that the UA is set once on the client at its construction site and
     flows through the three `llm::http` send helpers; record the caveat that client-default
     headers do not appear in `YLVA_REQUEST_DUMP_PATH` dumps (dump contents stay unchanged —
     `docs/cli-options.md` needs no change). Caveat goes here only (decision: do not touch
     `docs/cli-options.md` or `docs/ideas/user-agent-header.md`).

5. [ ] Run verification: `make format lint`, then the single mandatory gate
   `make test-coverage` (do not also run `make test`). Fix any failures.

## Implementation Hints

- Settled decisions: UA format is exactly `ylva/<version>` (no git commit, no contact URL);
  client-level only — `YLVA_REQUEST_DUMP_PATH` dump contents stay unchanged; no env/config
  override of the UA (do not add `YLVA_USER_AGENT` or any config key); wire-level assertion via
  one integration test with the extended mock server.
- Only client construction site in shipped code:
  `src/llm/openai_compatible_provider.rs:260` — `client: reqwest::Client::new()` inside
  `new_with_request_body_customizer_and_format`. `OpenRouterProvider` and `OllamaProvider`
  wrap `OpenAiCompatibleProvider`; the accessor `http_client()` (lines 281–283) hands the same
  client to the Ollama wrapper.
- Request send helpers (all outbound traffic): `src/llm/http.rs` — `get_json` (28–42),
  `post_json` (45–60), `post_with_retry` (69–154). Call sites:
  `openai_compatible_provider.rs` (`list_models` 320–330, `chat` 426–433),
  `ollama_provider.rs` (84–114). `mock_provider.rs` is in-process, no HTTP.
- reqwest 0.12.28 (resolved): `ClientBuilder::new()` inserts only `ACCEPT: */*`;
  `USER_AGENT` is inserted exclusively by `.user_agent()`. Confirmed Ylva never calls it —
  current requests carry no UA at all.
- Version SSOT: `src/build_info.rs:52` — `version: env!("CARGO_PKG_VERSION")` inside
  `pub const BUILD_INFO: BuildInfo`, re-exported at `src/lib.rs:60`. Integration tests can
  assert `ylva::BUILD_INFO.version == env!("CARGO_PKG_VERSION")` (pattern in
  `tests/build_info.rs:9`).
- `dump_request` (`src/llm/http.rs:212–259`) dumps the caller-supplied per-request `HeaderMap`
  only; a client-default UA will not appear in dumps unless also inserted into that map.
  Sensitive-header redaction list (`http.rs:159–178`) does not include `user-agent`.
- The per-request `HeaderMap` is built at `openai_compatible_provider.rs:382` — only
  `AUTHORIZATION` is inserted. `get_json`/`post_json` do not accept a `HeaderMap` (bearer-only),
  so client-level default header is the only coverage-complete placement.
- Mock server: `tests/common/mock_llm_server.rs` — binds `127.0.0.1:0` (line 209),
  `read_request_head` (513–563) already reads the full head incl. Content-Length but discards
  header values; `request_bodies()` accessor pattern at 180/270–275 is the template for a
  headers accessor. Must fully consume the request body (comment at line 509) to avoid racing
  reqwest.
- Integration tests that spawn the real binary need
  `#[tokio::test(flavor = "multi_thread")]` or the mock accept loop starves
  (`docs/development-workflow.md`). `cfg(test)` does not reach library code from `tests/*.rs`
  (`docs/known-test-issues.md:82–103`).
- No wiremock/httpmock/mockito anywhere; dev-deps are only `log`, `tempfile`,
  `tokio/test-util`. Do not introduce a mock-server crate.
- Docs to touch are named in step 4; `AGENTS.md` requires them in the same commit.
  `docs/architecture-design-records/` is empty (only `.gitkeep`) — no ADR constrains this change.
  `CONTEXT.md` defines no HTTP/transport term — no glossary change expected.
  `docs/cli-options.md` stays unchanged (dump contents unchanged by decision).
- No `benches/` directory exists.
