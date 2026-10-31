# Improve EdenAI reasoning-effort handling

## Context

This plan changes Ylva's EdenAI model catalog interpretation, engine effort validation,
runtime model selection, parameter display, and Chat Completions request serialization.
Its goal is to let an explicit reasoning effort reach EdenAI without claiming that a
gateway-valid value is verified for every upstream model.

Status: implementation authorized by the user with "Execute plan at
docs/plans/improve-edenai-provider-plan.md". Execution is complete.
No live inference, branch, staging, or commit is authorized.

The defect has three parts. EdenAI catalog parsing discards reasoning metadata. Engine
validation then drops explicit efforts because no values are advertised. Finally, EdenAI's
request customizer ignores the runtime effort. Fixing only one part is insufficient.

The input research, dated 2026-10-31, reports that EdenAI's public catalog advertised reasoning
capability but no nonempty model-specific effort lists. Its live OpenAPI schema documented an
optional, nullable, top-level `reasoning_effort` on `POST /v3/chat/completions`. That research
used public GET requests and local captures, not paid inference. This planning session
verified repository behavior but did not re-fetch the mutable external schemas.

The approved direction is the research's Option B. Preserve real capability evidence and
advertised efforts. When reasoning support is advertised but enumeration is missing, allow a
user-selected gateway value with explicit unverified status. Keep strict validation for other
providers. Invalid EdenAI efforts are rejected before dispatch, not silently dropped to default.
Rejected direct model switches and `/models` selections must leave settings unchanged. Effort
and provenance remain live-only, without session-schema or resume changes.

All grilling decisions are recorded in Implementation Hints. The user confirmed that this
document captures the shared understanding. No new ADR will be created.

### Acceptance criteria

- EdenAI catalog results preserve advertised reasoning capability and actual effort lists.
  Explicitly unsupported, unknown, and contradictory evidence remain distinguishable.
- Model-advertised efforts remain separate from gateway-valid unverified values.
- One effective EdenAI policy governs profile validation, raw catalog choices, named-engine
  choices, preview, and request dispatch. Other providers retain their current behavior.
- Omitted effort uses the provider default and sends neither null nor literal `"default"`.
- A selected effort reaches top-level `reasoning_effort` through the existing customizer.
  `disable` and `none` remain distinct; aliases and thinking budgets are not substituted.
- Gateway-invalid values cannot reach the network, including when catalog fetching fails,
  a model is absent, modalities are already known, or a provider is called directly.
- Rejected direct model switches and `/models` selections cannot change model, effort,
  provenance, modalities, context settings, or the active engine.
- Effective unverified effort is visible. Notices follow the approved timing policy and
  do not repeat on every tool continuation. Returning to default clears the explicit value.
- Parameter rejection identifies the model and effort and preserves backend error details.
  There is no silent retry with a different effort or provider default.
- Independent engine runtime handles do not leak settings through a shared connection.
  Each outgoing request uses a coherent snapshot of model and effort.
- Tests verify catalog edge cases, policy, selection, display, and actual HTTP construction.
  Affected documentation is updated and the required implementation gates pass.

### Constraints and exclusions

Preserve allowed-model checks, provider-based routing, exact named-engine shortcuts,
workspace configuration trust, session sticky routing, tools, headers, usage/cost handling,
request diagnostics/redaction, and provider-connection concurrency limits.

Do not add Responses transport, reasoning summaries, thinking-display controls, reasoning-token
accounting, transcript schema changes, model-family heuristics, or a compatibility database.
Higher effort does not guarantee visible thinking. The captured responses reported reasoning
usage but contained no readable thinking, only empty signed blocks or encrypted items.

Use the existing engine `reasoning` setting without a new opt-in key. Explicit profile values
and deliberately selected labelled overrides are sufficient opt-in. Prefer existing
dependencies. No new ADR is included. Additional documents require separate approval. No
branch, staging, commit, or live inference is authorized by this planning request.

## Implementation Plan

An implementing agent must change `[ ]` to `[x]` immediately after completing each step and
rewrite this file in place. Keep numbering consecutive and preserve completed states during
reordering. Execute in order, using failing regression tests before fixes. A step is complete
only with its affected documentation updated, even when the final documentation pass follows.

1. [x] Confirm the refined design before changing provider code.
   - [x] Resolve the design tree through grilling, record approved answers in Implementation
     Hints, update affected steps, and remove answered questions and the Open Questions section.
   - [x] Obtain final confirmation of shared understanding from the user.
   - [x] Obtain separate implementation consent. Keep the implementation consistent with
     existing ADRs. No new ADR is authorized or planned.

2. [x] Establish failing EdenAI catalog and serialization regressions.
   - [x] Open `src/llm/edenai_provider.rs` first. Run
     `cargo test --lib llm::edenai_provider::tests` as the focused baseline.
   - [x] Replace `catalog_never_reports_reasoning_efforts` with separate fixtures for true,
     false, and missing reasoning support; absent, empty, and populated effort lists; null
     and malformed objects; and contradictory evidence according to the approved policy.
   - [x] Cover advertised default membership, duplicate/invalid entries, and retained
     context, input modalities, text-output, and function-calling filters.
   - [x] Add customizer tests for an explicit `medium` effort and for omission, preserving
     `session_id`. Confirm the new capability/serialization tests fail before their fixes.

3. [x] Preserve EdenAI catalog evidence without inventing model-supported values.
   - [x] Introduce the smallest typed evidence representation needed by the approved matrix
     in `src/llm.rs` and the EdenAI catalog parser. Keep other providers' evidence neutral.
   - [x] Parse `capabilities.supports_reasoning` and nested effort metadata, distinguishing
     unknown, unsupported, supported-without-enumeration, and conflicting/malformed evidence.
     Missing/null optional enumeration is incomplete; wrong field types or malformed nonempty
     lists are invalid evidence for explicit effort. Do not silently turn them into overrides.
   - [x] Preserve actual advertised efforts; retain an EdenAI default only when advertised
     in its list. Do not infer a default or force a setting from `mandatory: true`.
   - [x] Carry evidence through selection DTOs and named shortcut reconstruction. Preserve
     `advertises_reasoning_effort` as exact advertised membership, not override acceptance.

4. [x] Implement one provider-owned policy and a fallible request-boundary check.
   - [x] Add owner-local policy tests for default, advertised membership, unverified override,
     explicit unsupported support, missing model, catalog failure, malformed/conflicting
     evidence, and all gateway-valid and invalid strings. Preserve gateway-valid explicit
     values as unverified when capability, model, or catalog evidence is unavailable; reject
     explicit unsupported, contradictory, or malformed evidence. Keep omission/default usable.
   - [x] Define one schema-derived EdenAI gateway list, not a maintained per-model mapping.
     Document its source and add contract fixtures for updates. Expose policy delegation through
     Provider/Engine with current strict behavior as the default for other providers.
     Resolve policy from the provider implementation, never the connection's arbitrary name.
   - [x] Return typed default, advertised, or unverified outcomes, or contextual errors.
     Keep effective provenance tied to the selected model in the runtime snapshot.
   - [x] Guard every EdenAI chat entry point using the same snapshot later serialized.
     Reject invalid wire values even for direct calls and runtime-handle writes. Use a
     fallible seam before network I/O rather than silently dropping values in the customizer.

5. [x] Apply the policy to configured efforts and lazy hydration.
   - [x] Add EdenAI cases in `src/llm/engine/tests/runtime.rs` alongside existing generic
     profile-validation regressions. Test omitted values, unverified accepted values, invalid
     values, known-modalities bypass, missing models, fetch failures, and repeated chats.
   - [x] Update `src/llm/engine/runtime.rs` to reject invalid EdenAI configured efforts before
     dispatch with engine/model/effort context, without silently resetting them. Ensure
     construction's direct runtime seeding cannot bypass wire validation.
   - [x] Tie asynchronous hydration results to the captured model/selection. Discard stale
     results instead of applying old evidence or provenance to a newly selected model.
   - [x] Preserve provider-default behavior and non-EdenAI drop-to-default semantics. Test
     configured copies and explicitly configured summarizers retaining independent settings.

6. [x] Make runtime selection consistent and safe before publication.
   - [x] Add tests in engine selection/router suites for raw and named choices, preview versus
     confirmation, named `Unchanged`, clearing to default without enumerated alternatives,
     target-specific validation, and rejected selections leaving all settings unchanged.
   - [x] Update `apply_model_option` in `src/llm/engine.rs`, named validation in
     `src/llm/engine/selection.rs`, and routing in `src/llm/engine/router.rs` to resolve the
     target, validate its complete candidate, then apply without partial mutation on rejection.
   - [x] For the EdenAI atomic path, publish model, effort/provenance, modalities,
     and context settings in one runtime update. Publish active routing only after success.
     Prevent stale candidates from overwriting intervening changes. Do not claim atomicity
     from merely reordering separate fallible setters.
   - [x] Make rejected direct EdenAI model switches leave settings unchanged as well. Resolve
     target evidence before publication through cached evidence or a validated switching path.
     Reuse the same policy; avoid a transaction redesign for other providers.

7. [x] Implement the approved unverified display and selection interaction.
   - [x] Extend `src/tui_driver/models.rs` and `src/tui_reducer_state/model_picker.rs` to
     preserve evidence and show effective effort/provenance independently of capability and
     provider-default labels, including named shortcuts after catalog failure.
   - [x] Implement a deliberately labelled unverified override mode with gateway-valid choices
     separate from ordinary advertised choices and a working default reset. Reuse the existing
     reasoning row and left/right controls, label every explicit override unverified, and apply
     on Enter without a separate toggle or confirmation dialog. Preserve Escape cancellation.
   - [x] Extend reducer and driver model-picker tests and, if controls change, overlay render
     tests. Verify rejection publishes no model-change event and changes no engine settings.
   - [x] Test explicit profile values and deliberately selected labelled overrides as sufficient
     opt-in without a new configuration key. Preserve untouched named-engine effort semantics
     and test Enter application without an extra acknowledgment dialog.

8. [x] Serialize effort and report uncertainty/errors without fallback behavior.
   - [x] Update `EdenAiProvider::customize_request_body` to insert top-level `reasoning_effort`
     from the validated runtime value, alongside existing `session_id`, and omit it for default.
   - [x] Show an unverified warning before dispatch in CLI and TUI, even if the request fails.
     Emit once for the current effective engine/model/effort selection, not every continuation.
     Reset warning state on default; returning to an unverified effort warns again. Share warning
     state only among agents using the same live engine, not independent runtime handles.
   - [x] Update `src/agent/request.rs` and notice forwarding as needed so failure does not hide
     warnings. Test timing, deduplication, resets, and shared versus independent warning state
     in owner-local request tests and assembled CLI tests.
   - [x] Wrap EdenAI pre-stream and streamed parameter errors with captured model/effort
     context while preserving status and backend details. Keep existing transient retry
     rules; never retry a parameter rejection by changing or removing effort.

9. [x] Verify assembled request wiring through a local HTTP test double.
   - [x] Extend `tests/http_provider.rs` using real EdenAI providers/engines. Cover profile
     and typed runtime selections reaching the same body, default reset omission, session ID,
     usage options, temperature, tools, authentication, and EdenAI tags.
   - [x] Extend `tests/common/mock_llm_server.rs` only with narrowly needed catalog JSON and
     error-body fixtures. Supply EdenAI-shaped capabilities and a finite SSE stream.
   - [x] Test invalid values produce zero chat POSTs, unsupported-effort HTTP 400/422 and SSE
     errors remain visible, and no changed-value/default retry occurs.
   - [x] Test independent engines sharing a connection and in-flight snapshot consistency.
     Retain other-provider serialization and validation regressions.

10. [x] Complete the affected documentation updates.
    - [x] Update `docs/provider-notes.md` to remove the no-request-shape claim and distinguish
      gateway acceptance, model advertisement, unverified overrides, and hidden thinking.
    - [x] Update `docs/configuration.md` for the approved explicit-effort validation, default,
      failure, explicit-selection opt-in, and notice behavior. No new opt-in key is introduced;
      existing configuration loading and workspace trust boundaries remain unchanged.
    - [x] Update `docs/features.md` for catalog states, effective effort labels, default reset,
      and the chosen picker interaction. Update `docs/implementation-notes.md` for shared
      policy, hydration safety, coherent runtime application, and EdenAI serialization.
    - [x] Update `docs/keybindings.md` if controls change. Update `CONTEXT.md` if effort
      provenance becomes a domain concept. Keep provenance live-only, with no session-schema
      or resume changes.
      Update `docs/persistence.md` only if clarification of the existing contract is needed.
    - [x] Keep this plan aligned with implemented decisions and actual progress. Do not edit
      the source research document or unrelated stale docs without consent.

11. [x] Run the required verification and report evidence.
    - [x] Follow `docs/development-workflow.md`: run `make format lint` and the single
      mandatory full-suite gate `make test-coverage`, maintaining at least 80% aggregate
      line coverage. Do not add a duplicate mandatory `make test` run.
    - [x] Run `make audit` if dependencies changed. Run configured `rumdl check` on affected
      Markdown and resolve failures. Review the diff for unintended provider changes,
      unauthorized files, and secrets.
    - [x] Report tested behavior, gates and limitations. Do not claim model-specific upstream
      acceptance or that effort was honored without separately authorized live evidence.

## Provider-neutral refactor

The user separately authorized replacing implementation-kind checks in generic contracts and
engine runtime. The accepted behavior matrix and exclusions above remain unchanged.

1. [x] Replace the provider-kind enum with `ReasoningEffortPolicy`, a provider-neutral trait.
   The strict default retains existing advertised validation and named-shortcut behavior.
   `EdenAiReasoningEffortPolicy` implements gateway rules, request checks, hydration needs,
   profile retention, unverified choices and notice wording inside the provider module.
2. [x] Separate runtime ownership through optional `Provider::bind_engine_runtime`.
   Bound providers share connection resources with a fresh engine-owned request handle.
   Coherent candidate publication depends on runtime binding, never reasoning identity.
   Providers declining binding retain their existing handles and setter behavior.
3. [x] Resolve display-only `ReasoningEffortOptions` through the exact selection target.
   Picker rows no longer carry a policy enum or infer a provider kind from override choices.
   Adapt compatibility fixtures without changing routing, popup controls or display behavior.
4. [x] Add neutral test doubles for independent runtime binding under strict reasoning,
   strict profile fallback and atomic rejection under binding, and custom reasoning validation,
   hydration and deduplicated notices without runtime binding. Preserve existing provider,
   selection, router, request and HTTP regressions.
5. [x] Update `docs/implementation-notes.md` and this existing plan with the actual contracts.
   Run focused provider, engine, picker, driver and HTTP tests. Parent verification reruns
   the required formatting, lint and full-suite coverage gates for the assembled checkout.

## Implementation Hints

- Approved in grilling round 3: the unverified override picker reuses the existing reasoning
  row and left/right controls, labels explicit choices unverified, keeps the provider-default
  slot, and applies on Enter without an extra toggle or confirmation dialog. Advertised effort
  lists retain the strict normal picker.
- Approved in grilling round 3: unverified warnings appear before dispatch in CLI and TUI,
  including failed requests, once per current effective selection. Default clears warning
  state; selecting an unverified effort again warns again. Agents sharing one live engine
  share warning state; independent engine runtime handles have independent state.
- Approved in grilling round 3: no new ADR will be created. The shared contract is documented
  in this plan and the named existing documentation, consistent with recorded ADRs.
- Approved in grilling round 2: an explicit engine `reasoning` value or deliberate labelled
  override selection is sufficient opt-in. No separate configuration switch is added.
- Approved in grilling round 2: `/models` provides a clearly labelled unverified override mode,
  separate from model-advertised effort choices, with a default-reset option.
- Approved in grilling round 2: gateway-valid explicit values are allowed as unverified when
  capability information is missing, a model is absent, or catalog fetching fails. Explicit
  unsupported, contradictory, or malformed evidence rejects explicit effort. Missing/null
  optional enumeration is incomplete, not malformed. Default omission remains usable.
- Approved in grilling round 1: Option B allows explicit unverified efforts when EdenAI
  advertises reasoning support without enumerated alternatives; no model metadata is invented.
- Approved in grilling round 1: invalid EdenAI configured efforts fail before dispatch with
  engine/model/effort context, without fallback to default. Other providers remain unchanged.
- Approved in grilling round 1: rejected direct EdenAI model switches and `/models` selections
  leave settings unchanged; coherent candidate publication also protects request snapshots.
- Approved in grilling round 1: effort/provenance remain live-only; existing profile,
  configured-copy, session-schema, and resume behavior are retained.
- `src/llm.rs` defines `ModelInfo`. Its three existing effort states are `None` for reasoning
  not advertised, `Some([])` for reasoning with no selectable alternatives, and `Some(values)`
  for advertised alternatives. They cannot distinguish explicit false from absent capability.
- `parse_reasoning_object` keeps nonempty default strings even outside advertised lists.
  Applying EdenAI's stricter default rule globally would change other providers. Global effort
  ordering includes aliases outside EdenAI's gateway enum and does not validate wire values.
- The researched gateway enum is exactly `minimal`, `low`, `medium`, `high`, `max`, `xhigh`,
  `disable`, `none`. The gateway contract is not a model-specific compatibility list.
- `EdenAiModelCatalog::parse` and `EdenAiProvider::customize_request_body` are private in
  `src/llm/edenai_provider.rs`; owner-local tests can reach them. The latter currently inserts
  only `session_id`. EdenAI derives `/v3` from the configured origin and uses Chat Completions.
- `RequestBodyCustomizer` in `src/llm/openai_compatible_provider.rs` is an infallible function
  pointer. Chat request construction snapshots `EngineRuntimeConfig` before the future runs,
  then assembles stream/usage options, temperature, customization, and tools. Its separate
  Responses branch does not call the Chat Completions customizer.
- `EngineConfig.reasoning` seeds `EngineRuntimeConfig.reasoning_effort` directly during
  construction. `EngineImpl::config()` does not return the live effort; `reasoning_effort()`
  does. Setter-only validation would miss profile seeding and raw runtime-handle writes.
- Hydration currently returns unit, skips when modalities are known, and clears unadvertised
  configured efforts after catalog success. Missing model/fetch failure preserves them.
  Its atomic flag is not an exclusive in-progress guard, and model changes can race the await.
- `apply_model_option` validates first but writes effort before switching model and other
  settings. Separate setters are not transactional. Direct `switch_model` retains effort and
  relies on later hydration for model-specific validation.
- Named `Set(None)` currently requires nonempty advertised alternatives; named `Unchanged`
  bypasses validation. Raw preview currently does not validate effort as confirmation does.
- `Provider::name()` identifies a provider connection, not its kind. Raw catalog rows route
  to the active engine on a connection or its alphabetically first engine; named rows target
  the exact engine key. These are intentionally different routing contracts.
- Model-picker labels currently hide explicit effort for `Some([])`. A parameter effort row
  exists only for nonempty advertised values. Popup edits are drafts until Enter.
- Engine notices are currently drained by `src/agent/request.rs` only after successful chat.
  An unverified warning queued during hydration can therefore be lost on request failure.
- Persisted `ModelChanged` stores provider connection and model, not effort, provenance,
  context override, or named-engine identity. Configured copies use independent runtime
  handles; agents sharing the same named live engine intentionally share its settings.
- `src/http.rs` retries transient failures, 429, and server errors, not ordinary 400/422
  parameter rejection. Shared streaming emits errors without restarting an active stream.
- `tests/common/mock_llm_server.rs` captures bodies and headers but emits generic model JSON,
  not EdenAI capability objects. Finite streams need frames and `end_after_frames: true`.
- Root integration tests import public APIs through `src/lib.rs`. Library `cfg(test)` helpers
  are unavailable to integration targets. Existing tracing/cwd isolation requirements are in
  `docs/known-test-issues.md`; `run_locked` and `SUBSCRIBER_LOCK` are the established helpers.
- `.rumdl.toml` sets 100-character line length. Verification policy and plan execution are
  documented in `docs/development-workflow.md`.
- ADR-0001 covers git conventions, ADR-0002 release automation, and ADR-0003 workspace trust.
  None defines EdenAI reasoning semantics. Provider policy does not authorize bypassing trust.
- The existing no-EdenAI-effort-request-shape statement in provider notes and provider source
  comments is stale relative to the research. It remains unchanged during planning.
- Repository exploration used the existing graph, whose broad query was truncated, and
  source-verified findings. Temporary research captures are not required to execute this plan.
- External evidence is available at <https://api.edenai.run/v3/docs/openapi.json>,
  <https://api.edenai.run/v3/models>, and
  <https://www.edenai.co/docs/v3/llms/chat-completions>. These mutable public documents do not
  prove that every upstream adapter accepts or honors a gateway-valid effort.

## Execution evidence

- Baseline `cargo test --lib llm::edenai_provider::tests` passed six tests.
  Before the core fixes, catalog membership and explicit-medium serialization regressions
  failed. The explicit-effort picker label regression also failed before its fix.
- Some additional regression tests could not execute at the red stage because concurrent
  typed-field migration temporarily prevented compilation. After migration, focused provider,
  engine, picker, agent-request, HTTP and CLI regressions passed in the assembled checkout.
- Before the provider-neutral refactor, `make format lint` passed. `make test-coverage`
  passed the full suite with aggregate line coverage of 88.95%, above the required threshold.
  Affected Markdown checks passed.
- After the provider-neutral refactor, focused offline suites passed: engine 70, EdenAI 14,
  model-picker filter 46, driver models 29, agent request 10, CLI 19, HTTP provider 42 with
  one opt-in test ignored, and rendered model-picker filter 14. All-target/all-feature Clippy
  passed with warnings denied. Both updated Markdown files passed `rumdl check`.
  Final assembled `make format lint` and `make test-coverage` passed after the refactor.
  Aggregate line coverage is 88.97%, above the required threshold.
- Dependencies, session schema, workspace trust, and transport choice did not change.
  No audit was required, and no live inference or upstream-acceptance claim was made.
- Existing unrelated work, including Bash error-title changes made concurrently, is outside
  this implementation. No branch, staging, or commit was performed.
