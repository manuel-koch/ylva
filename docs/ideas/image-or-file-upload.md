# Image and file input from tool results

**Status:** under investigation

## Problem

Ylva's `read` tool can now return arbitrary bytes losslessly:

```json
{
  "path": "image.png",
  "mode": "bytes",
  "encoding": "base64"
}
```

The result is standard padded Base64. This solves exact binary transport, but it
does not make an image visible to a vision-capable model. Base64 embedded in a
JSON string is still text unless the provider receives it as an image input.

The current provider path is:

1. `read` returns the Base64 string through `ToolCallResult::text()`.
2. The agent stores it in `ToolResult.result`.
3. `serialize_messages()` wraps it in a JSON string in a `role: "tool"` message.
4. `OpenAiCompatibleProvider` sends that message to `/chat/completions`.

There is currently no media or attachment representation in `ToolResult`,
`Message`, or the provider serializer.

### Observed failure

The failing trace contains the complete PNG
Base64 payload in the textual tool result, but no `image_url`, `input_image`, or
other multimodal content part. The provider response reports
`image_tokens: 0` and ends with `finish_reason: "length"`.

An earlier apparently successful trace is not a reliable vision test: the
conversation already contained the ImageMagick command, polygon coordinates,
and an explanation of the intended triangle. The model could answer from text
context without inspecting the PNG.

## Image versus file modalities

An image and a PDF have the same underlying transport problem — binary data
must not be sent only as text — but they are different model input modalities.

| Data | Appropriate input | Notes |
| --- | --- | --- |
| PNG, JPEG, WebP, GIF | `image_url` data URI or `input_image` | One visual image input |
| PDF | `file`/`input_file` or provider file upload | A document that may contain text, vector graphics, and many images |
| PDF without file support | Extract text or render pages to images | A fallback transformation, not the same as sending the PDF |

A PDF must not be sent as an `image_url` with
`data:application/pdf;base64,...`. The `image_url` modality is for image media
types. A scanned PDF may ultimately need vision processing, but it should first
be handled as a PDF file or explicitly rendered into page images.

## Current binary transport behavior

- `read` with `encoding: "base64"` requires an explicit `mode: "bytes"`.
- The complete Base64 value remains in the model-facing
  `ToolCallResult::text()`.
- Opaque Base64 is hidden from the live and replayed tool bubbles.
- Base64 that decodes to valid UTF-8 gets a text preview; opaque bytes do not.
- `lines` and `chars` reads reject invalid UTF-8.
- `write` decodes Base64 before creating directories or modifying the target.
- The current binary result metadata contains byte counts, but not yet the
  encoding or media type.

These behaviors should remain compatible while adding multimodal input.

## Provider protocol findings

### OpenAI Chat Completions

The documented Chat Completions schema permits image content parts in a
`role: "user"` message:

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "Analyze this image."},
    {
      "type": "image_url",
      "image_url": {
        "url": "data:image/png;base64,<BASE64_DATA>",
        "detail": "auto"
      }
    }
  ]
}
```

The documented `role: "tool"` content is a string or a text-only content-part
array. An `image_url` part is not part of the documented tool-message
contract.

The Responses API has an equivalent `input_image` representation, but Ylva
currently uses Chat Completions and would need a separate transport to use it.

References:

- [OpenAI Chat Completions reference](https://developers.openai.com/api/reference/resources/chat)
- [OpenAI image inputs](https://developers.openai.com/api/docs/guides/images-vision)

### OpenRouter

OpenRouter documents the same user-role `image_url` format and supports local
images as Base64 data URLs. Its documented supported image types include PNG,
JPEG, WebP, and GIF. It recommends putting text before image parts.

The human-readable request schema says content parts are only for user
messages and that tool content is a string. The current OpenAPI schema permits
image content in a tool message. This inconsistency is not a portable contract,
especially because OpenRouter can route a request to different upstream
providers.

OpenRouter's model catalog exposes `architecture.input_modalities`, which can
be used to identify models that advertise `"image"` input.

References:

- [OpenRouter image inputs](https://openrouter.ai/docs/guides/overview/multimodal/image-understanding)
- [OpenRouter request schema](https://openrouter.ai/docs/api-reference/overview)
- [OpenRouter model metadata](https://openrouter.ai/docs/guides/overview/models)

### Ollama

Ollama's native `/api/chat` endpoint uses an `images` array containing raw
Base64 data. Its `/v1/chat/completions` compatibility endpoint documents
OpenAI-style image parts and accepts local Base64 data URLs.

The documented Ollama example uses `image_url` as a string, while the current
Ollama parser also accepts the standard object form:

```json
{"type": "image_url", "image_url": "data:image/png;base64,<BASE64_DATA>"}
```

Ollama's parser accepts image parts in more roles, but parser acceptance does
not guarantee that every model template handles an image-bearing tool message.
The parser can also lose `tool_call_id` when converting image content parts.
Ollama exposes model capabilities, including `"vision"`, through `/api/show`.

References:

- [Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility)
- [Ollama vision](https://docs.ollama.com/capabilities/vision)
- [Ollama model details](https://docs.ollama.com/api-reference/show-model-details)

### PDF and general file inputs

OpenAI Chat Completions documents a `type: "file"` content part for user
messages. The file can be supplied as inline `file_data`, an uploaded
`file_id`, and optionally a `filename`. The Responses API has the equivalent
`input_file` representation.

OpenRouter supports PDF and other file inputs through its multimodal API, but
support and parsing behavior depend on the selected model and routed provider.
Its documentation describes PDF/file content separately from image inputs.

Ollama's documented OpenAI-compatible vision path accepts image content, not
general PDF files. Its native `images` array is also image-oriented. A PDF
therefore needs local text extraction or page rendering before it can be sent
to an Ollama vision model, unless a future Ollama endpoint adds explicit file
support.

References:

- [OpenAI Chat file content](https://developers.openai.com/api/reference/resources/chat)
- [OpenAI image and file inputs](https://developers.openai.com/api/docs/guides/images-vision)
- [OpenRouter multimodal capabilities](https://openrouter.ai/docs/guides/overview/multimodal/overview)

## Token and payload cost

A naive portable implementation would send the same Base64 twice:

1. as ordinary text in the `role: "tool"` JSON envelope;
2. inside the `data:image/...;base64,...` image content part.

That duplicates the HTTP payload and adds Base64 text tokens in addition to the
provider's image input tokens. It is not an acceptable default.

The multimodal wire representation should instead send a short tool result
marker and the image once:

```json
{
  "role": "tool",
  "tool_call_id": "call_123",
  "content": "{\"ok\":true,\"result\":\"[image attached]\",\"result_metadata\":{\"read_bytes\":2024,\"encoding\":\"base64\",\"media_type\":\"image/png\"}}"
}
```

followed by a small hidden user message:

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "The previous tool returned an image."},
    {
      "type": "image_url",
      "image_url": {
        "url": "data:image/png;base64,<BASE64_DATA>"
      }
    }
  ]
}
```

The complete Base64 result can remain in Ylva's domain model and persisted
session for replay and text-only fallback, but it must not also be emitted as
ordinary tool text in a vision-capable request.

The same rule applies to PDFs and other supported files. A file should be sent
once as a file input, not once as Base64 tool text and again as a file
attachment. The provider may parse the file into text, images, or both, and its
token accounting is provider-specific.

Ylva currently sends the complete conversation history on each Chat
Completions request. Consequently, an image may be reprocessed on later turns
even after it was initially sent once. Avoiding that recurring image cost is a
separate context-lifecycle problem requiring media-aware compaction, provider
conversation state, or replacing old image inputs with a textual summary.

## Proposed solution

### 1. Add a provider-neutral media attachment

Introduce a typed attachment concept rather than asking the provider to infer
media from arbitrary result metadata. The abstraction should cover both visual
and document inputs:

```rust
enum ToolAttachment {
    Image {
        media_type: String,
        base64_data: String,
    },
    File {
        media_type: String,
        filename: String,
        base64_data: String,
    },
}
```

`ToolCallResult` would expose optional attachments. The agent would carry them
through `AgentEvent::ToolDone` into the persisted `ToolResult`.

To avoid storing the same large Base64 string twice, the attachment could
reference the Base64 result text instead of owning another copy. The exact
ownership and persistence shape remains open.

### 2. Attach recognized images and files from `read`

For an explicit Base64 byte read, `read` should:

- retain the exact Base64 result for compatibility;
- identify supported media types from the bytes, preferably using file
  signatures rather than only the filename extension;
- create an `Image` attachment for recognized image data;
- create a `File` attachment for recognized PDF data (`%PDF-`) when the
  selected provider supports file inputs;
- leave valid UTF-8 text and unknown binary data as ordinary Base64 results.

The initial supported set should match the target provider contracts:
`image/png`, `image/jpeg`, `image/webp`, and `image/gif`.
PDF support should use `application/pdf` and retain the source filename when
available. Other document types can be added after their provider contracts
are understood.

Read metadata should also become self-describing, for example:

```json
{
  "read_bytes": 2024,
  "encoding": "base64",
  "media_type": "image/png"
}
```

This metadata is useful to the model and UI, but it is not a substitute for
the typed attachment.

### 3. Add a multimodal and file-input serialization projection

Keep the persisted/domain message model provider-neutral. Extend serialization
so a provider that supports the attachment's modality can project one
attachment-bearing tool result into:

1. a normal text-only `role: "tool"` envelope with a short attachment marker;
2. a following hidden `role: "user"` message containing text followed by the
   appropriate image or file content part.

Use an `image_url` part for images. Use a `file`/`input_file` part or uploaded
file identifier for PDFs and other documents when the provider supports it.
Do not force a PDF through the image path.

The second message should not become a visible user turn. It can be generated
as a wire-only expansion from the persisted attachment, or represented using
the existing `ImplicitToolNote` concept if the domain needs to retain it.

The existing text-only serialization must remain unchanged when no attachment
is present or when the selected model cannot receive images.

### 4. Track model and provider capability conservatively

Extend model metadata with input modalities or equivalent capability state:

- OpenRouter: parse `architecture.input_modalities`.
- Ollama: parse `/api/show` capabilities for `"vision"`.
- File-capable providers: distinguish `"file"`/PDF support from image support.
- Unknown OpenAI-compatible providers: treat image support as unknown rather
  than assuming it.

The capability should be separate from whether a provider accepts multimodal
content in a `role: "tool"` message. A model can support images or files while
its provider only supports them in user messages.

The initial portable policy should be:

- known image-capable model: use the hidden user follow-up;
- known file-capable model: use a file-input follow-up;
- known text-only model: retain the textual fallback;
- unknown model: retain the textual fallback unless the user explicitly opts
  into multimodal input.

Direct multimodal tool messages may be added later as provider/model/template
optimizations, but should not be the default.

### 5. Keep provider-specific wire differences isolated

OpenAI and OpenRouter use the standard object form:

```json
{
  "type": "image_url",
  "image_url": {"url": "data:image/png;base64,<BASE64_DATA>"}
}
```

Ollama's documented compatibility example uses a string-valued `image_url`,
although current Ollama versions accept the object form too. Provider-specific
serialization or an explicit wire-format capability should isolate this
difference instead of assuming every OpenAI-compatible endpoint is identical.
The same isolation is required for PDF/file inputs: file parts, uploaded file
identifiers, native document parsers, and page-rendering fallbacks are not
interchangeable provider features.

## Alternatives considered

### Send Base64 as text only

This preserves the current protocol and works for models that independently
decode Base64, but it does not activate vision and consumes substantial text
context. The captured failure demonstrates that it is insufficient.

### Put the image directly in `role: "tool"`

This can work for some OpenRouter schema paths and Ollama versions, but it is
not documented by OpenAI and is not consistently preserved by Ollama's
internal tool-message handling. It is unsuitable as the portable baseline.

### Send both the complete tool text and the image

This is easy to implement but duplicates the payload and adds unnecessary
Base64 text tokens. It should be rejected except as a temporary diagnostic
mode.

### Expose a temporary local or public image URL

A URL can reduce the request payload, but local/private files would require a
file server or upload service. That introduces lifecycle, access-control, and
data-exfiltration concerns. It is out of scope for the first solution.

### Use each provider's native image API

Native Ollama `images` and OpenAI Responses `input_image` may provide better
provider-specific behavior, but they would fragment the current shared
Chat-Completions transport. They can be considered later where compatibility
requires it.

### Render PDFs into images

Rendering each PDF page into PNG images gives an Ollama vision model a familiar
input shape and works for scanned documents. It can be expensive for
multi-page PDFs, loses native text structure, and may require local PDF
utilities. It should be a fallback when file input is unavailable, not the
primary PDF representation.

### Send PDFs through the image input

This is invalid: `application/pdf` is not an image media type, and providers
may reject the request or misinterpret the bytes. PDFs need a file/document
input or an explicit extraction/rendering step.

## Open design questions

1. Should a `read` Base64 image attachment be automatic, or should the tool
   require an explicit multimodal/media option?
2. Should PDF/file attachment production be automatic for recognized formats,
   or require an explicit media option?
3. Should an attachment own its Base64 data, or reference the existing
   `ToolResult.result` to avoid duplication in memory and session files?
4. Should the capability policy be automatic, explicit per engine, or both?
5. How should an initially configured model be classified before `/models` has
   fetched provider metadata?
6. Should Ylva impose maximum image/file sizes or modality-specific token
   budgets before adding an attachment?
7. Should request dumps redact image and document data, given that the existing request
   dump mechanism can persist complete request bodies?
8. After the model has answered, should later turns retain an image or PDF,
   replace it with a summary, or compact it like other context?
9. Should a provider-specific direct tool-message mode be implemented as an
   optimization after the portable user-follow-up path works?
10. Should the provider expose `detail: "auto"`/`"low"`/`"high"` as configuration,
   or should the first version omit `detail` and use provider defaults?
11. For image-only PDFs, should Ylva prefer provider PDF parsing, local text
    extraction, or rendered page images?
12. Should the file attachment model support uploaded file IDs in addition to
    inline Base64 data?

## Suggested implementation slices

1. Add a provider-neutral attachment type and a focused serialization fixture.
2. Add image/PDF detection and attachment production to `read`.
3. Add image and file wire projections with a short tool marker.
4. Add OpenRouter and Ollama capability parsing and model-selection plumbing,
   including file-support state where available.
5. Add provider-specific request fixtures and local mock-server assertions that
   each attachment appears exactly once.
6. Add PDF fallback handling through local text extraction or page rendering
   where file input is unavailable.
7. Add persistence/replay coverage and verify that image/document data stays out of UI
   previews and tracing fields.
8. Add live, opt-in image and PDF tests using configured capable providers.
9. Add media-aware context compaction as a separate follow-up change.

## Acceptance criteria

- A vision-capable Chat Completions model receives a real image content part.
- The same image Base64 is not also sent as ordinary tool text in that request.
- A file-capable model receives a PDF/file content part rather than an
  `image_url` part.
- Providers without file support use an explicit extraction or rendering
  fallback, or retain the text-only behavior.
- Text-only providers retain the existing result behavior.
- The persisted session can be replayed without losing the attachment.
- UI bubbles, tracing fields, and ordinary diagnostics do not expose opaque
  binary payloads.
- OpenRouter, Ollama, and any file-capable provider request shapes are covered
  independently.
- A request-level test proves that image input is present and the text-only
  fallback remains unchanged.
