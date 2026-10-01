# Add a SearXNG backend for the `web_search` tool

An implementing agent must flip `[ ]` to `[x]` on each step (and its
sub-steps) immediately after completing it and rewrite this file in place, so
progress is resumable across sessions.

## Context

Ylva's `web_search` agent tool searches the web through the injectable
`WebSearchBackend` seam (`src/agent/tools/backends/web_search.rs`) with a
single implementation today: a DuckDuckGo HTML-endpoint backend selected by
`[web_search] backend = "duckduckgo"` (the default). This plan adds SearXNG
as a second backend, selected by

```toml
[web_search]
backend = "searxng"

[web_search.backends.searxng]
base_url = "${SEARXNG_BASE_URL}"   # any instance URL; no default is shipped
```

Motivation: DuckDuckGo's endpoint is a scraped form-POST with bot detection
and no instance/engine control. SearXNG aggregates many engines behind a
documented JSON API, is self-hostable, and needs no API keys — users point
ylva at their own instance (or any instance that enables the JSON format).

**Decisions settled by grilling (binding):**

- `[web_search] backend` stays the single selector; per-backend settings live
  in nested `[web_search.backends.<name>]` tables (today only `searxng`).
- `base_url` is **required** when `searxng` is selected; no default instance
  is shipped. Only key in the table for now (`language`/`safesearch`/
  `categories` deferred).
- Unknown keys/tables are ignored (project-wide convention: no
  `deny_unknown_fields` anywhere in `src/config.rs`).
- Values of a *present* but unselected table are validated; the table's
  presence is only *required* for the selected backend.
- The backend issues **POST** `{base_url}/search` with form `q` +
  `format=json` (never GET with `format` in the query string).
- **No `#[ignore]`d live test** — live verification happens through the e2e
  setup (`tests/e2e/`).
- Docs use a neutral placeholder example (`https://searx.example.org`) plus a
  short "run your own" Docker snippet — never a real third-party instance.
- `docs/ideas/web-search.md` is updated separately by the user; do not edit
  it.

**Spike record (verified 2026-10-01):**

- SearXNG search API: `GET|POST /search`, `format=json` must be enabled in
  the instance's `settings.yml` (`search.formats`) or it answers `403`;
  response top-level keys `query`, `results`, `answers`, `corrections`,
  `infoboxes`, `suggestions`, `unresponsive_engines`; app errors arrive as
  `{"error": "…"}` with `application/json`.
- `https://searx.tiekoetter.com` → `403` (JSON format disabled) — unusable.
- `https://etsi.me` (public) and `https://searxng.operation-mindshift.de`
  (user-hosted) → `200 application/json`, verified with GET **and** POST.
- Bare `curl` gets `429` from default botdetection (missing
  `Accept-Encoding`/`Accept-Language`) — this, not IP blocklists, caused the
  identical `429`s observed across many public instances.
- A local Docker spike instance returned 27 results; the container was
  removed afterwards (image cache may remain).
- Upstream engines degrade per-instance over time (the user-hosted instance:
  `brave` rate-limited, `duckduckgo`/`startpage` CAPTCHA, `google cse`
  alive) — a documented reality of key-free search, not something ylva can
  fix.

Acceptance criteria:

- The config above loads and validates; selecting `searxng` without the
  table, without `base_url`, or with a non-`http(s)`/unparsable URL is a hard
  config error naming the config path, the backend, the key, and the reason.
- An unknown `backend` value is rejected naming both accepted values
  (`duckduckgo, searxng`).
- The backend performs one `POST {base_url}/search` (form `q`, `format=json`)
  with the required headers (see *Implementation Hints*), maps `results[]` to
  `WebSearchHit` (truncate to `limit`, `content` → whitespace-normalized
  `snippet`), and honors the fail-fast policy:
  - transport error → `WebSearchError::Request`
  - non-2xx → `WebSearchError::Status`, except `403` → a new hint-carrying
    variant stating the instance likely has `format=json` disabled
  - non-JSON body / JSON `{"error": …}` → `WebSearchError::Parse`
  - empty `results` **with** non-empty `unresponsive_engines` →
    `WebSearchError::Parse` naming the engines; empty `results` with no
    engine failures → `Ok(vec![])`
  - never a retry, never a silently-empty result set on a failed response.
- `network_target()` returns the configured instance's `scheme://host` so
  `[tools.web_search] network_request` rules can target it.
- Backend construction never silently falls back: `backend_for` and
  `WebSearchConfig::build_backend` return `Result`, and `default_registry`
  converts a construction error into an explicit validated-config `expect`
  — no code path can substitute DuckDuckGo for a configured SearXNG backend.
- The user-facing tool title becomes `Web Search (SearXNG)`; the
  model-facing description stays backend-agnostic (no tool changes needed).
- DuckDuckGo remains the default; an absent `[web_search]` table behaves
  exactly as before.
- Tests never touch the network: mock TCP server + fixtures only; live
  verification via the e2e setup.
- Docs affected by the change are updated in the same change set:
  `docs/configuration.md`, `docs/features.md`,
  `docs/implementation-notes.md` (and the e2e files under `tests/e2e/`).
- `make format lint` and `make test-coverage` pass (coverage threshold 80).

Key constraints:

- Fail-fast backend policy (module docs of the seam): no retries, no silent
  empty results on unrecognized/blocked responses.
- The `WebSearchBackend` trait must stay `dyn`-compatible (boxed futures).
- Network side effects must be constructor-injected (`docs/known-test-issues.md`).
- Full descriptive names, no abbreviations (`SearXngBackend`, `base_url`).
- Docs are part of the change set; a source change that leaves a doc stale is
  incomplete (`AGENTS.md`).

## Implementation Plan

1. [ ] Implement the backend module `src/agent/tools/backends/web_search/searxng.rs`
   and declare it in the seam (`pub mod searxng;` + `pub use searxng::SearXngBackend;`
   next to the `duckduckgo` declarations in `backends/web_search.rs`):
   - [ ] `SearXngBackend { client, base_url, dump_source }` mirroring
     `duckduckgo.rs`: constructors `with_base_url(base_url, http_timeout)`,
     `with_base_url_and_dump_path(…)`, private
     `with_base_url_and_dump_source(…)` building a `reqwest::Client` with
     `OUTBOUND_USER_AGENT` and the configured timeout; trailing slashes
     trimmed; **no** `DEFAULT_BASE_URL`.
   - [ ] `search()`: short-circuit `Ok(Vec::new())` on `limit == 0`; `POST {base_url}/search`
     with form fields `q` = terms and `format` = `json` (nothing in the query
     string); send `Accept: */*`, `Accept-Language: en-US,en;q=0.9`, and
     `Accept-Encoding: gzip` explicitly; dump request/response through
     `RequestDumpContext` exactly like `duckduckgo.rs` (no retry).
   - [ ] Response handling: non-2xx → `Status`, `403` → new
     `WebSearchError` variant carrying a hint ("the instance likely has
     format=json disabled in its settings.yml"); parse JSON with private
     serde DTOs (`results[].url/title/content`, `unresponsive_engines` as
     pairs, top-level `error`); JSON `{"error": …}` → `Parse` including the
     server message; empty results + non-empty `unresponsive_engines` →
     `Parse` naming each engine and reason; otherwise map hits (skip entries
     with empty `title`/`url`, mirror DDG's entry skipping), truncate to
     `limit`, normalize `snippet` whitespace but do not truncate it (the tool
     already caps snippets at `WEB_SEARCH_SNIPPET_MAX_CHARS`).
   - [ ] `network_target()` = `crate::http::network_target(&self.base_url)`
     with raw-`base_url` fallback (same expression as `duckduckgo.rs:103-105`).
   - [ ] Module docs state the fail-fast policy, the POST rationale (SearXNG's
     limiter counts only query-string `format`), and the required-header set.

2. [ ] Wire selection and config (one atomic change — `backend_for`'s
   signature moves):
   - [ ] In `backends/web_search.rs`: add `WebSearchBackendKind::SearXng`;
     extend `SUPPORTED_NAMES` to `&["duckduckgo", "searxng"]`, `as_str`
     (`"searxng"`), `display_name` (`"SearXNG"`), and the `from_name` match.
   - [ ] Change `backend_for` to
     `backend_for(backend_name: &str, base_url: Option<&str>, http_timeout: Duration) -> Result<Arc<dyn WebSearchBackend>, WebSearchBackendError>`:
     `duckduckgo` → `DuckDuckGoBackend::new`; `searxng` + `Some(base_url)` →
     `SearXngBackend::with_base_url`; unknown name → `Err` variant naming the
     supported names; `searxng` without `base_url` → `Err` variant naming the
     missing key. Define the `thiserror` enum `WebSearchBackendError` in the
     seam and **delete** the existing `tracing::error!` +
     DuckDuckGo-fallback arm — construction only ever yields the configured
     backend or an error (see step 3).
   - [ ] In `src/config/web_search.rs`: add
     `backends: WebSearchBackendsConfig` (nested structs
     `WebSearchBackendsConfig { searxng: Option<SearXngBackendConfig> }`,
     `SearXngBackendConfig { base_url: Option<String> }`), all with
     `#[serde(default)]` and `Debug, Clone, PartialEq, Eq, Deserialize`;
     update `Default`, the struct docs, and the section docs.
   - [ ] Extend `validate()`: backend name check first (existing); then
     selected-`searxng` requires the table and a present, non-empty
     `base_url` that parses via `reqwest::Url::parse` with scheme
     `http`/`https`; a `base_url` that *is* present (selected or not) must
     parse the same way (empty string while unselected is allowed); add a
     `ConfigError` variant beside `InvalidWebSearchBackend` in
     `src/config.rs` naming path, backend, key, and reason.
   - [ ] `build_backend()` passes `self.backends.searxng.and_then(|table| table.base_url)`
     into `backend_for` and returns its `Result` unchanged; update its doc
     comment (an `Err` here is only reachable when `validate()` was skipped —
     profile load always validates first).

3. [ ] Make construction failure loud at the registry — this step removes the
   consistency issue noted for this plan (silent DuckDuckGo substitution):
   - [ ] In `src/agent/tools.rs` (`default_registry`, the `build_backend()`
     call around line 783): handle the new `Result` with
     `.expect("web search backend builds from a profile config validated at load")`
     and state that invariant in the surrounding comment. `default_registry`
     keeps its infallible `-> ToolRegistry` signature (its ~21 test call
     sites stay untouched); `web_fetch::backend_for` keeps its native
     fallback — untouched, a separate concern outside this change.
   - [ ] Update the seam module docs (`src/agent/tools/backends/web_search.rs`
     header) so they no longer describe selection-by-name with a fallback:
     `backend_for` returns the configured backend or an error.
   - [ ] Pin the fix behaviorally in `src/config/web_search.rs` tests: a
     hand-built invalid state (e.g. `backend = "searxng"` with no
     `backends.searxng.base_url`) makes `build_backend()` return `Err` —
     never an `Ok` holding a different backend.

4. [ ] Tests for selection and config:
   - [ ] Seam tests in `backends/web_search.rs`:
     `backend_kind_names_roundtrip` gains the `searxng` roundtrip, the
     `display_name` assertion, and `supported_names_display() ==
     "duckduckgo, searxng"` (replacing today's `from_name("searxng") == None`
      assertion); `backend_for_builds_the_configured_backend` covers the new
      `Result` signature: `duckduckgo` and `searxng` + `Some(url)` → `Ok`
      (`Debug` contains `DuckDuckGoBackend` / `SearXngBackend`); unknown name
      and `searxng` + `None` → `Err` — no `Ok` ever carries a substituted
      backend.
   - [ ] Config tests in `src/config/web_search.rs`: defaults unchanged
     (`backend == "duckduckgo"`, `backends.searxng == None`); absent section
     ≡ default; explicit nested table parses; `searxng` without table → error
     naming path + key; `searxng` with missing/empty/invalid/non-http
     `base_url` → error naming path + reason; unselected `searxng` table with
     invalid URL → error; unselected with valid URL → validates;
     `duckduckgo` untouched by table presence; unknown-backend message lists
      both names; `build_backend()` `Ok` cases (default config; valid searxng
      config → `Debug` contains `SearXngBackend`) — the `Err` pin for
      invalid construction states lives in step 3.

5. [ ] Unit tests in `searxng.rs`, following the `duckduckgo.rs` test module:
   - [ ] Local raw `tokio::net::TcpListener` mock server asserting the wire
     shape: request line `POST /search HTTP/1.1`, **empty query string**,
     form body exactly `format=json&q=<terms>` (or `q`-first — assert the
     actual encoding produced by `.form(&[…])`), headers `user-agent`
     `ylva/*`, `accept: */*`, non-empty `accept-language`,
     `accept-encoding` containing gzip, `content-type:
     application/x-www-form-urlencoded`; plus request/response dump
     assertions (request dump `path == "/search"`, response dump `.json`
     extension).
   - [ ] Fixture-JSON parse tests: hits mapping (title/url/snippet),
     missing/whitespace `content` → empty snippet, entries with empty
     title/url skipped, `limit` capping, valid empty `results` → `Ok(vec![])`.
   - [ ] Failure tests: `403` → hint-carrying error mentioning `format=json`;
     `429` → `Status`; `200` with HTML body → `Parse`; JSON `{"error": …}` →
     `Parse` carrying the message; empty results + `unresponsive_engines` →
     `Parse` naming the engines.
   - [ ] `network_target()` (custom base URL with path/trailing slash →
     authority) and the zero-limit short-circuit.
   - [ ] **No** `#[ignore]`d live test (settled: e2e covers live checks).

6. [ ] Profile-level config tests in `src/config/profile.rs` (the
   `[web_search]` block around lines 2362-2421): a `searxng` section with
   `base_url` loads into the profile; `backend = "searxng"` without the
   table/`base_url` fails at `profile.web_search.validate(&path)` with the
   new error naming the config path; the existing absent-section →
   `duckduckgo` test keeps passing.

7. [ ] Complete the e2e wiring (two files are already modified in the working
   tree — finish and verify them, keep their env-var spelling):
   - [ ] `tests/e2e/custom/profiles/default/config.toml` (WIP lines 44-48)
     uses `backend = "${YLVA_WEB_SEARCH_BACKEND}"` and
     `[web_search.backends.searxng] base_url = "${SEARXNG_BASE_URL}"`;
     `tests/e2e/custom/profiles/default/.env` defines both keys (added).
     Because `${…}` interpolation runs on every string at load, both keys
     must always be defined or the whole profile fails — ensure the defaults
     are `YLVA_WEB_SEARCH_BACKEND=duckduckgo` and a syntactically valid
     placeholder URL (e.g. `http://localhost:20081`) that passes validation
     while `duckduckgo` is selected.
   - [ ] Verify `tests/e2e/web_search-tool.zsh` still runs with the defaults;
     document (in the script header or `docs/` where the e2e scripts are
     described) how to run it against SearXNG:
     `YLVA_WEB_SEARCH_BACKEND=searxng SEARXNG_BASE_URL=<instance-url>`
     pointing at a user-provided instance.

8. [ ] Update the docs affected by the change (same change set):
   - [ ] `docs/configuration.md`: the `[web_search]` table — `backend` row
     now lists both values; add the nested `[web_search.backends.searxng]`
     table (row or sub-section) with `base_url`: required for `searxng`,
     ignored by `duckduckgo`, `${ENV_VAR}` interpolation usable; TOML example
     for the searxng variant plus a short "run your own" Docker snippet whose
     config enables `formats: [html, json]` (JSON is off by default; disabled
     ⇒ HTTP `403`); a brief note on instance-side limiter realities (default
     budgets pace ylva fine; `pass_ip`/`trusted_proxies` are the admin's
     levers); network-rule `target` example around line 708 gains the
     `https://searx.example.org`-style authority form.
   - [ ] `docs/features.md` (`web_search` bullet, lines 377-389): name both
     backends and the `base_url` requirement.
   - [ ] `docs/implementation-notes.md`: the HTTP-client paragraph at lines
     164-184 (currently DuckDuckGo-specific) gains the SearXNG backend —
     form POST, required headers, serde JSON parsing, dump behavior; the
     `network_target` sentence at line ~543 notes that the SearXNG target
     derives from the configured instance URL.
   - [ ] Do **not** edit `docs/ideas/web-search.md` (owned by the user).

9. [ ] Verification gate: `make format lint`, then the single mandatory test
   gate `make test-coverage` (full suite + coverage threshold 80). Markdown
   added/changed in step 8 must pass `rumdl` with `.rumdl.toml` (included in
   the format/lint targets; fix with `rumdl fmt <paths>`). Fix all failures;
   do not add a separate `make test` run.

## Proposed Improvements

- Optional `language` / `safesearch` / `categories` keys in the searxng
  table, passed through as form fields.
- Multi-page fetching (`pageno=2…`) when one page returns fewer than `limit`
  hits — only if the shortfall shows up in practice.
- Additional backends (API-key based ones would need credentials in the
  per-backend tables — the nested-table design already accommodates them).

## Implementation Hints

**SearXNG search API** (docs `docs.searxng.org/dev/search_api.html`; source
`searx/webutils.py::get_json_response`, `searx/webapp.py:552-553`):

- Endpoints `GET|POST /` and `GET|POST /search`; parameters `q` (required),
  `format` (`json` must be listed in `search.formats` of `settings.yml`,
  else HTTP `403` — default installs only enable `html`), `pageno`,
  `language`, `categories`, `time_range`, `safesearch`. There is no
  result-count parameter: one request returns one page; `limit` is an upper
  bound.
- JSON body: `query`, `results[]`, `answers`, `corrections`, `infoboxes`,
  `suggestions`, `unresponsive_engines` (array of `[engine, reason]`
  pairs). Each result carries `url`, `title`, `content` (may be absent) plus
  engine metadata; datetimes serialize ISO-8601. App-level failures arrive
  as `{"error": "<message>"}` with `application/json`.

**Limiter/botdetection facts that shape the request** (source:
`searx/limiter.py`, `searx/botdetection/*`):

- On `/search` the check order is `http_accept` (requires `text/html` ∈
  Accept — `*/*` passes), `http_accept_encoding` (requires `gzip` or
  `deflate`), `http_accept_language` (requires a non-empty value),
  `http_user_agent` (rejects a regex of known bots incl. `curl`,
  `python-requests`, `Go-http-client`, and a *missing* UA — `ylva/<version>`
  passes), `http_sec_fetch` (only evaluated when the UA looks like a modern
  browser — a non-browser UA skips it; browser UAs without valid
  `Sec-Fetch-*` get a `302` to `/`), `ip_limit`. All requests also reject
  `Connection: close`.
- `ip_limit` sliding windows (hardcoded constants, not configurable via
  `limiter.toml`): `BURST_MAX` 15/20 s, `LONG_MAX` 150/600 s, and
  `API_MAX` **4 per 3600 s** for requests whose `format` appears in the
  **query string** (`request.args`). A form-encoded `format=json` (POST) is
  not counted against `API_MAX` — this is why the backend uses POST; ylva's
  5 s tool throttle fits inside BURST/LONG. Instance admins can only
  whitelist (`pass_ip`) or disable the limiter.
- Header set to send: UA `OUTBOUND_USER_AGENT` (client default), explicit
  `Accept: */*`, `Accept-Language: en-US,en;q=0.9`, `Accept-Encoding: gzip`
  (mirror `duckduckgo.rs:131`; reqwest's gzip feature decodes the body).

**Spike results (2026-10-01):**

- `https://searx.tiekoetter.com/search?...&format=json` → `403` (format
  disabled) — do not use it anywhere in code or docs.
- `https://etsi.me` → `200 application/json`; user-hosted
  `https://searxng.operation-mindshift.de` → `200` for GET with proper
  headers **and** for POST with form `format=json`.
- Bare curl (no `Accept-Encoding`/`Accept-Language`) → identical 17-byte
  `429 Too Many Requests` from essentially every instance — header probes,
  not IP bans.
- Local Docker instance (`formats` enabled, `limiter` off by default): 27
  results for `rust async await`; `unresponsive_engines: [["brave", "too many
  requests"]]`. Container `ylva-searxng-spike` was removed after the spike;
  `searxng/searxng` image may still be in the local image cache.

**Seam and tool facts:**

- Seam: `src/agent/tools/backends/web_search.rs` — trait at :135-147
  (`search` → `BoxFuture`, `network_target`), `WebSearchHit` {title, url,
  snippet} at :101-110, `WebSearchError` at :115-129, `WebSearchBackendKind`
  at :54-96 (`SUPPORTED_NAMES` / `as_str` / `display_name` / `from_name` with
  an `unreachable!` arm — all four gain the variant), `backend_for` at
  :154-166 (gains the `base_url` parameter and becomes fallible —
  `Result<Arc<dyn WebSearchBackend>, WebSearchBackendError>` with a new
  `thiserror` enum in the seam; its error-log-and-DuckDuckGo-fallback arm is
  deleted), constants `WEB_SEARCH_DEFAULT_RESULT_LIMIT = 15`,
  `WEB_SEARCH_MAX_RESULT_LIMIT = 20`, `WEB_SEARCH_SNIPPET_MAX_CHARS = 300`.
- Test assertions to change: `backend_kind_names_roundtrip`
  (`backends/web_search.rs:239-254`) currently asserts
  `from_name("searxng") == None` and `supported_names_display() ==
  "duckduckgo"`; `backend_for_builds_the_configured_backend` (:257-263)
  passes only a `Duration`.
- `WebSearchError` gains one variant for the `403` hint, e.g.
  `#[error("web search backend responded with HTTP {status}: {hint}")]
  StatusWithHint { status: u16, hint: String }` — name/wording at
  implementer's discretion, but the `403` message must mention that
  `format=json` is likely disabled on the instance.
- Construction call sites (the full ripple of the fallible change):
  `build_backend()` is called from `src/agent/tools.rs:783` (inside
  `default_registry`, which returns `ToolRegistry` infallibly —
  `tools.rs:711-714`, ~21 test call sites, hence the invariant `expect`
  instead of a `Result` signature) and one config test;
  `web_search::backend_for` only from `build_backend` and the seam tests.
  `web_fetch::backend_for` (`backends/web_fetch.rs:341-353`) has the same
  error-log-and-fallback shape but is intentionally out of scope.
- Backend template: `src/agent/tools/backends/web_search/duckduckgo.rs` —
  constructor chain (:41-100), request/dump flow (:107-182: build →
  `RequestDumpContext::new` → `dump_request` → execute →
  `dump_response_transport_error` on transport failure → buffer →
  `dump_buffered_response` → status check → parse), `network_target` (:103-105),
  `normalize_whitespace` (:274-276), mock TCP server test (:283-416), fixture
  constants (:418-473), zero-limit test (:596-608).
- Tool wiring needs no changes: `WebSearchTool::new` derives the display name
  from `WebSearchBackendKind::from_name` (`src/agent/tools/web_search.rs:344-346`),
  so `display_name() == "SearXNG"` yields the title `Web Search (SearXNG)`;
  the backend-agnostic description test (:788-817) uses the default config
  and stays valid; approval uses `backend.network_target()` against
  `SandboxContext::check_network_request`.

**Config facts:**

- `src/config/web_search.rs`: struct at :53-70 (all fields defaulted,
  derives `Debug, Clone, PartialEq, Eq, Deserialize`), `validate()` :86-95,
  `http_timeout()` floors at 1 s (:99-101), `build_backend()` :104-106,
  `effective_result_limit()` clamps `1..=20`. New error variant belongs next
  to `ConfigError::InvalidWebSearchBackend` (`src/config.rs:175-183`;
  sibling `InvalidWebFetchBackend` :184-192); validation is invoked from
  `src/config/profile.rs:440`.
- `Option` fields still need `#[serde(default)]` for absent keys to parse.
- `${ENV_VAR}` interpolation in profile TOML: every string value is
  interpolated (`src/config/profile.rs:659-726`) from process env + profile
  `.env` (dotenvy, `src/config.rs:244-299`); a missing variable is a hard
  error (`MissingEnvironmentVariable`), malformed refs too
  (`InvalidEnvironmentReference`); documented at
  `docs/configuration.md:136-141`.
- URL parsing precedent: `reqwest::Url::parse` (used in `duckduckgo.rs:262`);
  `url` is not a direct `Cargo.toml` dependency. Endpoint-key vocabulary in
  docs: `api_base_url` (`docs/configuration.md:71,75,340`).

**E2E facts:**

- `tests/e2e/custom/.envrc` exports `YLVA_HOME=$(PWD)` (profile selection);
  `tests/e2e/custom/profiles/default/config.toml` WIP lines 44-48 add the
  `[web_search]` / `[web_search.backends.searxng]` tables using
  `${YLVA_WEB_SEARCH_BACKEND}` and `${SEARXNG_BASE_URL}` (keep the
  spelling consistent between `config.toml` and `.env`);
  `profiles/default/.env` defines both keys (user-added — do not print or
  commit secret-looking values beyond what's already there).
- The zsh e2e scripts (e.g. `tests/e2e/web_search-tool.zsh`) are run
  manually, not by a Makefile gate; `make test-coverage` is the mandatory
  gate.

**Conventions and verification:**

- File-per-module (`searxng.rs`, never `searxng/mod.rs`); type name
  `SearXngBackend` (clippy acronym lint), display name `SearXNG`, config name
  `searxng`, tracing field `backend = "searxng"`.
- `OUTBOUND_USER_AGENT = "ylva/<version>"` lives once at `src/http.rs:30`;
  dumps via `RequestDumpSource`/`YLVA_REQUEST_DUMP_PATH`
  (`docs/cli-options.md:113`); no retry helpers on this path.
- Docs to touch: `docs/configuration.md:391-409` (table + example),
  `:707-708` (network `target` example), `docs/features.md:377-389`,
  `docs/implementation-notes.md:164-184` and `:543`. No `CONTEXT.md`,
  `README.md`, `docs/cli-options.md`, or ADR updates (no glossary entry,
  README already checks `web_search` off, ADR directory empty and the change
  follows the existing seam pattern).
- Verification: `make format lint` then `make test-coverage`
  (`Makefile:12`: `COVERAGE_THRESHOLD := 80`); markdown via `rumdl` with
  `.rumdl.toml`.
