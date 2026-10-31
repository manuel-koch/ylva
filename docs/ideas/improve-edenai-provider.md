# Improve EdenAI reasoning-effort handling

Research date: 2026-10-31.

Status: findings and proposed solutions, not an approved implementation plan.
No provider code or existing configuration was changed during this research.

## Summary

EdenAI supports a documented, top-level `reasoning_effort` parameter on
`POST /v3/chat/completions`. Its model catalog does not provide useful model-specific
allowed effort values. Ylva currently interprets that gap too restrictively: it discards
EdenAI reasoning metadata, rejects explicit efforts during catalog validation, and
never serializes an effort into EdenAI chat requests.

The reported absence of thinking bubbles is a different issue. Both models in the
captured conversation reported reasoning tokens. Their responses contained no readable
thinking text, only empty signed Claude thinking blocks or encrypted GPT reasoning items
with empty summaries. Sending a larger effort would not itself make that reasoning visible.

Recommended direction:

- Preserve reasoning capability independently of selectable effort metadata.
- Use advertised model-specific efforts when available.
- Allow an explicitly chosen, clearly labelled unverified effort when EdenAI advertises
  reasoning support but does not enumerate efforts.
- Validate the outgoing value against EdenAI's request contract without claiming that
  every model accepts every gateway value.
- Send the selected value through the existing EdenAI request-body customizer.
- Keep provider defaults when no effort is selected.
- Handle reasoning visibility and reported reasoning-token usage as separate follow-up work.

## Scope and evidence limits

This report covers catalog interpretation, engine configuration, runtime model selection,
validation, and Chat Completions request serialization. It also explains why the captured
conversation had no visible thinking.

Research used repository inspection, existing HTTP dumps, and public GET requests.
No credentials or paid inference calls were used. A guessed GitHub OpenAPI file returned
404; the live EdenAI OpenAPI endpoint provided the authoritative request schema instead.
No current public EdenAI server implementation was available to verify adapter behavior.

The schema proves the documented gateway request shape. EdenAI's official integration
instructions also describe parameter pass-through. Neither establishes accepted effort
values, default overrides, or thinking-display translation for every upstream model.
Public schemas and catalogs are mutable; counts and enum values below are snapshots.

## Captured conversation findings

The input was `tmp/tui_20261031155925.log` and the HTTP request/response dumps referenced
by its `dump_id` fields. All 57 logged chat requests were inspected, including one
session-title request.

| Observation | Claude Haiku 5.5 through Amazon | GPT-6 Luna through Azure |
| --- | ---: | ---: |
| Requests | 13 | 44 |
| Requests reporting positive reasoning-token usage | 12 | 20 |
| Total reported reasoning tokens | 15,400 | 11,838 |
| Maximum reasoning tokens in one request | 4,096 | 1,574 |
| Readable thinking characters | 0 | 0 |

Exact model IDs:

- `amazon/anthropic.claude-haiku-5-5@eu`
- `azure/gpt-6-luna@eu`

Every request omitted `reasoning_effort`, `reasoning`, and `thinking`.
The log's `reasoning_effort=default` describes an omitted explicit setting, not a
literal `"default"` request value or a known upstream effort.

Claude returned 12 signed thinking blocks with empty `thinking` strings and empty
`reasoning_content`. GPT returned 34 encrypted reasoning items across 20 responses;
every `summary` array was empty. These structures are evidence of hidden reasoning,
not readable reasoning fragments.

Representative local artifacts, relative to the workspace root:

- `tmp/tui_20261031155925_1793458819_825_3a79835a44954009ba29ab7b5a44d332_response.jsonl`:
  line 1 contains an empty signed thinking block; line 10 reports 36 reasoning tokens.
- `tmp/tui_20261031155925_1793460931_384_a0d483dec3364e0c935c52b04b7d6591_response.jsonl`:
  line 7 contains encrypted reasoning with an empty summary; line 9 reports 30
  reasoning tokens.

The artifacts are temporary and may not exist in another checkout. The findings above
are included here so the report remains understandable without them. Signatures,
encrypted reasoning payloads, credentials, and conversation content are not reproduced.

## EdenAI API findings

### Catalog capability does not enumerate selectable efforts

The public `GET /v3/models` snapshot contained 1,291 entries. Of those, 919 advertised
`capabilities.supports_reasoning: true`. No entry advertised a nonempty
`capabilities.reasoning.supported_efforts` list.

The two captured model entries advertised reasoning support but had no nested
`capabilities.reasoning` object. Other entries returned objects such as:

```json
{
  "supports_reasoning": true,
  "reasoning": {
    "mandatory": true,
    "default_effort": null,
    "supported_efforts": []
  }
}
```

These are distinct facts:

- Reasoning capability is advertised.
- Selectable effort values are not enumerated.
- The catalog does not specify a usable default effort.

An empty effort list is not proof that the gateway rejects an effort parameter.
Likewise, `mandatory: true` should not be used to invent a default or force a request
setting. Its semantics and adapter behavior remain unverified.

### Chat Completions has a documented effort field

The live OpenAPI operation `/v3/chat/completions` references `LlmCompletionBody`.
Its optional nullable `reasoning_effort` property accepts:

`minimal`, `low`, `medium`, `high`, `max`, `xhigh`, `disable`, `none`.

The property description is "The reasoning effort level for the LLM model."
No schema default is declared. The following illustrates the wire shape, not a
verified request for either captured model:

```json
{
  "model": "provider/model",
  "messages": [{"role": "user", "content": "Solve this task."}],
  "reasoning_effort": "medium",
  "stream": true,
  "stream_options": {"include_usage": true}
}
```

This enum is gateway-wide. It must not be presented as the model's supported-value list.
In particular, do not assume `medium`, `max`, `xhigh`, `none`, and `disable` work on
all reasoning models. Do not normalize `disable` to `none` without a documented mapping.

The same schema declares an optional `thinking` object and an `extra_body` object for
additional parameters passed to the provider API. EdenAI's official integration skill
explicitly says standard OpenAI parameters, including `reasoning_effort`, pass through.

### Narrative documentation has gaps

EdenAI's Chat Completions guide documents Claude extended thinking but omits
`reasoning_effort` from its available-parameters table. Its Claude section uses Gemini
model IDs in examples and gives a questionable budget comparison. The live schema is
better evidence for gateway field acceptance; native provider documentation is needed
for model-specific thinking modes and budgets.

The existing Ylva claim that EdenAI defines no effort request shape is stale.
It appears in `src/llm/edenai_provider.rs` and [provider notes](../provider-notes.md#edenai).

## Current Ylva behavior

Relevant implementation locations:

- `src/llm.rs`: `ModelInfo`, `advertises_reasoning_effort`, and effort ordering.
- `src/llm/edenai_provider.rs`: `EdenAiModelCatalog` and `customize_request_body`.
- `src/llm/engine/runtime.rs`: lazy profile hydration and
  `validate_profile_reasoning_effort`.
- `src/llm/engine/selection.rs` and `src/llm/engine/router.rs`: runtime selection.
- `src/tui_driver/models.rs`: model and parameter selection UI.
- `src/llm/openai_compatible_provider.rs`: request assembly and SSE parsing.

`ModelInfo.reasoning_efforts` currently has three documented states:

- `None`: reasoning support is not advertised.
- `Some([])`: reasoning is supported without selectable alternatives.
- `Some(values)`: reasoning is supported with advertised selectable values.

EdenAI currently hardcodes `None` and no default for every model. Its test
`catalog_never_reports_reasoning_efforts` enforces that behavior even if a fixture
contains nonempty effort metadata.

Both lazy profile validation and runtime selection use advertised-value membership.
An explicit profile `reasoning` value is dropped to the provider default if a listed
model does not advertise it. A catalog fetch failure or absent model preserves the
value unvalidated, but that does not solve EdenAI serialization.

EdenAI's request customizer adds only `session_id` and ignores the runtime effort.
The shared chat transport delegates provider-specific fields to that customizer.
Thus simply changing configuration or relaxing validation is insufficient: the selected
value must also reach the outgoing JSON body.

## Possible solutions

### Option A: preserve capability and only accept advertised efforts

Parse `supports_reasoning` and any real effort list. Use `Some([])` for a
reasoning-capable EdenAI model without enumerated alternatives. Keep strict membership
validation and omit the request field unless the catalog supplies an accepted choice.

Advantages:

- Small change with no unverified model-specific choices.
- Future nonempty catalog lists can work once serialization is added.

Limitations:

- Does not let users tune the current catalog's models.
- Still conflates missing enumeration with absence of configurable alternatives.

This is a useful baseline, but it does not fully address the user's configuration need.

### Option B: explicit unverified override for incomplete EdenAI metadata

Recommended near-term direction. Preserve advertised capability and introduce a narrow
EdenAI validation policy for explicit efforts when model-specific enumeration is absent.
The chosen value must belong to the gateway contract and carry unverified status.

Proposed policy:

| Model metadata | Explicit selection behavior |
| --- | --- |
| No effort selected | Keep provider default; omit the request field |
| Nonempty advertised effort list | Require membership; mark the choice advertised |
| Reasoning supported, effort enumeration absent or empty | Allow an explicit gateway value with an unverified warning |
| Reasoning explicitly unsupported | Reject explicit effort before dispatch |
| Model absent or catalog unavailable | Preserve existing unvalidated behavior, subject to EdenAI wire-value validation |
| Contradictory metadata | Report the conflict; do not invent selectable choices |

Apply the same effective policy to profile hydration and `/models` selection. Do not
relax validation globally for Copilot, Ollama, OpenRouter, or plain OpenAI-compatible
providers. A model switch must revalidate the effort against the target model and must
not mutate the active engine before validation succeeds.

For the UI, either retain a default-only row until an explicit override is supplied,
or provide a deliberately marked unverified selection mode. Do not place the full
gateway enum in the ordinary model-advertised picker. Display the effective effort and
its provenance separately from the provider default.

Keep the existing engine `reasoning` setting as the value source if practical. If an
additional opt-in setting is needed, define its scope and precedence before implementing
it; this report does not introduce a new configuration key.

Advantages:

- Allows deliberate tuning without fabricating model metadata.
- Preserves provider defaults and strict behavior elsewhere.
- Makes uncertainty visible instead of silently dropping the user's setting.

Risks:

- A gateway-valid value may still fail or be ignored upstream.
- UI and validation need a way to represent unverified selections.
- Warnings must be clear without repeating on every tool continuation.

### Option C: verified compatibility mappings

Maintain model/provider-specific allowed values and defaults where EdenAI confirms
support or controlled inference tests verify it. Prefer live catalog values when they
become available and retain provenance for any fallback mapping.

This can provide a normal picker for verified models, but broad family-name heuristics
are brittle. Azure, Amazon, Vertex, aliases, and region-qualified IDs may behave
differently. Do not derive model support from an unrelated provider's catalog or assume
that an alias will keep resolving to the same model.

Use this only for demonstrated gaps that Option B cannot adequately handle. A large
hand-maintained compatibility database is not justified by the current evidence.

## Recommended request path

The intended path is:

1. Read raw EdenAI capability and effort metadata without discarding it.
2. Resolve the engine's configured or runtime-selected model and effort.
3. Validate using model metadata plus the EdenAI-specific explicit-override policy.
4. Keep the validated selection in the existing engine runtime configuration.
5. Have `EdenAiProvider::customize_request_body` add top-level `reasoning_effort`
   when the runtime value is present.
6. Omit the field when the user selects the provider default.
7. Preserve `session_id`, usage options, tools, routing, and existing request headers.

Use one validation policy for configuration and runtime selection. The catalog and UI
must not maintain separate, inconsistent lists of accepted values. Keep any code-owned
EdenAI wire enum in one place; do not fetch OpenAPI on every chat request.

Do not automatically set `high` or `medium`, send literal `"default"`, or silently
translate effort into a Claude token budget. Do not send both `thinking` and
`reasoning_effort` without defining provider-specific precedence.

An unsupported-parameter response should name the model and selected effort. Avoid
silently retrying at a different effort: that changes requested behavior and could
repeat billable work. Any retry fallback needs an explicit policy and user-visible notice.
A successful request alone also does not prove that an upstream adapter honored the value.

## Reasoning visibility is separate

Distinguish these concepts in behavior and documentation:

- Reasoning capability: whether the model can reason.
- Thinking mode: whether thinking is active, adaptive, or disabled.
- Effort: how much computation the selected mode should spend.
- Display: whether readable summaries are returned.
- Usage: how many reasoning tokens the provider reports.

Anthropic's native documentation lists Haiku 5.5 as adaptive by default with thinking
display omitted. Its legacy `enabled` plus `budget_tokens` configuration is unsupported.
The captured empty signed blocks match omitted display, but the exact Amazon adapter
configuration was not inspected.

OpenAI and Azure do not expose raw reasoning through ordinary Chat Completions content.
Responses reasoning summaries require a separate opt-in, such as:

```json
{
  "reasoning": {
    "effort": "medium",
    "summary": "auto"
  }
}
```

EdenAI's `/v3/responses` schema accepts a generic `reasoning` object. The example above
is a native OpenAI-style configuration, not a verified EdenAI request for GPT-6 Luna.

Possible follow-up improvements:

- Record `usage.completion_tokens_details.reasoning_tokens` independently of input
  tokens and cost. Report missing usage as unknown, not zero.
- Show a completed-response indication that reasoning occurred but text was not exposed.
  Final usage cannot reliably drive a live "thinking" state before it arrives.
- Parse readable summaries from supported structured reasoning fields if real captures
  demonstrate them. Empty signatures and encrypted payloads are not displayable text.
- Investigate whether Amazon-backed Claude accepts and honors
  `thinking.display: "summarized"` through EdenAI.
- Investigate EdenAI Responses summaries for GPT without assuming a catalog endpoint
  preference that EdenAI has not advertised.

These are not prerequisites for correctly sending a selected Chat Completions effort.
Do not expand the effort change into a new provider transport or transcript format
unless separately approved.

## Verification for a future implementation

Write regression tests before changing behavior at the relevant seams.

Catalog tests should cover:

- Reasoning support with absent, empty, and nonempty effort metadata.
- Explicitly unsupported reasoning and missing capability information.
- Null or malformed reasoning objects and contradictory metadata.
- An advertised default only when it belongs to the advertised effort list.
- Existing text-output, function-calling, context-window, and modality filtering.

Configuration and selection tests should cover:

- Omitted effort retaining provider-default behavior.
- Advertised values accepted and nonmembers rejected.
- Explicit unverified EdenAI values preserved with a notice.
- Gateway-invalid values rejected even when the catalog cannot validate the model.
- Model switches revalidating without partial engine mutation.
- Other providers retaining their existing validation policy.

Request tests should exercise real request construction with a local HTTP test double:

- The explicit runtime value reaches top-level `reasoning_effort`.
- Default selection omits the field rather than sending null or `"default"`.
- Profile-selected and UI-selected efforts use the same request path.
- Independent engines sharing a provider connection do not leak settings.
- `session_id`, streaming usage options, tools, and headers remain unchanged.
- Unsupported-effort errors remain visible without a silent default retry.

A controlled live check requires separate consent because it sends inference requests.
Pin exact provider/model IDs, hold prompts constant, and compare default with one
explicit effort at a time. Record request JSON, response errors, reasoning usage,
readable summaries, latency, and cost without logging credentials. Repeat enough to
avoid treating natural output variation as proof that effort was honored. Do not expect
higher effort to guarantee more tokens or visible thinking on every request.

For code changes, follow [development workflow](../development-workflow.md), including
`make format lint` and `make test-coverage`. Run `make audit` if dependencies change.

## Documentation affected by implementation

An approved implementation plan should name and update:

- [Provider notes](../provider-notes.md): replace the unsupported-request-shape claim
  and distinguish gateway support from model-specific verification.
- [Configuration](../configuration.md): explain explicit effort validation, defaults,
  unverified status, and any new opt-in setting if one is approved.
- [Features](../features.md): describe catalog states and model-picker behavior.
- [Implementation notes](../implementation-notes.md): document the shared validation
  policy and EdenAI request serialization.
- [Keybindings](../keybindings.md): update only if the parameter picker gains controls.
- `CONTEXT.md`: update if reasoning terminology or selection provenance becomes a
  new domain concept.

The existing ADRs cover git conventions, releases, and workspace configuration trust,
not an EdenAI reasoning contract. Preserve workspace trust and routing boundaries.
If a future change introduces a cross-provider metadata redesign, assess whether it
needs a new ADR rather than hiding the decision in provider-specific code.

## Sources

Primary EdenAI sources:

- [Live OpenAPI schema](https://api.edenai.run/v3/docs/openapi.json), especially
  `LlmCompletionBody`, `ResponsesBody`, and `ChatCompletionMessage`.
- [Chat Completions reference](https://www.edenai.co/docs/api-reference/chat/chat-completions).
- [Public model catalog](https://api.edenai.run/v3/models).
- [Model listing guide](https://www.edenai.co/docs/v3/llms/listing-models).
- [Chat Completions guide](https://www.edenai.co/docs/v3/llms/chat-completions).
- [Inspected guide revision](https://github.com/edenai/docs/blob/d7224ad3d8c22fa5891c60e95b801fc46eb1703b/v3/llms/chat-completions.mdx).
- [Official EdenAI integration skill](https://github.com/edenai/edenai-skill/blob/master/SKILL.md).
- [Official Pi integration](https://github.com/edenai/pi-edenai/blob/main/index.ts),
  which recognizes reasoning from `supports_reasoning` rather than an effort list.

Native provider sources, not proof of EdenAI adapter translation:

- [OpenAI reasoning guide](https://developers.openai.com/api/docs/guides/reasoning).
- [Azure reasoning guide](https://learn.microsoft.com/en-us/azure/foundry/openai/how-to/reasoning).
- [Anthropic thinking modes and display](https://platform.claude.com/docs/en/build-with-claude/thinking).
- [Anthropic effort](https://platform.claude.com/docs/en/build-with-claude/effort).
- [Anthropic manual thinking and budgets](https://platform.claude.com/docs/en/build-with-claude/extended-thinking).
- [Gemini OpenAI compatibility and thinking controls](https://ai.google.dev/gemini-api/docs/openai#thinking).

Gemini also separates effort from thought-summary exposure. Its native OpenAI-compatible
mapping must not be assumed to match EdenAI's Google or Vertex adapters. No Gemini
model was used in the captured conversation.
