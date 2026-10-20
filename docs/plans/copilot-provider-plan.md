# GitHub Copilot provider for Ylva

## Context

Ylva (Rust TUI agent harness, crate `ylva`, lib + bin) has provider kinds
`mock`, `ollama`, `openrouter` (`ProviderKind` in `src/config/provider.rs`).
Ollama and OpenRouter share the OpenAI-compatible chat transport
(`OpenAiCompatibleProvider`). Goal: add a `copilot` provider kind so a
profile can use GitHub Copilot LLMs (incl. premium models such as GPT-5.x,
Claude, Gemini) through a `[provider_connections.<name>]` table with
`provider = "copilot"`, while Ylva keeps full control of the agent loop and
tool calls.

Auth model (from the original idea, verified externally only by community
reports, not by this repo):

- Fine-grained/classic PATs (`github_pat_`, `ghp_`) **cannot** be exchanged for a
  Copilot session token (HTTP 403 at `copilot_internal/v2/token`). The idea's
  wish "support `GH_TOKEN` with a PAT" is therefore not achievable for raw
  HTTP access; only a `ghu_` GitHub App user token from the OAuth device flow
  works.
- Device flow: client id `Iv1.b507a08c87ecfe98` (VS Code Copilot Chat app),
  scope `read:user`, `POST https://github.com/login/device/code`, poll
  `POST https://github.com/login/oauth/access_token` with grant type
  `urn:ietf:params:oauth:grant-type:device_code`. Result `ghu_…` is
  long-lived (no refresh token; revoked after 1 year of inactivity).
- Runtime: exchange `ghu_` via
  `GET https://api.github.com/copilot_internal/v2/token` (header
  `Authorization: token <ghu_>`, plus `Editor-Version`,
  `Editor-Plugin-Version`, `User-Agent`) → `{token, expires_at, endpoints.api}`.
  Session token TTL ≈ 25–30 min; refresh ahead of expiry (5 min buffer).
  API base = `endpoints.api` or fallback `https://api.githubcopilot.com`.
- Chat/model endpoints (`{api_base}/chat/completions`, `{api_base}/models`) are
  OpenAI-compatible and need headers on every request:
  `Authorization: Bearer <session token>`, `Copilot-Integration-Id: vscode-chat`,
  `Editor-Version: vscode/1.99.3`, `Editor-Plugin-Version: copilot-chat/0.26.7`,
  `User-Agent: GitHubCopilotChat/0.26.7`; recommended `Openai-Intent:
  conversation-panel`, `x-github-api-version: 2025-04-01`, `X-Initiator`
  (`user`/`agent`).
- The API is undocumented and may change; excessive request rates can get
  Copilot access suspended (suggested ≤ 1 request / 2 s).

Onboarding (decided): CLI subcommand (not a slash command; `/connect` deferred)
that runs the device flow, writes `[provider_connections.<name>]` with
`provider = "copilot"` and `api_key = "${COPILOT_GITHUB_TOKEN}"` into the
profile `config.toml` via `toml_edit`, and stores the token as
`COPILOT_GITHUB_TOKEN` in the profile `.env`. The env var is also used for
auto-detection of a `copilot` connection without any config.
Decisions: only `COPILOT_GITHUB_TOKEN` is read (never `GH_TOKEN`); PAT-looking
values are rejected; VS Code client id/headers are hardcoded and not
configurable; the terms/stability risk is documented in `docs/provider-notes.md`.

Acceptance criteria:

1. `provider = "copilot"` connection loads, resolves, and builds a provider;
   chat with streaming + tool calls works through the shared transport.
2. Session token is exchanged lazily, cached in memory, and refreshed before
   expiry without user interaction; the long-lived token comes from a profile
   `.env` secret (never written to `config.toml`, logs, or request dumps).
3. `/models` lists the account's Copilot catalog.
4. A guided onboarding (device flow) creates the connection and secret.
5. Unit/integration tests added; `make format lint`, `make test-coverage`
   (≥80 %) and `make audit` pass; docs updated in the same commit.

## Implementation Plan

Implementing agent: flip `[ ]` to `[x]` immediately after finishing each step
and rewrite this file in place, so progress is resumable across sessions.
Follow TDD; use full descriptive names (no abbreviations); every new source
file needs the Apache-2.0 header (copy from `src/lib.rs`).

1. [ ] Read [AGENTS](../../AGENTS.md), `docs/development-workflow.md`,
   `docs/known-test-issues.md`, `docs/provider-notes.md`, and the resolved
   decisions under *Implementation Hints*. Resolve the *Open Questions* first
   (they gate steps 3 and 7).
2. [ ] Config: add `ProviderKind::Copilot` (`"copilot"`) in
   `src/config/provider.rs` (`as_str`, serde lowercase). Fix all exhaustive
   matches (grep `ProviderKind::` in `src/llm/provider_registry.rs`,
   `src/llm/token_usage.rs`, `src/render.rs`, `src/agent/subagent_manager.rs`,
   `src/startup/engines.rs`, `src/llm.rs`). Add config-loading tests next to
   `src/config/profile/tests/loading.rs`.
3. [ ] Env/secret plumbing in `src/llm/provider_registry.rs`: add
   `copilot_github_token` (`COPILOT_GITHUB_TOKEN`) to `ProviderEnv`,
   `from_lookup`, `apply_missing_api_key`, `apply_missing_api_key_to_provider`.
   Never read `GH_TOKEN`. Treat `api_key` on a copilot connection as the
   long-lived `ghu_` token; reject values starting with `github_pat_` or
   `ghp_` with the error "PAT unsupported, run onboarding" (validate when the
   provider is built).
   Extend `from_provider_config`/`from_engine_config`/`build`:
   Copilot needs **no** `api_base_url` (derived at runtime); do not require it.
   Add `ProviderSelection` handling (new variant or reuse
   `OpenAiCompatible` with `provider_kind: Copilot`; prefer reuse, KISS).
   Extend `all_from_environment`: when `COPILOT_GITHUB_TOKEN` is non-empty add an
   auto-detected `copilot` selection (connection name `copilot`) after
   Ollama and OpenRouter, using `DEFAULT_COPILOT_MODEL = "gpt-4o"` unless `COPILOT_MODEL`
   (`ProviderEnv::copilot_model`) is set. An explicit
   `[provider_connections.copilot]` suppresses auto-detection, as for other
   kinds.
4. [ ] New module `src/llm/copilot_auth.rs` (pure, testable): types
   `CopilotSessionToken { token, expires_at, api_base }`, `CopilotAuth`
   (holds `ghu_` token, `reqwest::Client`, `tokio::sync::Mutex<Option<CopilotSessionToken>>`),
   `async fn session_token()` that returns the cached token if
   `expires_at > now + 300 s`, else exchanges (single-flight under the mutex).
   Base URLs injectable for tests (loopback server pattern as in
   `src/llm/openrouter_provider.rs` tests). Map 401/403/404 exchange errors to
   clear messages ("token not a ghu_ token / re-run onboarding"). Redact
   tokens in `Debug`.
5. [ ] New module `src/llm/copilot_provider.rs`: `CopilotProvider` wrapping
   `OpenAiCompatibleProvider` like `OpenRouterProvider` does (delegate all
   `Provider` methods incl. `with_runtime_config`, `set_model`,
   `set_reasoning_effort`, the four `chat_*` variants, `list_models`).
   Needs a per-request hook in `OpenAiCompatibleProvider` to (a) resolve
   the base URL/bearer token dynamically from `CopilotAuth` before each
   request (chat at `openai_compatible_provider.rs` ~l.400–520 and
   `list_models`) and (b) inject the extra integration headers. Add a minimal,
   generic extension point (e.g. a `RequestAuthResolver` async callback +
   static extra headers) rather than copying the transport. Register in
   `src/llm.rs` and re-export via `src/lib.rs` if integration tests need it.
   Set `.with_provider_kind("copilot")`; ensure `redact_chat_completion_request_body`
   and request dumps never contain the Authorization header or tokens.
6. [ ] Provider details: verify with a live smoke test (manual, not in CI)
   that streaming, `tools`, `stream_options.include_usage`, and reasoning
   fields behave; add a request-body customizer if Copilot rejects any
   OpenAI field. Record cost/usage behavior (Copilot reports premium-request
   multipliers, not USD – likely no `usage_cost_parser`) for the docs.
7. [ ] Wire registry `build()` to construct `CopilotProvider` and add
   `DEFAULT_COPILOT_MODEL` (`"gpt-4o"`). Update
   `src/startup/engines.rs` if engine resolution special-cases kinds.
8. [ ] Device-flow onboarding in `src/llm/copilot_auth.rs` (or sibling
   `copilot_device_flow.rs`): `request_device_code`, `poll_for_access_token`
   (handles `authorization_pending`, `slow_down` (+5 s), `expired_token`,
   `access_denied`, cancellation via `CancellationToken`, deadline). Returns
   `ghu_` token. Tests with loopback mock server and short intervals.
9. [ ] CLI subcommand (e.g. `ylva connect copilot`) in `src/cli.rs` /
   `src/cli_driver.rs` (follow the existing `Subcommand` enums): print
   `verification_uri` + `user_code`, poll, then persist:
   - connection name defaults to `copilot` (token variable
     `COPILOT_GITHUB_TOKEN`); `--name <other>` uses
     `COPILOT_GITHUB_TOKEN_<NAME>` (upper-cased, non-`[A-Z0-9_]` characters
     replaced by `_`), failing if that variable already exists in the profile
     `.env`; only the default connection is env-auto-detected,
   - the written `api_key` references the matching variable,
   - add `toml_edit` to `Cargo.toml` (run `make audit`); insert
     `[provider_connections.<name>]` with `provider = "copilot"` and
     `api_key = "${COPILOT_GITHUB_TOKEN}"` into the profile `config.toml`,
     preserving comments/other content; refuse to overwrite an existing table of
     that name,
   - write/update `COPILOT_GITHUB_TOKEN=<ghu_>` in the profile `.env`
     owner-only (see `src/private_fs.rs`, `write_seed_contents` in
     `src/init_command.rs`), keeping unrelated entries; never print the token.
   `/connect` slash command is out of scope (follow-up).
10. [ ] Tests: unit tests for auth caching/refresh/expiry buffer, header
    injection, error mapping, `ProviderKind` parsing, env plumbing, registry
    resolution; integration test (`tests/`) exercising chat + tool call against
    a loopback server through `CopilotProvider`; onboarding file-writing tests
    using temp dirs. Read `docs/known-test-issues.md` before touching
    `tracing`/cwd. Use `crate::test_support::run_locked` for async tests as in
    existing provider tests.
11. [ ] ADR: `docs/architecture-design-records/` is currently empty. Record a
    decision (use the `architecture-decision-record` skill) covering: use of
    the undocumented Copilot API with VS Code's client id, token storage in
    profile `.env`, session-token exchange/caching, and extending the shared
    OpenAI-compatible transport vs. a separate transport.
12. [ ] Docs (same commit): `docs/configuration.md` (provider kinds list at
    ~l.262 and ~l.586, new copilot example under `[provider_connections.*]`,
    env var, onboarding), `docs/features.md` (provider + connect subcommand),
    `docs/implementation-notes.md` (modules `copilot_auth`, `copilot_provider`,
    auth hook), `docs/provider-notes.md` (new *GitHub Copilot* section: usage/
    cost reporting, PAT limitation, ToS/stability caveats),
    `docs/cli-options.md` (new connect subcommand), `README.md` provider
    mentions, `CONTEXT.md` terms (e.g. *Copilot session token*), `docs/diagrams/class-diagram.mmd`
    (re-render via `make render-all-diagrams` if Docker is available), and
    `docs/ideas/copilot-provider.md` (mark as implemented or delete per
    project convention). Run `rumdl fmt` and `rumdl check` on changed
    markdown.
13. [ ] Verify: `make format lint`, `make test-coverage` (≥80 %), `make audit`
    if `Cargo.toml`/`Cargo.lock` changed. Do not commit or stage without user
    consent.

## Proposed Improvements

- Provide an onboarding result check: after onboarding, call the exchange and
  `/models` once and show plan + model count so failures surface immediately.
- Support Enterprise via configurable GitHub host (`github.com` is hardcoded in
  the idea; GHE would need different device-flow/token URLs).
- `/connect` slash command later, reusing the device-flow module.
- Keep a small model-capability map from `/models` (context window, tool
  support) to populate `context_length` rather than the default.
- Persist the session token cache nowhere (memory only) to avoid a second
  secret on disk; re-exchange on start is cheap.

## Consistency Issues

- [low] The idea's `GH_TOKEN` + PAT wish is dropped: PATs are rejected by the
  token exchange, so only `ghu_` tokens in `COPILOT_GITHUB_TOKEN` are supported.
- [low] The device flow impersonates the VS Code Copilot Chat client id and
  headers (accepted by the user; documented in `docs/provider-notes.md`).
- [low] `ProviderConnectionConfig.api_base_url` docs say it is used for
  discovery/chat; for Copilot it is derived at runtime and must be optional,
  but `from_provider_config` currently errors when it is missing for non-mock
  kinds (step 3 changes this).
- [low] `ProviderKind::Mock` currently builds through `OpenAiCompatibleProvider`
  in `build()` for the `OpenAiCompatible` selection; adding `Copilot` there must
  not fall into that arm (step 3/7).

## Implementation Hints

- Existing pattern: `OpenRouterProvider` (`src/llm/openrouter_provider.rs`)
  wraps `OpenAiCompatibleProvider::new_with_request_body_customizer(...)
  .with_provider_kind("openrouter").with_usage_cost_parser(...)` and delegates
  every `Provider` trait method; the `Provider` trait is in `src/llm.rs`
  (~l.229) with default methods for `with_runtime_config`, `chat_with_*`.
- `OpenAiCompatibleProvider` (`src/llm/openai_compatible_provider.rs`) holds
  `base_url`, `api_key: Option<String>` (cloned per request, sent as bearer via
  `crate::http::get_json(..., api_key, ...)` and the chat request at ~l.478–514),
  a shared `reqwest::Client` from `crate::http::provider_client_builder`, and
  a `RequestConcurrencyLimiter` keyed by provider name
  (`crate::llm::provider_request_limiter`) – reuse it to honor the ≤1 req/2 s
  guidance. Pre-stream 429/5xx are retried with backoff honoring `Retry-After`.
- `ProviderEnv` (`provider_registry.rs`) is a plain snapshot built from process
  env + profile `.env` (`crate::config::load_profile_environment`); process
  values win. Existing keys: `OLLAMA_*`, `OPENROUTER_*`.
- `ProviderRegistry::from_provider_config` lets engine-level `api_base_url`/
  `api_key` override connection values.
- Profile `.env` is created owner-only (`write_seed_contents` in
  `src/init_command.rs`); `.env` is gitignored by workspace init.
- `docs/architecture-design-records/` exists but is empty; `docs/plans/` had no
  other plans at planning time.
- Idea file also lists reference links (hermes-agent#27836, #7731, opencode#19338,
  LiteLLM github_copilot provider docs) useful for header/endpoint behavior.
- Existing deps cover HTTP/serde/chrono; `toml_edit` is approved as the only
  new dependency. `${ENV_VAR}` interpolation applies to every TOML string
  (`docs/configuration.md` ~l.301; example `api_key = "${OLLAMA_CLOUD_API_KEY}"`
  at ~l.228), so `api_key = "${COPILOT_GITHUB_TOKEN}"` is the established
  secret-reference spelling; missing variables fail at load.
- CLI subcommands are `clap` `Subcommand` enums in `src/cli.rs` (~l.175–270).
