# Add a Brave Search backend for the `web_search` tool

An implementing agent must flip `[ ]` to `[x]` on each step (and its
sub-steps) immediately after completing it and rewrite this file in place, so
progress is resumable across sessions. Each step names the tests that verify
it; a step is done only when those tests pass and — where the step says so —
its documentation is updated in the same change set.

## Context

Ylva's `web_search` agent tool searches the web through the injectable
`WebSearchBackend` seam (`src/agent/tools/backends/web_search.rs`) with two
implementations today: DuckDuckGo (the default, a scraped form-POST against
`https://html.duckduckgo.com`) and SearXNG (a JSON form-POST against a
user-configured instance). This plan adds **Brave Search** as a third
backend, selected by

```toml
[web_search]
backend = "brave"

[web_search.backends.brave]
api_key = "${BRAVE_API_KEY}"
```

Motivation: Brave provides a documented, independent-index JSON API — unlike
DuckDuckGo's bot-detected HTML endpoint it needs no scraping,
and unlike SearXNG it needs no self-hosted instance. It is already listed as
an unchecked roadmap item in `README.md`. The user has decided the config
shape: a single required `api_key` in a nested `[web_search.backends.brave]`
table. A selected Brave backend without a non-blank key is a hard config
error; secrets are sourced through `${ENV_VAR}` interpolation only (no
automatic `BRAVE_API_KEY` fallback). There is no `base_url` override or
query-knob config in scope. `docs/ideas/web-search.md` is the user's
brainstorm file and is not edited by this plan.

**Decisions settled by grilling (binding):**

- The Brave table contains only `api_key`; it is required and non-blank
  when Brave is selected. A present but unselected Brave table may omit the
  key or contain a blank value.
- Use profile `${ENV_VAR}` interpolation for the key; no implicit process
  environment fallback.
- Send only `q`, `count`, `result_filter=web`, and
  `text_decorations=false`; keep Brave's other API defaults.
- Use `X-Subscription-Token` header authentication and parse the Brave error
  envelope for non-2xx diagnostics. A successful response with absent/empty
  `web.results` is a valid empty result.
- Redact request-dump headers whose names end in `-token`, case-insensitively,
  in addition to the existing explicit sensitive-header names.
- Pass search terms through unchanged; do not locally reject or truncate
  Brave's 600-character/75-word constraints. Surface the API's error detail.
- Backend construction settings use a backend-specific enum, not a generic
  settings struct or positional list of unrelated optional values.
- Keep SearXNG selected in the e2e profile; tests never make live network
  requests. The user will handle this plan file's retention/removal after
  implementation.

Brave Search API facts (from `https://api-dashboard.search.brave.com/api-reference/web/search/get`,
verified 2026-10-01):

- `GET https://api.search.brave.com/res/v1/web/search` with query `q`
  (required, ≤600 chars/75 words; the backend passes it through unchanged),
  `count` (`1..=20`, default 20),
  optional `country`/`search_lang`/`safesearch`/`freshness`/`result_filter`/
  `text_decorations`/…
- Authentication: header `X-Subscription-Token: <token>` (required). A
  `api_key` query parameter also exists — do **not** use it (query strings
  are written verbatim into `YLVA_REQUEST_DUMP_PATH` dumps).
- 200 body: `{ type, query, web: { results: [{ title, url, description,
  page_age, … }] }, news, videos, … }`; `web` may be absent/null.
- Error bodies (400/401/403/404/422/429): `{ type, error: { id, status,
  detail, code }, time }` — `detail`/`code` carry the diagnosis.

Acceptance criteria:

- The config above loads and validates; selecting `brave` without the table,
  without `api_key`, or with a blank/whitespace-only `api_key` is a hard
  config error naming the config path, the backend, the key, and the reason.
- An unknown `backend` value is rejected naming all accepted values
  (`duckduckgo, searxng, brave`).
- The backend performs one `GET https://api.search.brave.com/res/v1/web/search`
  with `q` (passed through unchanged), `count` (clamped to `1..=20`), `result_filter=web`,
  `text_decorations=false`, the headers `Accept: application/json`,
  `Accept-Encoding: gzip`, `X-Subscription-Token`, maps `web.results[]` to
  `WebSearchHit` (truncate to `limit`, `description` → whitespace-normalized
  `snippet`), and honors the fail-fast policy:
  - transport error → `WebSearchError::Request`
  - non-2xx → `WebSearchError::StatusWithHint` when the body is a Brave
    error envelope with a non-empty `detail`/`code` (the hint carries that
    text), otherwise `WebSearchError::Status`
  - body is not the expected JSON shape → `WebSearchError::Parse`
  - absent `web` or empty `web.results` on HTTP 200 → `Ok(vec![])` (Brave
    has no "engine failed" signal; empty means no matches)
  - never a retry, never a silently-empty result set on a failed response.
- The subscription token never reaches a `YLVA_REQUEST_DUMP_PATH` artifact:
  `x-subscription-token` and other `*-token` headers are redacted
  case-insensitively, along with the existing credential headers.
- `network_target()` returns `https://api.search.brave.com` so
  `[tools.web_search] network_request` rules can target it.
- Backend construction never silently falls back: `backend_for` and
  `WebSearchConfig::build_backend` return `Result`, and `default_registry`
  converts a construction error into an explicit validated-config `expect`
  — no code path can substitute DuckDuckGo for a configured Brave backend.
- The user-facing tool title becomes `Web Search (Brave)`; the model-facing
  description stays backend-agnostic (no tool changes needed).
- DuckDuckGo remains the default; an absent `[web_search]` table behaves
  exactly as before.
- Tests never touch the network: mock TCP server + fixtures only; live
  verification via the e2e setup (no `#[ignore]`d live test — matches the
  SearXNG precedent).
- Docs affected by the change are updated in the same change set (step 9
  names every file).
- `make format lint` and `make test-coverage` pass (coverage threshold 80).

Key constraints:

- Fail-fast backend policy (module docs of the seam): no retries, no silent
  empty results on unrecognized/blocked responses.
- The `WebSearchBackend` trait must stay `dyn`-compatible (boxed futures).
- Network side effects must be constructor-injected
  (`docs/known-test-issues.md`).
- Full descriptive names, no abbreviations (`BraveBackend`, `api_key`).
- Docs are part of the change set; a source change that leaves a doc stale is
  incomplete (`AGENTS.md`).
- Secrets: no literal token in code, docs, or tests; the profile `.env`
  (`tests/e2e/custom/profiles/default/.env`, gitignored via `.gitignore:28`)
  already defines `BRAVE_API_KEY` — never print or copy its value.

## Implementation Plan

1. [ ] Restore the green baseline before touching anything else. The working
   tree already contains `[web_search.backends.brave] api_key = "${BRAVE_API_KEY}"`
   in `tests/e2e/custom/profiles/default/config.toml`; `${…}` interpolation
   runs over every TOML string at profile load, and
   `the_e2e_custom_profile_keeps_loading_with_target_rules`
   (`src/config/profile.rs:2280`) supplies an environment map without that
   key — the test currently fails with
   `MissingEnvironmentVariable { name: "BRAVE_API_KEY" }` (verified by
   running it).
   - [ ] Add `("BRAVE_API_KEY".to_string(), "test-key".to_string())` to the
     map at `src/config/profile.rs:2288-2299`, mirroring the
     `OLLAMA_CLOUD_API_KEY` entry.
   - Tests: `cargo test --lib the_e2e_custom_profile_keeps_loading_with_target_rules`
     passes; the suite is green again before step 2.

2. [ ] Generalize sensitive-header redaction and cover the Brave token
   (`src/http.rs`). The existing explicit list does not include
   `x-subscription-token`, and the current matcher has no generic token
   suffix rule; without this change a Brave search writes its credential
   into a `YLVA_REQUEST_DUMP_PATH` artifact, contradicting the invariant
   stated above `SENSITIVE_HEADER_NAMES` (`src/http.rs:405-412`).
   - [ ] Keep the existing explicit sensitive-header names and extend
     `is_sensitive_header_name` (`src/http.rs:429`) to also recognize any
     header name ending in `-token`, case-insensitively.
   - [ ] Extend `dump_request_redacts_sensitive_header_values`
     (`src/http.rs:1087`) to assert `x-subscription-token` and an unrelated
     token-suffixed name such as `x-custom-token` are redacted, including
     mixed-case input to `is_sensitive_header_name`; verify a non-suffix
     name such as `x-token-id` remains unaffected by the new rule. Assert
     the raw token values appear nowhere in the dump file.
   - Tests: `cargo test --lib dump_request_redacts_sensitive_header_values`.

3. [ ] Implement the backend module
   `src/agent/tools/backends/web_search/brave.rs` and declare it in the seam
   (`pub mod brave;` + `pub use brave::BraveBackend;` next to the existing
   declarations at `backends/web_search.rs:38-42`), mirroring
   `searxng.rs` (the closest analogue: JSON API):
   - [ ] `BraveBackend { client, api_key, dump_source }` with constructors
     `with_api_key(api_key, http_timeout)`,
     `with_api_key_and_dump_path(api_key, http_timeout, dump_path)` (test
     hook), and private `with_api_key_and_dump_source(…)` building a
     `reqwest::Client` with `OUTBOUND_USER_AGENT` and the configured
     timeout; implement `Debug` manually so it keeps the `BraveBackend`
     type name but never formats the API key (e.g. show `<REDACTED>`); a
     `DEFAULT_BASE_URL` const `https://api.search.brave.com` (endpoint
     `{base}/res/v1/web/search`) — no configurable URL.
   - [ ] `search()`: short-circuit `Ok(Vec::new())` on `limit == 0`; `GET`
     `{DEFAULT_BASE_URL}/res/v1/web/search` with query pairs `q` = terms,
     `count` = `limit.min(WEB_SEARCH_MAX_RESULT_LIMIT)`,
     `result_filter` = `web`, `text_decorations` = `false`; explicit headers
     `Accept: application/json`, `Accept-Encoding: gzip`,
     `X-Subscription-Token: <api_key>`; dump request/response through
     `RequestDumpContext` exactly like `searxng.rs` (dump the built URL —
     query included — plus `redacted_headers_json(request.headers())` and a
     `null` body; no retry). Do not locally reject, truncate, or otherwise
     rewrite `terms`; Brave's 4xx error envelope carries diagnostics for an
     empty or over-limit query.
   - [ ] Response handling: non-2xx → try to parse the Brave error envelope
     (`{error: {detail, code}}`); a non-empty `detail` (falling back to
     `code`) becomes `WebSearchError::StatusWithHint`, otherwise plain
     `Status`; HTTP 200 → `parse_brave_results`.
   - [ ] Parsing with private serde DTOs: `BraveSearchResponse { web:
     Option<BraveWebResults> }`, `BraveWebResults { results }`,
     `BraveWebResult { title, url, description: Option<String> }`; skip
     entries with empty `title`/`url` (mirror the other backends), take at
     most `limit`, whitespace-normalize `title`/`snippet` but do not
     truncate snippets (the tool caps them at
     `WEB_SEARCH_SNIPPET_MAX_CHARS`); absent `web` or empty `results` →
     `Ok(vec![])`; anything that is not the expected JSON shape → `Parse`.
   - [ ] `network_target()` = `crate::http::network_target(DEFAULT_BASE_URL)`
     with a raw-const fallback (same expression shape as
     `duckduckgo.rs:103-105`).
   - [ ] Broaden the doc comment of `WebSearchError::StatusWithHint`
     (`backends/web_search.rs:142-146`), which currently names only
     SearXNG's `403`, to cover Brave's error envelopes; extend the seam
     module docs (`:15-28`) with the new module.
   - [ ] Tests inside `brave.rs`, following the `duckduckgo.rs`/`searxng.rs`
     test modules (raw `tokio::net::TcpListener` one-shot mock + JSON
     fixtures, never the live API):
     - wire-shape test: request line `GET /res/v1/web/search?…` carrying
       `q`, `count`, `result_filter=web`, `text_decorations=false`; headers
       `x-subscription-token`, `accept: application/json`, `user-agent`
       `ylva/*`; request dump `path` = `/res/v1/web/search`, `query`
       contains `count=…`, `headers["x-subscription-token"]` ==
       `<REDACTED>`; response dump gets the `.json` extension.
      - parse tests: hit mapping (title/url/description → snippet with
        normalized whitespace), missing `description` → empty snippet,
        entries with empty title/url skipped, `limit` capping, absent `web`
        → `Ok(vec![])`, empty `results` → `Ok(vec![])`, non-JSON body →
        `Parse`.
      - failure tests: `401` with an envelope → hint-carrying error carrying
        the server's `detail`; `429` with an envelope → hint; non-2xx with a
        plain-text body → `Status`; `200` with an HTML body → `Parse`; `422`
        for an over-limit query → hint carrying Brave's detail, with the
        original query sent unchanged.
      - `network_target()` → `https://api.search.brave.com`; zero-limit
        short-circuit performs no request (mirror `duckduckgo.rs:596-608`).
      - Format the backend with a sentinel API key and assert the sentinel
        does not appear in Debug output while the output still identifies
        `BraveBackend`.
      - no `#[ignore]`d live test (settled: e2e covers live checks).
   - Tests: `cargo test --lib backends::web_search::brave`.

4. [ ] Wire selection and construction — one atomic, compiling change (the
   factory input changes and `WebSearchBackendsConfig` gains a
   field, so seam + config + every struct literal move together):
   - [ ] In `backends/web_search.rs`: add `WebSearchBackendKind::Brave`;
     add `BRAVE_NAME = "brave"` / `BRAVE_DISPLAY_NAME = "Brave"` constants;
     append `Self::BRAVE_NAME` to `SUPPORTED_NAMES` (order:
     `duckduckgo, searxng, brave` — default first); extend the `as_str`,
     `display_name`, and `from_name` matches.
   - [ ] Add a backend-specific `WebSearchBackendSettings<'a>` enum in
     `backends/web_search.rs` with variants `DuckDuckGo`,
     `SearXng { base_url: Option<&'a str> }`,
     `Brave { api_key: Option<&'a str> }`, and
     `Unsupported { backend_name: &'a str }`. Each supported variant carries
     only its own backend's setting; do not derive `Debug` for this enum,
     because the Brave variant contains a secret.
   - [ ] Change `backend_for` to accept
     `backend_for(settings: WebSearchBackendSettings<'_>, http_timeout: Duration)`
     and return `Result<Arc<dyn WebSearchBackend>, WebSearchBackendError>`:
     `DuckDuckGo` → `DuckDuckGoBackend::new`; `SearXng` with a non-blank
     `base_url` → `SearXngBackend::with_base_url` (unchanged semantics);
     `Brave` with a non-blank `api_key` → `BraveBackend::with_api_key`;
     missing/blank required settings → `Err` naming the backend and config
     key; `Unsupported` → `Err` naming the backend and supported names.
     `WebSearchConfig::build_backend()` maps `WebSearchBackendKind` plus its
     typed per-backend config tables to this enum (unknown names map to
     `Unsupported`) before calling the factory.
   - [ ] Generalize `WebSearchBackendError::MissingBackendSetting`
     (`backends/web_search.rs:185-188`): its `thiserror` message hardcodes
     `'searxng' … to name the SearXNG instance` and cannot carry Brave's
     missing key as written. Give it a `backend_name` field and a generic
     message (`web search backend '{backend_name}' requires {missing_key}`),
     and update its doc comment. The existing test only asserts the error
     contains `base_url`, so it survives.
   - [ ] In `src/config/web_search.rs`: add
     `BraveBackendConfig { api_key: Option<String> }` and
     `brave: Option<BraveBackendConfig>` on `WebSearchBackendsConfig`, both
     with `#[serde(default)]`; derive `Clone, PartialEq, Eq, Deserialize`
     for `BraveBackendConfig`, but implement `Debug` manually to mask the
     key (container/config Debug derives then safely delegate to it).
     Update the struct docs and `Default`; `build_backend()` maps the
     backend-specific config into the matching
     `WebSearchBackendSettings` variant instead of passing unrelated
     optional settings to the factory.
   - [ ] Re-export `BraveBackendConfig` next to `SearXngBackendConfig` in
     `src/config.rs:37`.
   - [ ] Fix the struct literals that gain the new field and the assertions
     that pin the supported-name list:
     `src/cli_driver.rs:1142` (`WebSearchBackendsConfig { … }`),
     `src/config/web_search.rs:468` (same), and the four
     `"duckduckgo, searxng"` occurrences at
     `backends/web_search.rs:314`, `backends/web_search.rs:349`,
     `src/config/web_search.rs:485`, `src/config/profile.rs:2436`.
   - [ ] Tests: extend `backend_kind_names_roundtrip` (brave roundtrip,
     `display_name() == "Brave"`, `supported_names_display() ==
     "duckduckgo, searxng, brave"`) and
     `backend_for_builds_the_configured_backend` (all backend-specific
     settings variants build the named backend; missing setting variants
     return `Err`; `Unsupported` names supported values); config tests for
     `build_backend()` gain a brave `Ok` case (config with `backend =
     "brave"` + key → Debug contains `BraveBackend` but not a sentinel
     token) and an `Err` pin (backend brave, no key → error naming
     `api_key`, no substituted backend), plus a containing `WebSearchConfig`
     Debug assertion proving the sentinel token is absent, and the nested
     `[backends.brave]` table deserialization test beside
     `nested_searxng_table_deserializes` (`src/config/web_search.rs:245`).
   - Tests: `cargo test --lib backends::web_search` and `cargo test --lib config::web_search`.

5. [ ] Validate the brave table at config load
   (`WebSearchConfig::validate` in `src/config/web_search.rs:115-124`
   currently delegates only to `validate_searxng_backend`):
   - [ ] Add `const BRAVE_API_KEY_KEY: &str = "[web_search.backends.brave] api_key";`
     beside `SEARXNG_BASE_URL_KEY` (`:36`).
   - [ ] Add `validate_brave_backend(path)` modeled on
     `validate_searxng_backend` (`:132-186`) but without URL parsing:
     selected + no table → `ConfigError::InvalidWebSearchBackendSetting`
     with key `[web_search.backends.brave]` and reason "the table is
     required when backend = \"brave\" selects it"; selected + table without
     `api_key` → key `[web_search.backends.brave] api_key`, reason "must
     hold the Brave Search API subscription token"; selected + blank/`""`
     value → same key, reason "must be a non-empty API key"; a table (or
     key) that is present but **unselected** validates nothing beyond
     presence — an empty string is tolerated there (mirror the searxng
     semantics documented on `WebSearchConfig::backends`).
   - [ ] Chain it from `validate()` after the existing searxng check.
   - [ ] Tests (matrix beside the searxng ones at `:289-400`): `brave`
     without table → error naming path + backend + key; without `api_key`;
     with empty and whitespace-only values; unselected brave table with
     empty `api_key` → validates; `duckduckgo` defaults untouched by a
     present brave table; `unknown_backend_is_rejected_naming_the_config_path`
     (`:260`) additionally asserts the message contains `brave`;
     `known_backend_validates` (`:287`) gains a brave case.
   - Tests: `cargo test --lib config::web_search`.

6. [ ] Profile-level config tests in `src/config/profile.rs` (the
   `[web_search]` block, `:2376-2496`), beside the searxng pair at
   `:2445-2489`:
   - [ ] A fixture with `backend = "brave"` and
     `[web_search.backends.brave] api_key = "…"` loads and asserts
     `profile.web_search.backends.brave.api_key`.
   - [ ] `backend = "brave"` without the table (and without `api_key`) fails
     at `profile.web_search.validate(&path)` with
     `InvalidWebSearchBackendSetting` naming the config path.
   - [ ] The existing absent-section → `duckduckgo` test and the
     `the_e2e_custom_profile_keeps_loading_with_target_rules` test (step 1)
     keep passing with the brave table present but unselected.
   - Tests: `cargo test --lib config::profile`.

7. [ ] Pin the user-facing wiring: title and sandbox target.
   - [ ] Extend (or sibling-add beside)
     `display_registry_titles_the_backend_the_profile_configures`
     (`src/cli_driver.rs:1137-1164`) with a brave profile —
     `backend = "brave"` plus `brave: Some(BraveBackendConfig { api_key: Some(…) })`
     — asserting `web_search.title() == "Web Search (Brave)"`.
   - [ ] Add a registry test beside
     `configured_web_search_rule_applies_through_default_registry`
     (`src/agent/tools.rs:1170`) whose `[tools.web_search]` rule carries
     `target = "https://api.search.brave.com"` and a brave-configured
     profile, proving a target-scoped `network_request` rule matches the
     brave endpoint's key (this is the `network_target()` contract made
     observable).
   - Tests: `cargo test --lib cli_driver` and `cargo test --lib agent::tools`.

8. [ ] Verify the e2e wiring end to end (files already in place; no secret
   values are printed or copied):
   - [ ] Confirm `tests/e2e/custom/profiles/default/config.toml` keeps the
     brave table it already carries and that `profiles/default/.env`
     (gitignored) defines `BRAVE_API_KEY` with the same spelling.
   - [ ] Document in the header comment of `tests/e2e/web_search-tool.zsh`
     how to run the script against Brave:
     `YLVA_WEB_SEARCH_BACKEND=brave` (with `BRAVE_API_KEY` present in the
     profile `.env`), beside the existing SearXNG invocation note. This
     script is run manually, never by a Make gate.
   - Tests: `cargo test --lib config::profile` (the fixture-load test
     covers the profile side); the zsh script itself is a manual check.

9. [ ] Documentation step — update **every** doc affected by this change, in
   the same change set (each sub-item is part of this step):
   - [ ] `README.md:34`: flip `- [ ] Brave` to `- [x] Brave`.
   - [ ] `docs/configuration.md`:
     - the `backend` row (`:396-400`) lists `"brave"` as a third value;
     - the per-backend table (`:402-406`) gains a `backends.brave.api_key`
       row: required — and non-empty — when `backend = "brave"` selects it,
       a secret, so `${ENV_VAR}` interpolation is the intended spelling;
       there is no automatic `BRAVE_API_KEY` environment fallback;
     - a TOML example selecting brave (beside the searxng example at
       `:417-432`), stating that a selected brave backend without the table
       or key is a hard config error naming path/backend/key/reason, that
       an unselected brave table is validated but otherwise unused, and
       that ylva ships a fixed Brave endpoint (no instance to run);
     - the network-rule match-key sentence (`:759-763`) gains
       `https://api.search.brave.com` for `brave`.
   - [ ] `docs/features.md:377-391`: the `web_search` bullet names `brave`
     (requiring `[web_search.backends.brave] api_key`) beside `duckduckgo`
     and `searxng`.
   - [ ] `docs/implementation-notes.md`:
     - the HTTP-client paragraph (`:164-193`) gains the Brave backend —
       one `GET` against `api.search.brave.com`, the
       `X-Subscription-Token` header, serde JSON parsing, dump behavior,
       and the redaction of that header;
     - the approval/`network_target` sentence (`:548-563`) notes that the
       brave target is the fixed `https://api.search.brave.com` authority.
   - [ ] Markdown added/changed here must pass `rumdl` with `.rumdl.toml`
     (run `rumdl fmt <paths>` to fix).
   - [ ] Update `docs/cli-options.md:115-124` to say that request-dump
     headers ending in `-token` are redacted case-insensitively, in addition
     to the existing authorization, cookie, and API-key-style redaction.
     No CLI option or special environment-variable handling is added;
     `BRAVE_API_KEY` is supplied through ordinary profile interpolation.
   - [ ] Explicitly **no** changes: `docs/ideas/web-search.md` (the user's
     brainstorm file — do not edit), `CONTEXT.md` (no glossary term exists
     for search backends and none is introduced), and
     `docs/architecture-design-records/` (directory is empty; the change
     follows the established seam pattern, so no new ADR is needed).
   - Tests: `rumdl check` on the touched markdown files.

10. [ ] Verification gate: `make format lint`, then the single mandatory
    test gate `make test-coverage` (full suite + coverage threshold 80).
    Fix all failures; do not add a separate `make test` run.

## Proposed Improvements

- Optional per-backend knobs in `[web_search.backends.brave]` —
  `country`, `search_lang`, `safesearch`, `freshness` — passed through as
  query parameters. Deferred: the API defaults (`US`/`en`/`moderate`) are
  reasonable and YAGNI applies until a user asks.
- An optional `base_url` override in the brave table for users behind a
  corporate API proxy. Deferred: the endpoint is fixed in the API contract
  and no user has asked for it.

## Consistency Issues

- `[high]` The working tree's e2e fixture
  (`tests/e2e/custom/profiles/default/config.toml:50-51`) already contains
  `[web_search.backends.brave] api_key = "${BRAVE_API_KEY}"`, but the
  profile-load test's environment map (`src/config/profile.rs:2288-2299`)
  has no `BRAVE_API_KEY`, so `${…}` interpolation fails the whole profile
  load with `MissingEnvironmentVariable` — the suite is red *before* any
  implementation work (verified by running the test). Step 1 restores the
  baseline; nothing else can be verified until it does.
- `[low]` `WebSearchBackendError::MissingBackendSetting`
  (`src/agent/tools/backends/web_search.rs:185-188`) hardcodes
  `web search backend 'searxng' requires {missing_key} to name the SearXNG
  instance` in its `thiserror` format string, so it cannot report Brave's
  missing `api_key` as written. Step 4 generalizes the variant; the only
  existing assertion on its text (`contains("base_url")`) survives.
- `[high]` Deriving `Debug` for `BraveBackendConfig` or `BraveBackend` would
  print the API key. `WebSearchConfig` and its containers derive `Debug`
  recursively, and the backend trait requires it; step 3 and step 4 must use
  custom redacting implementations with sentinel tests rather than copying
  SearXNG's key-free `Debug` derive.

## Implementation Hints

**Brave Search API** (source: `https://api-dashboard.search.brave.com/api-reference/web/search/get`,
retrieved 2026-10-01):

- Base URL `https://api.search.brave.com/res`, endpoint `GET /v1/web/search`.
  Reference cURL (token redacted):
  `curl "https://api.search.brave.com/res/v1/web/search?q=brave+search"`
  with headers `Accept: application/json`, `Accept-Encoding: gzip`, and
  `X-Subscription-Token: <YOUR_API_KEY>`.
- Query: `q` required (≤600 chars, ≤75 words, never empty); ylva passes
  `terms` unchanged and does not enforce those limits locally, so a 4xx
  response for an invalid query is surfaced using Brave's error envelope.
  `count`
  `1..=20` (default 20, "actual number delivered may be less than
  requested"); `offset` `0..=9` (irrelevant here: ylva's schema maximum of
  `WEB_SEARCH_MAX_RESULT_LIMIT = 20` equals the API's per-request `count`
  maximum, so one request always covers the full `limit` — no pagination);
  `result_filter` is a comma-delimited list (`web`, `news`, `videos`,
  `infobox`, `discussions`, `faq`, `query`, `summarizer`, `locations`) —
  passing `web` restricts the payload to web results and avoids
  plan-dependent extras; `text_decorations=false` suppresses highlight
  decoration markers in snippets; `safesearch` defaults to `moderate`.
- Auth header `X-Subscription-Token` (case-insensitive on the wire and in
  the redaction match). `Accept: application/json` is the default media
  type; `Accept-Encoding: gzip` matches the reference cURL (reqwest's
  `gzip` feature decodes before serde parsing).
- 200 body: top-level `type`, `query`, and per-vertical objects. Web
  results: `web.results[]` with `title` (string), `url` (string),
  `description` (optional string), `page_age` (optional), plus metadata
  ylva does not use. `web` itself is optional (`null`/absent).
- Error bodies for 400/401/403/404/422/429: `{ "type": …, "error": {
  "id": …, "status": <int>, "detail": <string?>, "code": <string> },
  "time": … }` — documented for 404/422/429; treat the same envelope as
  present on 400/401/403 and fall back to a plain `Status` when the body
  does not parse as one (proxies can answer plain text).
- No live token is needed for tests; the e2e profile `.env` supplies
  `BRAVE_API_KEY` for manual runs only (`.env` is gitignored at
  `.gitignore:28`).

**Seam facts** (`src/agent/tools/backends/web_search.rs`, 360 lines):

- Module declarations/exports `:38-42`; `SUPPORTED_NAMES` `:81-82`;
  `as_str` `:85-90`; `display_name` `:94-99`; `from_name` `:102-108`;
  `supported_names_display` joins `SUPPORTED_NAMES` `:111-113`;
  `WebSearchError` `:133-152` (with `StatusWithHint` `:142-146` whose doc
  comment names only SearXNG today); trait `:158-170`; error enum `:176-189`;
  `backend_for` `:197-222`.
- Match arms that must all learn the new variant: `as_str`,
  `display_name`, `from_name`, `backend_for` — plus `SUPPORTED_NAMES`
  (which drives both the unknown-backend config error and the
  `backend_for` unsupported-name error).
- Tests to extend: `backend_kind_names_roundtrip` `:294-316` (currently
  pins `from_name("startpage") == None` and
  `supported_names_display() == "duckduckgo, searxng"`),
  `every_supported_name_is_selectable_and_roundtrips` `:318-329` (loops
  `SUPPORTED_NAMES` — automatically covers `brave` once added),
  `backend_for_builds_the_configured_backend` `:331-359`.
- Every `backend_for` call site: `src/config/web_search.rs:206` and the four
  seam-test calls at `:333`, `:337`, `:345`, `:353` (nothing in `tests/`);
  the factory accepts `WebSearchBackendSettings` variants so each backend's
  setting stays attached to its own variant.
- `WebSearchBackendKind` name/display constants exist per backend
  (`DUCKDUCKGO_NAME`, `SEARXNG_NAME`, …) — Brave gets the same pair; the
  invariant is pinned by `every_supported_name_is_selectable_and_roundtrips`.
- Constants: `WEB_SEARCH_DEFAULT_RESULT_LIMIT = 15`,
  `WEB_SEARCH_MAX_RESULT_LIMIT = 20`, `WEB_SEARCH_SNIPPET_MAX_CHARS = 300`.

**Backend templates:**

- `src/agent/tools/backends/web_search/searxng.rs` (616 lines) is the
  closest analogue (JSON + serde DTOs): struct/constructors `:54-106`,
  `search()` request/dump/status flow `:109-184`, DTOs `:189-216`,
  `parse_*_results` `:225-271`, `normalize_whitespace` at the module
  bottom, mock-server test helpers `:288-347`, wire-shape test `:349-430`,
  fixture constant `:432-446`, failure-matrix tests `:451-606`.
- `duckduckgo.rs` (609 lines) adds the zero-limit short-circuit test
  `:596-608` and `network_target` test `:570-577`.
- Request/dump flow to copy verbatim (searxng `:123-164`): build request →
  `RequestDumpContext::new(&self.dump_source)` → `request_path()` →
  `dump_request(url, &redacted_headers_json(request.headers()), body, path)`
  → `execute` (on transport error: `dump_response_transport_error` + `WebSearchError::Request`)
  → read `content_type` → `response_path_for_content_type` → buffer bytes →
  `dump_buffered_response` → status check → parse. Never call
  `crate::http::post_with_retry` (retry is LLM-only).
- `crate::http::get_json`/`post_json` are **not** reusable here: they send
  `Authorization: Bearer`, while Brave requires `X-Subscription-Token`
  (build the request directly as `searxng.rs` does).
- Client-level default headers are not part of the per-request `HeaderMap`
  that `dump_request` records (`docs/implementation-notes.md`), so headers
  must be set on the request itself to appear in dumps.

**Config facts:**

- `src/config/web_search.rs`: `WebSearchConfig` `:56-78` (every field has an
  explicit `#[serde(default)]` + default fn), `WebSearchBackendsConfig`
  `:82-88`, `SearXngBackendConfig` `:91-98`, `Default` `:100-109`,
  `validate` `:115-124`, `validate_searxng_backend` `:132-186` (the
  selected-vs-present validation template: selected ⇒ required, non-empty,
  and well-formed; unselected ⇒ presence tolerated, empty tolerated),
  `http_timeout` `:190-192` (floors at 1 s), `build_backend` `:200-207`,
  `effective_result_limit` `:211-213` (clamps `1..=20`).
- Reuse `ConfigError::InvalidWebSearchBackendSetting { path, backend, key, reason }`
  (`src/config.rs:184-193`) for every brave validation failure — it already
  names path, backend, key, and reason; no new `ConfigError` variant is
  needed. Re-export the new config type at `src/config.rs:37`.
- Validation is invoked from `src/config/profile.rs:440`
  (`profile.web_search.validate(&path)?`).
- Project convention: no `deny_unknown_fields` anywhere — unknown tables are
  silently ignored, which is why the unimplemented `[web_search.backends.brave]`
  table in the e2e fixture never failed before this change.
- `${ENV_VAR}` interpolation runs over **every** TOML string
  (`src/config/profile.rs:659-726`) after process env + profile `.env` are
  merged; a missing variable is a hard `MissingEnvironmentVariable` error —
  this is the mechanism behind the step 1 baseline failure. Documented at
  `docs/configuration.md:136-141` ("expanded secret values are never
  logged").
- Struct literals that break when `brave` is added to
  `WebSearchBackendsConfig`: `src/cli_driver.rs:1142-1146` and
  `src/config/web_search.rs:468-469` (both enumerate fields explicitly).
- Assertions pinning the supported-name list (all four must gain `brave`):
  `src/agent/tools/backends/web_search.rs:314` and `:349`,
  `src/config/web_search.rs:485`, `src/config/profile.rs:2436`.

**Tool/registry wiring (expected unchanged):**

- `WebSearchTool::new` derives `backend_display_name` from
  `WebSearchBackendKind::from_name(&config.backend)` (`src/agent/tools/web_search.rs:344-346`),
  so `display_name() == "Brave"` yields the title `Web Search (Brave)`
  with no tool changes; `title()` is `:366-372`, `needs_approval` uses
  `backend.network_target()` against `SandboxContext::check_network_request`
  `:400-404`.
- `default_registry` builds with `config.web_search.build_backend()` and
  `.expect(...)` (`src/agent/tools.rs:783-796`) — the
  fallible-construction invariant is already in place; the CLI display
  registry must receive the profile's
  `[web_search]` section (already wired via `display_registry_config`, pinned
  by `cli_driver.rs:1137`).
- The model-facing description is deliberately backend-agnostic and a test
  (`src/agent/tools/web_search.rs:789-817`) asserts no backend name appears
  in it — do not add "Brave" to `WEB_SEARCH_DESCRIPTION`.
- Sandbox match key for brave: `network_target()` on
  `https://api.search.brave.com` → `https://api.search.brave.com`
  (`crate::http::network_target` returns `scheme://host[:port]`, http/https
  only).

**Redaction facts** (`src/http.rs`):

- `SENSITIVE_HEADER_NAMES` `:413-420` currently lists `authorization`,
  `proxy-authorization`, `x-api-key`, `api-key`, `cookie`, and `set-cookie`;
  `is_sensitive_header_name` `:429` currently matches those names
  case-insensitively; names ending in `-token` are not currently matched
  unless they are also in the explicit list. Matching names are replaced
  with `<REDACTED>` by `redacted_headers_json` `:460-476`;
  `dump_request` `:486+` writes `scheme`, `host`, `path`, **`query`**, and
  the redacted `headers` projection — hence header auth, never an `api_key`
  query param.
- Existing redaction tests to extend: `dump_request_redacts_sensitive_header_values`
  (`src/http.rs:1087`, asserts `authorization`/`x-api-key` are
  `<REDACTED>` and the raw values appear nowhere in the file) and the
  header-projection test around `:1061`.

**Testing conventions:**

- No `wiremock`/`mockito`/`url` crate in `Cargo.toml` — mock HTTP is a raw
  `tokio::net::TcpListener` one-shot server in the test module (patterns in
  `duckduckgo.rs:283-416`, `searxng.rs:288-347`, `tests/web_fetch_backend.rs`).
  `serde_json` (with `preserve_order`), `reqwest 0.12` (`json`, `stream`,
  `gzip`), `thiserror 1`, `tempfile`, `flate2` are already dependencies.
- `cfg(test)` does not reach library code from integration tests, and
  network side effects must be constructor-injected — hence the
  `with_api_key_and_dump_path` test hook and no process-env mutation
  (`docs/known-test-issues.md`).
- Tests touching `tracing` must hold the subscriber lock
  (`src/test_support.rs::run_locked` / `run_locked_paused`); the backend
  test modules use plain `#[tokio::test]`-style runtimes like their
  siblings.
- No new `tests/*.rs` integration test is planned: neither existing backend
  has one, the seam is public via `src/lib.rs:15` anyway, and in-module unit
  tests carry the coverage (Testing Pyramid — keep the unit tier first).
- Secret-bearing `BraveBackendConfig` and `BraveBackend` must use custom
  redacting `Debug` implementations; tests must prove a sentinel key is
  absent both from the backend Debug output and from a containing
  `WebSearchConfig` Debug output. Other config containers may keep derived
  Debug because it delegates to the redacting leaf implementation.
- Coverage threshold: `Makefile` `COVERAGE_THRESHOLD := 80`; the mandatory
  gate is `make test-coverage` (do not run `make test` as a duplicate).

**Docs map (exact anchors):**

- `README.md:31-34` — the `web_search` backend checklist (`Brave` unchecked
  at `:34`).
- `docs/configuration.md:391-462` — "Web search configuration": field table
  `:396-400`, per-backend table `:402-406`, default TOML `:408-415`,
  searxng example + hard-error semantics `:417-432`, SearXNG instance
  sub-section `:434-462`; network-rule match keys `:759-763`.
- `docs/features.md:377-391` — the `web_search` tool bullet.
- `docs/implementation-notes.md:164-193` — HTTP client paragraph (one
  sentence per backend); `:495-506` — the seam paragraph (no per-backend
  list, likely untouched); `:548-563` — `network_target` per-backend
  sentence; `:711-715` — CLI driver backend-title note (no change needed).
- `docs/cli-options.md:115-124` — request-dump header redaction behavior
  (update to document the `*-token` suffix rule); `:28`
  (`--auto-approve-web-search`) is unchanged.

**Working-tree state:**

- Modified but unrelated: `Cargo.toml` (version bump `0.10.13 → 0.10.14`) —
  leave it alone; do not stage or revert it as part of this plan.
- Modified and in scope: `tests/e2e/custom/profiles/default/config.toml`
  (already carries the brave table at `:50-51`; backend selection stays
  `${YLVA_WEB_SEARCH_BACKEND}`, whose `.env` value is `searxng` — the brave
  table is present-but-unselected, which config validation tolerates).
- `tests/e2e/custom/profiles/default/.env` (gitignored, never printed)
  already defines `BRAVE_API_KEY`.
- `docs/plans/` currently holds only `.gitkeep`; this file becomes its
  first entry. The user will handle this plan file's retention/removal after
  implementation; the implementing agent must not delete it.
- `docs/architecture-design-records/` contains only `.gitkeep` — no ADR to
  reconcile against.
