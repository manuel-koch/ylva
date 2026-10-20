# Anthropic provider

Add a direct provider implementation for the Anthropic API. It is not
implemented as a named provider connection today. Anthropic models may be
reached through OpenRouter, but that is not a direct Anthropic integration.

## Research findings

Researched 2026-10-20.

Sources: Anthropic API docs at <https://platform.claude.com/docs> (Messages,
streaming, models list, thinking, effort, prompt caching, errors, OpenAI SDK
compatibility). Nothing below was tested against a live account.

### Option A: OpenAI-compatible endpoint (rejected)

Anthropic offers `https://api.anthropic.com/v1/` as a Chat Completions
compatibility layer, so the existing shared OpenAI-compatible transport could
reach it with only a new connection kind. Anthropic itself says it is meant
for testing and comparison, not production. Limitations:

- Prompt caching is unsupported. A long agent loop would pay full input price
  on every request.
- Thinking and reasoning blocks are not carried properly. The native API
  requires thinking blocks (with `signature`) to be passed back unmodified
  alongside `tool_use` blocks.
- `strict` tool schemas are ignored; PDF input, citations and other native
  features are unavailable.
- System/developer messages are hoisted into one `system` string.

Verdict: a native Messages API provider is the right target.

### Option B: native Messages API (recommended)

Wire protocol differences from the OpenAI Chat Completions transport:

| Aspect | Anthropic Messages API |
| --- | --- |
| Endpoint | `POST /v1/messages`, `stream: true` (SSE) |
| Auth headers | `x-api-key`, `anthropic-version: 2023-06-01`, optional `anthropic-workspace-id` (needed only for keys that span workspaces) |
| System prompt | top-level `system`, not a message role |
| Required fields | `max_tokens` is mandatory; per-model cap is `max_tokens` in the models list |
| Tool schemas | `tools: [{name, description, input_schema}]` (no `function` wrapper) |
| Tool call | assistant `tool_use` content block (`id`, `name`, `input`) |
| Tool result | user message with `tool_result` block (`tool_use_id`, `content`, `is_error`); all results of one assistant turn go in one user message |
| Roles | strictly `user`/`assistant`; Ylva's `tool` role messages must be mapped |
| Reasoning | `thinking` blocks with `signature`; must be replayed unmodified with tool results |
| Effort | `output_config.effort` (`low`, `medium`, `high`, `xhigh`, `max`), model dependent |

Streaming event flow: `message_start`, then per block
`content_block_start` / `content_block_delta` / `content_block_stop`, then
`message_delta` (carries `stop_reason`, e.g. `end_turn`, `tool_use`), then
`message_stop`. `ping` events may appear anywhere. Delta types: `text_delta`,
`input_json_delta` (partial JSON string per tool call, assembled by `index`),
`thinking_delta`, `signature_delta`. Errors may arrive mid-stream as
`event: error` (for example `overloaded_error`).

Mapping to Ylva's `StreamEvent`: `text_delta` maps to content deltas,
`thinking_delta` to reasoning deltas, and `input_json_delta` fragments to
tool-call fragments (the agent's streaming task keeps assembling them, as
required by the `Provider` contract). The `signature` and thinking blocks
need a place in the stored message history so they can be replayed; the
current `Message` type likely has none (to verify).

### Models and capabilities

`GET /v1/models` (cursor pagination, `limit` up to 1000) returns per model:
`id`, `display_name`, `max_input_tokens`, `max_tokens`, and a `capabilities`
object with `image_input`, `pdf_input`, `effort` (per-level support flags),
`thinking.types` (`adaptive`, `enabled`, `disabled`, `between_tools`),
`batch`, `citations` and more. This maps well onto `ModelInfo`:

- `context_length` from `max_input_tokens`
- `reasoning_efforts` from the supported `effort` levels
- `input_modalities` from `image_input` / `pdf_input`

`reasoning_default` is not advertised; leave `None`.

### Images and PDFs

Ylva's `read` tool (and `web_fetch`) returns PNG/JPEG/WebP/GIF/PDF as a typed
attachment over a Base64 result; see
[binary attachments for `read`](../features.md#binary-attachments-for-read).
The OpenAI-compatible transport has to work around Chat Completions by
adding a hidden wire-only user message after the tool message. The native
Messages API is a better fit:

- A `tool_result` block's `content` may be an array of `text`, `image` and
  `document` blocks (also search results and others), so attachments travel
  inside the tool result. No synthetic user message is needed.
- Image block: `{"type": "image", "source": {"type": "base64", "media_type":
  "image/png", "data": "…"}}`. Sources may also be a URL or a Files API
  `file_id`.
- PDF block: `{"type": "document", "source": {"type": "base64", "media_type":
  "application/pdf", "data": "…"}}`. Sources may also be a URL or `file_id`.
  Ylva sends Base64 only: URL sources need public hosting and the Files API
  needs an extra upload step with a 500 MB per-file limit.
- PDF limits: 32 MB request size (the whole payload, not just the PDF),
  600 pages per request (100 on models with a context window under 1M
  tokens), standard PDFs only (no password or encryption). Dense PDFs can
  fill the context before the page limit. Each page costs both extracted
  text and an image-rendered page, so token cost is high.
- PDFs are supported on the native API only, not through the
  OpenAI-compatible layer. Non-PDF documents (`.docx`, `.xlsx`) are not
  accepted; plain text goes as `text/plain` document or ordinary text.
- Media types accepted for images: PNG, JPEG, WebP and GIF; this matches the
  four types Ylva already detects. Image size and count limits were not
  extracted during this research; read the vision docs when implementing.
- Capability gating: `ModelInfo.input_modalities` comes from
  `capabilities.image_input.supported` and `capabilities.pdf_input.supported`
  in `GET /v1/models`, so unlike OpenRouter and Ollama the catalog is
  authoritative. Unsupported modalities keep the Base64 text result, as today.
- A tool result that is an error (`is_error: true`) with an attachment is
  not documented; keep attachments to successful results.
- Request-dump redaction must also cover Anthropic's `source.data` field, not
  only the OpenAI `image_url` / `file` shapes.
- Prompt caching: attachments in earlier turns stay in the cached prefix, so
  re-sending history is cheap while caching is on.
- Open question: whether to wire user-supplied attachments (not tool
  results) as top-level `image`/`document` blocks in user messages; Ylva has
  no such user-attachment input today.

### Thinking and effort

- Current models (Opus 5.x, Sonnet 5.x, Haiku 5.5 and others) have adaptive
  thinking on by default; manual `budget_tokens` returns 400 on them. Older
  4.6/4.7/4.8 models need `thinking: {type: "adaptive"}` to enable it.
- `display` defaults to `"omitted"` (empty thinking text, still billed and
  still needs replay). Send `display: "summarized"` to show reasoning in the
  TUI.
- Per-model support differs (some reject `disabled`), so the request builder
  must be driven by the models list, not hardcoded names.
- Forced `tool_choice` is incompatible with some models; Ylva uses the
  default `auto`.

### Prompt caching

- Not enabled by default. Add top-level `"cache_control": {"type":
  "ephemeral"}` for automatic caching of the growing prefix (tools, system,
  messages). Default TTL is 5 minutes; 1 hour costs 2x the base input price.
- Usage reports `cache_creation_input_tokens` and `cache_read_input_tokens`
  next to `input_tokens`. Note `input_tokens` excludes cached tokens, so the
  context-size estimate must sum all three.

### Usage and cost

- `message_start.usage` has input and cache token counts; `message_delta.usage`
  has cumulative `output_tokens`. Take the last value, do not sum deltas.
- The API reports no per-request cost. By the policy in
  [provider-notes](../provider-notes.md) Ylva records no cost for Anthropic
  (same as Ollama). Add an Anthropic row to the usage table when implemented.

### Errors and limits

- HTTP errors: 400 `invalid_request_error` (also spend limit reached), 401,
  403, 404, 413, 429 `rate_limit_error`, 500 `api_error`, 529
  `overloaded_error`. Honor `retry-after`. A tier spend-cap 429 has no
  `retry-after` and must not be retried blindly.
- Every response has a `request-id` header; log it for diagnostics.
- Rate-limit headers: `anthropic-ratelimit-{requests,tokens}-{limit,remaining,reset}`.

### Implementation sketch

- New `ProviderKind::Anthropic` and `src/llm/anthropic_provider.rs` (plus an
  `anthropic` submodule for the request builder and SSE parser, in the style
  of `openai_responses`), registered in `provider_registry.rs`.
- Reuse `reqwest`, the shared HTTP helpers (retry, request dumps) and the
  existing SSE framing; only the event parser and message mapping are new.
- Config: API key (env var, never logged), optional base URL and workspace
  id, default model, effort.
- Not in scope initially: batch API, citations, files API, server-side
  tools, 1M-context beta, structured outputs.

### Open questions

- How to store thinking `signature` blocks in `Message` and sessions, and
  whether to drop them on compaction (allowed outside tool-use turns).
- Whether to enable automatic prompt caching by default (cache writes cost
  more than plain input; likely worth it for agent loops).
- Whether to also support OAuth / Claude subscription auth; the docs describe
  only API keys, so treat it as out of scope.
- Choose a default `max_tokens` when the model list lacks one.
- Smoke-test against a live key before documenting behavior as verified.
