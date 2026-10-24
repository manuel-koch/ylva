# OpenRouter app attribution headers

## Context

Ylva is a Rust terminal agent harness. OpenRouter lets applications
advertise themselves (app attribution, see
<https://openrouter.ai/docs/app-attribution>) by sending headers on API
requests. Goal: make Ylva show up on OpenRouter by sending these headers on
every request to the OpenRouter provider:

- `HTTP-Referer: https://github.com/manuel-koch/ylva`
- `X-OpenRouter-Title: Ylva`
- `X-OpenRouter-Categories: cli-agent,personal-agent`

The original idea required the headers to be sent only once Ylva is publicly
available on GitHub. The repository `manuel-koch/ylva` is now public (verified
with `gh repo view`), so that precondition is met.

Acceptance criteria:

- Every chat and model-listing request made by the OpenRouter provider carries
  the three headers; no other provider (Ollama, Copilot, plain
  OpenAI-compatible, mock) sends them.
- Tests prove both the presence (OpenRouter) and absence (other providers).
- Request dumps and logs still never expose headers beyond existing redaction.
- Docs describe the behavior, including the privacy/data-sharing implication.
- `make format lint` and `make test-coverage` pass.

Plan execution: an implementing agent must flip `[ ]` to `[x]` immediately
after completing each step and rewrite this file in place, so progress is
resumable across sessions. Follow `docs/development-workflow.md` → *Plan
execution*; docs updates ship in the same commit as the code.

## Implementation Plan

1. [x] Read `docs/architecture-design-records/` (ADRs 0001–0003) and confirm
   none covers provider headers; no new ADR is expected.
2. [x] Write a failing test first in `src/llm/openrouter_provider.rs` (tests
   module): a loopback server (reuse the pattern of
   `delegates_identity_runtime_settings_and_chat`) asserts the three headers
   are present on `POST /api/v1/chat/completions`. Add a similar assertion for
   the model-listing `GET /api/v1/models` request if an existing loopback
   pattern for `list_models` is available in `tests/http_provider.rs`.
3. [x] Add a builder method on `OpenAiCompatibleProvider`
   (`src/llm/openai_compatible_provider.rs`, next to
   `with_request_authorizer`) such as `with_extra_headers(self,
   Vec<(&'static str, &'static str)>) -> Self` that sets `extra_headers`
   without requiring a request authorizer.
4. [x] Define the three headers as named constants in
   `src/llm/openrouter_provider.rs` (full descriptive names, no magic strings
   inline): referer value `env!("CARGO_PKG_REPOSITORY")`, title literal
   `Ylva`, categories `cli-agent,personal-agent`. Apply them always (no
   config opt-out) in `OpenRouterProvider::new` via the new builder. Add a
   test asserting the referer equals `https://github.com/manuel-koch/ylva`.
5. [x] Add a test that a plain `OpenAiCompatibleProvider` (and Ollama) sends
   none of the headers.
6. [x] Update docs: `docs/provider-notes.md` (OpenRouter section: headers sent
   and why), `docs/features.md` and `docs/implementation-notes.md` (where the
   OpenRouter provider/headers mechanism is described), and the README
   "Provider data sharing" bullet (README.md ~line 77) to state that
   OpenRouter requests identify Ylva via attribution headers.
7. [x] Delete `docs/ideas/openrouter-app.md` (superseded by this plan and the
   docs). First check how other implemented ideas in `docs/ideas/` are
   handled and follow that convention if it differs.
8. [x] Run `rumdl fmt` and `rumdl check` on changed markdown, then
   `make format lint` and `make test-coverage`.

## Implementation Hints

- `OpenRouterProvider` is in `src/llm/openrouter_provider.rs`; `new()` builds
  `OpenAiCompatibleProvider::new_with_request_body_customizer(...)
  .with_provider_kind("openrouter").with_usage_cost_parser(...)`. Base URL is
  the origin; `/api/v1` is appended.
- `OpenAiCompatibleProvider` already has a `extra_headers:
  Vec<(&'static str, &'static str)>` field ("static headers added to every chat
  and model-listing request"). Currently it is only populated through
  `with_request_authorizer` (Copilot). Chat applies it at ~line 738 via
  `HeaderName::from_static`/`HeaderValue::from_static`; model listing passes it
  to `crate::http::get_json_with_headers` (~line 571). Header names must
  therefore be lowercase `&'static str` (`http-referer`, `x-openrouter-title`,
  `x-openrouter-categories`); HTTP header names are case-insensitive, and
  `from_static` panics on uppercase names.
- Request logging never records headers (comment at ~line 779); dumps use
  `crate::http::redacted_headers_json`. The attribution headers are not
  secrets.
- `clone_with_runtime_config` is used by `with_runtime_config`; verify it
  preserves `extra_headers` (it should clone the whole struct) in a test.
- `OpenRouterProvider::new` is also constructed in `src/llm/provider_registry.rs`
  and `src/connect_command/providers.rs` (connect flow); tests in
  `tests/http_provider.rs` and `src/llm/engine/tests/router.rs` use it with
  loopback servers, so the headers will appear there too, harmless.
- The `ylva connect openrouter` flow may call the OpenRouter API (key
  validation / model listing); check `src/connect_command/providers.rs`
  whether it goes through `OpenRouterProvider` or a separate HTTP call, and
  whether headers should apply there too.
- Crate version is 0.31.1; repository URL in `Cargo.toml` is
  `https://github.com/manuel-koch/ylva` (the referer value). Releases are
  automated by release-plz with Conventional Commits; use `feat(llm): ...`.
- Source files need the Apache-2.0 license header (already present in
  touched files).
- Provider data sharing is documented in README.md ~line 77.
- Decisions: attribution is always on with no opt-out setting (YAGNI);
  referer comes from `env!("CARGO_PKG_REPOSITORY")` (SSOT with `Cargo.toml`,
  guarded by a test); title is the literal `Ylva` (package name is lowercase);
  headers apply to chat and model listing via the shared `extra_headers`, and
  to the connect flow only if it already goes through `OpenRouterProvider`
  (no separate work otherwise).
