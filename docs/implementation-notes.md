# Implementation Notes

Internal mechanics, module/struct/field references, and design rationale that
back the user-facing behavior described in [features](./features.md). This
document is for contributors working on Ylva's internals, not end users.

## Layered configuration and persona assembly

`config::layered` resolves each runtime-layout-relative file independently
from the project `.ylva/` layer, the home `YLVA_HOME` layer, and the
code-owned built-in layer. The first existing file wins without merging lower
layers. Project discovery walks upward from the current directory, preferring
the nearest `.ylva/` ancestor and otherwise the nearest `.git` ancestor. The
global `YLVA_HOME` directory is excluded from `.ylva` marker detection, so a
project beneath the user's home directory cannot select the global home as its
workspace root.

`ProfileConfig` passes the selected file's path into the existing TOML parser
and environment interpolation code, so malformed or unreadable files still
fail with the path that actually won. `ProfileConfig::default()` is the
implicit default floor; explicit profile names still fail when no project or
home layer defines them.

`load_soul` uses the same resolver for the global `SOUL.md` persona.
The code-owned `DEFAULT_SOUL` value is the resolver's built-in floor and is
also used by `Context::new()` and `init_command`. `AgentFactory` stores the
resolved text through both root and child factory paths. `AgentLoop::run`
constructs the initial context from the composed global SOUL and agent-type
prompt, then appends a child's briefing and runtime-budget constraint. Direct
library/test loops that do not receive a resolved prompt retain
`Context::new()` as the compatibility fallback.

`config::instructions` **appends** to that baseline instead of replacing it:
`compose_root_system_prompt` = `load_soul` + `load_trusted_instructions`,
joined by blank lines with one fixed safety preamble and, per file, a
`[instructions from <source>]` … `[end of instructions from <source>]`
sentinel pair. Discovery lives in a pure inner core
(`load_trusted_instructions_from_roots(workspace_root, working_directory,
ylva_home)` — the `load_soul_from_layers` pattern, so unit tests never mutate
the process environment); the public `load_trusted_instructions` resolves
`ylva_home()` and emits one `tracing::info!` line (path, raw byte size) per
contributing file, keeping unit tests independent of subscriber state.

Mechanics worth knowing:

- The allowlist `TRUSTED_INSTRUCTION_FILE_NAMES` is ordered `CLAUDE.md`,
  `AGENTS.md`, and each directory's entries are consumed in that order, so
  `AGENTS.md` wins same-directory conflicts by position.
- Matching compares the *directory-entry* name (`file_name() == "AGENTS.md"`),
  never a path lookup: a case-insensitive filesystem (macOS, Windows) would
  otherwise satisfy `agents.md` for `AGENTS.md`.
- Containment canonicalizes both the file and its owning root (workspace root
  or Ylva home) and requires `starts_with`, so a symlink resolving outside
  its root is a hard error (`ConfigError::InstructionFileOutsideTrustBoundary`)
  rather than a silently trusted path. `InstructionFile` keeps the
  **non-canonical** discovered path so `strip_prefix` comparisons match the
  roots as given.
- `read_to_string` maps I/O failures *and* non-UTF-8 content to
  `ConfigError::Unreadable`, matching `layered::read_external_file`; an
  unlistable chain directory fails the same way, while a `NotFound` listing
  is treated as empty. A raw-size check (`metadata.len()` over
  `MAX_INSTRUCTION_FILE_BYTES`) runs before the content is read, so an
  oversized file never allocates.
- One leading UTF-8 BOM is stripped after reading; content that is empty or
  whitespace-only is skipped without a section. Dedupe compares the
  **BOM-stripped content byte-for-byte** against files already collected in
  composition order — exact duplicates only, first occurrence wins;
  near-duplicates and partial overlaps load as authored.
- `InstructionFile::source` is derived during discovery, where both roots are
  known: `$YLVA_HOME/<file_name>` for the home scope (the variable's name, so
  no username or home path enters the prompt) and the workspace-relative path
  joined with `/` for the workspace scope.
- The composed prompt is not persisted (`Context::set_store`): the two
  leading system messages are rebuilt from the running binary on every run
  and resume, so instruction files are re-read from disk exactly like the
  SOUL.

`agent_type_file` enumerates user-authored `.md` and `.toml` files in global,
profile, and project agent directories. Markdown files use either TOML
frontmatter delimited by `+++` or YAML frontmatter delimited by `---`; the
frontmatter deserializes through the standard `AgentConfig`, then the trimmed
body **overrides** `persona` — the body *is* the persona, so a body-less or
whitespace-only Markdown file fails with `ConfigError::MalformedAgentType`
instead of contributing an empty section. The highest-precedence
file replaces the complete lower-scope definition; inline
`[agents.<type>]` values are deserialized as optional field overrides and
applied afterward. A same-scope file and inline definition is rejected as
ambiguous, while a profile override of a lower-scope file is valid — an
inline `persona` therefore *replaces* the file-sourced persona while the
other overridden fields merge individually. The one agent `persona` is
composed after the global SOUL
(with its trusted instruction sections) and before the tool protocol;
`compose_agent_system_prompt` emits it as a single section. After
the profile's engine set is materialized,
`AgentFactory` validates every agent type's explicit engine reference before
the store or CLI/TUI loop starts; environment-derived engine sets use the same
validation path.

`load_profile_environment_at_workspace_root` merges the home profile `.env`,
the project `<workspace>/.ylva/profiles/<name>/.env`, and process variables in
that order. This snapshot feeds both TOML interpolation and provider
discovery; environment values are never logged. `init_command` writes the
code-owned SOUL, shared profile starter, and annotated profile environment
starter to the home layer. Workspace initialization also writes
`.ylva/.gitignore` to exclude profile environment files. Both record written
and skipped paths in `InitReport`; neither seeds agent-type files.

## Engine/provider runtime configuration

`EngineConfig` contains the engine's immutable connection settings and the
initial values for its model and sampling policy. `EngineRuntimeConfig` is the
shared mutable snapshot for request-level settings (`model`, `temperature`,
`context_length`, and `reasoning_effort`).

`OpenAiCompatibleProvider` and `EngineImpl` share one `EngineRuntimeConfigHandle` rather
than maintaining independent mutable copies. The provider clones the snapshot
before starting a request and releases the lock before network I/O, so a
runtime update cannot produce a request with mixed settings. Updates apply to
subsequent requests; an already-running stream keeps the snapshot it started
with. `context_length` remains available to the engine's context-budgeting
logic and is not sent as an OpenAI request parameter. `reasoning_effort` is
translated into the provider-specific request field when it is explicit;
`None` leaves the provider's own default behavior in place.

Each routed chat request emits a safe structured selection event containing
the named engine, provider connection, and model. The provider emits the
request-start event with the exact runtime snapshot used for that request:
provider connection, model, temperature, effective context length, and
reasoning effort. These events intentionally exclude API keys, authorization
headers, and request bodies; the surrounding `agent_stream` span supplies the
root or child run correlation.

`OpenRouterProvider` reuses that transport for both chat and OpenRouter's
metadata-rich `/models` catalog. Its selected effort is sent as
`reasoning.effort`. `OllamaProvider` accepts the Ollama server origin as its
base URL, derives `/v1/chat/completions` for chat, lists models from
`/api/tags`, and enriches each entry with `/api/show` model metadata. Since
`/api/tags` entries carry no `thinking` object, every model goes through
`/api/show`: its top-level `thinking` object — a `values` array plus a
`default` — provides the selectable levels and the advertised default. Each
`values` element maps to an effort name (non-empty strings pass through
unchanged, `false` maps to `none`, `true` maps to `high`, other JSON types
are skipped, duplicates are removed in server order) and `thinking.default`
uses the same mapping. The show response only upgrades what `/api/tags`
could not provide: an already-populated effort list is never replaced, so a
capability-derived `Some([])` becomes `Some(values)` when the show response
carries `thinking.values`, and a missing advertised default is filled from
`thinking.default`. Without mappable `thinking` metadata the capability
fallback applies (`thinking` capability → `Some([])`, no `capabilities` key →
`None`). An explicit Ollama effort is sent as the OpenAI-compatible
`reasoning_effort` field; models without a mappable `thinking` object expose
no selectable effort alternatives and use Ollama's own default. The Ollama
base URL must be the server origin and must not include the `/v1` path.

`OpenRouterProvider` likewise accepts only the OpenRouter origin and derives
`/api/v1` before constructing the shared OpenAI-compatible transport. A
configured OpenRouter path is not rewritten or supported.

The shared chat transport receives a request-body customization function from
the concrete provider wrapper. This keeps provider-specific wire fields in
`OpenRouterProvider` and `OllamaProvider` rather than centralizing them in a
format-dispatch enum.

The shared `reqwest::Client` is built once at its construction site inside
`OpenAiCompatibleProvider` with a client-level default
`User-Agent: ylva/<version>` (version from `CARGO_PKG_VERSION`). The
non-streaming model/catalog requests use the shared `crate::http` helpers
(`get_json` and `post_json`); chat uses `post_with_retry`; the DuckDuckGo
backend owns a client for its HTML endpoint and submits a `ddgr`-shaped form
`POST` with `DNT: 1` and gzip negotiation; the `reqwest` `gzip` feature decodes
compressed responses before HTML parsing. The non-streaming helpers and
DuckDuckGo create paired dump paths through `RequestDumpSource`, dump the
request before sending, buffer the response once, and dump it before status or
JSON/HTML parsing. `Environment` resolves `YLVA_REQUEST_DUMP_PATH` in
production; an explicit `Template` source lets tests use a temporary path
without mutating process-global environment state. A `RequestDumpContext`
retains the timestamp and dump id while the response path is selected after
headers arrive. After `reqwest` content decoding, buffered response bytes
remain unmodified; the extension maps the media type (`.json`, `.html`, `.txt`,
`.xml`, common media extensions, or `.bin` fallback), while SSE retains its
`.jsonl` event transcript. Transport failures leave a `[transport]` marker in
the `.jsonl` fallback when no response headers exist.
Client-level default headers are not part of the per-request `HeaderMap` that
`dump_request` records, so they do not appear in `YLVA_REQUEST_DUMP_PATH` dumps.

`ModelInfo.reasoning_efforts` normalizes provider metadata into three states:
`None` means no advertised reasoning support, `Some([])` means supported
reasoning without selectable alternatives, and `Some(values)` means supported
reasoning with those alternatives. `ModelInfo.reasoning_default` carries the
provider-advertised default effort — Ollama's `thinking.default`,
OpenRouter's `reasoning.default_effort` — independently of those states: it
is rendered only in the picker's default slot (`reasoning: default
(medium)`) and never changes what counts as advertised. OpenRouter's
`reasoning` object also carries `mandatory` and `default_enabled`, which Ylva
intentionally does not consume (YAGNI): they do not change what is sent, and
`default_effort` already surfaces the fallback behavior. The `/models` picker
adds an implicit
provider-default choice before `values`; the chosen explicit value is carried
in `ProviderModelOption` and applied when the selection is confirmed. Picker
rows render available context lengths as `context: <size>` labels using
integer binary `kb`/`mb` sizes.

`ModelInfo.input_modalities` uses the same conservative distinction for model
input: `None` means unknown, `Some([])` means known text-only, and supported
values are `Image` and `File`. OpenRouter reads
`architecture.input_modalities` from `/models`, while Ollama reads
`capabilities` from `/api/show` and maps only `vision` to `Image`. Unsupported
catalog names are ignored. The `/models` picker appends a bracketed marker
segment with a fixed letter order (`[I]`, `[F]`, or `[IF]`) when a row's
model advertises a non-text input modality; unknown and known text-only
models get no marker segment. The selected `ModelInfo` is copied into the
shared `EngineRuntimeConfig` when `/models` confirms a selection.
Independently, `EngineImpl::hydrate_catalog_metadata` discovers the
configured model's catalog metadata — input modalities plus the
profile-seeded reasoning effort — from the provider catalog once per engine,
lazily before the first chat request; it skips the fetch when a `/models`
selection already supplied the modalities, because such a selection also
validated the effort itself, and a failed attempt logs a warning and is not
retried, so only discovery failure or absent catalog data leaves an initially
configured model's modalities unknown until a `/models` selection resolves
them. The same pass validates the profile `reasoning` value against the
model's advertised efforts with the rule shared with the `/models` switch
(`ModelInfo::advertises_reasoning_effort`): an effort that is a member of the
model's `Some(values)` list is kept silently; an effort the model does not
advertise — including `None`/`Some([])` efforts, which expose no sendable
alternative — is dropped to the provider default with a warning naming the
model, the effort, and the advertised alternatives; a model missing from the
catalog, or a failed catalog fetch, keeps the value unvalidated (logged at
debug and warning level respectively), mirroring modality hydration's
one-shot posture. A dropped effort is a user-facing event, not just a log
line: `EngineImpl` queues a one-shot notice that
`Engine::take_pending_notices` hands out (the router drains every configured
engine, so a connection switch cannot strand one), the agent loop drains it
in `start_request` right after `engine.chat(...)` resolves and forwards it
as `UiUpdate::Notice { text, run_id }` before `StreamStarted`, and each
driver surfaces it — the TUI as a routed `ChatEntry::Notice` handled directly
by `process_one_update` like `UiUpdate::ToolError` (no `DomainEvent`), the
CLI as a `---[ Notice ]---` marker printed ahead of the response.
Both the `named engine selected for chat request` and `chat request
started` log lines carry `reasoning_effort` and `input_modalities` fields
rendered by `format_reasoning_effort_for_log` /
`format_input_modalities_for_log` (`default` or the effort; `unknown`,
`text-only`, `image`, `file`, or `image+file`). Every provider-catalog
fetch — hydration as well as `/models` — logs `model catalog queried`
from `EngineImpl::list_models` with `provider_connection` and
`api_base_url`.

`OpenAiCompatibleProvider` passes the runtime modality snapshot and an explicit
content-part policy to `openai_compatible_messages::serialize_messages_with_policy`.
OpenAI/OpenRouter use object-form `image_url` parts. Ollama selects its
documented string-valued compatibility form for image parts. File parts retain
the provider-neutral `application/pdf` media type and filename, and are emitted
only when `File` is advertised.

`EngineRouter` owns the configured engine set when a profile or environment
provides more than one provider connection. It delegates chat and runtime
configuration to the active engine, groups engines by their named provider
connection, queries each connection once and concurrently for `/models`, and
switches the active connection only after it successfully accepts the
selected model. Connection settings remain owned by each underlying engine;
switching connections does not mutate another connection's configuration.

When constructing a child, `AgentFactory` compares the child's resolved engine
name with the root's resolved name. Matching names reuse the root engine
router, so the child's provider/model display and first requests reflect the
root's active selection at spawn time. A different resolved engine name builds
the child from its own configured engine instead of inheriting the root's
active selection.

`AgentConfig.engine` is a complete-engine selection, not a property-override
layer. There is no per-agent merge of `EngineConfig` fields: specialized
model, sampling, context, reasoning, or connection settings require a
separate named engine. Agents sharing one named engine also share its mutable
runtime selection, including model and reasoning-effort changes. Each
in-flight `Engine::chat` request uses the provider/model snapshot captured when
that request started; a later switch affects the next request, even when it is
another request in the same child run after a tool call.

The `/models` slash command normalizes the draft to `/models <filter>` and
fetches the full catalog once via `Effect::ListModels { filter }`, storing
every fetched `ModelChoice` in `SlashOverlay::ModelPicker { choices, filter,
selected }` — `choices` is never re-filtered out of the vector. Mouse-wheel
events move the highlighted choice through the same bounded selection helper
as keyboard navigation, so the rendered window follows the pointer without
changing the chat scroll position. A left-click on a rendered row updates the
highlight through the same row mapping, preserves the current popup window
when that row is already visible, and never emits `SwitchModel`; only Enter
confirms the choice. The input is
deliberately *not* cleared: `AppState::refresh_slash_overlay` re-derives
`filter` from the live draft on every keystroke while the picker is open (a
prior no-op) and `matching_model_indices` — shared by selection movement,
`selected_model`, reasoning-effort cycling, and rendering — recomputes which
indices are currently displayed/selectable from `choices` + `filter` without
touching the network. `matching_model_indices` splits `filter` on whitespace
and requires every word to appear somewhere in the choice's
`provider_connection/model` identity, case-insensitively — an AND match
across words in any order, not a single joined-phrase substring, so
`/models deepseek v3` and `/models v3 deepseek` both match `stub/deepseek-v3`.
Confirming (`Enter`) or cancelling (`Esc`) the picker
clears the input. When `matching_model_indices` returns no indices,
`tui_widgets::draw_slash_overlay` keeps the panel open and renders a single
"Nothing matched your model filter." row (selection `None`) instead of
hiding the overlay, so the filter stays editable. When the rendered rows
overflow the popup's capped height, the view window is anchored to the
selected row and `render_chat_scrollbar` draws a one-column scrollbar in the
popup's right interior gutter.

`ProviderRegistry` centralizes provider-name validation, environment-based
selection, and construction. A profile separates
`provider_connections.<name>` (connection identity and endpoint) from its
`provider` field (implementation kind), and an engine references the
connection with `provider_connection`. Multiple connections may therefore use
the same implementation. Engine-level endpoint and credential values override
connection values. A missing API key may be filled from the matching profile
environment variable.

Environment discovery loads the selected profile's `.env` plus exported
process variables, and the merged snapshot is used both for provider
discovery and `${ENV_VAR}` interpolation in every TOML string. Typed numeric
and boolean fields parse expanded strings after interpolation. Process
variables win over `.env` values, and missing or invalid interpolation values
fail profile loading. The merged snapshot is never logged.

## Tool-result wire protocol

Every `role:"tool"` message carries its `content` as a JSON struct derived
from `ToolEnvelope`. Success shape:

```json
{
  "ok": true,
  "result": "...",
  "result_metadata": { "field": value, ... },
  "result_metadata_schema": { "field": { "type": "...", "description": "..." }, ... }
}
```

Failure shape:

```json
{ "ok": false, "error": "..." }
```

`result_metadata` and `result_metadata_schema` are omitted when a tool
produces no structured metadata. When present, `result_metadata_schema` is
derived automatically from the tool's typed metadata struct (via `schemars`)
and covers only the fields actually populated for that result — the schema
can never drift from the data. Each tool description sent in the `tools`
array includes its argument schema.

The `glob` result metadata contains flat `files`, `directories`, and `total`
counts, with `total` always equal to `files + directories`, plus a required
boolean `truncated`. `truncated` is `false` when all matching paths are
returned and `true` when `limit` omits additional paths; the omission count is
intentionally not reported. The supplied search root is never a match and is
excluded from returned paths and every count, regardless of the pattern or
option flags. `limit` is optional; omitting it requests no result-count limit,
while arbitrary large values do not represent a separate unlimited mode.
`git_ignore` applies `.gitignore` rules only; it does not
suppress the `.git` directory or other dot-prefixed paths. `dot_ignore`
controls those paths independently and defaults to `false`.
CLI/TUI summaries are a separate tool-provided projection of result metadata,
so tools can omit default-valued properties without changing the complete
metadata sent to the LLM/API. `glob` omits zero-valued `files` and
`directories`, `truncated=false`, and `total` when either type count is zero,
because that total is inferable, except that an empty result displays
`total=0`.

## Exact binary data in `read` and `write`

`FileEncoding` is the shared per-call transport selector for the two
file-system tools. It defaults to `utf8`, preserving the existing text
behavior for `read`'s `bytes` mode and for text `write` calls. Text-oriented
`lines` and `chars` reads require valid UTF-8 and fail with a structured tool
error that suggests retrying with `mode: "bytes"` (and `encoding: "base64"`
for binary data) rather than inserting replacement characters. No CLI flag,
profile key, or other persistent configuration controls it.

`read`'s `mode` is optional; omission selects autodetect. Autodetect reads
valid UTF-8 with `lines` semantics, returns a whole file whose bytes infer a
supported attachment media type (PNG/JPEG/WebP/GIF/PDF) through the Base64
path below — so the attachment and modality-aware projection apply without
the model naming `mode`/`encoding` — and fails with a retry hint for any
other binary file or ranged binary read. Explicit `mode` values are honored
as requested: `lines`/`chars` stay strict UTF-8 and `bytes` keeps its
documented lossy `utf8` behavior.

`read` accepts `encoding: "base64"` only with an explicitly supplied
`mode: "bytes"`. `bytes` mode seeks to `offset` and reads at most `limit`
bytes, so unselected bytes are never materialized; the file's metadata length
decides whether the selection is the complete file before that slice is
encoded with the standard padded Base64 engine without line breaks. The
Base64 string is the complete LLM-facing `ToolCallResult::text()` value, so
NUL bytes and invalid UTF-8 are not lossy. When the selected bytes are valid
UTF-8, the user-facing result projection uses the decoded text preview and the
normal `read_bytes`/`read_lines` metadata. Opaque or invalid UTF-8 bytes
report only `read_bytes`, the selected raw-byte count; they do not report the
encoded string length or `read_lines`, and they suppress `ContentSection`
previews. UTF-8 results retain their current line metadata and content
sections. The default `utf8` encoding in `bytes` mode remains a compatibility
path and may lossily decode invalid bytes; callers that need exact arbitrary
bytes should use Base64.

Text-oriented reads (`lines`, `chars` and the omitted-`mode` path) stream
fixed-size chunks instead of buffering the file: UTF-8 is validated
incrementally with only an incomplete trailing sequence carried between
chunks, the selected line or character window is retained, and the scan
continues to EOF so whole-file validation semantics are preserved. Whole-file
buffering remains only for results that are returned whole — a `bytes`
selection within the output cap, and the complete recognized media read that
bypasses the cap, whose media type is sniffed from the first chunk before the
complete file is re-read from the same open handle.

Every text-oriented `read` result is capped at 262144 raw selected bytes
(256 KiB) before decoding or Base64 encoding. Content past the cap is cut at
the cap — backed off to a UTF-8 char boundary for `utf8` results — with no
in-text marker, reported as `truncated: true` in `ReadResultMetadata` (the
property is pruned from the metadata schema when absent), and labeled
`Partial result` in the content section, matching `bash` and `search`. The cap
wins over a larger explicit `limit`. Complete recognized media reads bypass it
entirely: a truncated image or PDF is unusable, and `media_type` is only ever
recognized for a complete file, so a truncated result never carries a
`media_type` or an attachment. They are bounded instead by
`READ_MEDIA_MAX_BYTES` (8 MiB): a larger whole file fails with an explicit
size error — naming the size, the limit, and the range-read remedy — before
any whole-file load, in both the `bytes` and the autodetect paths.

`ToolCallResult::text()` remains the complete model-facing result for every
tool. `ToolCallResult::display_text()` is the optional user-facing result
projection; Base64 `read` results return `None` so the encoded transport text
cannot appear in the tool bubble. The agent persists that choice as the
`result_section_ids::HIDDEN` result-section identifier, and replay applies the
same suppression while retaining the original result for the model transcript.

`write` accepts standard padded Base64 with the same engine. Decoding happens
before parent-directory creation or target-file opening/truncation, and a
decoder error is surfaced as a failed tool result naming the target path and
decoder reason. `bytes_written` is the decoded byte count. Base64 writes
whose decoded bytes are valid UTF-8 use that decoded text for pending and
completed `ContentSection` previews; opaque or invalid UTF-8 writes retain only the
byte-count confirmation and metadata. UTF-8 writes retain their existing text
previews. The implementation deliberately adds no binary-transfer size limit
or streaming behavior.

The encoded `content` string is never placed in tracing fields, `tool_args`
spans, pending previews, or completed previews. UTF-8 content uses the
existing size/line summary. A valid Base64 write uses
`REDACTED(bytes=<decoded length>)`; an invalid Base64 value uses
`REDACTED(base64_chars=<encoded length>)`. Cross-field read validation runs
after schema validation and before sandbox approval or file I/O, while the
write decoder also guards direct execution paths before any filesystem
mutation.

Complete Base64 `read` results are inspected after byte slicing with `infer`.
Only PNG, JPEG, WebP, GIF, and PDF signatures are accepted. A complete result
produces a `ToolAttachment` descriptor; the descriptor stores only the media
type and, for a PDF, a safe basename. It references the containing
`ToolResult.result` Base64 string rather than owning another copy. Partial
ranges never become attachments even when their leading bytes match a supported
signature. Base64 metadata includes `encoding: "base64"` and includes
`media_type` for recognized files. UI content sections continue to use the
decoded UTF-8 preview when available and remain absent for opaque bytes.

The provider serializer keeps the persisted/domain message model provider
neutral. For a capability-compatible attachment it emits the normal contiguous
`role:"tool"` envelope with a short marker, followed by one hidden,
wire-only `role:"user"` content array with text before image or file parts. The
leading text explicitly maps each media-part position to its originating
`tool_call_id`, tool name, and attachment index, because Chat Completions media
parts have no portable tool-call correlation field. Images use `image_url` data
URIs; PDFs use Chat Completions `file` parts with inline `file_data` and the
retained filename. Unknown, text-only, or modality-incompatible models retain
the complete Base64 tool text and receive no synthetic user message. The
synthetic message is never committed or rendered. Resumed sessions reconstruct
the same projection from the persisted descriptor and canonical result.

`crate::http::dump_request` sanitizes only a cloned request-body projection.
Base64 tool results and inline image/file data become stable redaction markers;
roles, IDs, metadata, media types, and filenames remain available for
diagnostics. `crate::http::dump_buffered_response` writes complete
non-streaming responses as their original bytes; only streaming event
transcripts are line-oriented JSONL. Request metadata retains the encoded URL
query string so search terms and other query parameters remain diagnosable.
Live requests and structured tracing fields are unchanged.

## Path-like argument normalization

Path-like arguments (`read`/`write`/`edit`/`search`'s `path`, `glob`'s
`pattern`) are normalized before dispatch: an absolute value that falls
inside the process's current working directory is rewritten to its
cwd-relative form — this is what actually executes and what the UI renders
for the call, not merely cosmetic. Values that are already relative, or
absolute but outside the cwd, pass through unchanged. This is a
display/consistency nicety, not a security boundary — sandbox approval
checks run independently on whichever path form ends up being used. Each
tool declares its own path-like argument keys via
`Tool::path_like_arguments()`.

The same loop first expands the value's path tokens with
`sandbox::expand_path_tokens(value, lookup)` — a pure helper whose `lookup`
closure is supplied by the caller, so unit tests never mutate the process
environment:

- `$NAME` and `${NAME}` (`NAME` = `[A-Za-z_][A-Za-z0-9_]*`) are replaced by
  `lookup(NAME)` when defined and left literal when undefined — a literal
  `$` in a file name survives, as do malformed or unterminated forms;
- substitution runs exactly once: a substituted value is never rescanned for
  further tokens;
- after environment substitution, a leading `~` or `~/` expands through
  `lookup("HOME")`; `~user/…` is deliberately unsupported.

`ToolRegistry::create_call` performs expansion first and then rewrites the
expanded value with `to_relative_path`, storing whichever of the two results
differs from the original — so an expansion landing outside the cwd keeps its
absolute form instead of being dropped. Expansion is string substitution in
Rust, never a shell, and it runs before sandbox evaluation: the sandbox sees
only the expanded, canonicalized path, and the `outside_workspace` approval
floor still applies. `src/main.rs` exports the resolved `YLVA_HOME` into the
process environment at startup, which is what makes a verbatim
`$YLVA_HOME/AGENTS.md` sentinel source work for file tools and `bash` child
processes alike.

## Sandbox `cross_filesystem` rule

The opt-in `cross_filesystem` named path rule (`src/sandbox.rs`) matches when
the candidate path's filesystem device differs from the canonical workspace
root's. It is evaluated from `SandboxContext::evaluate_named_rule` during the
ordinary `check_path_with_dot_path_access` rule walk, so ordering semantics
(configured list or default floor first, CLI catch-all glob last, last match
wins, no match → `None`) are unchanged.

Three module-level helpers implement it:

- `resolve_device(path)` stats the path and returns the unix `st_dev` value
  (`MetadataExt::dev()`); on non-unix targets it always returns `None`, so
  `is_cross_device` never reports a difference there and the rule never
  matches (the serde variant still parses everywhere).
- `is_cross_device(candidate, workspace_root)` is a pure comparison: `true`
  only when both IDs are known and differ; any `None` (metadata unavailable
  on either side) yields `false` — the rule fires only on a positively
  established device difference.
- `canonicalize_existing_anchor(path)` is a sibling of
  `SandboxContext::canonicalize_best_effort` (whose signature/behavior is
  untouched, since `src/store.rs::project_key` and `docs/persistence.md`
  depend on it): it returns only the canonicalized deepest existing ancestor,
  used as the candidate's device anchor when the target itself does not
  exist yet.

Both devices are resolved **fresh per `check_path` call** — workspace root
from the defensively re-canonicalized root, candidate from the anchor above —
with no caching on `Sandbox`, so a mid-session remount cannot leave stale
device IDs behind.

## `bash` tool process-group cleanup

The shell inherits the process's current working directory, which is the
workspace launch directory in normal runtime. Bash commands should therefore
use relative paths directly rather than starting with a redundant `cd` to the
workspace root.

On timeout or cancellation the entire process group is killed via `SIGKILL`
(Unix) using `nix::sys::signal::killpg` — grandchildren spawned by the script
are also cleaned up. Default timeout is `DEFAULT_BASH_TIMEOUT_SECS` (30s).

Output collection (`collect_output`) drains stdout/stderr lines in arrival
order up to `BASH_OUTPUT_LIMIT_LINES` (1000) whole lines or
`BASH_OUTPUT_LIMIT_BYTES` (65536) bytes, whichever is hit first. Lines beyond
the line cap are discarded. A line that alone exceeds the remaining byte
budget (e.g. a single unterminated base64/JSON blob larger than the cap) is
kept as a UTF-8-safe prefix up to the byte cap rather than discarded wholesale,
so `output_bytes` in result metadata never reports 0 for non-empty command
output that only hit the byte cap. Result metadata records `truncated: true`
when any cap discarded or cut content.

`BashInputArgs` also carries a display-only optional `label`, declared after
`command` so schema rank keeps `command` the bare title primary and `label` a
`label=…` prop. `needs_approval` stays command-only, and
`BashCall::is_default_argument` suppresses empty/whitespace-only labels from
`title_arguments` as if the argument were omitted.

The expanded bubble body is built from one shared section builder
(`label_and_command_display_sections`): a `Label` section when the label is
non-blank, then a `Command` section when the command is non-empty.
`BashCall::pending_content_sections` feeds the pending/running/denied bodies
from the raw arguments, and `BashResult` stores `label`/`command` purely for
display so `content_sections()` can prepend the same sections ahead of its
output section — a completed bubble reads `Label` → `Command` → `Result`,
or → `Partial result` when `BashResult::truncated` is set (the output cap
discarded content; the `truncated=true` title summary prop is unchanged).
The `Command` section carries `file_extension: "sh"`, so the expanded
bubble renders it through syntect with the active theme (`Label` and
`Result`/`Partial result` stay plain; the collapsed/inline title keeps
region styling only, and the CLI ignores the extension hint).
`run_bash`
executes only `command`.

Tool errors merge agent-side: `PendingTool::execute` captures
`pending_content_sections()` before `run()` consumes the call, and every
`ToolError` emission that replaces a pending/running bubble (execution
failure with the `Partial result` section appended after them, outer
timeout, user denial, sandbox auto-denial, stop-while-pending) carries
those sections ahead of its payload sections — so `Message::ToolError`
persists exactly what the live bubble showed and replay matches without
reducer changes.

## CLI option parsing

Implemented in `src/cli.rs` as a `clap` derive model (`Cli`, `CliCommand`),
re-exported from `lib.rs`. `main.rs` consumes the parsed `Cli` instead of
scanning raw arguments:

- `main()` pre-scans `std::env::args()` for `--version` so the rich
  `BUILD_INFO` rendering pre-empts everything (clap's own version flag is
  disabled via `disable_version_flag`).
- Everything else goes through `Cli::try_parse_from`; clap errors are routed
  through the binary's usage-error path (stderr, exit `2`) — clap never exits
  on its own, so post-parse exits can still flush `LogGuards`. Parse errors
  occur before `init_logging`, so there is no log guard on that path.
- Value parsers in `cli.rs` own case-insensitive enum validation
  (`--color`, `--log-level`, `--log-format`) and `--conversation-id` uuid
  validation, preserving the distinctive error phrases the tests assert.
- `Cli::log_config()`, `Cli::resolved_color_mode()` (the `--no-color`
  migration hint), and `Cli::auto_approve_tools()` are the assembly points
  `main.rs` reads; prompt precedence (`--prompt` → piped stdin → demo
  prompt) stays in `main.rs`.

## CLI driver

Implemented in `src/cli_driver.rs`; selected automatically by `main.rs` when
stdout is not a terminal or forced via `--no-tui`.

When `--auto-approve-spawn` is set and the assistant calls `spawn_subagent`,
the CLI keeps the process alive past the assistant's own final `StreamDone`
until every spawned/queued child call has reported a terminal outcome
(`ToolDone` on success/failure/cancellation, or `ToolError` in the
structurally-unreachable no-`ChildAgentManager` fail-safe) — otherwise the
one-shot process would exit as soon as the parent goes idle, silently
discarding the child's result since `spawn_subagent` resolves immediately
with an admission event (`event: "subagent_accepted"`) while the child keeps
running concurrently. Without `--auto-approve-spawn`, `spawn_subagent` is denied
outright in CLI mode (`UserApprovalRequired` has no approval path headless),
so this only matters when that flag is used.

Fixed UI colors are centralized in `theme.rs`, including message roles,
controls, overlays, and Markdown rendering colors. `tui_widgets.rs` and
`markdown.rs` consume that palette, so a future theme switch has one
color-definition module to update. Completed `spawn_subagent` bubbles render
`status=failure` in the error color, and the drill-in header reuses the same
renderer. ANSI styling is enabled only when the
headless output writes to a terminal that is not `TERM=dumb` and `NO_COLOR` is
unset in `--color=auto` mode. `--color=no` and `--color=yes` explicitly
disable or enable ANSI styling for that output path.

Tool-pending and tool-done bodies print the event's `content_sections`
(`--- <label> ---` blocks), so a labeled `bash` call surfaces its
`Label`/`Command` sections in headless output with no CLI-specific code;
the CLI `ToolError` arm prints only the error line. `UiUpdate::Notice` —
for example a profile reasoning effort the pre-chat validation dropped —
prints a `---[ Notice ]---` marker (theme `NOTICE_COLOR`) plus the notice
text ahead of the response, tagged `[child <id>] notice: …` when a child
loop raised it, and never ends the turn.

## Store & persistence

Implemented in `src/store.rs`. See [persistence](./persistence.md) for the
user-facing on-disk contract; this section is the contributor view.

- **One handle, one writer.** `Store` is a cheap `Arc` handle. Every clone
  shares one `flock`ed file and one writer task, so append order is the commit
  order regardless of who holds the clone. The advisory lock is released once
  the last clone drops and the writer drains.
- **Off the hot path.** `Store::record_*` enqueue onto an `mpsc` channel and
  never block; the writer task appends one line per event, `flush`ing but never
  `fsync`ing. `Store::flush().await` is a barrier used by tests and shutdown
  paths — it is fed by a second channel of `oneshot` acks, and the writer
  drains all pending events before acknowledging.
- **One commit point.** `Context::commit` is the only place a message enters
  the conversation, and therefore the only place a `message` event is produced.
  This upholds the blueprint §4C ownership rule: only the outer loop mutates
  `Context`, so no streaming task can ever write. `AgentLoop::with_store`
  attaches the store to the `Context` the loop creates.
- **Session replay.** `Context::set_store` reads the attached session through
  `Store::read_messages` and calls `Context::hydrate`, which appends directly
  to the in-memory message list instead of using `commit`; this prevents
  already-persisted lines from being written again. The rule is unconditional
  for fresh and resumed stores because a fresh file reads as an empty
  transcript. `hydrate` also synthesizes an in-memory `ToolError` for an
  unanswered generic tool call, keeping the next provider payload valid
  without creating a new disk event.
- **Replay projection.** `tui_reducer_state::project_messages` maps persisted
  `Message` values to terminal-free `ChatEntry` values before the first TUI
  frame. `ToolResult`/`ToolError` carry persisted `summary`,
  `content_sections`, and `duration`, and `ToolResult` also carries an
  optional result-section identifier for tools such as `clarify`. This keeps
  the resumed result heading identical to the live bubble; absent identifiers
  fall back to the standard result section. `push_tool_error` preserves the
  actual duration instead of hard-coding zero. Assistant tool calls persist the
  tool-specific `title_arguments` captured when the call is created, so
  `clarify` keeps only its question inline while its context and choices remain
  body sections, and
  replayed bubbles use the same title projection as live bubbles; calls
  without title arguments use an empty title projection. The projection is
  schema-based, so unknown input keys are omitted from CLI/TUI titles, and
  rank follows the schema's field (declaration) order: `bash`'s optional
  `label` rides the persisted `title_arguments` behind `command` as a
  `label=…` prop, with empty/whitespace-only values suppressed via
  `ToolCall::is_default_argument` as if omitted.
  System messages and implicit harness notes remain model-only.
- **Prior-session input history.** At TUI startup, `Store::read_past_explicit_user_messages`
  scans prior top-level sessions in newest-first order, collects at most the
  configured `ProfileConfig::input_history_limit` explicit user messages, and
  deduplicates exact text while retaining the newest occurrence, and returns
  that bounded set oldest-first. The driver prepends it to the current
  session's history without adding those messages to the transcript or model
  context; child-agent transcripts are skipped.
- **Tool-result payload.** `Message::ToolResult` carries a serializable
  `ToolResult` payload as its flattened value, and
  `Context::push_tool_result` forwards that value unchanged. This keeps the
  persisted JSON shape stable while avoiding a positional argument list for
  the complete result.
- **Child replay and lazy loading.** `ChildStart`/`ChildResult` carry
  `call_id` (plus `label` on `ChildStart`) so `Store::read_child_runs` can
  repair a replayed `spawn_subagent` bubble to a finished or interrupted
  state. `replay_messages` creates `ChildConversation` metadata with
  `needs_lazy_load`; `tui_driver` reads
  `Store::session_path(store.chats_root(), store.project_key(), run_id)` only
  on the first drill-in, then caches the projected child messages. Production
  child transcripts use the run id as their session id through
  `Store::write_child_transcript_for_run`, avoiding a second path-correlation
  field. Legacy records missing correlation fields remain readable but
  undrillable/degraded.
- **Every event carries a millisecond-precision timestamp.** `now_timestamp()`
  formats RFC 3339 at `SecondsFormat::Millis`, and every `StoreEvent` variant
  has a timestamp field: `meta.created_at`, `message.timestamp`,
  `child_start.created_at`, `child_result.completed_at`, and
  `model_changed.changed_at`. For a `message` event this is the instant
  `Context::commit` accepted it — effectively "sent" for a user message and
  "response fully assembled" for an assistant message, since only one commit
  point exists per message today; it is *not* a stream-start or
  first-token instant (no such event is persisted). `#[serde(default)]`
  reads a `message` line written before this field existed back as `""`
  rather than failing.
- **Immediate `meta` header, utility-generated title.** The header is written
  to disk right away with an empty `title`, so event commits never wait on
  network I/O. `SessionWriter` tracks only the first explicit user message
  and first non-empty complete assistant reply; assistant messages that only
  contain tool calls are skipped. `Store::title_prompt()` exposes the exact
  delimited prompt once both are available. The CLI/TUI driver resolves
  `ProfileConfig::utility_engine_name()` through `AgentFactory::engine()` and
  invokes that raw engine directly with one user message and no tools, store,
  or UI channel. A normalized non-empty response is persisted by
  `Store::set_title()`, whose writer command reuses the temp-file-plus-atomic-
  `rename()` rewrite of only the first line, then reacquires the lock on the
  replacement inode. `Store::resume` rebuilds the two source messages from
  committed transcript events, so an empty title can be retried by the next
  process; child transcripts use the same flow with their own store handle.
- **`model_changed`, not `meta.provider_connection`/`meta.model`.**
  `meta.provider_connection`/`meta.model` capture only the
  connection/model pair resolved at session creation and are
  never rewritten afterwards. `StoreEvent::ModelChanged` carries the actual
  `provider_connection`/`model` (read off `EngineConfig`, never a
  config-table name) and
  is recorded via `Store::record_model_changed` on every process start (fresh
  or resumed) and again after every successful `/models` switch
  (`Effect::SwitchModel` in `tui_driver.rs` → `AgentCommand::RecordModelChanged`
  → `Context::store()`, so it follows `/clear` rollovers correctly) — it is
  the only authoritative, replayable history of what has actually served the
  session, including that first value. A child's own session file gets its
  own independent `model_changed` history from its resolved engine, passed
  through `ChildTranscriptTarget::engine_config` into
  `Store::write_child_transcript`.
- **Repair-on-open.** `Store::resume` truncates a trailing incomplete or
  unparseable line before the first append. `list_sessions` and any other read
  path never mutate, not even a file with a corrupt tail.
- **Child agents.** `ChildAgentManager::with_store` holds the *parent's* store
  and writes `child_start` (from `start`) and `child_result` (from
  `record_child_result`, called by `AgentLoop::run`'s `finish_rx` arm). The
  child's own transcript is written once at completion by
  `agent_factory::write_child_transcript` via
  `Store::write_child_transcript_for_run`, which opens a fresh sub-session
  under the same project directory using the child run id and passes the
  parent's session id as `meta.parent_session`. The parent link is therefore
  discoverable from the child file too, not just top-down via the parent's
  lifecycle events. The older general-purpose
  `Store::write_child_transcript` helper remains available for callers that
  need a newly generated child session id. `Store::start_new_session` takes an
  explicit `parent_session: Option<Uuid>` for exactly this reason — `/clear`
  passes `None` (a rollover is a fresh session, not a child run).
- **Testing seam.** `chats_root` is an explicit parameter on `Store::create`,
  `Store::resume`, and `Store::list_sessions`; only `main.rs` resolves the real
  `~/.ylva/chats` through `default_chats_root()`. Unit tests pass a `TempDir`
  directly, and the end-to-end test sets `YLVA_HOME` for the spawned binary —
  no `#[cfg(test)]` seam in library code.
- **Mandatory startup persistence.** `main.rs` fails startup when
  `default_chats_root()` cannot resolve the Ylva home or when the session file
  cannot be created or resumed. The CLI has no transient, non-persisted mode;
  `Option<Store>` remains available only to lower-level APIs and tests that
  intentionally run without persistence.
- **`~/.ylva` resolution.** `src/ylva_home.rs` is the single source of truth for
  the Ylva home shared by `chats/` and `profiles/`: `ylva_home()` returns a
  non-empty `YLVA_HOME` verbatim as the home, else `dirs::home_dir()` with
  `.ylva` appended. Its pure core,
  `resolve_ylva_home(override_value, home_directory)`, takes both inputs as
  arguments so it is unit-testable without mutating the process environment.
  `store::default_chats_root()` and `config::default_profiles_root()` both build
  on it and map a missing home to their own domain error.

## Child-agent delegation internals

- `AgentFactory::create` wires a `ChildAgentManager` onto any `Role::Root`
  `AgentLoop` whose resolved `AgentConfig::can_spawn` is `true`; children are
  constructed via the same factory (`Role::Child(ChildRoleConfig { .. })`)
  and never receive `spawn_subagent` themselves.
- A root's `spawn_subagent` tool is built with a sorted catalog of every
  configured agent type and its optional `description`. The catalog is
  serialized in the tool description sent with each model request, so it
  remains available whenever the model can select `agent_type` without adding
  unrelated system-prompt state.
- The current scheduler runs at most one child at a time; further
  `spawn_subagent` calls are queued in an unbounded FIFO and started
  automatically as each preceding child finishes. The model-facing lifecycle
  contract does not expose that scheduling detail, so a future concurrent
  scheduler can deliver completions out of order without changing the
  protocol.
- The immediate `spawn_subagent` tool result is an admission event, not a child
  outcome. It includes `event: "subagent_accepted"`, `run_id`, `tool_call_id`,
  `label`, `status`, `outstanding_count`, and `all_done: false`.
- A child's outcome — success, failure, cancellation, or an "unstructured"
  fallback — is injected into the parent's conversation as a hidden harness
  notice containing `event: "subagent_completed"`, the correlation fields,
  `status`, `result`/`error`, `sources`, `outstanding_count`, and `all_done`.
  It never rewrites or duplicates the provisional tool result; the
  tool-calling wire protocol only permits one tool-result message per
  `tool_call_id`. The notice also tells the root that the result is already
  shown to the user in the tool bubble's Result/Error/Sources sections, so it
  should not repeat or quote it back. Agent-reported prose is rendered as
  Markdown, while runtime diagnostics (for example, an unknown engine) are
  rendered literally so identifiers and punctuation are not reformatted.
  Provider and stream failures are terminal runtime outcomes, not
  unstructured child results.
  Intermediate completion notices do not force an acknowledgement turn. When
  `all_done` becomes true, the root is re-prompted once (immediately if idle,
  or after its current stream/tool round) so it can aggregate the results
  without waiting for a new user message.
- `AgentCommand::Stop` on the parent cancels the running child (and drains
  the queue) via `ChildAgentManager::cancel_all`; each cancelled/queued call
  is resolved to a terminal `ToolDone` (`status: "cancelled"`) and logged
  (`tracing::info!`) — durable persistence of child outcomes is deferred to
  a later phase. Unlike a natural finish, this never re-prompts the root
  (the user explicitly asked to stop).
- `AgentCommand::Stop` emits `UiUpdate::AgentIdle` when it reaches idle
  synchronously (including idle, clarify-pending, and approval-pending
  states). The TUI driver keeps `TuiMode::Aborting` through a pending
  tool's `ToolError` and routes that idle boundary through
  `domain_aborting`, so Ctrl-C can quit and Esc can return to idle without
  depending on a stream-cancellation event.
- A child's own tool calls go through the exact same `ToolPending` →
  approve/deny pause as the root's — a child is never auto-denied just for
  being a child. Each `AgentLoop` stamps its own `run_id` (`None` for the
  root, `Some(child_run_id)` for a `Role::Child`) onto every
  `UiUpdate::ToolPending`/`ClarifyPending` it emits. Drivers (`cli_driver`,
  `tui_driver`) track that `run_id` alongside the pending call and, when
  answering, wrap the verdict in `AgentCommand::ForChild { run_id, command }`
  whenever it's `Some`. The root's command loop resolves `run_id` to the
  still-running child's own command channel via
  `ChildAgentManager::running_command_sender` and forwards the inner command
  verbatim; a `run_id` with no matching running child (already finished, or
  unknown) is logged (`tracing::warn!`) and dropped rather than panicking or
  stalling the root.

### Drill-in TUI conversation view internals

- `ChildAgentManager::start` emits `UiUpdate::ChildAgentStarted { run_id,
  call_id, agent_type, task, label }` immediately before its usual
  `ToolProgress`; `call_id` is the `spawn_subagent` tool call's own id (the
  root `ToolBubble`'s `id`), threaded through purely to correlate a `run_id`
  with its bubble (`ChildRunInfo::bubble_id`). The child's own streamed
  text/thinking tokens are forwarded as `UiUpdate::ChildAgentToken { run_id,
  text }` (merged text and thinking into one buffer — no separate thinking
  bubble for the child), and `UiUpdate::ChildAgentDone { run_id, status,
  result }` marks that child's conversation as finished. The child's own
  `StreamStarted`/`StreamDone`/`StreamCancelled`/`AgentIdle` lifecycle
  events are swallowed, not forwarded — they would otherwise corrupt the
  parent's own `TuiMode`.
- `AppState` tracks `active_view: ConversationView` (`Root` or `Child {
  run_id, agent_type, task, label }`, default `Root`), `child_conversations:
  HashMap<Uuid, ChildConversation>` keyed by `run_id` (each entry holds
  `info: ChildRunInfo { run_id, bubble_id, agent_type, task, label }`,
  `messages: Vec<ChatEntry>`, and `partial_text: String`), `running_child:
  Option<ChildRunInfo>` (the child currently *executing* — cleared on
  `ChildAgentDone`, but its `child_conversations` entry is never removed, so
  a finished child's transcript stays drillable), and `auto_switched_to_child:
  bool`. A `run_id`'s entry is created once, on `ChildAgentStarted`, and
  persists for the lifetime of the TUI session — this is the fix for a bug
  where the entire drill-in model tracked only a single global child
  conversation, which got silently overwritten by whichever child started
  most recently while the user was still viewing an older one's transcript
  (including the "queued child starts while an active one's view is open"
  case). `running_child` (*which child is executing*), `active_view` (*which
  child the user is viewing*), and `child_conversations` (*every child ever
  seen*) are deliberately three independent concerns — conflating
  "executing" with "viewable" was the root cause. The model tracks each
  child by its own map entry rather than a single shared slot specifically
  so it stays correct if child execution ever becomes truly parallel
  (`ChildAgentManager` today still runs one child at a time, but nothing in
  the TUI's viewing/storage layer assumes that).
- Spawning a child never changes `active_view` — the user stays on whatever
  they were looking at. Only an *interactive pause belonging to the
  currently-executing child* (`ToolPending`/`ClarifyPending` tagged with
  that child's `run_id`) auto-switches `active_view` to `Child` and arms
  `auto_switched_to_child` — unless the user is already viewing that exact
  running child's view. This auto-switch fires regardless of what
  `active_view` was beforehand: `Root`, or a different (e.g.
  already-finished) child's view the user had manually drilled into. The
  view active immediately before the auto-switch is snapshotted into
  `AppState.pre_pause_view` (`Root` or the other child's `Child { .. }`) so
  it can be restored precisely. When that pause resolves, the view
  auto-returns to `pre_pause_view` (falling back to `Root` if none was
  recorded) **only if** the switch that got there was automatic — a user who
  navigated to `Child` manually (double-clicking the spawn bubble) is never
  yanked back automatically.
  - Concurrent root-pending-approval and child-interactive-pause precedence
    is not implemented — `TuiMode`/`Focus` remain a single, global pause
    state (matching the "one active child at a time" delegation-depth-1
    constraint), so this is a known, deferred edge case.
- Double-clicking a `spawn_subagent` tool bubble's title resolves the
  clicked bubble's id to its owning `run_id` via
  `AppState::run_id_for_bubble` (a reverse lookup over `child_conversations`
  matching `info.bubble_id`) and switches to that specific child's view
  (`switch_to_child_view(run_id)`) — never "whichever child is currently
  running" or "whichever child started most recently". This is what makes
  drilling into an *older*, already-finished subagent's bubble correct even
  while a newer one is running or queued. This is deliberately distinct from
  the single-click collapse/expand toggle every bubble (including spawn
  bubbles) always supports, so the two gestures never collide. A first click
  never toggles immediately: it only arms `last_bubble_click` `(Uuid,
  Instant)`; the actual collapse/expand toggle is deferred until
  `DomainEvent::BubbleClickWindowExpired` fires (a `tokio::time::sleep`
  future polled in `tui_driver`'s `select!`, mirroring the existing
  `confirm_timeout`/`ConfirmExpired` double-press-abort pattern) with no
  confirming second click having arrived within `DOUBLE_CLICK_WINDOW`
  (250 ms). This keeps the bubble's rendered height — and therefore any
  bottom-pinned auto-scroll — unchanged between the two clicks of a
  double-click, so the second click's row reliably resolves back to the same
  bubble; toggling immediately on the first click was found to break this
  correlation in practice. Because a child's `child_conversations` entry
  isn't removed on completion, drilling in still works after the subagent
  has finished — a lone single click on a finished bubble only
  expands/collapses it once the window expires.
- While viewing `Child { run_id, .. }`, the main chat area renders that
  specific `child_conversations[run_id].messages` instead of `chat_entries`.
  A non-scrolling header above it re-renders the *real* root `spawn_subagent`
  `ToolBubble` (same chrome, same collapsed/expanded state, same
  `agent_type`/`label`/`task` content the user saw before drilling in)
  rather than a separately-maintained header string — the bubble is found
  via `ChildRunInfo::bubble_id`, an exact, permanent correlation captured at
  `ChildAgentStarted` time (the spawn_subagent tool call's own id), not by
  guessing "the most recently started spawn bubble". The header's title is
  shortened to `"Subagent"` instead of the plain title-cased `"Spawn
  Subagent"` shown for the same bubble in the root conversation (via
  `ToolBubbleArgs::display_title_override` in `tui_widgets.rs`). A child's
  transcript is **not** seeded with a synthetic task message — the header
  bubble already shows the task. If the child is paused for its own
  approval/clarify, the input box behaves normally (`Enter` routes to the
  child's flow via `Focus::ToolApproval`/`ClarifyPending`, unaffected by
  `active_view`); if the child is merely running, `Enter` with a non-empty
  draft is a no-op instead of silently steering the *root* agent — the
  previous behavior queued a steer for `Root` while the user was looking at
  the child's transcript, with no visual indication of which conversation
  the message actually went to. This is called out in the footer (`enter
  send` is omitted entirely while viewing a running child), not via a
  separate overlay on the input box, since keystrokes are still buffered
  normally either way.
- The spawn-bubble uses the same `TOOL_BUBBLE_BORDER_COLOR` border as every
  other tool bubble — no separate color styling was introduced for it.
- `tui_widgets::draw`'s header-to-chat divider (rendered only while the
  header bubble, i.e. the child view, is showing), its chat-to-input divider
  (rendered unconditionally, gated per-frame on
  `TuiState::child_view.child_view_bubble.is_some()`), and the conversation
  scrollbar (`render_chat_scrollbar`'s `color` parameter, threaded through
  from the same per-frame check) are all tinted `CHILD_VIEW_SEPARATOR_COLOR`
  (an alias of `TOOL_BUBBLE_BORDER_COLOR`, so it always matches whatever
  color tool bubbles are themed with) instead of the root conversation's
  neutral `SEPARATOR_LINE_COLOR`/`CHAT_SCROLLBAR_COLOR` while drilled into a
  child — a persistent cue to distinguish subagent conversation from main
  conversation.
- The footer/header spinner glyph is driven by `AppState.spinner_frame`,
  incremented once per tick in `tui_driver`'s event loop — gated by
  `spinner_should_tick()`, which checks `agent_mode() != AgentMode::Idle`
  rather than raw `TuiMode`. This matters specifically for a spawned
  subagent: the root's own `TuiMode` settles to `Idle` almost immediately
  after `spawn_subagent` is called (the root's own turn is done; it's just
  waiting on the tool), so gating on raw `TuiMode` alone would freeze the
  spinner glyph on both the footer and the child-view header bubble for the
  subagent's entire run. `agent_mode()` already reports `ToolRunning`
  whenever a tool bubble (including `spawn_subagent`) is still `Running`,
  keeping the spinner animating — including while the subagent itself is
  paused for its own inner tool approval, since that only sets
  `Focus::ToolApproval` and never touches the root `spawn_subagent` bubble's
  `Running` state.
- The footer's right-aligned `named-engine → model-id` identity and explicit
  reasoning effort are projected from the live root engine or the configured
  child engine, so they follow the transcript currently on screen. The named
  engine distinguishes agents that share one provider connection. The provider
  connection remains available in the display snapshot for runtime diagnostics,
  but is intentionally omitted from the compact footer. The effort is shown in
  parentheses only when it is not the provider default. Root model or provider
  switches refresh the label before the next frame.
- The main-conversation footer derives separate running and queued subagent
  counts from root `spawn_subagent` bubbles. A `Running` bubble with
  `status=queued` in its interim progress arguments contributes to the queued
  count; other active `Running` bubbles contribute to the running count.
  Terminal bubbles and the retained `child_conversations` history are not
  counted. The compact label is rendered as `Subagents: <running> running,
  <queued> queued`, omitting zero-valued categories. The counts are omitted
  from the drilled-in child footer.
- Footer keybinding hints are represented as structured key/action entries
  rather than one formatted string, so `tui_widgets` can render the key in
  bold, the action in normal text, and ` · ` between entries.
- A thin separator line (the same dim `─` rule used between the chat area
  and the input/footer panel) is rendered directly below the child-view
  header bubble, so it never reads as part of the scrollable
  conversation-list beneath it.
- The header bubble scrolls independently of the chat area when its
  rendered content exceeds the clamped max height (40% of the terminal):
  `AppState.child_view.header_bubble_scroll_offset` drives a
  `Paragraph::scroll` offset, clamped each frame in `tui_widgets::draw` to
  the header's actual overflow, and a scrollbar (reusing
  `render_chat_scrollbar`) appears in a 1-column gutter to its right only
  when the content overflows. Mouse-wheel events over the header's rect
  (`AppState.layout.header_bubble`, hit-tested in `presenter::route_mouse`
  ahead of the clarify-banner/chat-area checks) scroll the header instead of
  the chat area. The header also keeps a plain-text line cache and selection
  range in `AppState.child_view`; a left drag maps screen rows through the
  independent scroll offset, highlights the visible range, and copies a
  non-empty selection on mouse-up. The offset resets to `0` whenever a fresh
  child view is entered (`switch_to_child_view` / `ChildAgentStarted`).
- `switch_to_child_view` (manual drill-in via double-click) also re-pins the
  main chat area's scroll to the bottom (`conversation.scroll_offset = 0`,
  `conversation.scroll_locked = true`) — these fields are shared between the
  root and child views (not per-view state), so a non-bottom position left
  over from scrolling up while viewing `Root` would otherwise carry straight
  into the child transcript. Before doing so, it saves the root's own
  `(conversation.scroll_offset, conversation.scroll_locked)` into
  `AppState.saved_root_scroll` (an immediate best-effort fallback), and also
  captures the drilled-into `spawn_subagent` bubble's `Uuid` into
  `AppState.saved_root_scroll_bubble`. Manual "back to parent" navigation
  (`switch_to_root_view`, triggered by `Esc` with an empty draft) restores
  `saved_root_scroll` for the current frame, and moves
  `saved_root_scroll_bubble` into `AppState.pending_scroll_to_bubble` for
  precise resolution: once `active_view` has flipped back to `Root` and the
  next frame's `AppState.conversation.bubble_title_rows` reflects root's
  transcript again, `render::sync_from_draw` looks up that bubble's
  *current* row and scrolls there, unlocking scroll — this stays correct
  even if the root conversation grew while the child view was open. The net
  effect: the user lands back exactly at the root `spawn_subagent` bubble
  they drilled in from, not just near it. `note_child_interactive_pause`'s
  auto-switch into `Child` (an interactive pause, not manual navigation)
  never populates `saved_root_scroll`/`saved_root_scroll_bubble`, so its
  later auto-return via `switch_to_root_view` is a no-op on scroll.
- When a tool-approval is pending for the tracked subagent and the user is
  looking at `Root` (having manually navigated back via `Esc` while the
  subagent's own tool call is still awaiting approval), the approval
  footer's title is prefixed with `"Subagent tool '<name>' requests your
  approval:"` instead of the plain `"Tool '<name>' requests your approval:"`
  — otherwise the pending approval is invisible from the root transcript.
  The distinction is derived from whether `Focus::ToolApproval`'s `tool_id`
  lives in any `child_conversations` entry's `messages` (subagent) or
  `chat_entries` (root), independent of `active_view`.
- Clicking anywhere on the child-view header bubble toggles its collapsed
  state after mouse-up when no drag range was selected — unlike a normal chat
  bubble, this needs no click-vs-double-click debounce, since the drill-in
  gesture only applies from the root transcript.
  `AppState::header_bubble_id()` is the single source of truth for which real
  `chat_entries` bubble the header re-renders (used by both
  `render::project_for_draw` and `presenter::route_mouse`), and
  `AppState::toggle_header_bubble_collapsed()` flips that bubble's
  `collapsed` flag directly — deliberately not reusing
  `toggle_tool_bubble_collapsed`, which also clamps
  `conversation.scroll_offset`/`conversation.scroll_locked` against
  `conversation.bubble_title_rows`; those fields describe the
  currently-visible chat area (the child's own transcript), not the header,
  so touching them here would be a side effect on the wrong view's scroll
  state.
- The `collapsed` flag toggled above lives on the same `chat_entries`
  `ToolBubble` entry that represents the `spawn_subagent` call itself, so it
  survives the subagent finishing: neither `DomainEvent::ToolDone` (which
  only patches the entry's `state`) nor `ChildAgentDone` (which only flushes
  pending partial text and may switch `active_view` back to `Root`) touch
  `collapsed`. A manual expand is therefore preserved when the subagent
  completes, and re-entering the drilled-in view later (e.g. by
  double-clicking the now-finished bubble from `Root`) still shows it in
  whichever state it was last left. Like any other tool bubble, a
  `spawn_subagent` bubble still *starts* `collapsed` by default until the
  user toggles it once.

## TUI module layout

The TUI follows a **reducer/MVP** split across source modules. It is selected
automatically in `main.rs` via `std::io::stdout().is_terminal()`; the TUI is
a rendering layer on top of the headless `AgentLoop` — the agent architecture
itself is unchanged.

### Model

`src/tui_reducer_state.rs` — pure view-model with no I/O. Defines `AppState`,
`TuiMode`, `Focus`, `AppLayout`, `Effect`, `InputEvent`, and `DomainEvent`.

### Presenter

`src/presenter.rs` — pure `update(state, event) → Vec<Effect>` reducer. No
ratatui imports, no channel sends; fully unit-testable without a live
terminal.

`route_domain` (the `DomainEvent` dispatch entry point) logs a `DEBUG`
`"domain event handled"` event for every `DomainEvent`, carrying only the
event's variant tag (`event`), its tool/clarify `id` when it has one
(`correlation_id`), and the `TuiMode` transition (`mode_before`/
`mode_after`) — never chunk text, tool arguments, or tool output. This is
enough to follow the TUI's state-transition flow and match up a call's
`ToolStart`/`ToolProgress`/`ToolDone` (or `ClarifyPending`) sequence across
log lines, without risking sensitive payloads in the log file.

### View

`src/render.rs` + `src/tui_widgets.rs` — `render(frame, state)` entry point.
Writes computed geometry back to `state.layout`; delegates widget rendering
to helpers in `tui_widgets.rs`. `AppState::display_metadata` carries the
selected runtime profile, its resolved config path, Ylva home, and workspace
root into the startup logo and help popup. Runtime paths beneath the
operating-system home directory are rendered with a `~` prefix; paths outside
that directory remain absolute. The view stores the plain rendered lines and
content geometry for both surfaces, allowing the Presenter to map mouse drags
through centered or scrolled content and emit clipboard effects without
coupling it to ratatui.

### Driver

`src/tui_driver.rs` and `src/cli_driver.rs` — async event loops, one per
runtime mode, selected by `main.rs` based on whether stdout is a terminal.

`tui_driver.rs` owns `TextArea` and channel handles, converts `AppAction →
InputEvent` and `UiUpdate → DomainEvent`, calls `update()`, then executes the
returned `Vec<Effect>` via `execute_effects()`.

The real TUI also owns the terminal-title lifecycle. It saves the existing
title before entering the alternate screen, displays the persisted session
title with the `Ylva Agent Harness` suffix, and restores the previous title on
exit. A successful background title-generation task sends a metadata-only
notification to the TUI loop; the loop performs the terminal write so title
updates cannot interleave with frame rendering. Notifications carry the
session ID and are ignored after `/clear` if they belong to the previous
session.

`cli_driver.rs` consumes `UiUpdate` events from the agent and streams the
assistant response to stdout. The session store remains the canonical
conversation record; `main.rs` prints only the session resume hint to stderr
after the run.

## Slash commands

`src/slash_command.rs` is the whole parser: a closed allowlist
(`SLASH_COMMANDS`) plus `parse_slash_command` (exact match on the raw draft)
and `slash_command_suggestions` (prefix match, drives the overlay). It is
pure and has no knowledge of the TUI.

The Presenter (`presenter.rs`) checks the **raw**, un-dedented draft on
`Enter`, so only a `/` in the input's first cell can start a command. While
suggestions are visible, the selected index is kept in `AppState`; `Tab` emits
`Effect::SetInputText` with the selected command's canonical name, and plain
`Enter` activates the selected command even when the draft is only a prefix.
The `/models` picker queries provider-connection-qualified
options from `EngineRouter`, displays `provider_connection/model`, has the
same bounded `↑`/`↓` and mouse-wheel movement semantics, maps left-clicks to
highlight-only selection without unnecessary window repositioning, and uses
`page-up`/`fn-up` and
`page-down`/`fn-down` to move the highlighted option by three entries, uses
`←`/`→` to cycle reasoning efforts when available, and `Enter` activates both
the selected connection, model, and effort;
`Esc` closes any open overlay and clears the draft (before the usual
draft-clear/abort ladder), since leaving the draft in place would make the
next per-action `refresh_slash_overlay` recompute reopen the very overlay
just dismissed.
Model rows identify reasoning as unavailable, at the default (naming the
provider-advertised default when one is known, e.g. `default (medium)`), or
adjustable with a `⚙` marker, and append `[I]`/`[F]` input-modality markers
when the model advertises a non-text input modality. The user-facing fixed
value is rendered as `default`, suffixed with the advertised default in
parentheses when the catalog provides one. The
footer switches from the normal `enter send` hint to the controls for the
active command overlay, including the model picker's reasoning, switch, and
cancel keys.
Left-dragging within any visible slash-command overlay row selects its
rendered text, highlights the range while dragging, and copies the selected
text to the clipboard on release without changing the command or model
selection.

The same selection pipeline is used by the startup wordmark and help popup:
the View records the rendered lines and geometry, while the Presenter owns
mouse routing and emits `Effect::CopyToClipboard` on release. Help-popup
coordinates include the current vertical and horizontal scroll offsets.

Slash effects (`Effect::ClearConversation`/`ShowTools`/`ListModels`/
`SwitchModel`) are the only ones that need the engine, so
`tui_driver::split_slash_effects` peels them out of each batch and
`execute_slash_effects` handles the immediate ones; the runtime driver hands
`ListModels` to a background task, while `execute_effects` matches all of
these engine effects as no-ops.
The driver sets `SlashOverlay::LoadingModels` after the `/models` draft is
normalized and starts provider catalog discovery in a cancellable
driver-local task. The next normal loop render displays the loading state.
The task's result is handled by a separate `select!` branch, so the main loop
continues consuming agent updates, input, timeouts, and spinner ticks while
discovery is pending. Dismissing the loading overlay cancels the pending
request; completed results are ignored unless the loading overlay is still
active.

Two rules keep this consistent with blueprint §4C:

- Nothing on the Driver side writes to the session store. `/models`'
  switch is persisted by sending `AgentCommand::RecordModelChanged` (a
  structured `model_changed` store event only — never a conversation
  message, since a mid-conversation `role:"system"` entry is rejected by
  some providers/chat templates), and `/clear` rolls the session file over
  by sending `AgentCommand::ClearContext` — both handled inside the agent
  loop, the sole `Context` mutator. The human-readable confirmation is a
  TUI-only notice (`AppState::push_notice`) that is never added to
  `Context`.
- `AgentCommand::ClearContext`'s handler reads the *live* `engine.config()`
  (not a value cached from session creation) and passes its
  `provider_connection`/
  `model` to `Store::start_new_session`, so the successor session's `meta`
  header reflects whatever backend actually served the conversation up to
  that point — including a `/models` switch that happened since the session
  was created — rather than reusing whatever
  `meta.provider_connection`/`meta.model` were
  written at creation.
- `AgentCommand::ClearContext`'s handler first interrupts whatever is
  in-flight — cancelling the current stream/tool token, calling
  `child_agent_manager.cancel_all()`, and draining `pending_tools`/
  `pending_steers` — the same shutdown `AgentCommand::Stop` performs. This
  alone can't close the race: cancellation is cooperative, so an
  already-running stream/tool/child task can still emit one more
  `AgentEvent`/`FinishResult` after the clear, produced against the
  soon-to-be-replaced `Context`. A per-loop `generation` counter (bumped on
  every `ClearContext`) is stamped onto every `AgentEvent` at the moment its
  producer is spawned; the loop drops any event whose stamped generation
  doesn't match the current one. A `cancelled_run_ids` set records child
  `run_id`s cancelled by `Stop`/`ClearContext` so a straggling `FinishResult`
  for one of them is dropped by the `finish_rx` arm instead of being applied
  to a `Context` it no longer belongs to.

`AppState::available_tool_names` is captured once, at TUI startup, from the
resolved `AgentLoop::registry()` (sorted, since the registry is a `HashMap`),
which is why `/tools` needs no I/O.

## TUI behavior internals

- **Tool-bubble args line vs body sections:** multiline scalar title
  arguments are additionally stashed as expandable content sections (the
  value stays sanitized inline on the args line). In
  `append_tool_bubble_lines`, a title-derived extra whose label
  case-insensitively matches an existing state section label is dropped —
  so a multiline `bash` command renders once, under the state `Command`
  section, keeping `Label` before `Command` — and the expanded args line
  is skipped entirely when every title key is covered by a
  content-section label (bash: `Label`/`Command` cover `label`/`command`).
  The collapsed title always keeps the args; tools whose sections do not
  cover their title keys (`edit`, `write`, `spawn_subagent`) and old
  persisted bubbles without the covering sections render the args line
  unchanged. The gate is data-driven (content-section labels only, not a
  tool-name check) and leaves `has_args` — which also feeds the expand
  indicator and the `→` summary arrow — untouched.
- **Steer bubble lifecycle:** a steer (a message typed while the agent is
  busy) is rendered `Queued` immediately, near the bottom of the chat, but is
  only *staged*, not yet committed to the conversation log — this avoids
  fixing its position ahead of thinking/tool-call content for the in-flight
  round that hasn't been flushed yet. It becomes `Steered` and is committed
  to the log — after everything already flushed for that round — once
  `SteerApplied` arrives, at which point it renders identically to a normal
  `Sent` message: once injected, the model has seen it like any other user
  message, so the UI only distinguishes "queued" from "seen", not *why* a
  message was seen.
- **Double-press `Ctrl+C`/`Esc`:** each key has its own 2-second confirm
  window (`CONFIRM_TIMEOUT`); only a second press of the *same* key confirms
  — pressing the other key starts its own fresh window instead, so e.g.
  `Ctrl+C` then `Esc` never quits on that `Esc`.
- **Smart auto-scroll:** `DomainEvent::StreamDone`/`StreamCancelled` do
  **not** force auto-scroll back on — a completed response respects
  whatever scroll-lock state the user left it in, the same way in-flight
  `StreamChunk`/`StreamThinkingChunk` updates already did; otherwise
  scrolling up to view an earlier subagent bubble (or a plain scroll-back in
  a long conversation) would get yanked back to the bottom the moment the
  current response finished.
- **Footer mode-word disambiguation:** `TuiState::agent_mode` follows the
  transcript currently shown, while `TuiState::parent_agent_mode` retains the
  root loop's status. Once a viewed child's `spawn_subagent` bubble reaches a
  terminal state while the root is still busy producing its final reply or
  running another child, the footer overrides the word with `"Subagent done,
  parent working…"` instead of the misleading `"Idle"`/root status. While
  drilled into a live child, the root's `agent_mode()` would report
  `"Running tool…"` for as long as the `spawn_subagent` bubble is `Running`
  — correct for the root footer but misleading for the child's own
  transcript — so the footer instead inspects the child's own `TurnBuffer`
  (projected per-child via `render::project_for_draw`) and shows `"Subagent
  thinking…"`/`"Subagent streaming…"`/`"Subagent working…"` as appropriate.
- **Queued subagent bubbles:** show a static `≡` glyph instead of the
  animated spinner while `status=queued` (waiting for the running slot to
  free up); the spinner only appears once `status=running`. The internal
  `run_id` used to correlate child lifecycle events is omitted from the
  rendered title in both the root and child conversation views, and live
  `status=running`/`queued` values are communicated by the indicator and
  footer rather than rendered as title properties.
- **Shift+Enter / Alt+Enter:** insert a newline via the terminal's
  modified-key encoding. The driver enables the kitty keyboard protocol
  (`DISAMBIGUATE_ESCAPE_CODES`) for terminals that support it; terminals that
  encode Shift+Enter as a line feed (such as Zed's terminal) are normalized
  before presenter routing.
- **Message dedenting:** submitted messages are automatically dedented —
  common leading whitespace is stripped (via `textwrap::dedent`) and
  leading/trailing blank-only lines are removed.
