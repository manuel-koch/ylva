# Plan: `web_fetch` tool (native HTTP GET backend)

Implementing agent: flip `[ ]` to `[x]` immediately after completing each step and
rewrite this file in place, so progress is resumable across sessions.

## Context

Ylva (Rust TUI/CLI agent-harness) exposes LLM-callable tools through a
`Tool`/`ToolCall`/`ToolCallResult` trait set and a `ToolRegistry`. The idea
`docs/ideas/web-fetch.md` asks for a new `web_fetch` tool that lets the model
fetch arbitrary www content via HTTP `GET` on a given `url`. The README already
lists `web_fetch` as an unchecked planned tool.

Goal: register a `web_fetch` tool in both CLI and TUI tool sets. The model
passes a `url`; the tool issues an HTTP `GET`, converts the response body to
model-friendly text (HTML → markdown; text/json/xml verbatim) or — for
recognized media — returns it as a typed Base64 attachment like `read` does
for local files, and reports the outcome as a structured tool result.

Motivation: models need to read specific web pages (docs, articles, API
references, images) at runtime; `web_search` only returns title/URL/snippet
lists.

Scope of this iteration (grilled and settled):

- One fetch backend named `native`: a plain local `GET` via `reqwest`, with
  the Ylva user-agent. Headless browsers (camofox et al.) and external fetch
  services are deferred; the backend seam plus the `[web_fetch] backend`
  config key (default `"native"`) is where they will slot in later.
- Backend seam `WebFetchBackend` (mirroring `WebSearchBackend`), also
  mandatory for testability: network side effects must be caller-injected.
- Content handling mirrors `read`'s autodetect philosophy:
  - Magic-number-recognized media (PNG/JPEG/WebP/GIF/PDF, via `infer` like
    `read`) → returned as Base64 with a typed attachment
    (`ToolAttachment::Image`/`File`), **complete response only**, with the
    same size constraints as a local fs `read`.
  - `text/html` → markdown via the `htmd` crate.
  - `text/*`, `application/json`, `application/xml` (incl. `+json`/`+xml`
    suffixes) → verbatim UTF-8 text.
  - Other content types or non-UTF-8 bodies → clear error.
- Size constraints (aligned with `read`):
  - Plain response limit = `READ_MEDIA_MAX_BYTES` (8 MiB) — the same ceiling
    `read` enforces on complete Base64/mime-typed media, shared as a single
    constant (SSOT). For recognized media this is a hard complete-file
    limit: a larger response fails with an explicit size error, never a
    truncated attachment (a partial image/PDF is unusable).
  - Returned text output (HTML-derived markdown or verbatim text) is capped
    at 64 KiB (`WEB_FETCH_MAX_OUTPUT_BYTES`); over-cap text is truncated and
    flagged, same vocabulary as capped `read`/`bash`/`search` output: a
    `truncated` metadata field and the `Partial result` content-section
    label, no in-text marker.
- Approval follows the network sandbox pattern with a new capability: the
  `network_request` rule gains an optional `target` parameter (glob over
  `scheme://host`) so users can scope verdicts per host. Absent `target`
  means any target (today's meaning, backward compatible). No rule
  parameterization for path/bash tools.
- Redirects are followed normally; host-scoped rules evaluate the requested
  URL only (documented caveat — an accepted trade-off).

Acceptance criteria (definition of done):

- The model can call `web_fetch` with a `url` argument and receives the
  fetched content as tool-result text plus structured metadata
  (`url`, `final_url`, `content_type`, `media_type` when applicable,
  `truncated`).
- Only `http`/`https` URLs are accepted; other schemes fail before any
  request is made.
- HTML is converted to markdown (`htmd`), text/json/xml pass through
  verbatim; recognized media (PNG/JPEG/WebP/GIF/PDF) returns as a Base64
  typed attachment with `read`-identical size constraints (complete file ≤
  `READ_MEDIA_MAX_BYTES`, larger → explicit size error); other binary or
  non-UTF-8 bodies fail with a clear error.
- Text output is capped at 64 KiB with `truncated` metadata and a
  `Partial result` label (no in-text marker).
- Approval: user approval by default; a `[tools.web_fetch]`
  `network_request` rule with or without `target`, or the
  `--auto-approve-web-fetch` catch-all, overrides it.
- The tool is registered in `DEFAULT_CLI_TOOLS` and `DEFAULT_TUI_TOOLS`.
- No test touches the real network (stub seam in unit tests, local mock
  server in integration tests).
- All affected docs updated in the same change; `make format lint` and
  `make test-coverage` (≥80% line coverage) pass.

Key constraints:

- Network side effects must be caller-injected behind a trait (see
  `docs/known-test-issues.md` → `cfg(test)` does not reach library code).
- Binary + library split: types integration tests need must be reachable via
  `src/lib.rs`.
- Coverage threshold ≥80% on testable files (`make test-coverage`).
- `docs/architecture-design-records/` is currently empty — no recorded
  decisions constrain this change.
- Full descriptive names, file-per-module layout (see `AGENTS.md`).
- Existing `{ rule = "network_request", … }` configs (e.g.
  `tests/e2e/custom/profiles/default/config.toml`) must keep working
  unchanged.

## Implementation Plan

1. [ ] Create the `web_fetch` backend seam in `src/web_fetch.rs` (register the
   module in `src/lib.rs` alongside `pub mod web_search`), mirroring
   `src/web_search.rs`'s structure:
   - `pub enum WebFetchBody { Text { text: String }, Media { bytes: Vec<u8>, media_type: String } }`.
   - `pub struct WebFetchResponse` with fields `final_url: String`,
     `status: u16`, `content_type: Option<String>`, `body: WebFetchBody`,
     `truncated: bool` (only ever `true` for `WebFetchBody::Text`).
   - `pub enum WebFetchError` (`thiserror`): `InvalidUrl`, `Request`,
     `Status`, `TooLarge`, `UnsupportedContentType`, `Conversion` — each
     mapped later to a `ToolError` message; never retried.
   - `pub trait WebFetchBackend: Send + Sync` with an async
     `fetch(&self, url: &str) -> BoxFuture<'_, Result<WebFetchResponse, WebFetchError>>`
     (same `futures::future::BoxFuture` style as `WebSearchBackend`).
   - `pub enum WebFetchBackendKind` mirroring `WebSearchBackendKind`:
     one variant `Native`, wire name `"native"`, `SUPPORTED_NAMES = ["native"]`,
     display name `"Native"`.
   - Constants: text output cap `WEB_FETCH_MAX_OUTPUT_BYTES` = 65536 (64
     KiB) and `WEB_FETCH_OUTER_TIMEOUT_GRACE_SECS` (mirroring
     `WEB_SEARCH_OUTER_TIMEOUT_GRACE_SECS`). The plain response limit is
     `read`'s `READ_MEDIA_MAX_BYTES` (8 MiB) — reference that constant
     (SSOT), do not duplicate the number.
   - Pure helpers: URL scheme allowlist (only `http`/`https`, else
     `InvalidUrl` before any request) and body classification
     `convert_body_to_result(content_type, raw_body) -> Result<WebFetchBody, WebFetchError>`:
     - Magic-number sniff first via `infer::get` (mirroring
       `src/agent/tools/read.rs:1248`): recognized PNG/JPEG/WebP/GIF/PDF →
       `WebFetchBody::Media` with the sniffed `media_type` (Content-Type
       header never overrides magic numbers — headers can lie).
     - `text/html` → markdown via `htmd` (`WebFetchBody::Text`).
     - `text/*`, `application/json`, `application/xml`, `+json`/`+xml`
       suffixes → verbatim UTF-8 text (`WebFetchBody::Text`).
     - Anything else or non-UTF-8 bytes → `UnsupportedContentType`.
   - UTF-8-safe output truncation at `WEB_FETCH_MAX_OUTPUT_BYTES` sets
     `truncated` on text bodies only; media is never truncated.
   Tests: pure unit tests for scheme validation, media sniffing precedence
   over Content-Type, per-content-type conversion (HTML fixture → markdown;
   JSON/xml/text passthrough), unrecognized-binary rejection, and UTF-8-safe
   text output truncation.

2. [ ] Implement `HttpFetchBackend` in `src/web_fetch.rs`:
   - `reqwest` GET with `OUTBOUND_USER_AGENT` (`src/http.rs`) as user-agent,
     request timeout from `WebFetchConfig::timeout_secs`, default redirect
     following (documented caveat: host-scoped `target` rules evaluate the
     requested URL only), non-success HTTP status → `WebFetchError::Status`.
   - Response size handling by body kind: text bodies are read to at most
     `READ_MEDIA_MAX_BYTES` (over-limit → `truncated = true`, classification
     and conversion continue on the partial body) and capped at
     `WEB_FETCH_MAX_OUTPUT_BYTES`; media bodies must arrive complete —
     Content-Length over `READ_MEDIA_MAX_BYTES` fails fast with
     `WebFetchError::TooLarge`, and a streamed body exceeding the limit
     fails with `TooLarge` as well (mirroring `read`'s
     `read_media_too_large_error`: a partial file is unusable, never
     truncated).
   Tests: integration test `tests/web_fetch_backend.rs` against a local mock
   HTTP server (reuse the raw `tokio::net::TcpListener` pattern of
   `tests/common/mock_llm_server.rs`) covering: successful HTML fetch and
   markdown conversion, JSON/xml passthrough, a PNG fixture fetched as
   `WebFetchBody::Media` with sniffed `media_type`, non-success status
   error, unrecognized-binary error, oversized media → `TooLarge`, text
   response truncation (raw and 64 KiB output), redirect following with
   `final_url`, and invalid-scheme rejection before any socket work. No
   external network.

3. [ ] Extend the `network_request` sandbox rule with an optional `target`
   parameter:
   - `src/sandbox.rs`: `SandboxRuleConfig::Named` variant for
     `NamedSandboxRule::NetworkRequest` gains `target: Option<String>` —
     a glob (`glob::Pattern`) over the request's `scheme://host` (explicit
     port included when present in the URL). Absent `target` matches any
     target (backward compatible). `SandboxContext::check_network_request`
     takes the match key (`scheme://host` string) and matches configured
     `target` globs against it; rules without `target` match every call.
     Existing precedence is kept: configured rules in list order (last match
     wins), then appended catch-all rules (`--auto-approve-*`) which always
     win.
   - `src/config/profile.rs`: validation — `target` is only valid under
     network tools (`web_search`, `web_fetch`); extend the `is_network_only`
     special-case (and its error text) to cover `web_fetch`; reject `target`
     under path/bash tools.
   - Callers: `web_fetch` passes the requested URL's `scheme://host`;
     `web_search` passes its configured backend endpoint's `scheme://host`
     (e.g. `https://html.duckduckgo.com`) so `target` works uniformly.
   Tests (in `src/sandbox.rs` / `src/config/profile.rs`): target-less rule
   still matches any call (existing tests keep passing); `target` glob
   matching (`https://docs.rs`, `https://*.example.com`, `*://host`); non-
   matching `target` abstains; list-order last-match-wins; appended
   catch-all still wins; `target` under `[tools.read]`/`[tools.bash]` is a
   config error; `tests/e2e/custom/profiles/default/config.toml` keeps
   loading.

4. [ ] Add the `[web_fetch]` profile configuration:
   - `src/config/web_fetch.rs`: `pub struct WebFetchConfig` with fields
     `pub backend: String` and `pub timeout_secs: u64`
     with defaults `backend = "native"` (validated at config load; unknown
     value → `ConfigError::InvalidWebFetchBackend`, mirroring
     `ConfigError::InvalidWebSearchBackend` in `src/config.rs`) and
     `timeout_secs = 30`, plus `build_backend() -> Arc<dyn WebFetchBackend>`
     like `WebSearchConfig::build_backend`.
   - Wire it: `mod web_fetch` + re-export in `src/config.rs`; field
     `web_fetch: WebFetchConfig` on `ProfileConfig` (`src/config/profile.rs`)
     with default + `validate()` call; tool-sandbox rule validation accepts
     `network_request` (with or without `target`) under `[tools.web_fetch]`.
   Tests: config parse with/without `[web_fetch]` table, defaults, unknown
   backend rejected with the supported-name list in the message,
   `build_backend` returns the native backend, and
   `[tools.web_fetch] network_request` (with and without `target`) loads
   while other rule kinds are rejected.

5. [ ] Implement the tool in `src/agent/tools/web_fetch.rs`:
   - `pub struct WebFetchTool` with `NAME = "web_fetch"`,
     `WebFetchInputArgs { url: String }` (`JsonSchema` + `Deserialize`,
     field description), and `WebFetchResult` implementing `ToolCallResult`.
   - `WebFetchResult` handles both body kinds:
     - `WebFetchBody::Text` → `text()` carries the (capped) text;
       content section labeled `Partial result` when `truncated`.
     - `WebFetchBody::Media` → `text()` carries standard padded Base64 of
       the bytes (like `read`'s media results) plus a typed attachment via
       `ToolAttachment::Image`/`ToolAttachment::File`
       (`src/agent.rs:479`), mirroring `read`'s construction in
       `src/agent/tools/read.rs:464`; no truncation ever.
   - `metadata()` = `{url, final_url, content_type, media_type?, truncated}`;
     `metadata_schema()` accordingly.
   - Constructor takes `Arc<dyn WebFetchBackend>`, `&WebFetchConfig`, and a
     `SandboxContext` (same injection as `WebSearchTool`); user-facing
     `title()` hints at the configured backend (e.g. `Web Fetch (Native)`),
     mirroring `WebSearchTool::title`.
   - `needs_approval` = `sandbox_context.check_network_request(&match_key)`
     with `match_key` derived from the requested URL's `scheme://host`,
     `.unwrap_or(ApprovalCheck::UserApprovalRequired)`.
   - `timeout_secs` = config timeout + `WEB_FETCH_OUTER_TIMEOUT_GRACE_SECS`;
     the call runs the backend future under that outer timeout.
   - `non_default_redacted_arguments` passes through (no secrets in
     arguments). The model-facing description states that recognized media
     is returned as an attachment with `read`-identical size limits.
   Tests (unit, stub `WebFetchBackend`, `crate::test_support::run_locked`
   pattern as in `src/agent/tools/web_search.rs` tests): every schema
   property has a description (`assert_all_properties_have_descriptions`),
   approval matrix (default / target-less rule / matching `target` rule /
   non-matching `target` abstains / auto-approve catch-all), text success
   text + metadata, truncation metadata + `Partial result` section, media
   result carries Base64 text + `ToolAttachment` + `media_type` metadata,
   each `WebFetchError` mapped to `ToolError` (including `TooLarge` with
   `read`-style explicit size wording), missing/empty `url` rejected.

6. [ ] Register and wire the tool end to end:
   - `src/agent/tools.rs`: `pub mod web_fetch`; add
     `web_fetch::WebFetchTool::NAME` to `DEFAULT_CLI_TOOLS` and
     `DEFAULT_TUI_TOOLS`; `AutoApproveTool::WebFetch`; `ToolRegistryConfig`
     field `web_fetch: crate::config::WebFetchConfig` (with `Default`);
     register in `default_registry` via `sandbox_context_for(
     web_fetch::WebFetchTool::NAME, AutoApproveTool::WebFetch)` using
     `config.web_fetch.build_backend()` (copy the `web_search` block).
   - `src/agent_factory.rs` and `src/cli_driver.rs`: pass
     `web_fetch: profile.web_fetch.clone()` into `ToolRegistryConfig`.
   - `src/cli.rs`: `--auto-approve-web-fetch` flag + entry in
     `auto_approve_tools()`.
   - `src/lib.rs`: re-export any `web_fetch` types integration tests need.
   Tests: update `default_registry_registers_cli_tools` /
   `default_registry_registers_tui_tools_including_clarify` expectations;
   registry keeps `web_fetch` out when absent from `allowed_tools`;
   auto-approve flag wiring; `tests/cli_help.rs` if it snapshots flags.

7. [ ] Documentation (same commit as the feature):
   - `README.md`: check the `web_fetch` checklist item.
   - `docs/features.md` → "Tools to be used by models": add a `web_fetch`
     bullet (arguments, text conversion behavior, media attachment behavior
     with the `read`-identical size constraints, text output cap and the
     `truncated`/`Partial result` signaling, approval semantics including
     `target` rules, redirect caveat).
   - `docs/configuration.md`: document the `[web_fetch]` table
     (`backend`, `timeout_secs`) and `[tools.web_fetch]` `network_request`
     sandbox rules including the optional `target` parameter (glob over
     `scheme://host`, absent = any target) and its `web_search` applicability.
   - `docs/cli-options.md`: add the `--auto-approve-web-fetch` row to the
     auto-approve flag table.
   - `docs/implementation-notes.md`: add a `web_fetch` section covering the
     backend seam, content-type classification (magic-number-first, `htmd`
     for HTML), the size constraints (shared `READ_MEDIA_MAX_BYTES`, 64 KiB
     text output cap), and `network_request` `target` matching.
   - Run `rumdl fmt` on every touched markdown file and check with `rumdl`.

8. [ ] Optional manual e2e script `tests/e2e/web_fetch-tool.zsh`, mirroring
   `tests/e2e/web_search-tool.zsh` (live network + live LLM; not part of
   `make test-coverage`).

9. [ ] Final verification: `make format lint`, then `make test-coverage`
   (must pass at ≥80%); fix any fallout; ensure this plan's checkboxes match
   reality.

## Proposed Improvements

- Report HTTP `status` and `final_url` in result metadata (already in the
  plan) and keep them visible in the tool bubble — they make bot-blocking /
  redirect surprises diagnosable from a session alone.
- A later iteration could add an optional `format` argument
  (`markdown` | `text` | `raw`) once the model shows a need; do not build it
  now (YAGNI).
- When the headless-browser backend lands (idea's camofox note), it slots
  into `WebFetchBackend` + `WebFetchBackendKind` + the existing
  `[web_fetch] backend` key, mirroring `WebSearchBackendKind` selection.
- If host-scoped rules prove insufficient because of redirect escapes, a
  stricter redirect policy (same-host only or per-hop rule re-check) can be
  added inside `HttpFetchBackend` later.

## Implementation Hints

- Tool trait set: `Tool`, `ToolCall`, `ToolCallResult`, `ToolRegistry`,
  `ToolRegistryConfig`, `AutoApproveTool`, `default_registry`,
  `DEFAULT_CLI_TOOLS`, `DEFAULT_TUI_TOOLS` — all in `src/agent/tools.rs`
  (re-exported through `src/lib.rs`). `Tool::needs_approval` returns
  `ApprovalCheck` (`AutoApproved` / `AutoDenied` / `UserApprovalRequired`,
  default the latter).
- The closest template is `web_search` end to end:
  - `src/agent/tools/web_search.rs` — tool with injectable
    `Arc<dyn WebSearchBackend>`, sandbox network approval via
    `SandboxContext::check_network_request().unwrap_or(UserApprovalRequired)`
    (around line 399), outer timeout = internal timeout +
    `WEB_SEARCH_OUTER_TIMEOUT_GRACE_SECS`, stub-backend unit tests using
    `crate::test_support::run_locked`.
  - `src/web_search.rs` — backend seam (`BoxFuture`-returning method),
    `WebSearchError` via `thiserror`, `reqwest` client with
    `.user_agent(OUTBOUND_USER_AGENT)`, HTML parsing with `scraper`.
  - `src/config/web_search.rs` — `[web_search]` profile table
    (`backend`, `timeout_secs`, `result_limit`) with `build_backend()`;
    validation error lives in `src/config.rs`
    (`ConfigError::InvalidWebSearchBackend`, line ~153).
- `read`'s media pipeline is the alignment target (settled):
  - `READ_MEDIA_MAX_BYTES: usize = 8 * 1024 * 1024`
    (`src/agent/tools/read.rs:107`) — hard ceiling on a **complete**
    recognized media payload; larger → explicit size error
    (`read_media_too_large_error`, `read.rs:158`), never truncation.
  - `READ_OUTPUT_LIMIT_BYTES: usize = 256 * 1024` (`read.rs:97`) is `read`'s
    *text* cap; `web_fetch`'s text cap stays the settled
    `WEB_FETCH_MAX_OUTPUT_BYTES` = 64 KiB (LLM-context decision).
  - Magic-number sniffing: `infer::get(bytes)?.mime_type()`
    (`read.rs:1248`); recognized set is PNG/JPEG/WebP/GIF/PDF only;
    `media_type` is only ever recognized for complete payloads.
  - Attachments: `ToolAttachment::Image { media_type }` /
    `ToolAttachment::File { media_type, .. }` (`src/agent.rs:479`),
    constructed as in `read.rs:464`; the provider wire projection
    (`src/llm/openai_compatible_messages.rs`) already handles them — no LLM
    layer changes needed.
- HTML→markdown conversion uses `htmd` (turndown-style converter,
  Apache-2.0 — matches the project license, actively maintained, 0.5.x).
  Add `htmd = "0.5"` to `[dependencies]`; `pulldown-cmark` is markdown
  *rendering* only and does not convert HTML→markdown.
- `OUTBOUND_USER_AGENT` is `concat!("ylva/", env!("CARGO_PKG_VERSION"))` in
  `src/http.rs:30` — the idea's "Ylva user-agent" requirement is already
  covered by this constant.
- Settled `network_request` rule grammar (backward compatible):

  ```toml
  { rule = "network_request", verdict = "auto_approved" }                              # any target
  { rule = "network_request", target = "https://docs.rs", verdict = "auto_approved" }  # scoped
  ```

  `target` is a `glob::Pattern` over `scheme://host` (explicit port
  included when present in the URL; e.g. `https://docs.rs`,
  `https://*.example.com`, `*://docs.rs`). Absent `target` = any target.
  The name `network_request` is kept (no rename). Match keys:
  `web_fetch` passes the requested URL's `scheme://host`; `web_search`
  passes its configured backend endpoint's `scheme://host`
  (`https://html.duckduckgo.com` for DuckDuckGo). Precedence stays:
  configured rules in list order (last match wins), appended
  `--auto-approve-*` catch-alls last and always winning. `target` is
  invalid under path/bash tools.
- Redirects: `reqwest` default redirect following is kept (settled). Known
  caveat to document: a `target` rule that auto-approves one host can be
  redirected to another host; the rule is not re-evaluated per hop.
- Size semantics (settled, `read`-aligned):
  - Plain response limit = `READ_MEDIA_MAX_BYTES` (8 MiB, shared constant).
    Text bodies over the limit are read to the limit and flagged
    `truncated` (classification/conversion continues on the partial body).
    Media bodies must be complete: Content-Length or streamed size over the
    limit → `WebFetchError::TooLarge` with `read`-style explicit size
    wording; never a truncated attachment.
  - Text output (markdown or verbatim) capped at 64 KiB UTF-8-safe prefix,
    `truncated = true` metadata + `Partial result` content label — the same
    vocabulary capped `read`/`bash`/`search` output uses, with **no
    in-text marker** (consistent with the existing bash/read decision;
    even `bash curl` workarounds hit the same caps).
  - Media results bypass the text cap entirely (a truncated image/PDF is
    unusable), exactly like `read`'s complete-media exemption.
- Binary policy (settled): recognized media (magic-number PNG/JPEG/WebP/
  GIF/PDF) → Base64 typed attachment; unrecognized binary / non-UTF-8 →
  clear error naming the content type.
- Network sandbox rules: `NamedSandboxRule::NetworkRequest` in
  `src/sandbox.rs`, evaluated by `SandboxContext::check_network_request`
  (line ~1234; signature gains the match-key parameter). `[tools.<name>]`
  tables only accept `network_request` for network tools — enforced in
  `src/config/profile.rs` via the `is_network_only` special-case (line ~137,
  its doc comment already anticipates `web_fetch`).
- CLI auto-approve pattern: `src/cli.rs` fields `auto_approve_*` +
  `auto_approve_tools()` (line ~177); docs table in `docs/cli-options.md`.
- Registry wiring call sites of `ToolRegistryConfig`: `src/agent_factory.rs`
  (~line 342) and `src/cli_driver.rs` (~line 273) construct it from the
  profile; both need the new `web_fetch` field.
- `Cargo.toml` already has `reqwest` (json, stream, gzip), `thiserror`,
  `futures`, `serde`, `schemars`, `tokio`, `infer`; `htmd` is the only new
  dependency.
- Test conventions: unit tests live in `#[cfg(test)] mod tests` in the tool
  module; async tool execution uses `crate::test_support::run_locked` while
  holding the tracing subscriber lock; integration tests use `tests/common.rs`
  - `tests/common/mock_llm_server.rs` (raw `tokio::net::TcpListener` mock
  HTTP server — suitable for `HttpFetchBackend` tests on `127.0.0.1`).
  Read `docs/known-test-issues.md` before writing tests involving `tracing`,
  cwd, or network.
- E2E script convention: `tests/e2e/<tool>-tool.zsh` (manual, live LLM +
  live network), e.g. `tests/e2e/web_search-tool.zsh`.
- `web_search` is currently *undocumented* in `docs/features.md`,
  `docs/configuration.md`, `docs/cli-options.md`, and its README checkbox is
  still `[ ]` — pre-existing staleness, out of scope here (reported in the
  session), but do not copy that gap for `web_fetch`.
