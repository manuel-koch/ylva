# Render readable EdenAI reasoning summaries

## Context

This plan addresses missing thinking bubbles for EdenAI Chat Completions sessions. In the observed
session, reasoning-token usage was reported, but streamed chunks carried `reasoning_items`
containing encrypted content and empty summaries. Ylva's parser ignored `reasoning_items`, so no
readable thinking text reached the existing TUI bubble path. The evidence indicates an
upstream-content limitation as well as an unparsed summary shape, not a defect in subagent thinking
rendering.

The goal is to parse and render readable reasoning summaries from EdenAI Chat Completions, without
inventing content or displaying encrypted payloads. Request a summary only if the exact parameter
shape and constraints are documented for the Chat Completions endpoint. Do not use the Responses
API's `reasoning.summary` option as evidence for Chat Completions support, and do not switch EdenAI
to Responses for this purpose. Do not add a placeholder for hidden reasoning or expose
reasoning-token usage just to construct one.

The original investigation is recorded in `docs/ideas/edenai-hidden-thinking.md`; this plan is
self-contained and does not require it for implementation. EdenAI's public Chat Completions
reference documents `reasoning_effort` and a generic `thinking` object, but does not specify a
reasoning-summary request field. The implementation therefore parses returned summaries only.
Empty summaries may be deployment-specific; scope is protocol-level testing with local fixtures,
not a live inference or multi-route matrix.

### Acceptance criteria

- Chat Completions streaming emits non-empty readable text from EdenAI
  `reasoning_items[].summary[].text` as ordinary thinking content.
- Empty summaries and encrypted-only items emit no thinking text; encrypted payloads are never
displayed or treated as prose.
- No summary-request option is sent because the official Chat Completions reference and OpenAPI
  schema do not document an exact reasoning-summary request field.
- Existing root and subagent thinking bubbles render summary fragments through their current event
  path.
- Local parser tests cover summary-present, summary-empty, encrypted-only and unrelated payloads;
  standard OpenAI-compatible behavior remains covered by regression tests.
- `docs/provider-notes.md` documents summary-request status, readable-summary parsing, hidden /
encrypted reasoning limitations and deployment uncertainty for empty summaries.

### Constraints

Preserve Chat Completions transport and event ordering, existing TUI thinking bubbles and subagent
event routing, EdenAI request customization, and provider request concurrency. Keep
provider-specific changes isolated where possible. Do not show a placeholder, add reasoning-token
accounting, switch EdenAI to Responses, add a model-family compatibility table, or change session
persistence. No dependency change, new ADR, or live-provider test is required.

## Implementation Plan

1. [x] Reconfirm the documented EdenAI Chat Completions contract and establish focused baselines.
   - [x] Checked the official [Chat Completions reference](https://www.edenai.co/docs/api-reference/chat/chat-completions)
     and [OpenAPI schema](https://api.edenai.run/v3/docs/openapi.json) on 2026-11-01. The endpoint
     lists a generic `thinking` request object but no documented summary request shape or
     constraints. The separate Responses API's `reasoning.summary` option does not establish
     Chat Completions support.
   - [x] Inspected `src/llm/openai_compatible_provider.rs`, `src/llm/edenai_provider.rs` and
     their owner-local tests. Focused provider baselines passed: 41 OpenAI-compatible tests and 14
     EdenAI tests.
2. [x] Add failing local regression tests.
   - [x] EdenAI parser tests cover non-empty and multiple summary fragments, empty summary arrays
     and entries, encrypted-only items and unrelated payloads; they assert only readable summary
     text becomes thinking events.
   - [x] A regression test confirms ordinary OpenAI-compatible content retains precedence over
     EdenAI summary metadata. No request-customization test applies because no summary-request option
     is documented; existing non-EdenAI parser tests cover shared behavior.
3. [x] Implement EdenAI summary parsing and preserve standard provider behavior.
   - [x] Added an EdenAI-owned stream adapter. It delegates standard payloads to the unchanged
     shared parser, and only projects `reasoning_items[].summary[].text` when no standard event was
     produced. Empty/whitespace-only text is skipped; encrypted fields are not read.
   - [x] Routed EdenAI Chat Completions stream frames through the adapter. No request option,
     placeholder or API-path change was introduced. The emitted `StreamEvent::Thinking` uses the
     existing root and subagent event path.
4. [x] Verify local protocol behavior and update affected documentation.
   - [x] EdenAI parser tests and OpenAI-compatible provider tests passed after the change. Existing
     agent and subagent routing already forwards `StreamEvent::Thinking` through the same UI event
     path; no changes to those layers were needed.
   - [x] Updated `docs/provider-notes.md` and `docs/implementation-notes.md` with the
     request-contract status, readable-summary handling and encrypted-content limitation. Kept
     `docs/features.md` provider-neutral; the thinking-bubble contract did not change.
   - [x] No new domain term, ADR or dependency was needed. `rumdl check` passed for all four
     changed Markdown files. `make format lint` passed. `make test-coverage` passed: 2,198 tests
     passed, one was ignored, and aggregate line coverage was 88.97% (threshold: 80%). No audit was
     needed because dependencies did not change.

## Implementation Hints

- Agreed during grilling: use the existing EdenAI Chat Completions path; add a summary-request field
  only if official documentation confirms its exact shape and constraints. Do not use the Responses
  API option as evidence and do not switch APIs to obtain summaries.
- Do not add a hidden-reasoning placeholder or reasoning-token accounting. Restrict behavior to
  readable summary text actually returned by the provider.
- Test protocol behavior with local fixtures and document that empty summaries may be
  deployment-specific. A multi-route matrix and live inference are out of scope.
- The original observed session involved `azure/gpt-6-luna@eu`; `reasoning_items` carried
  `encrypted_content` and `summary: []`. These are observations from one session, not a claim about
  every EdenAI route.
- Current official Chat Completions reference:
  <https://www.edenai.co/docs/api-reference/chat/chat-completions>. Checked 2026-11-01; it
  documents `reasoning_effort` and generic `thinking`, but not a reasoning-summary request field.
  OpenAPI: <https://api.edenai.run/v3/docs/openapi.json>. These sources are mutable.
- The shared parser is `parse_sse_payload` in `src/llm/openai_compatible_provider.rs`; EdenAI's
  provider-specific adapter is in `src/llm/edenai_provider.rs`. Existing thinking events flow
  through `src/agent/request.rs`, `src/agent_factory.rs`, `src/tui_driver/updates.rs` and
  `src/tui_reducer_state/conversation.rs`.
- `src/llm/openai_responses.rs` builds `reasoning: { effort, summary: "auto" }` on the distinct
  Responses API path. This does not establish Chat Completions support.
