# Conversation compaction

Implemented for root and subagent conversations. The implementation preserves
the full session transcript while giving the model a compacted view. See
[features](../features.md#context-management-and-compaction) for the current
user-facing contract; this note retains the research and remaining ideas.

## Current implementation

The calibration follow-up is implemented. The per-message/context estimate and
the separate per-request baseline both use `tiktoken-rs` `cl100k_base`. Before
each OpenAI-compatible Chat Completions request is sent, Ylva applies that
encoding to the serialized JSON request body as text. This is not a simulation
of provider-side request processing or the selected model's tokenizer. When
`YLVA_TOKEN_USAGE_DUMP_PATH` is set, it appends a
metadata-only JSONL record containing a request ID, provider and connection,
model, estimate/source, and provider-reported input count/source if available.
The request enables streaming usage and recognizes `usage.prompt_tokens` and
`usage.input_tokens`. Missing usage, including an interrupted stream, remains
`null`. The provider-reported count is the serving-side observation when
available; the local serialized-body estimate is a comparison baseline. The
file is independent of request dumps, session persistence, and
compaction state; it contains no prompt or payload content. See the
[CLI-options record contract](../cli-options.md#debug).

### Ten-fetch incident and refined behavior

On 2026-10-06, a ten-`web_fetch` batch grew the root's estimated context from
54,292 to 99,622 against a 98,304-token window. Checks occurred only after the
batch: no summary started, and the original hard fallback dropped all 41
non-system messages. The system-only continuation answered
`What would you like me to do?`. Evidence is the local
`tests/e2e/custom/logs/tui_20261006174357.log` and adjacent request/response dumps;
deterministic regressions use generated fixtures, not copied dump contents.

The [current implementation](../implementation-notes.md#context-management-and-compaction)
replaces that fallback:
completed-prefix summaries start after tool outcomes while later tools run;
only the first six requested positions can execute, without backfill.
Calls share a measured result allowance and can pause only for an existing
summary's remaining deadline. Every continuation fits the advisory local safe
ceiling or fails explicitly. Staged model-only reductions preserve a verbatim
task anchor unless the ceiling requires disclosed shortening; they never turn
an active task into a system-only or omission-only request. Original output and
UI sections remain canonical, and persisted projections restore on resume/clone.
Failed or inadequate summaries recover lossily rather than looping. Configured
ratios and timeout defaults are unchanged.

Controlled mock-provider measurements for ten small operations that explicitly
resubmit only unfinished work: cap six uses two tool batches plus a final-answer
request (three model requests), four skipped outcomes, ten executions, and no
lossy recovery; test-only cap four uses three batches plus final answer (four
requests), eight skipped outcomes, ten executions, and no lossy recovery. Both
retain the task in every request. These are deterministic synthetic measurements,
not provider latency/cost measurements or evidence that six is optimal; the
fixed cap remains six, never adaptive or configurable.

Compaction lifecycle events also report estimated model-view effectiveness:
token estimates before and after, signed net token savings and percentage
change, plus projected message counts before and after. The summary can
increase the estimate if it is larger than the replaced prefix; the metric is
signed so this is visible. These log measurements do not use or replace
provider-reported request usage.

## Problem

Compaction needs to know how much of the active model's context window is
occupied. Context stores advisory per-message token counts; the common local
estimate uses `cl100k_base` across providers, but it does not claim to match
the active model's tokenizer:

- Every model uses `tiktoken-rs` `cl100k_base`, although OpenRouter and Ollama
  serve models with many unrelated tokenizers and OpenAI model families also
  use different encodings.
- The model-view estimate includes projected bodies, tool definitions, result
  envelopes, IDs, metadata, and a small fixed message-framing estimate. It still
  excludes provider templates, exact multimodal accounting, and other serving
  transformations; Base64 text counting is not native image/PDF token costing.
- Context entries cache canonical counts, while overridden bodies,
  summary/handoff, anchors, and schemas are measured in the current projection.
  This still does not reproduce the exact serving-side request representation.

These are not merely provider-family differences. The count can differ by
model, exact request shape, and serving implementation. A context window is
measured in the model's effective input tokens, so even the correct text
tokenizer alone cannot precisely predict every provider's accepted prompt.

## Research question

What token-count source should drive compaction thresholds and the context
readout, while remaining useful across hosted providers, OpenAI-compatible
gateways, and local inference servers?

## Findings

### 1. Provider-reported prompt usage is the strongest practical feedback

For a completed inference request, a provider's prompt/input token usage is
measured after the provider has tokenized and prepared the request for the
serving model. It therefore incorporates the actual model tokenizer and much
of the request formatting that local text tokenization misses.

- **OpenRouter:** Chat Completions supports a final streaming usage chunk when
  `stream_options.include_usage` is requested. Its usage reports
  `prompt_tokens`, including tools and images, and describes its counts as
  using the model's native tokenizer. OpenRouter can route or fall back to a
  different upstream provider, which makes its gateway-reported usage more
  useful than locally identifying a tokenizer. Its documented streaming usage
  frame may not arrive if the stream is interrupted.
- **Ollama native API:** `/api/chat` returns `prompt_eval_count` in its final
  response or final stream chunk. Ollama documents this as the number of input
  tokens in the prompt. The current implementation calls the OpenAI-compatible
  `/v1/chat/completions` endpoint instead; Ollama's compatibility documentation
  says `stream_options.include_usage` is supported, so this is likely
  accessible there too, but must be verified for supported Ollama versions and
  actual response shapes. If not, the native endpoint would require a separate
  provider transport and wire-format compatibility.
- **OpenAI Chat Completions:** `stream_options.include_usage` adds a final
  streaming chunk with request usage. The chunk can be absent on interruption.
  It is actual usage for that request, but it does not supply a count before
  the first request.
- **Other OpenAI-compatible servers:** usage extensions vary. OpenRouter and
  Ollama provide explicit evidence above; vLLM and compatible servers may
  support usage fields, but Ylva must treat usage as optional unless the
  configured provider contract guarantees it.

Usage is measured per full request, while current compaction policy wants
message/prefix sizes. Therefore provider usage cannot simply replace each
cached message count. It can calibrate an estimator or provide authoritative
measurements of the *last full model view*. On the next turn, appending a
message, changing tools, switching model, or applying a compaction changes the
payload and requires re-estimation.

### 2. Dedicated count-input endpoints offer preflight counts

But only for matching provider APIs

- **OpenAI Responses input-token endpoint:** `POST /v1/responses/input_tokens`
  accepts Responses-format input including messages, instructions, tools,
  images, and files. OpenAI describes it as counting the input model receives,
  including structural tokens and model-specific processing. This is the best
  documented preflight route for a supported OpenAI Responses request. It is
  tied to the Responses API shape; Ylva currently sends Chat Completions, so
  request conversion or a move to Responses is needed to count exactly the
  payload being generated. The endpoint is OpenAI-specific and is not implied
  by OpenAI-compatible APIs.
- **Anthropic Messages `count_tokens`:** accepts system content, messages,
  client tools, images and PDFs and returns an estimate. Anthropic explicitly
  notes small differences from actual request usage and that counts may include
  system-added tokens. It is only useful for a direct Anthropic Messages API
  integration (or a gateway that explicitly exposes that API); it does not
  count Ylva's current Chat Completions wire request.
- **Ollama:** documented `/api/chat` and `/api/generate` responses provide
  prompt counts after evaluation, not a separate preflight count endpoint in
  the API reference researched here. A throwaway non-streaming generation
  should not be used as a counter: it runs inference and can incur latency,
  resource load, or cloud charges.
- **OpenRouter:** the model catalog exposes a tokenizer-family label, not a
  token-count endpoint. Tokenizer family metadata can guide a local fallback,
  but cannot represent all provider framing or transformations.

Preflight counting is most useful for an initial request or an explicit
validation before sending a near-limit payload. Calling a remote count API on
every turn adds another network round trip, provider coupling, rate-limit/error
paths, and transmission of the same conversation content a second time. Count
APIs can themselves return estimates (Anthropic says so), and a count request
can fail independently of inference.

### 3. Model-specific local tokenizers improve fallback quality not request exactness

Tokenizer algorithms and vocabularies are model artifacts, not provider
properties. For example, `tiktoken-rs` 0.5 used here offers `cl100k_base` and
`o200k_base`; newer releases document additional mappings such as
`o200k_harmony` for GPT-OSS. OpenRouter's catalog lists tokenizer families such
as GPT, Claude, Gemini, Llama, Qwen and Mistral. Hugging Face's `tokenizers`
can load tokenizer artifacts in Rust, while Transformers' chat-template
documentation shows why the selected model's chat template contributes
control tokens around the message text.

#### What Hugging Face Tokenizers provides

Hugging Face Tokenizers is a Rust tokenizer engine and serialized tokenizer
format ecosystem, not an automatic mapping from an arbitrary provider/model
name to its correct tokenizer. A tokenizer pipeline includes normalization,
pre-tokenization, a model algorithm/vocabulary (such as BPE, Unigram or
WordPiece), and post-processing such as special-token insertion. Loading a
matching `tokenizer.json` captures those components and is materially more
reliable than applying one unrelated tokenizer or a character ratio to the
text.

The Rust crate can deserialize a local tokenizer artifact with
`Tokenizer::from_file` or `Tokenizer::from_bytes`; `Tokenizer::from_pretrained`
is available with its optional `http` feature. Once loaded, encoding and
counting are local, fast, and repeatable, and the crate supports multiple
tokenizer algorithms and custom added tokens. The tokenizer can be cached per
artifact/model so counting does not involve a network call for each message.

The important condition is artifact availability and compatibility:

- The exact model tokenizer artifact must be identified and fetched or already
  installed. Model name alone is not enough; repositories can have revisions,
  tokenizer aliases, or configuration variants. OpenRouter's catalog's broad
  `architecture.tokenizer` value is a family label, not a downloadable
  tokenizer specification or immutable revision.
- A complete compatible `tokenizer.json` must represent the pipeline. Some
  Transformers models use other backends, including SentencePiece, a custom
  Python tokenizer, or Mistral's `mistral-common` backend. Transformers can
  select those specialized implementations; using the generic Rust
  `tokenizers` crate alone does not guarantee parity for them.
- The tokenizer file handles tokenizing text and its configured special-token
  processing. It does not generally execute the model's Jinja chat template
  (`tokenizer_config.json` / `chat_template.jinja`). Even if it does for an
  individual repository, the template the inference server actually uses may
  differ or be overridden. Chat structure, tools and multimodal preprocessing
  remain separate from ordinary text encoding.
- Hugging Face-hosted repositories are not guaranteed to correspond to the
  model name used by Ollama or an OpenRouter route. Ollama's `/api/show`
  exposes model template and tokenizer metadata, but not a generic Rust
  `tokenizer.json` handle or a documented tokenize-only endpoint. Deriving the
  correct Hugging Face artifact from GGUF metadata is a separate resolver and
  artifact-compatibility problem.

Thus, Hugging Face Tokenizers would be a better **general-purpose local
tokenizer engine** than hard-coding `cl100k_base` for all OpenAI-compatible
providers, provided Ylva can reliably resolve the exact supported tokenizer
artifact. It is not, by itself, a better universal first implementation for
all currently supported models. Without reliable artifact mapping, it simply
replaces one guess (provider → tokenizer family) with another (model name →
Hub artifact), and still cannot produce the provider's exact chat request
count.

#### Comparison with `tiktoken-rs`

| Concern | `tiktoken-rs` | Hugging Face `tokenizers` |
|---|---|---|
| Main coverage | Known OpenAI encodings and model-family mapping | Many tokenizer pipelines serialized as `tokenizer.json` |
| Current Ylva integration | Already a dependency; `cl100k_base` is used across models | New dependency and tokenizer artifact resolution/loading required |
| Model selection | Good when the model is known to use a supported OpenAI encoding | Flexible when the exact compatible artifact is available |
| OpenRouter | Does not cover most non-OpenAI families | Potential coverage when a compatible artifact and mapping are known; not guaranteed for closed/hosted models |
| Ollama | Current code uses the same `cl100k_base` baseline and records returned usage where available | Useful only if a matching artifact can be resolved; `/api/show` metadata alone is insufficient |
| Chat formatting | Tokenizer only; Chat Completions framing is not exact | Tokenizer only; the crate does not automatically guarantee execution of the serving chat template |
| Operational behavior | Small scope and fixed local encodings; updates needed for newer mappings | Artifact download/cache, revision pinning, failures/offline behavior and tokenizer variants become product concerns |

For OpenAI models, keeping `tiktoken-rs` with model-family selection is a
possible corrective step: the current baseline uses fixed `cl100k_base`, not
that it is a tokenizer library. For heterogeneous
OpenRouter/local models, `tokenizers` broadens the set of models for which a
local, artifact-backed estimate can be computed, but only after building a
resolver and explicit support policy. The two libraries can coexist: use
`tiktoken-rs` for known OpenAI encodings, Hugging Face artifacts for supported
artifact-backed models, and a conservative fallback otherwise. This avoids
claiming that either tokenizer covers all provider models.

**Starting-point decision:** if the first deliverable is a low-risk correction
for the supported OpenAI models, keep `tiktoken-rs` and select its encoding by
model. If the first deliverable is intended to support multiple open-model
tokenizer families offline, Hugging Face `tokenizers` is a stronger engine to
build around, but only if the feature includes a deterministic artifact source
(for example, a user-configured/local `tokenizer.json`) and explicit handling
for unsupported models. The best extensible architecture is an estimator
interface with both backends; the best initial scope depends on whether Ylva
will own tokenizer artifact discovery and caching in that change. Neither
choice removes the need to learn from provider usage.

Potential local fallback path:

1. Use the selected model ID, and if present provider catalog metadata, to
   resolve a known tokenizer family/encoding.
2. For OpenAI model families, select the supported encoding by model family
   rather than using `cl100k_base` for all traffic.
3. For Hugging Face-hosted local models, optionally load a matching tokenizer
   artifact from configured/local model metadata. Apply the chat template only
   if Ylva has a compatible renderer and has confirmed it matches the active
   serving endpoint; generic tokenizer loading alone does not do this.
4. Add a conservative framing and tool-schema allowance around content token
   counts, and use an error margin when tokenizer/template identity is unknown.

This cannot be generalized into a complete solution for every Ollama or
OpenRouter model. Ollama `/api/show` exposes tokenizer-related metadata and a
template, but that does not provide an already loaded Rust tokenizer API;
reconstructing arbitrary GGUF vocab/merges and model template behavior is a
separate implementation. OpenRouter may proxy closed models, modify requests,
or route requests across providers. A model-family tokenizer is a better
fallback, not proof of the serving-side count.

### 4. Chat templates and wire projection affect actual input length

Transformers documents that chat message dictionaries are converted into a
model-specific token sequence with role/control tokens. Two models sharing a
base architecture may use different templates. For the current code, the
provider serializes messages as Chat Completions JSON and includes tool-call
IDs, serialized JSON arguments, ToolEnvelope result JSON, tool schemas, and
sometimes synthetic image/file follow-up messages. Tokenizing only message
content misses these components.

Local accuracy could improve by estimating the same provider-projected request
shape rather than summing message text alone. However, tokenizer equality and
the server's exact template must also be known. Counting serialized JSON bytes
or tokenizing JSON wire text is not equivalent to counting the provider's
rendered model input.

### 5. Context-window limits still need headroom

Reported usage is the best observed count, not a promise the next request has
the same shape. Tool definitions and active model can change, providers may
insert hidden/system content, and multimodal preprocessing may be model-specific.
Compaction should keep a safety reserve below the configured window even when
using reported usage. Failed, interrupted, or unavailable usage must leave the
last known count intact and fall back to a conservative estimate; it must not
disable compaction or claim exactness.

## Alternatives

### Option A — retain a shared local estimate

Keep `cl100k_base` as the local estimate for every provider and model.

- **Advantages:** no additional requests or state; fast and deterministic.
- **Disadvantages:** ignores model identity, so tokenizer mismatch is common, and
  the current totals omit much of the actual wire payload. A broad
  fixed estimate cannot be assumed accurate for the selected model.
- **Assessment:** inadequate as the primary count for reliable compaction.

### Option B — choose a local tokenizer by model metadata

Use model ID and provider catalog metadata to select a known tokenizer, using
`tiktoken-rs` for known OpenAI encoding families and Hugging Face `tokenizers`
for a model with a resolved compatible `tokenizer.json`; count text and
serialized tool structures and apply a framing allowance.

- **Advantages:** local, no latency/network dependency, better for known model
  families, possible incremental message caching. Hugging Face Tokenizers
  supports many serialized tokenizer pipelines and is a stronger foundation
  for a broad artifact-backed fallback than applying `cl100k_base` to
  unrelated models.
- **Disadvantages:** tokenizer artifacts may be unavailable or incompatible;
  catalog labels can be broad; model aliases/custom names complicate lookup;
  specialized Transformers backends may not be represented by a generic
  `tokenizer.json`; and local counts still omit private provider transforms
  and multimodal details. OpenAI-compatible does not imply an OpenAI model.
- **Assessment:** best local fallback architecture once model-to-artifact
  resolution exists. For an initial change, correct OpenAI encoding selection
  and introduce an estimator interface first; add HF artifacts for explicitly
  resolvable model sets rather than make arbitrary remote downloads the default.

### Option C — preflight every full model-view request with provider count API

Before every generation, submit the same logical input to the provider's count
endpoint and use its returned input-token count.

- **Advantages:** closest pre-send count where a native count API exists;
  includes model tokenizer, request framing, tools, and supported modalities.
- **Disadvantages:** provider-specific APIs do not cover all Ylva connections;
  OpenAI's endpoint is Responses-shaped while Ylva sends Chat Completions;
  Anthropic count is approximate and requires Messages format; Ollama/OpenRouter
  do not document a general matching preflight endpoint; doubles payload
  transfer, latency and failure modes. A preflight/generation mismatch can
  occur when routing, model revision, or request processing changes.
- **Assessment:** useful for explicitly supported direct APIs or near-limit
  checks, too costly and incomplete as the universal per-turn path.

### Option D — reconcile with usage returned by the actual generation request

Enable and parse provider usage from the existing stream when supported. Treat
the returned prompt token count as the authoritative observation for that
specific request. Maintain estimator state for subsequent turns and apply
conservative headroom.

- **Advantages:** no duplicate count call; measures the request that actually
  ran and includes the provider's model/tokenizer and much request framing;
  works naturally with existing streaming completion flow.
- **Disadvantages:** arrives only after a request finishes, may be missing on
  errors/cancellation, and describes the just-sent model view rather than each
  message. Usage support and semantics differ by provider and may require
  provider-specific stream decoding. It cannot prevent the first oversized
  request unless a local pre-send guard already exists.
- **Assessment:** strongest general operational source where supported; should
  complement rather than erase a local pre-send estimate.

### Option E — expose server tokenizer/render endpoints for local servers

Use server-side tokenization/render APIs from the configured inference server.
vLLM documents renderer endpoints that apply preprocessing/chat templates and
return token IDs for supported configurations. Other servers expose their own
tokenize/render surfaces, but support and request formats vary.

- **Advantages:** server has the model tokenizer and template; can be exact for
  that serving deployment and useful before inference.
- **Disadvantages:** non-standard and server-specific, often opt-in, may expose
  operationally sensitive endpoints, and does not cover hosted providers
  generally. vLLM render API is not the same as ordinary Ollama/OpenRouter
  behavior.
- **Assessment:** optional adapter for deployments that explicitly advertise
  the capability; not a provider-wide assumption.

### Option F — infer token counts from response text or character ratios

Continue heuristics, possibly tune them from observed prompt usage.

- **Advantages:** universally available, simple, no external dependency.
- **Disadvantages:** token/character ratio changes with language, code,
  whitespace, JSON, and tokenizer. Naive per-message sums have boundary
  effects; image/file costs are not proportional to encoded text. A learned
  ratio can drift across model/provider switches.
- **Assessment:** fallback only, with an explicit uncertainty reserve.

## Recommendation

Use a **hybrid, provider-aware measurement strategy**:

1. **Improve the pre-send estimate.** Select tokenizer by active model/provider
   catalog metadata, not just provider connection. Count the actual model-view
   projection as closely as possible, including the system block, summary,
   tool-call arguments, tool results/errors, tool schemas and provider-known
   attachment projection. Keep a conservative overhead/headroom for framing
   and unknown provider behavior. Unsupported models use a conservative
   fallback and are explicitly represented as estimates.
   Prefer a small first step that selects among `tiktoken-rs` encodings for
   known OpenAI models. Design the estimator boundary to accept a cached
   artifact-backed tokenizer later. Use Hugging Face `tokenizers` where Ylva
   can resolve and pin a matching `tokenizer.json`; do not infer an arbitrary
   Hub repository from an OpenRouter/Ollama model slug and silently download
   it as if it were authoritative.
2. **Capture actual prompt usage from the request already sent.** Enable
   supported usage reporting for OpenAI Chat Completions and OpenRouter
   streams; verify and parse Ollama's OpenAI-compatible usage behavior, with
   native `prompt_eval_count` as an Ollama-specific option if needed. Normalize
   successful observations in a provider-neutral usage event carrying model,
   provider, request identity, input-token count and source/quality. A response
   usage count is not billed usage by itself and should be named as prompt
   tokens/observed input count rather than conflated with the estimate.
3. **Use observations to calibrate the next estimate.** Compare actual usage
   with the predicted count for the same submitted model view. Keep a
   model/provider-scoped correction for framing/estimator error, or use the
   last authoritative count as the base and add the locally estimated delta
   for appended content. Invalidate or rebuild calibration when active model,
   provider connection, modalities, tools, system prompt, or request projection
   changes. Do not apply a correction learned from one provider/model to
   another.
4. **Keep compaction protective.** Trigger compaction from a conservative
   upper-bound estimate, with more reserve when the count source is weaker.
   Missing usage, interrupted streams, failed count requests, model changes,
   and multimodal content must retain a usable fallback and never turn off the
   hard compaction backstop. Use actual counts to improve subsequent sweeps,
   not to retroactively represent a count as exact for a different request.
5. **Reserve count APIs for explicit capabilities.** Add preflight count
   adapters only where the provider's documented count API matches the exact
   request semantics Ylva uses. Prefer them for a near-limit validation or
   before the first request when available; avoid a universal extra API call
   on every turn. Record that OpenAI's count API requires Responses-shaped
   input, and Anthropic's endpoint has a documented small estimation variance.

This separates two needs that the current single integer blends together:
the **pre-send estimate** needed to make compaction decisions before context
overflow, and the **observed input-token count** returned after inference. The
first protects the next request; the second provides the best feedback for
correcting future estimates.

## Suggested accuracy validation

Before changing the implementation, build a comparison corpus from serialized
Ylva requests covering:

- short/long prose, code, JSON, Unicode, and non-English text;
- system prompt, compaction summary, assistant tool calls, arguments, tool
  results and errors;
- the configured tool schemas and all active tool subsets;
- base64 image/PDF results both when projected as attachments and when retained
  as ordinary text;
- model switches among tokenizer families and OpenRouter-routed models.

For each request record the local estimate, provider-reported prompt usage,
absolute/relative error, whether usage was present, provider/model, and whether
the request approached or exceeded the advertised context length. Keep prompts
and credentials out of logs; the corpus can use synthetic or redacted content.
Compare underestimation rate and maximum underestimation near the context
limit, not just average error. Compaction's main failure mode is undercounting
that causes a request rejection; overcounting causes early, potentially
unnecessary compaction.

## Implementation notes and deferred work

- Current estimator implementation: [`src/llm/token_estimate.rs`](../../src/llm/token_estimate.rs).
- Current message accounting: [`estimate_message_tokens`](../../src/agent.rs),
  which caches payload-only counts as messages are committed. The compaction
  budget estimates the projected model view and tool definitions. During a
  request, the footer instead estimates the serialized JSON body, including
  transport framing; after compaction applies, it shows the model-view estimate.
- Provider serialization: [`src/llm/openai_compatible_messages.rs`](../../src/llm/openai_compatible_messages.rs)
  and `src/llm/openai_compatible_provider.rs`.
- `Context::set_token_estimator` recalculates cached message counts on model
  or estimator changes. The model-view readout does not sum these cached
  payload-only values; it projects the complete active view and tools before
  estimating model-visible content for compaction. The footer instead shows
  the estimate of the exact serialized request body emitted by the provider
  before it waits for the response.
- OpenAI-compatible stream decoding captures the final usage chunk directly
  into the optional per-request dump. It does not add usage to the agent event
  protocol or use it to change compaction budgeting.
- Caller-side compaction timeouts append a `[failure]` line to the matching
  response dump when request dumping is enabled. The request-scoped callback is
  bound before the provider waits for response headers so it can record a
  timeout even after stream processing is cancelled. The wall-clock timeout is
  `[compaction].timeout_sec`, not on the engine whose conversation is being
  compacted. `[compaction].engine` selects the summarizer independently.
- OpenRouter's model catalog includes `architecture.tokenizer`; current
  `ModelInfo` does not retain this field. Ollama `/api/show` includes tokenizer
  metadata and a model template; neither is equivalent to a loaded generic
  tokenizer API or a documented Ollama tokenization endpoint.
- The Hugging Face Rust crate consumes tokenizer pipeline artifacts such as
  `tokenizer.json`; Transformers' full `AutoTokenizer` backend selection can
  also involve SentencePiece or specialized/custom implementations. Do not
  equate “the model exists on the Hub” with “generic `tokenizers` reproduces
  its serving tokenizer and prompt.”
- Any remote tokenizer artifact resolver needs explicit trust and stability
  rules: pin a repository revision or artifact digest, cache locally, avoid
  executable model code, define behavior when offline or unavailable, and
  expose the estimate source. A tokenizer artifact is data, but its origin and
  revision still determine correctness and reproducibility.
- Existing configured `soft_compaction_threshold`,
  `hard_compaction_threshold`, and `compact_target_ratio` values describe
  policy. Changing their interpretation or adding estimator-quality settings
  requires updating `docs/configuration.md`, `docs/features.md`,
  `docs/implementation-notes.md`, and the persistence/readout contract as
  applicable.

## Sources

Provider behavior researched 2026-10-02. API availability and model mappings
can change; implementation should verify current provider contracts and test
actual response payloads.

- OpenAI, [Token counting](https://platform.openai.com/docs/guides/token-counting):
  local tokenizer limitations; Responses input-token count API; tool/media
  support and count semantics.
- OpenAI, [Input Tokens API reference](https://platform.openai.com/docs/api-reference/responses/input-tokens):
  `/v1/responses/input_tokens` input and response shape.
- OpenAI, [Chat Completions API reference](https://platform.openai.com/docs/api-reference/chat/create):
  streamed usage via `stream_options.include_usage`; final usage chunk caveat.
- OpenAI, [Prompt caching](https://platform.openai.com/docs/guides/prompt-caching):
  actual rendered context includes instructions, tools, messages and supported
  media; compaction changes the prefix. This concerns caching, but corroborates
  that the model sees more than plain message text.
- OpenAI, [tiktoken repository](https://github.com/openai/tiktoken):
  tiktoken is a fast BPE tokenizer for OpenAI models and maps model families to
  encodings.
- Anthropic, [Token counting](https://docs.anthropic.com/en/docs/build-with-claude/token-counting):
  model-specific count endpoint, supported structured inputs, explicit
  estimation variance, model tokenizer changes, and rate limits.
- OpenRouter, [API reference](https://openrouter.ai/docs/api-reference/overview):
  Chat Completions usage schema, usage chunk behavior, and model-native token
  counts; [streaming](https://openrouter.ai/docs/api/reference/streaming) details
  the final usage chunk.
- OpenRouter, [List models and their properties](https://openrouter.ai/docs/api/api-reference/models/list-all-models-and-their-properties):
  catalog `architecture.tokenizer`, model/context metadata, and model family
  information.
- Ollama, [Usage](https://docs.ollama.com/api/usage): `prompt_eval_count` and
  final streaming usage fields.
- Ollama, [OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility):
  supported Chat Completions fields, including `stream_options.include_usage`.
- Ollama, [Chat API](https://docs.ollama.com/api/chat) and
  [Show model details](https://docs.ollama.com/api-reference/show-model-details):
  native chat prompt usage, model template and tokenizer metadata.
- Hugging Face, [Chat templates](https://huggingface.co/docs/transformers/main/en/chat_templating):
  role-based messages are rendered into model-specific control-token
  sequences; templates can add generation prompts and special tokens.
- Hugging Face, [Tokenization algorithms](https://huggingface.co/docs/transformers/main/en/tokenizer_summary):
  BPE, Unigram and WordPiece tokenizer differences.
- Hugging Face, [Tokenizers](https://huggingface.co/docs/tokenizers/index):
  Rust tokenization library, serialized pipelines and supported algorithms.
- Hugging Face, [`tokenizers` Rust API](https://docs.rs/tokenizers/latest/tokenizers/):
  `Tokenizer::from_file`/`from_bytes`, optional HTTP-backed
  `from_pretrained`, pipeline components and encoding API.
- Hugging Face, [Tokenizers repository](https://github.com/huggingface/tokenizers):
  Rust reference implementation; tokenizer JSON loading; version-specific
  binding capabilities. The repository currently documents a v1 release
  candidate whose bindings have a narrower feature set, while the stable 0.23
  Rust crate documents the existing loading and encoding API.
- Hugging Face Transformers, [Tokenizers](https://huggingface.co/docs/transformers/main/en/fast_tokenizers):
  model-specific backend selection and cases where SentencePiece or specialized
  tokenizers are used instead of the generic fast-tokenizer backend.
- vLLM, [Renderer APIs](https://docs.vllm.ai/en/stable/serving/online_serving/renderer.html):
  server-side rendering/token IDs and multimodal preprocessing.
- Rust, [`tiktoken-rs` 0.5.9 documentation](https://docs.rs/tiktoken-rs/0.5.9/tiktoken_rs/):
  version used by this project family documents model lookup and the available
  encodings. The project's direct dependency is currently `tiktoken-rs = 0.5`.

## Related

- [Context management and compaction](../features.md#context-management-and-compaction)
  (current behavior, budgeting, root and subagent scope)
- [Compaction implementation](../implementation-notes.md#context-management-and-compaction)
  (cached estimates, model projections, scheduling, and persistence)
- `docs/ideas/tool-result-size-guard.md` (per-result wire guard; defers the
  aggregate to this idea)
- `docs/configuration.md` (`[compaction].engine`, `context_length`)
- `docs/features.md` (root and child compaction scope)
- `docs/implementation-notes.md` (child lifecycle and per-agent setting resolution)
- `docs/persistence.md` (append-only contract, resume, `parent_session`)

## Deferred

- A manual `/compact` command.
- Hierarchical summarization if the compact engine cannot accept the selected
  prefix.
- Per-result wire-size protection and the default `glob` ceiling. See
  [`tool-result-size-guard.md`](./tool-result-size-guard.md).

No new architecture decision record is planned for the initial implementation.
