# Provider-Aware Image And File Input From Tool Results

## Context

Ylva's `read` tool can already return an exact Base64 representation of
arbitrary bytes when the model explicitly requests `mode: "bytes"` and
`encoding: "base64"`. The Base64 string currently remains only the text of a
`role: "tool"` result. It is persisted in `ToolResult.result`, replayed with
the session, and sent through the shared OpenAI Chat Completions serializer as
ordinary JSON text. A vision or document-capable model therefore receives no
image or file input part and cannot inspect the bytes as media.

This change covers the agent-harness path from binary `read` results through
tool-result persistence and replay to provider-specific Chat Completions
requests. It introduces a provider-neutral attachment descriptor, recognizes
supported image and PDF byte signatures with the `infer` crate, projects
supported attachments into a wire-only user content message, and retains the
current text-only behavior when the active model's capabilities are unknown or
insufficient.

The initial media set is PNG, JPEG, WebP, GIF, and PDF. Recognition uses
`infer`'s magic-number result but accepts only those five MIME types. An
attachment is created only when the selected Base64 range is the complete
file; a partial range remains ordinary Base64 even if its leading bytes look
like a supported format. Images use an OpenAI-compatible `image_url` data URI.
PDFs use a Chat Completions `file` content part when the selected
provider/model advertises file input. A PDF is never sent through
`image_url`; providers without file support retain the existing Base64 text
fallback rather than adding local PDF extraction or page rendering to this
change.

The attachment descriptor reuses the existing Base64 `ToolResult.result` as
the canonical payload. It must not own or persist a second copy of the same
Base64 string. The persisted/domain model remains provider-neutral. The
provider serializer may expand one contiguous group of tool results into the
normal tool messages followed by a hidden, non-persisted user message so that
tool responses remain contiguous and media parts are placed in a documented
user-role location.

Definition of done:

- A known image-capable Chat Completions model receives a real image content
  part containing the selected image bytes.
- The complete image Base64 is not also sent as ordinary tool text in that
  request, and it appears once as the image data URI.
- A known file-capable model receives a PDF/file content part rather than an
  `image_url` part.
- Unknown, text-only, or modality-incompatible providers retain the existing
  text-only result behavior without a hidden media message.
- Existing text reads and unrecognized Base64 reads remain behaviorally
  compatible.
- Persisted sessions retain enough attachment information for a resumed
  request to project the same media input, while old sessions without the new
  field still deserialize.
- UI bubbles, tracing fields, and request dumps do not expose opaque binary
  payloads. Request dumps retain safe structure/markers rather than media
  bytes.
- OpenRouter, Ollama, and the generic OpenAI-compatible request path have
  independent parser/serialization coverage.
- The focused and full repository verification gates pass, including the
  configured coverage threshold.

The implementation must preserve the existing full-file-in-memory tool model,
Base64 result contract, automatic approval behavior for `read`, session
commit/hydration rules, and provider-neutral domain messages. There is no
existing ADR governing multimodal input; the implementation must not assume
that acceptance of image content in a tool-role message is portable.

The resolved design does not add a media argument, profile capability setting,
size limit, image-detail setting, upload service, file ID, direct tool-role
media mode, PDF transformation fallback, or media-aware compaction. Model
capabilities are discovered automatically and retained only in runtime state;
an initially configured model is unknown until matching provider metadata is
available. Request dumps redact binary strings while preserving safe structure.

Start by opening `src/agent/tools/read.rs`, `src/agent.rs`, and `src/llm.rs`,
then run the focused existing read and serializer tests before changing code:
`cargo test --lib agent::tools::read` and
`cargo test --lib llm::tests::serialize_messages`.

The implementing agent must flip each plan checkbox from `[ ]` to `[x]`
immediately after completing that step and rewrite this sidecar in place so
progress remains resumable across sessions.

## Implementation Plan

1. [ ] Establish the provider-neutral attachment and capability contracts.
   - [ ] Add the direct `infer` dependency at version `0.22` with default
     features disabled, allowing the implementation to use byte-buffer
     detection without pulling in its optional CFB support; let Cargo update
     `Cargo.lock`.
   - [ ] Add a serializable `ToolAttachment` descriptor next to the persisted
     tool-result domain types in `src/agent.rs`, with image and file variants,
     media type, and (for files) a safe source filename. Document that the
     descriptor references the containing successful tool result's Base64
     `result` value rather than owning another payload.
   - [ ] Add an optional `attachments` field to `ToolResult` with
     `#[serde(default)]`, preserving old JSONL sessions and keeping the
     existing flattened `Message::ToolResult` shape.
   - [ ] Extend `ToolCallResult` with a default-empty attachment accessor so
     existing tools remain unchanged and `read` can opt in.
   - [ ] Add provider-neutral input-modality state to `ModelInfo`, with a
     representation that distinguishes unknown (`None`) from known text-only
     (`Some([])`) and known image/file support. Carry the selected capability
     snapshot in the shared engine runtime state without adding a new profile
     setting.
   - [ ] Define the serializer policy/wire-format types needed to keep
     semantic model capabilities separate from provider-specific content-part
     differences. Preserve the existing `serialize_messages` text-only
     behavior as the compatibility projection.
   - [ ] Add or update unit fixtures for equality, serde defaults, capability
     states, and the unchanged no-attachment serialization path.

2. [ ] Detect supported media in explicit Base64 `read` results.
   - [ ] Run `infer::get` after `offset`/`limit` slicing, not from the path
     extension, and map only `image/png`, `image/jpeg`, `image/webp`,
     `image/gif`, and `application/pdf` to attachments. All other detector
     results remain ordinary Base64.
   - [ ] Create an attachment only when the selected range is the complete
     file, meaning it starts at byte zero and reaches the end of the source
     bytes. A partial range remains ordinary Base64 even when `infer` detects a
     supported leading signature.
   - [ ] Make recognized image data produce an `Image` descriptor and
     recognized PDF data produce a `File` descriptor with `application/pdf`
     and a basename derived from the source path, using a safe fallback name
     when the path has no usable basename.
   - [ ] Keep valid UTF-8 previews and unknown binary behavior compatible with
     the current Base64 read implementation. Add `encoding: "base64"` and a
     recognized `media_type` to read metadata without exposing encoded content
     in UI sections; preserve raw byte counts and text-preview rules.
   - [ ] Do not add a new size limit, network upload, URL, extraction utility,
     or explicit media argument in this step; explicit Base64 byte mode is the
     existing opt-in boundary for media recognition.
   - [ ] Add focused `read.rs` tests for every signature, false positives,
     complete-file versus partial-range detection, filename handling,
     metadata, UTF-8 fallback, unknown binary fallback, and absence of opaque
     content sections.

3. [ ] Carry attachment descriptors through tool execution without exposing
   them to the UI.
   - [ ] Extract attachments in `PendingTool::execute`, add them to
     `AgentEvent::ToolDone`, and copy them into the committed `ToolResult` in
     the agent-loop completion branch.
   - [ ] Keep `UiUpdate::ToolDone`, CLI output, TUI content sections, and
     pending/completed tool bubbles attachment-free. Continue using the hidden
     result-section identifier when the Base64 display projection is absent.
   - [ ] Ensure child-agent completion and normal tool completion use the same
     carrier path, while tool errors never manufacture attachments.
   - [ ] Add an agent-loop test proving the descriptor survives
     `ToolCallResult -> AgentEvent::ToolDone -> ToolResult`, while the live UI
     update contains no Base64 or attachment payload.

4. [ ] Implement capability-aware Chat Completions message projection.
   - [ ] Add a serializer entry point that accepts the active model capability
     state and provider wire-format policy, while retaining the current
     no-policy/text-only projection for existing callers and tests.
   - [ ] Serialize a supported attachment-bearing tool result with a short
     marker such as `[image attached]` or `[file attached]` in the `role:
     "tool"` envelope instead of the complete Base64 result. Keep result
     metadata and the tool call ID intact.
   - [ ] When a contiguous group of tool results contains supported
     attachments, emit all `role: "tool"` messages first, then one wire-only
     `role: "user"` content array with text before image/file parts. This
     avoids breaking the Chat Completions requirement that responses to one
     assistant tool-call turn remain contiguous. Do not persist or render the
     synthetic user message.
   - [ ] Render images as standard object-form `image_url` data-URI parts and
     PDFs as Chat Completions `file` parts using inline `file_data` and the
     retained filename. Never render `application/pdf` as `image_url`.
   - [ ] Project only modalities supported by the current capability state. A
     recognized but unsupported attachment remains the existing complete
     Base64 tool text fallback; unknown capability never opts in implicitly.
   - [ ] Verify mixed image/file groups, multiple tool results, multiple
     attachments, tool errors, empty history, and exact Base64 occurrence
     counts. The no-attachment JSON must remain byte-for-byte equivalent to the
     current fixture semantics.

5. [ ] Populate and retain model/provider input capabilities.
   - [ ] Extend OpenRouter model-list parsing to read
     `architecture.input_modalities`, recognizing image and file values while
     ignoring unsupported modality names. Preserve unknown when the catalog
     omits the field.
   - [ ] Extend Ollama `/api/show` parsing to map `vision` to image support;
     do not infer PDF/file support from vision. Ensure the existing early return
     does not skip `/api/show` when modality data is still unknown.
   - [ ] Keep generic OpenAI-compatible providers conservative: use explicit
     advertised modality metadata when present, otherwise leave capability
     unknown. Do not infer capability from model names or endpoint identity.
   - [ ] Update the provider/engine selection path so a model selected from
     `/models` carries its `ModelInfo` capability into subsequent requests,
     while a model configured before discovery remains unknown until matching
     metadata is obtained. Copy the field through named-engine picker plumbing.
   - [ ] Isolate provider-specific content-part formatting in the provider
     layer. OpenAI/OpenRouter use the standard object form; Ollama's selected
     compatibility form must be explicit rather than assumed for every
     OpenAI-compatible endpoint. Keep native Ollama `/api/chat` and OpenAI
     Responses `input_image` transports out of scope.
   - [ ] Add parser and runtime-state tests for absent, known text-only, image,
     and file capabilities, model switching, initial unknown models, OpenRouter
     catalog entries, and Ollama detail responses.

6. [ ] Make request dumps safe for media-bearing requests.
   - [ ] Sanitize only the request-dump projection in `src/llm/http.rs` (never
     mutate the live request) by replacing Base64 tool-result payloads and
     inline image/file data with stable redaction markers while retaining
     roles, IDs, media types, filenames, and non-sensitive metadata.
   - [ ] Cover both the text fallback and the multimodal wire expansion so
     opaque binary data cannot remain in a dump merely because the model was
     text-only or capability was unknown.
   - [ ] Keep existing header redaction and structured tracing behavior intact;
     do not add request bodies or payload strings to tracing fields.
   - [ ] Add unit tests with distinctive binary sentinels that assert the dump
     contains no encoded payload, decoded payload, or data URI bytes and still
     contains safe request structure.

7. [ ] Preserve persistence, replay, and backward compatibility.
   - [ ] Add attachment serde coverage to `src/agent.rs` proving new results
     round-trip and legacy `ToolResult` JSON without `attachments` defaults to
     empty.
   - [ ] Verify store hydration does not recommit attachment-bearing messages,
     and that resumed provider requests reconstruct the same wire projection
     from the persisted descriptor and canonical Base64 result.
   - [ ] Keep replay projection and TUI/CLI bubbles unchanged apart from the
     already-established hidden result behavior; attachment descriptors must
     never become visible user messages or content sections.
   - [ ] Extend unit and integration coverage for full-fidelity replay,
     interrupted-tool compatibility, no duplicate persistence, and one-time
     media projection in a resumed request.

8. [ ] Add independent provider and request-shape integration coverage.
   - [ ] Extend `tests/common/mock_llm_server.rs` with model-list/detail
     capability fixtures and retain request-body capture without logging or
     printing payloads.
   - [ ] Add OpenRouter tests that advertise image and file modalities and
     assert image/file content parts, tool markers, text-before-media ordering,
     and exactly-once Base64 placement.
   - [ ] Add Ollama tests that advertise `vision`, assert its selected image
     compatibility shape, and prove PDFs use the text fallback because no file
     capability is advertised.
   - [ ] Add a generic OpenAI-compatible request test for unknown capability
     fallback and a known capability test where the catalog explicitly
     advertises the modality.
   - [ ] Exercise the normal agent/tool path where practical rather than only
     constructing serializer fixtures, and keep all tests independent of
     ambient provider credentials.

9. [ ] Update the documentation for the changed contracts.
   - [ ] Update `docs/features.md` to describe media recognition from explicit
     Base64 `read`, provider/model capability policy, image/file distinctions,
     text fallback, hidden wire-only user expansion, and the fact that current
     binary reads are no longer text-only for capable providers.
   - [ ] Update `docs/implementation-notes.md` with the attachment descriptor
     contract, canonical Base64 ownership, media signatures, serialization
     ordering, provider capability rules, request-dump redaction, and replay
     behavior.
   - [ ] Update `docs/persistence.md` with the additive `ToolResult` attachment
     field, legacy defaults, and the rule that synthetic media user messages
     are wire-only and never committed.
   - [ ] Update the affected schema/architecture excerpts in `docs/blueprint.md`
     so they no longer claim that provider serialization has no typed
     attachment and so they describe the capability-aware projection.
   - [ ] Update `docs/cli-options.md` to state that request-body media is
     redacted in request dumps; do not add a configuration key for capability
     policy, size limits, or image detail in this change.
   - [ ] Keep `docs/ideas/image-or-file-upload.md` unchanged as the source
     requirements and rationale; do not delete its alternatives or design
     discussion.

10. [ ] Run the required repository verification gates.
    - [ ] Run `make format` so Rust and Markdown are formatted, including the
      plan and documentation changes.
    - [ ] Run `make lint` and fix all Rust/compiler/clippy warnings without
      blanket lint suppression.
    - [ ] Run `make test-coverage`; the full suite must pass and maintain the
      configured minimum of 80% line coverage.
    - [ ] If a test exercises tracing or process-global cwd behavior, apply the
      `run_locked` and `SUBSCRIBER_LOCK` rules from
      `docs/known-test-issues.md` before marking the step complete.

## Proposed Improvements

- Add media-aware context compaction in a separate change. Ylva currently
  resends the full conversation history, so an attachment can be reprocessed on
  later turns even after the model has answered.
- Add provider-uploaded file IDs, native Ollama image arrays, OpenAI Responses
  `input_image`/`input_file`, or direct tool-role media as provider-specific
  optimizations only after the portable user-follow-up path is stable.
- Add explicit PDF text extraction or page-rendering fallback if retaining the
  Base64 text fallback proves insufficient for Ollama or another provider.
- Add configured image detail, modality-specific budgets, or bounded media
  sizes only after measuring real request/token costs; the existing binary tool
  contract currently has no size limit.
- Add ignored live-provider image/PDF tests using the repository's existing
  environment-forwarding conventions after local mock coverage is complete.

## Implementation Hints

- `src/agent/tools/read.rs` currently applies byte slicing before Base64
  encoding and stores the result in `ReadResult.content`; `display_text()` is
  already `None` for Base64, which causes the agent to persist the hidden result
  section while retaining the model-facing result.
- The repository has no direct file-type recognition dependency. `infer`
  `0.22` provides `infer::get(&[u8]) -> Option<Type>` and static MIME strings
  for the five accepted formats; its default feature set also enables optional
  CFB support, so the plan disables default features. Detection is advisory,
  not full media validation.
- `src/agent/tools.rs` defines `ToolCallResult`; all concrete result types use
  its default methods, so an attachment accessor can be additive.
- `src/agent.rs` carries successful tool output through
  `PendingTool::execute`, `AgentEvent::ToolDone`, the outer completion branch,
  and `ToolResult`; `Message::ToolResult` flattens the serializable result.
- `src/llm.rs::serialize_messages` currently maps one domain message to one
  wire message. It emits a string-valued `role: "tool"` content containing
  `ToolEnvelope`, so attachment projection requires a flattening/grouping
  serializer rather than a simple field addition.
- `src/llm/openai_compatible_provider.rs` is the shared Chat Completions
  transport for generic OpenAI-compatible providers, OpenRouter, and the
  OpenAI-compatible Ollama endpoint. Its current request-body customizer only
  changes top-level provider fields and receives `EngineRuntimeConfig`.
- `src/llm/openrouter_provider.rs` delegates model listing to the shared
  OpenAI-compatible parser. OpenRouter's `architecture.input_modalities` must
  therefore be parsed in that shared catalog path without assuming every
  backend supplies it.
- `src/llm/ollama_provider.rs` combines `/api/tags` and `/api/show`; its
  existing early return skips `/api/show` when context and reasoning metadata
  are present, so the new modality state must participate in that condition.
- `ModelInfo` currently contains only `id`, `context_length`, and
  `reasoning_efforts`. `EngineRuntimeConfig` is the provider/engine shared
  snapshot used to make one coherent request configuration.
- `tests/common/mock_llm_server.rs` already captures request bodies and serves
  OpenAI model lists plus Ollama tags/show responses. `tests/http_provider.rs`
  contains the provider discovery integration tests to extend.
- `src/llm/http.rs::dump_request` currently serializes the complete body after
  redacting sensitive headers only. Media redaction must be applied to a copy
  used for the dump.
- `docs/known-test-issues.md` requires `run_locked` for tests that exercise
  tracing-instrumented tool functions and requires `SUBSCRIBER_LOCK` around
  process-global cwd reads.
- `docs/architecture-design-records/` contains only `.gitkeep`; there is no
  existing media-attachment ADR to reconcile.
- `docs/development-workflow.md` requires `make format lint` and the single
  mandatory `make test-coverage` gate for code changes, with at least 80% line
  coverage.
