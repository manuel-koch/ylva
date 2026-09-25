# Plan: Ollama thinking metadata, advertised reasoning defaults, and profile effort validation

## Context

Three related changes to Ylva's reasoning-effort handling, one driven by the TODO in
`src/llm/ollama_provider.rs:87-89` and two settled alongside it:

1. **Consume Ollama's new thinking metadata.** Ollama PR #18473 (merged 2026-09-18,
   <https://github.com/ollama/ollama/pull/18473>) added a `thinking` object to
   `/api/show`:

   ```json
   {
     "capabilities": ["completion", "thinking", "tools"],
     "thinking": { "values": [false, "low", "medium", "xhigh"], "default": "medium" }
   }
   ```

   `values` holds booleans and/or model-defined level names; `default` is the value
   used when a request omits thinking controls. Ylva currently maps only the
   `thinking` *capability* to `Some(Vec::new())`
   (`src/llm/ollama_provider.rs:211-218`), so `/models` users can never cycle an
   Ollama thinking level — `cycle_selected_model_reasoning_effort`
   (`src/tui_reducer_state.rs:2406-2446`) returns early on an empty list.

2. **Expose the provider-advertised default.** Add `ModelInfo.reasoning_default`
   and render it in the picker label (`reasoning: default (medium)`), for both
   Ollama (`thinking.default`) and OpenRouter (`reasoning.default_effort`,
   currently ignored by the parser).

3. **Validate profile `reasoning` against the catalog.** `EngineConfig.reasoning`
   enters the runtime unvalidated at `EngineImpl::new`
   (`src/llm/engine.rs:661-677`); only `/models` switches validate
   (`switch_model_option`, `src/llm/engine.rs:393-405`). A bad profile value
   surfaces as a mid-conversation HTTP 400 instead of a diagnosable event.

**Scope decisions (settled):** catalog discovery only — chat keeps the
OpenAI-compatible top-level `reasoning_effort` field (both old and new Ollama
accept it; native `/api/chat` `think`/`options` are out of reach of Ylva's
OpenAI-compatible transport and unnecessary). OpenRouter's `mandatory` /
`default_enabled` fields are intentionally unconsumed (YAGNI) but must be
recorded in `docs/implementation-notes.md` so they aren't rediscovered.

**Acceptance criteria:**

- Ollama models advertising `thinking.values` expose mapped, selectable levels
  via `ModelInfo.reasoning_efforts = Some(values)`; `←`/`→` cycling and
  `switch_model_option` validation work; capability-only and metadata-less
  models keep `Some([])` / `None` behavior unchanged.
- `ModelInfo.reasoning_default` is `Some` for Ollama (from `thinking.default`)
  and OpenRouter (from `reasoning.default_effort`) models that advertise one;
  the picker's default slot renders it; all other renderings (explicit effort,
  switch notice, footer, logs) are unchanged.
- A profile `reasoning` value is validated once, lazily before the first chat,
  against the catalog: definitive mismatches are dropped to provider default
  with a warning; unverifiable cases (model absent, fetch failed) keep the
  value; every other state that is not an advertised member is dropped
  (strict — matching the `/models` switch rule).
- e2e mock catalogs advertise reasoning metadata so the fixture
  `reasoning = "max"` survives validation; an HTTP test pins
  `reasoning_effort` on the wire.
- `make format lint` and `make test-coverage` pass; docs updated in the same
  change (`docs/features.md`, `docs/configuration.md`,
  `docs/implementation-notes.md`).

**Key constraints:**

- `ModelInfo.reasoning_efforts` three-state contract (`src/llm.rs:70-89`) is
  provider-neutral: `None` = not advertised, `Some([])` = supported without
  selectable alternatives, `Some(values)` = selectable alternatives. The new
  `reasoning_default` must not alter those semantics.
- Effort values are free-form strings end-to-end; no enum or allowlist exists.
- Project rules: docs are part of the change set; plan steps must be flipped
  `[ ]` → `[x]` in this file as they complete; final gate is
  `make format lint` + `make test-coverage`.

## Implementation Plan

An implementing agent must flip `[ ]` to `[x]` on each step (and sub-step)
immediately after completing it and rewrite this file in place, so progress is
resumable across sessions.

1. [x] Add `ModelInfo.reasoning_default: Option<String>` in `src/llm.rs`
   (next to `reasoning_efforts`, with a doc comment: the provider-advertised
   default effort when the provider exposes one, otherwise `None`; rendered
   only in the picker's default slot) and set the field in **every**
   `ModelInfo { .. }` struct literal in `src/` and `tests/`. Enumerate sites
   with `rg 'ModelInfo \{'` — roughly 40 literals across `src/llm/*.rs`,
   `src/tui_driver.rs`, `src/tui_reducer_state.rs`, `src/presenter.rs`,
   `src/render.rs`, `src/llm/provider_registry.rs` (tests), `tests/tui_render_state.rs`,
   `tests/cli_driver.rs`. Use `reasoning_default: None` everywhere except the
   parsing sites filled in steps 2, 3, and 4.

2. [x] Ollama metadata parsing in `src/llm/ollama_provider.rs`:
   - [x] Extend `parse_reasoning_efforts` (211-218) to prefer the top-level
     `thinking.values` array over the `capabilities` check. Map each element:
     non-empty string → as-is; `false` → `"none"`; `true` → `"high"`; other
     JSON types → skip. De-duplicate preserving order. If nothing maps (or
     `thinking`/`values` absent), fall through to the existing capability logic
     unchanged (`"thinking"` capability → `Some(vec![])`, no `capabilities`
     key → `None`).
   - [x] Add a sibling parser for `thinking.default` → `Option<String>` using
     the same mapping (`true` → `"high"`, `false` → `"none"`, non-empty string
     → as-is, anything else → `None`).
   - [x] Remove the addressed TODO at lines 87-89; keep the two upstream links
     as a doc comment on the parsing helper.
   - [x] Fix the enrichment gate in `list_models` (line 120-122): `parse_tags`
     can already have produced `Some([])` from a `capabilities` array in
     `/api/tags`, which would block richer `/api/show` metadata. Extract a
     pure helper (e.g. `merge_reasoning_metadata(current_efforts,
     current_default, body)`) that upgrades `None`/empty efforts and `None`
     default from the body, never downgrades a populated list, and never
     replaces a populated list — wire it into the show-response enrichment.
     The early return above it (old lines 100-105) was removed, not just the
     `is_none()` check: its only satisfiable skip case was a tags entry with
     `context_length` + `capabilities` + modalities — i.e. exactly the
     thinking-capability models that need `/api/show` for `thinking.values`
     (tags carries no `thinking` object), so the skip blocked the feature.
   - [x] Unit tests (in-source `mod tests`): named-levels passthrough in
     server order; mixed bool/string mapping + dedup (`[false, true]` →
     `["none", "high"]`; `[false, true, "high"]` → `["none", "high"]`);
     thinking without a `capabilities` key still yields levels; absent/empty/
     all-unmappable `thinking` falls back to capability behavior; default
     parsing for string/bool/absent cases; merge-helper upgrade/no-downgrade
     rules. Keep (rename if wording shifts)
     `parse_reasoning_efforts_maps_thinking_capability_without_provider_defaults`.

3. [x] OpenRouter/generic default parsing in
   `src/llm/openai_compatible_provider.rs`: read
   `reasoning.default_effort` from each `/api/v1/models` entry (sibling to the
   existing `parse_reasoning_efforts` at 665-681; non-empty strings only) into
   `ModelInfo.reasoning_default`. Populate it in `parse_model_list`
   (625-645). Unit tests: `default_effort` present → `Some`; absent or
   non-string → `None`; `supported_efforts` parsing unchanged.

4. [x] Picker rendering in `src/tui_reducer_state.rs`
   (`format_model_choice_label`, 1160-1190) — one change point covers both
   `model_choice_label` and `engine_choice_label`:
   - [x] `None` → `reasoning: unavailable` (unchanged).
   - [x] `Some([])` → `reasoning: default`, or `reasoning: default (X)` when
     `reasoning_default = Some(X)`.
   - [x] `Some(values)` with no selected effort → `reasoning: default ⚙`, or
     `reasoning: default (X) ⚙`; with a selected effort →
     `reasoning: {effort} ⚙` (unchanged).
   - [x] Copy `reasoning_default` from the catalog into engine-shortcut rows
     alongside `reasoning_efforts` in `apply_model_list_result`
     (`src/tui_driver.rs:625-641`).
   - [x] Do **not** change the switch-confirmation notice
     (`src/tui_driver.rs:744-755`), the footer (`src/tui_widgets.rs:2531-2543`
     reads `EngineDisplayInfo`, not `ModelInfo`), or log rendering.
   - [x] Unit tests: label with default present in both `Some([])` and
     index-0 `Some(values)` states; existing label tests (fixtures without a
     default) must keep passing unchanged.

5. [x] Profile `reasoning` validation before the first chat, folded into the
   existing lazy catalog hydration in `src/llm/engine.rs`:
   - [x] Rename `hydrate_input_modalities` → `hydrate_catalog_metadata` and
     the guard `input_modalities_hydrated` → `catalog_hydrated` (the method now
     does two jobs behind one one-shot guard; an honest name beats a doc
     lie). Update the trait default (369-379), `EngineImpl::chat` (680-690),
     `EngineRouter::chat` (511-516), the guard field (654-658, 674), the four
     hydration tests (1260-1341), and their doc comments.
   - [x] Inside the existing `Ok(models)` branch (after the modality lookup),
     when `runtime_config.reasoning_effort` is `Some(effort)`:
     - model found and `reasoning_efforts == Some(values)` containing
       `effort` → keep;
     - model found and `values` misses `effort`, or efforts are `None` /
       `Some([])` → set effort to `None` + `tracing::warn!` naming model,
       effort, and reason (strict: only advertised members are sendable);
     - model not found in the catalog → keep + `tracing::debug!`
       (validation impossible; same for the `Err(fetch)` branch, which keeps
       its existing warn-and-set-guard behavior).
   - [x] Extract the membership predicate shared with
     `switch_model_option` (393-405) into one helper (SSOT) so profile and
     `/models` validation cannot drift.
   - [x] Update the `EngineConfig.reasoning` doc comment (81-86) — it
     currently promises no validation before `/models`.
   - [x] Engine tests (follow the existing `CatalogProvider` patterns,
     1260-1341): keep-on-member; drop on membership miss; drop on `Some([])`
     and on `None` with model present; keep when model absent; keep on fetch
     failure with no retry; skip-path invariant (modalities already known →
     no fetch, effort untouched — it was switch-validated).

6. [x] Extend `tests/common/mock_llm_server.rs` model-list responses
   (`write_model_list_response`, 476-506, and the Ollama mock shape at
   509-536) with reasoning metadata so strict validation passes in e2e:
   OpenRouter shape `reasoning: { "supported_efforts": [...including "max"],
   "default_effort": ... }`; Ollama shape `capabilities` (plus `thinking` where
   a fixture exercises levels). The e2e profile's `reasoning = "max"`
   (`tests/e2e/custom/profiles/default/config.toml:37`) currently survives only
   if the mock advertises `"max"` — otherwise it is silently dropped.

7. [x] Wire-level test in `tests/http_provider.rs`: with a runtime reasoning
   effort set, assert the serialized Ollama chat request body contains the
   top-level `"reasoning_effort"` field (the file already runs mock-server
   fixtures; today only the pure customizer unit test pins this field).

8. [x] Documentation (same change set, per project rule):
   - [x] `docs/features.md` — Model Provider reasoning section (three-state
     list ~157-166): Ollama models now advertise concrete thinking levels
     from `/api/show` `thinking.values`; the picker's default slot shows the
     provider-advertised default when one is known; capability-only models
     remain `Some([])`.
   - [x] `docs/configuration.md` — `reasoning` row (line 253): document the
     new pre-first-chat validation (strict membership; drop-to-default with a
     warning; no validation when the model is absent from the catalog or the
     fetch fails).
   - [x] `docs/implementation-notes.md` — (a) the `OllamaProvider` paragraph
     (~83-99) describing `thinking.values`/`thinking.default` parsing, the
     `false`→`none` / `true`→`high` mapping, and enrichment precedence (show
     metadata upgrades a capability-derived `Some([])`); (b) the
     `ModelInfo.reasoning_efforts` three-state paragraph (~110-117) gains
     `reasoning_default`; (c) the hydration section (~129-142) reflects the
     rename and the reasoning validation rules; (d) record that OpenRouter's
     `reasoning` object also carries `mandatory` and `default_enabled`,
     which Ylva intentionally does not consume (they do not change what is
     sent; `default_effort` already surfaces fallback behavior) — this note
     is an explicit deliverable.
   - [x] Re-read each edited passage to confirm no text still claims Ollama
     exposes no selectable alternatives or that profile reasoning is
     unvalidated.

9. [x] Verify: `make format lint`, then the single mandatory gate
   `make test-coverage` (enforces the ≥80% coverage threshold; do not also
   run `make test`). Fix all clippy warnings, compile fallout from step 1's
   literal sweep, and coverage gaps before reporting done.

10. [x] Surface a dropped profile effort as a user-facing notice — TUI
    transcript notice, headless `---[ Notice ]---` marker (the first
    *Proposed Improvements* item), pull-based so the engine stays UI-free:
    - [x] `Engine::take_pending_notices(&self) -> Vec<String>` (default
      empty) in `src/llm/engine.rs`; `EngineImpl` keeps a
      `Mutex<Vec<String>>` filled by `validate_profile_reasoning_effort`
      only when the reset to the provider default succeeded — message names
      provider connection, model, and dropped effort, mirroring the
      `/models` switch notice wording; `EngineRouter::take_pending_notices`
      drains every configured engine so a mid-request connection switch
      cannot strand a notice.
    - [x] New `UiUpdate::Notice { text, run_id }` in `src/agent.rs`, drained
      in `start_request` right after `engine.chat(...)` resolves (hydration
      has completed by then) and before `UiUpdate::StreamStarted`, tagged
      with the loop's own `run_id` (same routing rule as `UiUpdate::Error`).
    - [x] TUI driver handles it directly in `process_one_update` like
      `UiUpdate::ToolError` (routed `ChatEntry::Notice`, no `DomainEvent`,
      mapped to `None` in `ui_update_to_domain_event`); `src/cli_driver.rs`
      prints it instead of dropping it — flush the current section, write a
      `---[ Notice ]---` marker (theme `NOTICE_COLOR`) plus the text, tag
      child notices `[child <id>] notice: …` like child errors, and never
      set `response_finished` (a notice does not end the turn).
    - [x] Tests: engine drop yields exactly one notice and a second take is
      empty, keep-cases yield none, router drains its engines; the agent
      loop forwards `Notice` before `StreamStarted`; the driver renders it
      as a `ChatEntry::Notice`; the headless driver prints the marker ahead
      of the response.
    - [x] Documentation: `docs/features.md` (the profile-reasoning paragraph
      gains that a dropped effort is also surfaced as a notice above the
      first response) and `docs/implementation-notes.md` (hydration section:
      the one-shot `take_pending_notices` plumbing; CLI driver section: the
      `---[ Notice ]---` rendering).

## Proposed Improvements

- [x] Surface a dropped profile effort as a user-facing notice (today it is
  log-only via `tracing::warn!`); would require plumbing a one-shot message
  from the hydration path into the driver. — implemented as step 10.
- Assert in an e2e test that the profile effort is present on the wire after
  fixture advertising (step 6 makes this true; step 7 pins it for the
  non-e2e path only).

## Consistency Issues

- [low] `docs/configuration.md:253` and the `EngineConfig.reasoning` doc
  comment (`src/llm/engine.rs:81-86`) both currently promise that profile
  reasoning is applied at construction with validation only at `/models`
  switch time. Step 5's strict pre-chat validation reverses that promise —
  both texts must be updated in the same commit (steps 5 and 8) or docs and
  behavior contradict each other.

## Implementation Hints

- **Upstream API (Ollama ≥ PR #18473 release):** `GET /api/tags` entries carry
  `name`/`capabilities`/`context_length` but no `thinking` object; `POST
  /api/show` returns top-level `capabilities` plus optional `thinking`
  (`{ "values": [...], "default": ... }` — `api.ShowResponse.Thinking`,
  struct `model.Thinking{Values []any; Default any}`). Valid metadata
  guarantees non-empty distinct bool/string values with `default` among them
  — so the displayed default is always one of the selectable options after
  the bool mapping. Metadata presence, not server version, drives all
  fallbacks; never parse a version string.
- **Wire behavior of `/v1/chat/completions` (old and new Ollama alike):**
  top-level `reasoning_effort` is accepted (nested `reasoning.effort` also
  works but is unneeded). Server conversion: absent/empty → model default;
  `"none"` → `think: false`; with valid metadata all other strings pass
  through (unknown names fall back to the model default); without metadata,
  legacy validation accepts `minimal|low|medium|high|max|xhigh|ultra`
  (`minimal`→`low`, `xhigh`/`ultra`→`max`) and 400s anything else. Boolean-only
  models resolve any non-`none` effort to `think: true` — hence `true`→`"high"`
  works there, and `"high"` is a real level for named models. Known edge
  case: a model advertising both `true` and names but not `"high"`
  (e.g. `[false, true, "medium"]`) receives `"high"`, which the renderer
  falls back to the model default for; Ylva cannot express boolean `true`
  through `reasoning_effort` at all, so this is the least-bad mapping
  (Ollama's own Codex integration labels boolean-on `"high"`).
- **OpenRouter facts (verified against `GET https://openrouter.ai/api/v1/models`):**
  188 models expose `reasoning.default_effort` alongside
  `supported_efforts`, always a member of it; the same object carries
  `mandatory` and `default_enabled`, which Ylva does not consume (recorded in
  docs per step 8). Ylva parses `reasoning.supported_efforts` at
  `src/llm/openai_compatible_provider.rs:665-681`; nothing reads a default
  today. The generic OpenAI-compatible provider shares `parse_model_list`, so
  any server emitting this shape gets `reasoning_default` for free.
- **Existing Ylva flow:** `ModelInfo` three-state doc `src/llm.rs:70-89`;
  Ollama customizer sends `reasoning_effort` only when `Some`
  (`src/llm/ollama_provider.rs:58-65`), invoked at
  `src/llm/openai_compatible_provider.rs:384`; picker cycling
  `src/tui_reducer_state.rs:2406-2446` (index 0 = `None` = send nothing);
  label builder `format_model_choice_label` `src/tui_reducer_state.rs:1160-1190`;
  switch validation `src/llm/engine.rs:393-405`; hydration pattern
  `src/llm/engine.rs:748-789` (one-shot guard, no retry on failure, skip when
  modalities already known).
- **Validation skip-path invariant:** `set_input_modalities` is only reached
  via `switch_model_option` (which validates and sets the effort) or via the
  hydration fetch itself — so "modalities already known → no fetch" never
  skips validation of an unvalidated profile effort. A failed first fetch
  means the effort is never validated (guard set, no retry) — accepted,
  mirroring modality hydration's posture.
- **Label churn is minimal:** existing label/footing test fixtures construct
  `ModelInfo` without a default (`reasoning_default: None`) → rendered strings
  are byte-identical, so existing assertions keep passing; add new fixtures
  with `reasoning_default: Some(..)` for the new suffix cases. The literal
  `"default"` already means "no explicit effort" in the footer
  (`src/tui_widgets.rs:2536`) and logs (`src/llm.rs:104-106`); a model
  advertising a level literally named `"default"` would be ambiguous —
  pre-existing free-form caveat, not introduced here.
- **Fixtures:** `tests/common/mock_llm_server.rs` currently emits no
  reasoning metadata at all (476-536); the e2e profile sets
  `reasoning = "max"` but nothing asserts it end-to-end, so without step 6
  the strict rule silently neutralizes it. `MockProvider.list_models` returns
  `vec![]` (`src/llm/mock_provider.rs:70-72`) → "model absent" branch is a
  no-op in mock-driven tests.
- **Tests to run:** in-source unit tests per steps 2-5; before writing tests
  involving `tracing`, cwd, or integration side-effects, check
  `docs/known-test-issues.md`. Final gate: `make format lint` +
  `make test-coverage` (≥80% coverage, see `docs/development-workflow.md`).
