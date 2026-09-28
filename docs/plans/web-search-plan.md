# Add `web_search` tool

An implementing agent must flip `[ ]` to `[x]` immediately after completing each step and rewrite this sidecar file in place, so progress is resumable across sessions.

## Context

Ylva is a Rust TUI/CLI agent harness. LLMs call tools registered in a `ToolRegistry`
(`src/agent/tools.rs`); each tool lives in its own file under `src/agent/tools/`, is
registered in `default_registry`, and is exposed to the LLM via a JSON schema.

**Goal:** add a new tool `web_search` that lets the LLM search the web for given
`terms` via a configurable search backend.

**Motivation:** `README.md:31` lists `web_search` as an open roadmap item;
`docs/ideas/web-search.md` sketches the idea (candidate backends DuckDuckGo /
SearXNG / StartPage; own-HTTP vs CLI-tool approaches). All open questions from that
idea have been decided — see **Implementation Hints** for the settled decisions.

**Acceptance criteria:**

- LLM can call `web_search` with a single `terms` string and receives ranked
  results (title, URL, truncated snippet) as tool result metadata; default
  `limit` 10, schema max 20.
- Backend is own-HTTP only; DuckDuckGo HTML endpoint ships as the sole backend,
  selected via `[web_search] backend = "duckduckgo"` (explicit default, validated
  at config load).
- Consecutive requests are throttled: minimum interval 5000 ms between request
  starts plus 0–10% jitter; calls already spaced farther apart are not delayed.
- Fail fast on HTTP errors/blocks (clear `ToolError`, no retry).
- Approval: user approval by default; opt-in via `--auto-approve-web-search`.
- Tool is in `DEFAULT_TUI_TOOLS` and `DEFAULT_CLI_TOOLS`.
- Network access is injectable; tests run offline (fixtures + local mock HTTP
  server) per `docs/known-test-issues.md` (side effects must be caller-injected);
  one `#[ignore]`d live test plus a `tests/e2e/web-search-tool.zsh` script validate
  against real DDG when run manually.
- Docs updated in the same commit: `docs/features.md`, `docs/implementation-notes.md`,
  `docs/configuration.md`, `docs/cli-options.md`, `README.md` (check the roadmap
  box), `CONTEXT.md` only if the Tool glossary entry needs extending.
- `make format lint` and `make test-coverage` pass (≥80% coverage); `rumdl` passes
  on touched markdown.
- Scope is `web_search` only; `docs/ideas/web-fetch.md` (`web_fetch`) is a separate
  follow-up plan.

**Key constraints:**

- Project is fully tokio-async; `ToolCall::run` returns `BoxFuture`; outer timeout
  + `CancellationToken` are enforced by `PendingTool::execute` (`src/agent.rs:872-886`).
- `reqwest 0.12` already exists (`Cargo.toml:23`), currently used only in `src/llm/`.
- `DEFAULT_TUI_TOOLS` doubles as the config-validation allowlist
  (`src/agent_factory.rs:644-664`); both default lists must include the new tool.
- Default tool timeout is 5 s (`DEFAULT_TOOL_TIMEOUT_SECS`, `src/agent.rs:848`);
  this tool overrides it: internal HTTP timeout 30 s, tool `timeout_secs` 40 s so
  the outer timeout fires after max delay (5000 ms + 10% = 5500 ms) + 30 s request
  + grace (bash grace pattern, `BASH_OUTER_TIMEOUT_GRACE_SECS`).
- Secrets must never be logged; no secrets are needed for DDG, but keep the
  `${ENV_VAR}` pattern in mind for future backends.
- Implementation + tests + doc updates land in one commit; plans in `docs/plans/`
  are deleted once implemented (`docs/development-workflow.md:141-170`).
- No ADR deviation expected (`docs/architecture-design-records/` is empty; this
  change follows existing config/tool patterns) — add a record only if that
  changes during implementation.

## Implementation Plan

1. [ ] Define the backend seam. Create `src/web_search.rs` with a
   `WebSearchBackend` trait (`async fn search(&self, terms: &str, limit: usize)
   -> Result<Vec<WebSearchHit>, WebSearchError>`), a `WebSearchHit { title, url,
   snippet }` struct, and a `WebSearchError` enum. The backend is injected into the
   tool via a constructor field so tests pass a stub (known-test-issues: network
   side effects must be caller-injected). Wire the module into `src/lib.rs`
   re-exports as needed by tests.
   Verify: unit test with a stub backend returns hits unchanged.

2. [ ] Implement the DuckDuckGo HTTP backend in `src/web_search.rs`: own `reqwest`
   requests against `html.duckduckgo.com/html`, parse result HTML with the
   `scraper` crate (new dependency in `Cargo.toml`), extract title/URL/snippet,
   cap at `limit`. Fail fast: any non-success status (incl. 403/captcha) or parse
   failure returns `WebSearchError` mapped to `ToolError` with the HTTP status —
   no retries. Send the GET with Ylva's existing outbound user-agent constant
   `OUTBOUND_USER_AGENT` (`src/llm/openai_compatible_provider.rs:43`, currently
   private): make it `pub` and re-export it (or move it to a shared module) so the
   search backend uses the same SSOT constant instead of re-declaring the
   `concat!("ylva/", …)` pattern. Expose a test-only base-URL override on the
   constructor so a local mock server can be targeted.
   Verify: fixture-based unit tests (canned DDG HTML per result shape + error
   pages) through the parser; no network in tests.

3. [ ] Implement the `web_search` tool file `src/agent/tools/web_search.rs`
   following the `search` tool pattern (`src/agent/tools/search.rs`):
   - `WebSearchInputArgs`: `terms` (required single string), `limit` (optional,
     default 10, max 20) with schema descriptions; `input_schema = schema_for!(...)`.
   - `WebSearchResult` implementing `ToolCallResult`: `text()` = numbered
     title+URL list; `metadata()` = `{title, url, snippet}` array with snippets
     truncated to ~300 chars; `metadata_schema`, `content_sections`.
   - `WebSearchCall` implementing `ToolCall` with `run` in a `tracing` `tool_args`
     span.
   - `WebSearchTool` implementing `Tool`: `NAME = "web_search"`, description
     naming the configured backend, `timeout_secs` = 40 (internal HTTP timeout
     30 s), `create_call`.
   - `needs_approval`: default `UserApprovalRequired` (no override to
     auto-approve-by-default).
   Verify: schema test calling `assert_all_properties_have_descriptions`;
   truncation/limit unit tests.

4. [ ] Implement the consecutive-request throttle inside `WebSearchTool`: state is
   `Mutex<Option<Instant>>` on the tool struct (registry holds one
   `Arc<dyn Tool>` per process → process-wide naturally; fresh instances in tests
   avoid cross-test interference). Before each HTTP request, if the previous
   request *started* less than 5000 ms ago, sleep the remainder plus jitter of
   0–10% of 5000 ms (0–500 ms). Record the timestamp at request start regardless
   of outcome (success or failure throttles equally). Calls already spaced ≥ the
   interval are not delayed. First request is never delayed. The sleep runs inside
   `run()` and is cancelled automatically when the outer future is dropped on
   `CancellationToken`/timeout.
   Verify: unit tests with paused tokio time (`start_paused = true`) covering
   no-delay for spaced calls, delay for consecutive calls, jitter bounds, and
   timestamp recording on failure.

5. [ ] Register the tool: add `mod web_search;` to the module list
   (`src/agent/tools.rs:40-48`), register in `default_registry`
   (`src/agent/tools.rs:694-781`) wiring the backend built from profile config,
   add to `DEFAULT_TUI_TOOLS` and `DEFAULT_CLI_TOOLS` (`src/agent/tools.rs:605-624`),
   add `AutoApproveTool::WebSearch` (`src/agent/tools.rs:631-639`) with the
   `--auto-approve-web-search` CLI flag wired through `src/cli.rs` and
   `src/main.rs`.
   Verify: registry test asserts `web_search` present with non-empty schema;
   `[tool.web_search]` config validates (allowlist updated).

6. [ ] Add configuration: `[web_search]` profile section with `backend`
   (string, explicit default `"duckduckgo"`, validated at load — unknown value is
   a `ConfigError` naming the config path; only `duckduckgo` accepted for now),
   `timeout_secs` (default 30 = internal HTTP timeout), `result_limit`
   (default 10). Follow the `ProfileConfig` / `ProviderConnectionConfig` patterns
   (`src/config/profile.rs`, `src/config/provider.rs`); register the module in
   `src/config.rs`. Thread the resolved config into tool construction in
   `AgentFactory` (`src/agent_factory.rs`).
   Verify: unit tests for parsing, defaults, and unknown-backend rejection with a
   path-naming error message.

7. [ ] Integration tests: extend `tests/tools_agent_loop.rs` with a scripted
   `web_search` call using `MockProvider` and an injected stub backend; use
   `#[tokio::test(start_paused = true)]` and the existing `drive_until` approval
   helper. Add a local mock HTTP server test (bind `127.0.0.1:0`, precedent in
   `tests/common/mock_llm_server.rs`) pointing the real DDG backend at it via the
   base-URL override, covering request shape, success parsing, and fail-fast error
   paths. Add one `#[ignore]`d live test hitting real DuckDuckGo (no secrets
   needed).
   Verify: `make test-coverage` includes the new tests and stays ≥80%.

8. [ ] Add e2e script `tests/e2e/web-search-tool.zsh` modeled on
   `tests/e2e/search-tool.zsh` (debug binary + heredoc prompt driving a
   `web_search` call); wire env via an `tests/e2e/…/.envrc` only if the existing
   pattern requires it.
   Verify: script runs manually against the debug binary (not part of CI gates).

9. [ ] Documentation (must land in the same commit as the code):
   - `docs/features.md`: add a `web_search` bullet in the tools section (~line 402),
     including throttle and fail-fast behavior.
   - `docs/implementation-notes.md`: backend seam, DDG parsing, throttle
     (5000 ms + 0–10% jitter, recorded at request start), timeout relationship
     (40 s outer / 30 s internal / max 5.5 s pre-delay).
   - `docs/configuration.md`: `[web_search]` key table + example; re-check the
     tool-name validation list (lines 531-535); note `[tool.web_search]` sandbox
     rules are unsupported (tool has neither path nor command arguments —
     `clarify` precedent).
   - `docs/cli-options.md`: `--auto-approve-web-search`.
   - `README.md`: check off the `web_search` roadmap box (line 31).
   - `CONTEXT.md`: extend the Tool glossary entry only if "searching the web"
     changes the term's scope (line 77-79).
   - Run `rumdl fmt` on touched markdown.

10. [ ] Final verification: `make format lint` and `make test-coverage`; confirm no
    secrets appear in logs (tracing fields use presence indicators only); confirm
    cancellation aborts an in-flight search or throttle sleep promptly; confirm
    startup error message for unknown tools includes `web_search` in the allowlist
    source.

## Implementation Hints

**Settled design decisions (from requirements review):**

- Own HTTP implementation only; no CLI-tool backends (e.g. `ddgr`) in v1. The
  `WebSearchBackend` seam allows adding them later without schema changes.
- Only DuckDuckGo ships; SearXNG/StartPage/others deferred. Config field
  `backend = "duckduckgo"` exists explicitly with that default and is validated.
- Approval: user approval by default; `--auto-approve-web-search` opts out.
- `web_search` is in both default tool lists from day one.
- Fail fast on HTTP errors/blocks — no retry, no partial-result plumbing.
- The DDG GET reuses Ylva's existing `OUTBOUND_USER_AGENT` constant
  (`src/llm/openai_compatible_provider.rs:43`) — promote/relocate it rather than
  duplicating the value.
- Throttle: min interval 5000 ms between request starts + jitter 0–10% (of the
  5000 ms interval → 0–500 ms); recorded at request start regardless of outcome;
  spaced-out calls not delayed; state in tool struct (`Mutex<Option<Instant>>`).
- Input: single string `terms` (not an array).
- Results: default limit 10, schema max 20, snippets truncated ~300 chars,
  present in both `text()` and `metadata()`.
- Timeouts: internal HTTP 30 s; tool `timeout_secs` 40 s (> 5.5 s max delay +
  30 s request + grace) so the outer timeout fires last.
- Testing: fixtures for parser, local mock HTTP server for the real reqwest path,
  `#[ignore]`d live test, plus `tests/e2e/web-search-tool.zsh`; no e2e coverage of
  other backends (none exist).
- `web_fetch` (`docs/ideas/web-fetch.md`) explicitly out of scope.

**Codebase facts:**

- Tool trait / registry: `src/agent/tools.rs` — `Tool` (358-485), `ToolCall`
  (267-331), `ToolCallResult` (125-197), `ToolRegistry` (492-571),
  `default_registry` (694-781), `DEFAULT_CLI_TOOLS`/`DEFAULT_TUI_TOOLS` (605-624),
  `AutoApproveTool` (631-639), `assert_all_properties_have_descriptions` (788-811).
- End-to-end pattern to copy: `src/agent/tools/search.rs` (args struct with schema
  doc comments, metadata struct, `Tool` impl with `with_timeout`, colocated tests).
- HTTP: `reqwest = "0.12"` (features `json`, `stream`) in `Cargo.toml:23`;
  helpers in `src/llm/http.rs`. User-agent: `OUTBOUND_USER_AGENT` is
  `const … = concat!("ylva/", env!("CARGO_PKG_VERSION"))` at
  `src/llm/openai_compatible_provider.rs:43`, used at line 265 and currently
  module-private — the DDG GET must reuse this constant (promote to `pub` and
  re-export, or relocate to a shared module), not duplicate the concat.
  `scraper` is a new dependency — add it to `Cargo.toml` and re-run the build.
- Timeouts/cancellation: outer timeout in `PendingTool::execute`
  (`src/agent.rs:886`), default 5 s (`agent.rs:848`); `tokio::select!` over
  `CancellationToken` (`agent.rs:872-885`) cancels the whole `run` future,
  including any throttle sleep — no special cancellation code needed inside the
  tool.
- Grace pattern: `BASH_OUTER_TIMEOUT_GRACE_SECS` (`src/agent/tools/bash.rs:48-52`)
  — outer must exceed inner; here outer 40 s > 5.5 s delay + 30 s request.
- Config: `ProfileConfig` in `src/config/profile.rs:37-94`; module wiring in
  `src/config.rs:22-33`; `ConfigError` variants name the config path
  (`src/config.rs:57-152`); `${ENV_VAR}` interpolation rules in
  `docs/configuration.md:136-143`.
- Hard validation: unknown `[tool.<name>]` keys fail startup
  (`src/agent_factory.rs:644-664`); error message lists allowed tools — update
  both allowlist and message source together (both derive from the default lists).
- Approval: `Tool::needs_approval` defaults to `UserApprovalRequired`
  (`tools.rs:397-400`); `clarify` is the precedent for a non-sandboxed tool
  (`tools.rs:757-762`) — `web_search` has neither path nor command arguments, so
  `[tool.web_search]` sandbox rules are unsupported.
- CLI auto-approve flags: pattern in `src/cli.rs:96-120, 173-195`, wired in
  `src/main.rs:240-241`.
- Testing: colocated `#[cfg(test)]` tests; `src/test_support.rs` (`run_locked`,
  `capture_logs`); integration pattern in `tests/tools_agent_loop.rs`
  (`MockProvider`, `drive_until`, `start_paused = true`); local HTTP mock
  precedent `tests/common/mock_llm_server.rs` (bind `127.0.0.1:0`); e2e precedent
  `tests/e2e/search-tool.zsh`; coverage gate ≥80% (`make test-coverage`, excludes
  `tui_widgets.rs`, `render.rs`, `main.rs`).
- Known test issues: hold `SUBSCRIBER_LOCK`/`run_locked` for tracing tests; inject
  network side effects via trait/closure (`docs/known-test-issues.md:82-103`);
  `cfg(test)` does not reach library code from integration tests.
- Docs: plan format and one-commit rule in `docs/development-workflow.md:141-170`;
  docs are part of the change set (AGENTS.md); `rumdl` with `.rumdl.toml`;
  verification = `make format lint` + `make test-coverage`.
- `title_case_tool_name("web_search")` → `"Web Search"` already asserted at
  `src/agent/tools.rs:1526`.
- README roadmap boxes to check: line 31 (`web_search`); leave line 32
  (`web_fetch`) unchecked.