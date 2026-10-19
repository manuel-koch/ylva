# Model usage cost tracking

## Context

Ylva (Rust TUI/CLI agent harness; crate `ylva` is both `[lib]` and `[[bin]]`)
should track the cost of completed model requests, accumulate a run total while
the TUI or headless CLI is active, show it in the TUI footer, and print it on
exit. Source idea: `docs/ideas/costs.md` (the "Context usage" half of that idea
is already implemented as the footer context-token estimate; only "Model usage
costs" remains).

Requirements:

- Only provider-reported costs count; no price tables or token-based
  computation.
- Totals are kept separately per currency and span the whole process (no reset
  on `/clear` or session switch). No "partial total" marker: tracking is
  best-effort and the provider's own billing is authoritative.
- Accumulate cost of every completed model request in the process: root agent,
  subagents, **utility** (session title) and **compaction** requests.
- The total is run-scoped: never persisted, never stored in session data, no
  usage history.
- If a request reports no usage or no pricing, leave it out of the total. Never
  estimate a billed amount from the context-token estimator.
- Show the accumulated total in the TUI footer.
- On exit, if a total is available, print it with the resume hint:

  ```text
  Session: <session_id>
  Costs:   <value> <currency> [<value> <currency>]+
  Resume with: ylva --conversation-id <session_id>
  ```

Acceptance criteria:

- Footer shows the running totals (left of the context usage, 2 decimals,
  ` + `-joined per currency) after requests complete; hidden when no request
  has reported a cost.
- Title and compaction requests are included in the total.
- Exit output contains the `Costs:` line only when at least one cost was
  recorded; the existing `Session:`/`Title:`/`Resume with:` lines keep their
  wording and order (`Costs:` sits between `Title:` and `Resume with:`).
- Nothing about costs is written to the session store (`docs/persistence.md`
  format unchanged).
- Coverage threshold in `docs/development-workflow.md` is maintained;
  `make format lint` and `make test-coverage` pass.

Constraints:

- Follow `AGENTS.md`: descriptive names, license headers on new files,
  file-per-module layout (`foo.rs`, not `foo/mod.rs`), thin `main.rs`, docs
  updated in the same commit, tests per `docs/development-workflow.md`.
- Read `docs/known-test-issues.md` before writing tests involving `tracing`,
  cwd or integration-test side effects.
- Check `docs/architecture-design-records/` (currently empty) for decisions
  touching this area; introduce a record if the change deviates from one.

## Implementation Plan

An implementing agent must flip `[ ]` to `[x]` immediately after completing
each step and rewrite this file in place, so progress is resumable across
sessions.

1. [ ] Add a cost domain type module (e.g. `src/llm/usage_cost.rs`, re-exported
   via `src/lib.rs` if integration tests need it): `RequestCost` (`f64`
   amount, currency code) and `CostLedger` (`Arc`-shareable, internally
   synchronized; `record(RequestCost)`, `totals()` returning one total per
   currency, ordered deterministically, empty until a request is
   recorded). Write unit tests first (TDD): empty ledger, accumulation,
   separate per-currency totals, concurrent recording.
2. [ ] Parse provider-reported `usage.cost` in the SSE stream of
   `src/llm/openai_compatible_provider.rs` next to
   `parse_reported_input_tokens` (usage-only frames have no choices, so parse
   independently of `parse_sse_payload`). Add a pure
   `parse_reported_cost(data: &str) -> Option<f64>` with unit tests for
   present, absent, non-numeric and zero cost frames. The currency comes from
   the provider kind, not the payload: only OpenRouter (`USD`) reports cost;
   the plain OpenAI-compatible and Ollama providers record nothing.
3. [ ] Record the parsed cost into the ledger once per request at the point
   where the stream completes (where `token_usage_record(reported_input_tokens)`
   is appended today). Record any request, including failed or cancelled ones, only if the provider
   reported a cost for it.
4. [ ] Log each recorded cost at `debug` level via `tracing` (provider
   connection, model, amount, currency; no prompt or response content) at the
   recording point from step 3, to help diagnose mismatches against provider
   billing. Read `docs/known-test-issues.md` before testing `tracing` output.
5. [ ] Inject the shared `CostLedger` into providers: thread it through the
   provider construction path (`src/llm/provider_registry.rs`,
   `src/llm/engine/runtime.rs` / `with_runtime_config`,
   `src/startup/engines.rs`, `src/agent_factory.rs`) so every engine used for
   conversation, subagent, compaction and title requests shares one ledger per
   process. Because recording happens inside the provider, utility and
   compaction calls (`src/session_title.rs`, `src/agent/compaction.rs`) are
   covered without per-caller plumbing.
6. [ ] Expose the totals to the TUI: add a `cost_total_text: Option<String>`
   field to the render state in `src/tui_render_state.rs`, populated in
   `src/tui_driver/updates.rs` (next to `context_usage_text`) by reading the
   ledger on every redraw (no new event). Verify a redraw happens after a
   request completes (the footer already refreshes on `ContextUsage`); if
   not, add the minimal trigger. Format: `<amount> <CUR>` with 2 decimals,
   several currencies joined with ` + ` (e.g. `0.12 USD + 0.10 EUR`).
   A non-zero total below 0.01 shows as `<0.01 <CUR>`; a zero total shows
   `0.00 <CUR>` once any request reported a cost (including zero); `None`
   when the ledger is empty. The footer shows the process-wide total in every
   view, including subagent views.
7. [ ] Render the cost text in the footer (`src/tui_widgets/footer.rs`, wide
   and narrow layouts), placed left of the context usage text. The context
   usage keeps priority: the cost text is truncated/dropped first on narrow
   widths. Cover with footer tests in `src/tui_widgets/footer/tests.rs`.
8. [ ] Print the cost on exit: extend `resume_hint_text` in
   `src/startup/session.rs` with an optional cost argument producing the
   `Costs:   <value> <currency>` line (aligned with `Session:`/`Title:`) using
   the same formatting as the footer, joined with ` + ` for several
   currencies, and
   pass the ledger total from `src/startup.rs` (`print_resume_hint` call site,
   ~line 318). Update the existing unit tests in `session.rs` and add cases
   with and without cost. This covers both the TUI and headless CLI paths
   because both flow through `startup.rs`.
9. [ ] Add an integration test using the mock provider (`src/llm/mock_provider.rs`
   extended to optionally script a cost) asserting: total accumulates across
   root plus title/compaction requests, and absent cost leaves the total
   `None`.
10. [ ] Update docs in the same commit:
    - `docs/features.md`: new section on cost tracking (what is counted, not
      persisted, footer, exit output); also adjust the "Context management"
      wording if the footer description changes.
    - `docs/implementation-notes.md`: ledger, parse point, injection path,
      footer refresh mechanism.
    - `docs/configuration.md`: any new setting (pricing override, currency) if
      introduced.
    - `docs/cli-options.md`: only if an option is added.
    - `docs/keybindings.md`: no change expected.
    - `docs/diagrams/class-diagram.mmd` (+ regenerate `.svg` per
      `docs/diagrams/README.md`) if the ledger is a major type.
    - `docs/ideas/costs.md`: mark the "Model usage costs" half as implemented
      or remove per repository convention.
    - Run `rumdl fmt` / `rumdl check` on changed markdown.
11. [ ] Run `make format lint` and `make test-coverage`; fix failures. If
    dependencies changed, also run `make audit`.

## Implementation Hints

- Idea source: `docs/ideas/costs.md`. Existing context footer behavior is
  documented in `docs/features.md` ("Context management and compaction") and
  `docs/implementation-notes.md` (estimate callback, `ContextUsage`).
- `Provider` trait: `src/llm.rs` (`chat`, `chat_with_session_id`,
  `chat_with_session_id_and_estimate`, `chat_with_session_id_and_diagnostics`).
  `ModelInfo` has no pricing fields today.
- OpenAI-compatible streaming: `src/llm/openai_compatible_provider.rs` already
  sends `"stream_options": { "include_usage": true }` (~line 477) and parses
  `usage.prompt_tokens`/`usage.input_tokens` via `parse_reported_input_tokens`
  (~line 714); the final usage frame is seen in the SSE loop (~line 648) and a
  `TokenUsageRecord` is appended after the loop (~line 701). OpenRouter wraps
  this provider (`src/llm/openrouter_provider.rs`). OpenRouter responses can
  carry a `usage.cost` value (credits, USD) — verify against current OpenRouter
  docs before relying on it. Ollama has `src/llm/ollama_provider.rs`; local and
  Ollama Cloud usage typically has no price.
- `src/llm/token_usage.rs` is a metadata-only opt-in JSONL dump
  (`YLVA_TOKEN_USAGE_DUMP_PATH`); it is diagnostic and separate from the ledger.
- Utility/compaction callers: title request in `src/session_title.rs`
  (`chat_with_session_id`, ~line 231); compaction summary in
  `src/agent/compaction.rs` (`chat_with_session_id_and_diagnostics`, ~line 525);
  root/subagent requests in `src/agent/request.rs` (~line 227). Engines are
  built in `src/agent_factory.rs` and `src/startup/engines.rs`.
- Footer: `src/tui_widgets/footer.rs` (`context_usage_text` used in narrow meta
  row ~line 442 and `draw_footer_context_usage` ~line 684); state in
  `src/tui_render_state.rs` (~line 228) filled in `src/tui_driver/updates.rs`
  (~line 345); `src/render.rs` ~line 376 passes it on.
- Exit output: `resume_hint_text` / `print_resume_hint` in
  `src/startup/session.rs` (~lines 120-160), called from `src/startup.rs`
  (~line 318) after both TUI and CLI runs. Wording is unit-tested there.
- Headless runs drop usage UI updates (`docs/implementation-notes.md`), so the
  ledger must not depend on the UI update channel.
- `docs/plans/` is the plan location; verification policy and plan execution
  rules are in `docs/development-workflow.md`.
- The exit snapshot (`SessionExitHandle`) carries session id and title only;
  costs come from the in-memory ledger and never enter that snapshot or the
  store.
- `StreamEvent` (`src/agent/protocol.rs`) is matched exhaustively in
  `request.rs`, `compaction.rs` and `session_title.rs`; the plan leaves it
  unchanged by recording cost inside the provider.
