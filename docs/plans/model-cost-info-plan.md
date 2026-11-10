# Model picker: show price and model score

## Context

Component: the `/models` picker of Ylva (Rust TUI agent harness; binary + library crate
`ylva`). Each picker row currently shows identity plus detail segments
(`context: …`, `reasoning: …`, input-modality markers). It shows no price and no
indication of how capable a model is, so models are hard to compare.

Goal: make rows comparable by adding (a) **price** per model, taken from the provider's own model
catalog, and (b) an optional **model score** (a user-supplied rating, not a measurement) from a
user-maintained local file `model-scores.toml` (decided; external services are out of scope for this
plan).

Constraints and project rules (from `AGENTS.md` and `docs/`):

- Ylva never computes cost from a price table for the *usage cost total*
  (`docs/provider-notes.md` → "Usage and cost reporting"). Catalog prices shown in the
  picker are display-only and must not feed the cost ledger. This must be stated in
  the docs to avoid contradicting that rule.
- No file edits beyond the plan without user consent; docs are part of the change set
  (update `docs/features.md`, `docs/provider-notes.md`, `docs/configuration.md`,
  `docs/implementation-notes.md` only when the code does not explain it, and
  `README.md` / attribution where required). Ask before creating new doc files.
- Full descriptive names, license header on every new source file, `foo.rs` module
  style, anything integration tests need re-exported through `src/lib.rs`.
- Verification: `make format lint` and `make test-coverage`; `rumdl` on changed
  Markdown (`rumdl fmt <paths>`); `make audit` if `Cargo.toml`/`Cargo.lock` change.
- TDD; follow `docs/development-workflow.md` and read `docs/known-test-issues.md`
  before tests involving `tracing`, cwd or integration side-effects.

Acceptance criteria:

1. Picker rows show an input/output price (USD per 1M tokens) when the provider
   catalog advertises one; rows without data show nothing (no placeholder noise, no
   misleading zero).
2. Free models (price `0`) are distinguishable from unknown price.
3. Providers without catalog pricing (Ollama, plain OpenAI-compatible, EdenAI if
   it advertises none) are unaffected.
4. Rows show `score: <value>` when `model-scores.toml` has a value for the
   normalized model name (and effort); one entry covers all providers. No file or
   no match: nothing shown (missing score is logged, not an error).
5. A broken file never blocks the picker, except under `--strict-config`, where it
   is a startup error.
6. Docs updated, including a "Model score" glossary entry in `CONTEXT.md` (a
   user-supplied rating shown in the picker, not a measurement); format, lint, coverage gate pass.

### Research: ways to compare model strength (candidate score sources)

| Source | Metric | Access | Fit for Ylva |
| --- | --- | --- | --- |
| Artificial Analysis (AA) | Composite Intelligence Index (v5.0.1 at time of research), also price, speed | REST `https://artificialanalysis.ai/api/v2`, header `x-api-key`; free tier = `GET /api/v2/language/models/free` (index, median performance, input/output prices), 100 requests per 24 h window; Pro tier 500; attribution to <https://artificialanalysis.ai/> mandatory; subject to their Terms of Use and Data Platform Terms. Use model/creator IDs, not slugs, as stable keys. Nulls mean "not measured". | Best single-number comparable index and includes price, but needs a user-supplied key + attribution; must be opt-in. Model-ID mapping to provider ids (e.g. OpenRouter `vendor/model`) is the hard part. |
| OpenRouter catalog | No quality score; `pricing.prompt`/`pricing.completion` (USD per token, strings), `supported_parameters`, `reasoning`, `knowledge_cutoff`, `hugging_face_id` | `GET https://openrouter.ai/api/v1/models`, already queried by Ylva | Zero extra requests for price. Rankings by usage exist on the website but no documented stable API for them. |
| LMArena (Chatbot Arena) | Elo from human preference votes | No official public API; community-mirrored datasets/leaderboard files | Unreliable access and licensing; not recommended as a primary source. |
| Epoch AI Benchmarking Hub | Per-benchmark results, ECI capability index | Downloadable CSV data (CC BY license, unverified in detail) | Keyless and cacheable, but coverage of small/new models is uneven; candidate alternative. |
| LiveBench / Aider polyglot leaderboard | Contamination-resistant general / coding scores | Public datasets and YAML/JSON in their repositories | Coding-specific (Aider) is relevant to an agent harness; keyless; but a static, hand-fetched file, not a query API. |
| Hugging Face Open LLM Leaderboard | Average of IFEval, BBH, MATH-5, GPQA, MuSR, MMLU-Pro | HF dataset `open-llm-leaderboard/contents` (last modified 2025-03; retired) | Covers open-weight models only and is stale; not recommended. |
| Provider-native hints | Copilot `capabilities` and billing multiplier (to verify), model "tier" naming | Provider catalogs | Cheap relative-cost hint for Copilot, no strength. |

Decisions: the score comes from a user-maintained local file (no key, no attribution,
no network); display only, no sorting/filtering; prices are per connection catalog;
cache prices live in the data model and the selected row's detail area, the list row
shows `$in/$out per 1M`.

Findings: no source gives a keyless, stable, complete, license-clear strength API.
Artificial Analysis is the strongest candidate but is gated by key and attribution,
so the plan delivers price first (phase 1, no new dependencies on third parties)
and score as an opt-in, pluggable enrichment (phase 2).

## Implementation Plan

Implementing agent: flip `[ ]` to `[x]` immediately after completing each step and
rewrite this file in place so progress is resumable across sessions. Phase 2
(steps 11–16) starts only after the open questions are answered.

1. [x] Read `AGENTS.md`, `docs/development-workflow.md`, `docs/known-test-issues.md`,
   `docs/provider-notes.md` (usage and cost section) and `CONTEXT.md`; check
   `docs/architecture-design-records/` for decisions touching network access or
   persisted state.
2. [x] Add a price type to the model info struct (`ModelInfo`, reachable via
   `ProviderModelOption.model` in `src/llm/engine/selection.rs`): e.g.
   `pricing: Option<ModelPricing>` with `input_per_million_tokens`,
   `output_per_million_tokens`, `cache_read_per_million_tokens` and
   `cache_write_per_million_tokens` as `Option<f64>` (USD). Use TDD with a unit test
   on construction/defaults; update every `ModelInfo { … }` literal (providers,
   mock provider, tests).
3. [x] Parse OpenRouter prices in `OpenRouterModelCatalog::parse`
   (`src/llm/openrouter_provider.rs`): `pricing.prompt` / `pricing.completion` are
   decimal strings in USD per token; parse, reject negative/non-finite values (a
   negative value is used by OpenRouter for variable-priced routers such as
   `openrouter/auto`; treat as unknown), multiply by 1,000,000. Tests: free (`"0"`),
   normal, missing, malformed, negative.
4. [x] Parse EdenAI prices in its model catalog (`src/llm/edenai_provider.rs`):
   `pricing.input_cost_per_token`, `pricing.output_cost_per_token`,
   `pricing.cache_read_input_token_cost`, `pricing.cache_creation_input_token_cost`
   (USD per token). Ignore all other keys (tiers, flex/priority/batch variants).
   Prefer `pricing` (effective price) over `list_pricing`; the two differ when
   `discount` is set. Document the 5.5 % platform fee caveat.
5. [x] Copilot (`src/llm/copilot_provider.rs`): capture a real authenticated
   `GET /models` response and check for a `billing` object; map a price only if it
   carries per-token rates. If it only has a multiplier or nothing, show no price
   (published rates exist only as an HTML table at
   <https://docs.github.com/en/copilot/reference/copilot-billing/models-and-pricing>,
   in AI credits at 1 AIC = $0.01, which the "never use a price table" rule excludes).
   Record the outcome in `docs/provider-notes.md`.
6. [x] Ollama, Ollama-Cloud and plain OpenAI-compatible: no catalog prices
   (see `docs/provider-notes.md`); keep `pricing: None`. No work beyond a test
   that rows stay unchanged.
7. [x] Render price in `format_model_choice_label`
   (`src/tui_reducer_state/model_picker.rs`) as a detail segment, e.g.
   `price: 3/15 $ per 1M` (input/output; cache prices appear only in the selected
   row's detail/preview area, not in the list row), `price: free` for zeros, nothing when
   unknown. Add a compact number formatter (no trailing zeros, sub-cent precision).
   Update snapshot/label tests in `src/tui_reducer_state/tests/model_picker.rs`,
   `src/tui_driver/tests/models.rs` and `src/tui_widgets/overlays.rs` tests.
8. [x] Verify the row still fits narrow terminals: check how `overlays.rs` wraps or
   truncates detail segments and keep price ahead of low-value segments if
   truncation drops trailing ones.
9. [x] Ensure named-engine shortcut rows (`engine_choice_parts`) and the preview
   area show the price of the resolved target model.
10. [x] Docs for phase 1: `docs/features.md` (picker section, near "`/models` queries
   each connection once"), `docs/provider-notes.md` (new "Catalog prices" subsection
   per provider; state explicitly they are display-only and never enter the cost
   total). Run `rumdl fmt` on changed files, then `make format lint` and
   `make test-coverage`.
11. [x] (Phase 2) Add `src/llm/model_scores.rs` (license header; re-export via
    `src/lib.rs`): `ModelScore` (unitless finite `f64`, higher is better, scale chosen
    by the user and neither validated nor converted; TOML integer or float; a plain
    value or a map effort → number with optional `default` key; strings, booleans
    and non-finite values are invalid entries; negative values are allowed), a TOML loader
    for `model-scores.toml` in the user config directory (`dirs`), and
    `normalize_model_name`. Normalization: lowercase; drop vendor prefix up to the
    last `/`; drop `:suffix` routing tags (`:free`, `:thinking`); drop an `@…`
    suffix (region such as `@eu`, or date); drop a trailing `-` plus 4, 6 or 8
    digits or an ISO date (`-1017`, `-0613`, `-20250514`, `-2025-05-14`); keep
    3-digit suffixes; treat `.` and `-` between digits as equal (`4.5` = `4-5`).
    Match is exact on the normalized key; no fuzzy/prefix inheritance (`gpt-5-mini`
    never inherits `gpt-5`). User-file keys are normalized the same way. Duplicate
    normalized keys: warn, later entry wins. TDD with table-driven tests for the
    normalizer, loader and lookup.
12. [x] (Phase 2) Load the file when the picker catalog is populated and cache the
    parsed result with the same lifetime and mechanism as the `/models` catalog
    cache (30 minutes within a run; locate it via `docs/features.md` and the
    `/models` population code). A missing file is silent.
13. [x] (Phase 2) Error handling: an unparsable file or invalid entry logs once
    (see `docs/development-workflow.md#logging`), yields no scores and never blocks
    the picker. With `--strict-config` (`src/cli.rs`, handled in `src/startup.rs`
    ~line 217 as config warnings) any such error is fatal at startup, so the file
    is also validated at startup. Log at debug level when a picker model has no
    score entry or a model's name cannot be mapped to any score entry.
14. [x] (Phase 2) Render `score: <value>` as its own detail segment after
    `price:` in `format_model_choice_label`. A plain value shows as is
    (`score: 85`, trailing zeros trimmed); a map shows its minimum–maximum over all
    values including `default` as an en-dash range (`score: 60–90`, a single value
    when all are equal). The list row therefore does not depend on the selected
    effort and `refresh_label` needs no score recompute. Check the picker renders
    the en dash.
15. [x] (Phase 2) Show each effort's score next to the effort values in the
    parameter popup and the full map plus the active effort's score (e.g.
    `score: low 60 · medium 75 · high 90 (active: high)`) in the selected row's
    detail/preview area.
16. [x] (Phase 2) Docs: `CONTEXT.md` (glossary), `docs/configuration.md` (file location, format,
        plain vs map values, unitless higher-is-better scale, normalization rules, example),
        `docs/cli-options.md` (`--strict-config` also covers the scores file), `docs/features.md`,
        `docs/implementation-notes.md` (only the cache/normalization constraint),
        `docs/keybindings.md` if popup behavior changes; `rumdl fmt`; `make format lint`,
        `make test-coverage`.

Execution notes: the scores file lives in the Ylva home
(`~/.ylva/model-scores.toml`, `$YLVA_HOME`) rather than `dirs::config_dir`, to
match other per-user state. Copilot (step 5) shows AI credit prices
(`AIC per 1M`, never converted to USD) from `billing.token_prices`, which
`/models` returns only for `x-github-api-version` 2026-06-01 or newer. Cache
prices and the effort score map appear as extra lines under the picker's
compaction preview line.

## Proposed Improvements

- Sort or filter the picker by price once price data exists (additive follow-up).
- Show a relative cost hint ("cheap/medium/expensive" buckets) so values are
  scannable without reading numbers.
- Later plugin: fetch scores from Artificial Analysis (key in an env var, 24 h disk
  cache, mandatory attribution, free tier 100 requests per 24 h) behind the same
  `ModelScore` lookup; not part of this plan.
- Sorting/filtering the picker by price or score (explicitly out of scope now).
- Document that prices are per token and exclude the extra reasoning tokens a higher
  effort produces (price is not bound to effort; request cost is); parsing
  OpenRouter `internal_reasoning` / EdenAI `output_cost_per_reasoning_token` is a
  possible follow-up.
- Fix the typos in the source idea doc when it is moved/archived.

## Consistency Issues

- [low] `docs/provider-notes.md` states Ylva "never computes a cost from a price
  table". Catalog prices are display-only; the docs must say so to avoid an apparent
  contradiction, and no code may feed them into the usage-cost ledger.

## Implementation Hints

- Picker row model: `ModelChoice` and `ChoiceLabel { identity, details }` in
  `src/tui_reducer_state/model_picker.rs`; details are assembled in
  `format_model_choice_label` (context, reasoning, modality markers).
- Catalog parsing hook: `ModelCatalog::parse(&self, body: &serde_json::Value) ->
  Vec<ModelInfo>` implemented per provider (e.g. `OpenRouterModelCatalog` in
  `src/llm/openrouter_provider.rs`); it already reads `context_length`,
  `top_provider.max_completion_tokens`, `reasoning` and
  `architecture.input_modalities`.
- OpenRouter `/api/v1/models` entries contain `pricing` (`prompt`, `completion`, and
  optional `web_search`, strings in USD per token), `hugging_face_id`,
  `canonical_slug`, `knowledge_cutoff`.
- `/models` queries each connection once and reuses the result for 30 minutes within
  a run (`docs/features.md`, around line 434).
- Dependencies already available: `reqwest 0.13` (json, stream, gzip), `serde`,
  `serde_json`, `dirs 7`, `chrono`.
- Artificial Analysis: header `x-api-key`; envelope fields `tier`, `pagination`,
  `data`; free endpoint `/api/v2/language/models/free`; OpenAPI spec at
  <https://artificialanalysis.ai/api/v2/openapi>; headers `X-RateLimit-Limit`,
  `X-RateLimit-Remaining`, `X-RateLimit-Reset`, `Retry-After` on 429; index field
  `artificial_analysis_intelligence_index`; coding/agentic indices are always null;
  nulls mean not measured. Verify exact free-tier field names against the OpenAPI
  spec before coding.
- Catalog pricing survey (live responses): OpenRouter `pricing` keys are `prompt`, `completion`,
  `input_cache_read`, `input_cache_write`, `input_cache_write_1h`, `internal_reasoning`,
  `web_search`, `image`, audio keys and `overrides`; values are USD per token strings. EdenAI
  `GET /v3/models` (public, no key) returns `pricing` and `list_pricing` objects for every model
  (floats, USD per token) with `input_cost_per_token`, `output_cost_per_token`, optionally
  `cache_read_input_token_cost` and `cache_creation_input_token_cost`, plus many tier/variant keys
  (`*_above_200k_tokens`, `*_flex`, `*_priority`, `*_batches`); ~85 models carry a `discount`.
  Copilot: `billing.token_prices` (AI credits) with API version 2026-06-01+. Ollama(-Cloud) and
  generic OpenAI-compatible: none.
