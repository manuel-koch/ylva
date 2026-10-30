# Eden AI provider (`edenai`) with `ylva connect edenai`, model discovery and cost tracking

## Context

Ylva is a Rust TUI/CLI agent harness. Providers are selected through named
`[provider_connections.<name>]` tables whose `provider` field is a
`ProviderKind`. Today the kinds are `mock`, `ollama`, `openrouter` and
`copilot`. Ollama and OpenRouter are thin wrappers around the shared
`OpenAiCompatibleProvider` transport.

Goal: add provider kind `edenai` for [Eden AI](https://www.edenai.co/), a hosted
AI gateway with an OpenAI-compatible LLM API (`POST /v3/chat/completions`,
`GET /v3/models`, base `https://api.edenai.run`). Scope:

- the provider implementation (chat, streaming, tools, model discovery);
- onboarding through `ylva connect edenai`;
- cost tracking per request through the existing run-scoped `CostLedger`
  (Eden AI reports `cost` in USD);
- Ylva attribution on requests, in addition to the `User-Agent` header that
  `src/http.rs` already sends on every outbound request, like OpenRouter's
  app-attribution headers — if Eden AI offers a mechanism for it.

Acceptance criteria:

- `provider = "edenai"` loads from `config.toml`; any other unknown value still
  fails at load.
- `ylva connect edenai [--name N] [--workspace] [--force]` writes the connection
  and stores the API key in `.env` as `EDENAI_API_KEY` (or `EDENAI_API_KEY_<NAME>`).
- `/models` lists Eden AI chat-capable models with context length and input
  modalities; reasoning efforts are shown only where Eden AI advertises them.
- Streaming and non-streaming chat with tool calls work through the shared
  transport; the final usage frame's `cost` is added to the process `CostLedger`
  in `USD` (footer total and exit `Costs:` line).
- The attribution mechanism (see Open Questions) is sent on chat and model
  listing, survives runtime-config cloning, and no other provider sends it.
- Docs are updated in the same change; `make format lint` and
  `make test-coverage` pass; `make audit` only if dependencies change (none
  expected).

Constraints:

- Reuse the shared OpenAI-compatible transport; do not fork it.
- Follow the AGENTS rules: full descriptive names, Apache-2.0 header on every
  new source file, file-per-module layout (no `mod.rs`), thin `main.rs`,
  anything tests in `tests/` need is re-exported via `src/lib.rs`.
- Eden AI facts below are from public docs (researched 2026-10-30), not tested
  against a live account. Verify streaming cost shape and reasoning parameters
  with one live call before finalizing the parsers.
- No file edits beyond this plan until the user consents to implementation.

## Implementation Plan

An implementing agent must flip `[ ]` to `[x]` immediately after completing
each step and rewrite this file in place, so progress is resumable across
sessions. Work test-first (TDD); each step bundles its tests.

1. [ ] Check the change against `docs/architecture-design-records/` (ADR 0003
   trust-by-content-hash covers `.env`/`config.toml` of workspace profiles; the
   connect flow must keep honoring it with `--workspace`). Record a new ADR only
   if the design deviates.
2. [ ] Add `Edenai` to `ProviderKind` in `src/config/provider.rs`
   (`as_str()` → `"edenai"`; serde lowercase gives `edenai`). Update the
   `api_base_url` doc comment (Eden AI: origin, `/v3` derived). Add a loading
   test next to the existing ones in `src/config/profile/tests/loading.rs`.
3. [ ] Add `DEFAULT_EDENAI_BASE_URL = "https://api.edenai.run"` and
   `DEFAULT_EDENAI_MODEL = "openai/gpt-latest"` to
   `src/llm/provider_registry.rs`. Extend `ProviderEnv` with
   `edenai_base_url`, `edenai_model`, `edenai_api_key` (Eden AI is requested
   when any of `EDENAI_BASE_URL`, `EDENAI_MODEL`, `EDENAI_API_KEY` is non-empty,
   like OpenRouter), the API-key lookup
   helpers at lines ~88/109, `all_from_environment` (order: Ollama, OpenRouter,
   Eden AI, Copilot — update its doc comments), `from_engine_config`
   (`"edenai"` arm) and `from_provider_config` (`ProviderKind::Edenai` joins the
   Ollama/OpenRouter arm), and `build` (construct `EdenAiProvider`). Add unit
   tests beside the existing OpenRouter ones in that file. Update the
   documented auto-detection precedence (Ollama > OpenRouter > Eden AI >
   Copilot) in code comments and any tests asserting the default-provider order.
4. [ ] Refactor model-catalog handling out of the shared transport
   (behavior-preserving, do first; existing tests must keep passing). Today
   `parse_model_list`/`parse_context_length`/`parse_reasoning_efforts`/
   `parse_reasoning_default`/`parse_input_modalities`/
   `parse_copilot_input_modalities`/`serves_a_chat_endpoint` and
   `ResponsesOnlyModels` in `src/llm/openai_compatible_provider.rs` (~lines
   296–345, 1160–1300) hold OpenRouter and Copilot shapes and state. Replace
   them with a trait object, mirroring `RequestAuthorizer` (~lines 49–67, 448):
   - In that file define `pub(crate) trait ModelCatalog: Send + Sync +
     std::fmt::Debug` with `fn parse(&self, body: &serde_json::Value) ->
     Vec<ModelInfo>` (may update internal state), `fn is_loaded(&self) -> bool`
     (default `true`) and `fn uses_responses_api(&self, model: &str) -> bool`
     (default `false`). Replace the `responses_only_models` field and
     `with_responses_only_models` with `model_catalog:
     Arc<dyn ModelCatalog>` and `with_model_catalog`.
   - A `DefaultModelCatalog` stays in the shared module and is deliberately
     minimal: entries with a string `id`, every other `ModelInfo` field `None`
     (what plain OpenAI-compatible backends advertise).
   - `list_models` (~line 593) calls `model_catalog.parse(&body)` (replacing both
     `replace_from_catalog` and `parse_model_list`). `chat` (~lines 664–695)
     lists models first when `!is_loaded()` (failure falls back to chat
     completions, as now) and picks `/responses` via `uses_responses_api`; only
     clone `self` for that listing when the catalog is not loaded.
   - Move provider shapes out: OpenRouter (`context_length`,
     `architecture.input_modalities`, `reasoning.supported_efforts`/
     `default_effort`) into `OpenRouterModelCatalog` in
     `src/llm/openrouter_provider.rs`; Copilot (`capabilities.type`,
     `supported_endpoints`, `capabilities.limits.*`,
     `capabilities.supports.vision`/`reasoning_effort`, plus the
     responses-only set with its `RwLock` and loaded flag) into
     `CopilotModelCatalog` in a new `src/llm/copilot_provider/catalog.rs`.
   - Ollama is unaffected: `OllamaProvider::list_models` uses its own native
     `/api/tags` + `/api/show` parsing.
   - Shared, genuinely generic helpers (e.g. `order_reasoning_efforts` in
     `src/llm.rs`) stay shared; avoid duplicating them.
   - Move the matching tests from
     `src/llm/openai_compatible_provider/tests/catalog.rs` next to their
     catalogs (including `responses_only_models_are_those_listed_without_chat_completions`);
     keep tests for the default catalog and the trait wiring in the transport
     (`is_loaded` triggers one listing before chat; `uses_responses_api` selects
     the Responses request body).
   - Update `docs/implementation-notes.md` where it describes model discovery and
     the Responses routing.
5. [ ] Create `src/llm/edenai_provider.rs` modeled on
   `src/llm/openrouter_provider.rs`:
   - `EdenAiProvider::new(name, base_url, model, api_key)` derives
     `{base_url}/v3` and builds `OpenAiCompatibleProvider` with
     `.with_provider_kind("edenai")`, `.with_usage_cost_parser(parse_reported_cost)`
     and the attribution tag header (see step 7).
   - `parse_reported_cost` reads top-level `/cost` (USD), also accepting
     `/usage/cost` defensively; rejects negative/non-finite values.
   - Request-body customizer: pass reasoning parameters only in the shape Eden AI
     documents (see Implementation Hints); otherwise leave the body untouched.
   - Delegate all `Provider` methods to `inner` exactly like OpenRouter
     (including `with_runtime_config`, `chat_with_session_id_and_diagnostics`).
   - Register in `src/llm.rs` (`pub mod edenai_provider;`,
     `pub use edenai_provider::EdenAiProvider;`).
   - Unit tests: cost parsing, attribution constant values.
6. [ ] Eden AI model discovery: implement `EdenAiModelCatalog` (the step 4 trait)
   in `src/llm/edenai_provider.rs` and register it with `with_model_catalog`.
   Read `context_length`, `capabilities.input_modalities` (`image`/`file` map to
   `InputModality`; others ignored); skip entries whose
   `capabilities.output_modalities` lack `text` or whose
   `supports_function_calling` is false (user decision); keep `alias_of`
   entries (user decision; the default `openai/gpt-latest` is one); map
   `capabilities.reasoning` (`supported_efforts`, `default_effort`) into
   `reasoning_efforts` / `reasoning_default` when present and send no reasoning
   parameters otherwise. Test with catalog fixtures in `edenai_provider.rs`
   tests.
7. [ ] Send `x-edenai-tags: app=ylva,version=<CARGO_PKG_VERSION>` on chat and
   model listing through `with_extra_headers` (lowercase name:
   `HeaderName::from_static` panics on uppercase; values must be
   `&'static str`, so build it with `concat!` and `env!("CARGO_PKG_VERSION")`).
   Add a unit test that the resulting value satisfies Eden AI's tag value rules
   value rules (see Implementation Hints), since pre-release versions may not.
8. [ ] Add the connect flow. In `src/connect_command/providers.rs` add
   `EndpointConnect::edenai()` (kind `"edenai"`, secret prefix
   `EDENAI_API_KEY`, default base URL `DEFAULT_EDENAI_BASE_URL`,
   `secret_is_optional: false`); add `ConnectCommand::Edenai` in `src/cli.rs`
   (same flags as `Openrouter`, doc comment "Ask for an Eden AI URL and API key
   and add an `edenai` connection to the profile") and the match arm in
   `src/connect_command.rs` (~line 181). Mirror the OpenRouter tests in
   `providers.rs` (default URL, required key, named-connection variable
   `EDENAI_API_KEY_<NAME>`) and the parse test in `src/cli.rs` (~line 974).
9. [ ] Integration tests in `tests/http_provider.rs`: model listing from
   `/v3/models` (set `model_path: Some("/v3/models")` on `MockLlmConfig`),
   attribution on chat + listing, survival across `with_runtime_config`, and
   absence on Ollama/OpenRouter (extend `assert_no_attribution_headers` names).
   Extend `tests/usage_cost.rs` with an Eden AI cost frame (top-level `cost`)
   accumulating in the shared ledger; extend `tests/common/mock_llm_server.rs`
   only if the frame shape needs it.
10. [ ] Update `src/init_command.rs` scaffold comments and the "no provider
   configured" guidance (pointing to `ylva connect ollama` / `openrouter`) to
   mention `edenai`; update affected tests (`tests/cli_exit_codes.rs`,
   `tests/cli_help.rs` if they assert the text).
11. [ ] Documentation (same change set, run `rumdl fmt` then `rumdl check` on
    changed files):
    - `README.md`: provider list (lines ~44–57), data-sharing note (~77–80,
      reword "headers identify the application only" to also cover the Eden AI
      tag header: application name and version only, no user data), feature checklist (~125, add Eden
      AI).
    - `docs/features.md`: provider list (~line 404), connect paragraph (~411),
      model-discovery capabilities (~646), cost section (~367).
    - `docs/provider-notes.md`: usage/cost table (~30) and a new `### Eden AI`
      section (cost field, currency USD, 5.5% platform fee included in the
      reported cost? — verify, attribution, `/v3/models` is public, rate limit
      10 req/s per account); state that Eden AI has no app-attribution feature,
      so the tag only labels the user's own usage and does not advertise Ylva;
      reword "No other provider sends them" for the OpenRouter headers.
    - `docs/implementation-notes.md`: cost-ledger section (~414–430), attribution
      headers note, discovery parsing changes.
    - `docs/configuration.md`: every `provider` value list
      (`mock|ollama|openrouter|copilot`, ~306, ~723; grep for all), example
      connection (~268, ~374), `EDENAI_*` variables.
    - `docs/cli-options.md`: connect commands (~379–412), no-provider guidance
      (~487), environment variables table (~502).
    - `CONTEXT.md`: only if a new domain term is introduced.
    - `CHANGELOG.md` is generated by release-plz; do not edit.
12. [ ] Run `make format lint`, then `make test-coverage` (single mandatory test
    gate; maintain the coverage threshold from `docs/development-workflow.md`).
    Run `make audit` only if `Cargo.toml`/`Cargo.lock` changed.
13. [ ] Manual verification by the user with a real Eden AI key (no live test in
    the suite, no key stored in the repo): `ylva connect edenai`, `/models`, one
    tool-calling turn; confirm the footer cost and the final-stream-frame cost
    shape; fix parsers if the shapes differ from the docs.

## Proposed Improvements

- Show per-model pricing in the `/models` picker for every provider that
  advertises it, as informational text only (never to compute costs; Ylva totals
  only provider-reported costs). Add an optional `ModelInfo.pricing` filled by
  each `ModelCatalog` (step 4): Eden AI `pricing.input_cost_per_token` /
  `output_cost_per_token`, OpenRouter `pricing.prompt` / `pricing.completion`
  (USD per token, strings). Copilot (billing multiplier only) and Ollama (no
  pricing) render nothing when unknown.
- Send `x-edenai-metadata: enabled` when debugging to log which provider served a
  routed model (`edenai_metadata.attempts`); optional, off by default.
- Offer `view=models` listing later so routable names (provider routing,
  e.g. `gpt-5.6-sol`) and `policy/{name}` routing policies appear in `/models`.

## Implementation Hints

Eden AI facts (public docs, <https://www.edenai.co/docs>):

- Base `https://api.edenai.run`; chat `POST /v3/chat/completions`; models
  `GET /v3/models` (public, no key needed; `view=models` gives grouped routable
  names). Auth: `Authorization: Bearer <key>`. The OpenAI SDK works with
  `base_url=https://api.edenai.run/v3`.
- Model ids are `provider/model` (e.g. `openai/gpt-latest`,
  `google/gemini-flash-latest`); a bare name (no provider) enables provider
  routing.
- `/v3/models` entry: `id`, `owned_by`, `context_length`, `description`,
  `capabilities` (`input_modalities`, `output_modalities`, `supports_reasoning`,
  `supports_function_calling`, `supports_tool_choice`, `supports_native_streaming`,
  `supports_system_messages`, …), `pricing` (USD per token), `regions`,
  `zero_data_retention`, optional `alias_of`.
- Cost: every response carries a top-level `cost` (USD) next to `usage`; in
  streams, with `stream_options.include_usage: true` (already sent by the shared
  transport) the final usage event before `[DONE]` carries `cost` (prompt-caching
  docs). The reported cost includes cache prices; Eden AI bills provider price
  plus a 5.5 % platform fee (verify whether `cost` includes the fee).
- Live catalog check (2026-10-30, `GET /v3/models`): 1292 entries, 15 with
  `alias_of`, 128 without `supports_function_calling`, output modalities only
  `text` or `image,text`. `openai/gpt-latest` and `anthropic/*-latest` are
  themselves aliases (`alias_of` points to the concrete id). Reasoning data is
  `capabilities.reasoning = {mandatory, default_effort, supported_efforts}`
  (currently always empty/`null` default) plus `supports_reasoning`; entries
  also carry `source` (pricing URL).
- Attribution/labels: header `X-EdenAI-Tags: key=value,…` (max 10 tags, keys
  `[A-Za-z0-9-_.:/]` ≤64 chars stored lowercase, values ≤128 chars, `Claude Code`
  style single spaces allowed, no commas/`=`). `x-edenai-metadata: enabled`
  adds `edenai_metadata` to responses. No referer/title headers documented.
- Reasoning: only Anthropic models take a `thinking` object; map
  `capabilities.reasoning.supported_efforts` to `ModelInfo.reasoning_efforts`
  and `default_effort` to `reasoning_default` when present; never invent values.
- Rate limit: 10 requests/second per account, shared across keys. Sandbox tokens
  exist for no-cost testing.

Codebase facts:

- Closest template: `src/llm/openrouter_provider.rs` (wrapper, cost parser,
  extra headers) and the OpenRouter assertions at `tests/http_provider.rs`
  ~1296–1380.
- Shared transport hooks in `src/llm/openai_compatible_provider.rs`:
  `with_provider_kind` (sets `provider` on token-usage records),
  `with_usage_cost_parser` (applied to every SSE payload, first cost frame
  recorded before the terminal event), `with_extra_headers` (sent on chat and
  `list_models`), `models_url()` = base + `/models`. Before step 4,
  `parse_model_list` (~line 1166) reads OpenRouter and Copilot shapes and
  `ResponsesOnlyModels` holds Copilot state; step 4 replaces both with the
  `ModelCatalog` trait.
- `RequestCost::new(amount, "USD")` and `CostLedger::process()` are in
  `src/llm/usage_cost.rs`; nothing else is needed for totals display.
- User-Agent is `ylva/<version>` via `OUTBOUND_USER_AGENT` in `src/http.rs`;
  the referer constant for OpenRouter comes from `CARGO_PKG_REPOSITORY`.
- Connect flow: `EndpointConnect` in `src/connect_command/providers.rs` builds
  the secret variable via `secret_variable(kind, prefix, connection_name)`;
  `api_key = "${VAR}"` is written to `config.toml`, the secret to `.env`.
- Test conventions: `docs/development-workflow.md` (testing strategy, shared
  helpers); read `docs/known-test-issues.md` before tests involving tracing,
  cwd or integration side-effects.
- Decisions: no zero-data-retention (ZDR) marker for now (deferred by the user;
  Eden AI exposes per-model `zero_data_retention` true/false/null, OpenRouter
  and Ollama have no comparable per-model field in their model lists). The
  reported `cost` is shown
  as-is; Eden AI charges a 5.5 % platform fee, and whether `cost` includes it is
  unverified — document that uncertainty in `docs/provider-notes.md`.
