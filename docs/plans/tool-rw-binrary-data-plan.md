# Exact Binary Data Support for the `read` and `write` Tools

## Context

Ylva's model-facing file-system tools are implemented in
`src/agent/tools/read.rs` and `src/agent/tools/write.rs`, registered through
`src/agent/tools.rs`, and transported as JSON tool calls/results. Both tools
currently expose `String` content. The `read` tool already reads raw bytes and
supports byte ranges, but converts the selected bytes with lossy UTF-8
decoding. The `write` tool writes the UTF-8 bytes of its `content` string.
Consequently, neither tool can transfer arbitrary binary data exactly through
the JSON boundary.

The goal is to add an explicit standard Base64 representation for binary
payloads while leaving calls that omit the new option behaviorally unchanged.
The `write` idea selects Base64 rather than copying from a local source or
fetching a URL; local copy and URL fetching remain out of scope. The sparse
`read` idea is resolved here symmetrically: `read` gains the same optional
`encoding` selector, and `encoding: "base64"` is valid with `mode: "bytes"`.
Byte `offset`/`limit` slicing happens before Base64 encoding, so a model can
read an exact byte range. Text-oriented `lines` and `chars` modes continue to
use the existing lossy UTF-8 behavior and are not silently converted into a
different unit.

Definition of done:

- A model can write arbitrary bytes by sending `content` as standard Base64
  with `encoding: "base64"`, and a subsequent Base64 read returns a payload
  that decodes byte-for-byte to the file contents.
- A model can read arbitrary bytes with `mode: "bytes", encoding: "base64"`,
  including NUL bytes and invalid UTF-8, without payload corruption.
- Omitting `encoding` preserves the current UTF-8 text behavior for both
  tools, including existing line/character slicing, write confirmations,
  approval behavior, and result rendering.
- Invalid Base64 is reported as a structured tool failure without a panic or a
  partial write; incompatible read mode/encoding combinations are rejected
  before file I/O.
- Generated tool schemas and descriptions document the new argument and its
  default. Standard Base64 payloads are never copied into tracing logs,
  argument spans, or user-facing content sections; redacted summaries remain
  useful for valid and invalid payloads while the complete Base64 read result
  remains available to the model.
- Unit and integration coverage verifies round trips, raw-byte ranges,
  invalid input, UTF-8 backward compatibility, schema/default behavior, and
  logging redaction.

Key constraints are the existing JSON string tool-result contract, the
existing path sandbox (`read` remains auto-approved and `write` remains
approval-gated), the current full-file-in-memory I/O design, and the
repository's tracing-test isolation rules. The implementation must use the
standard Base64 alphabet without introducing network access, source-path
copying, a new CLI/configuration setting, or a new approval policy.

Start by opening `src/agent/tools/read.rs` and `src/agent/tools/write.rs`,
then run the focused library tests for the two modules before changing code.
The implementing agent must flip each plan checkbox from `[ ]` to `[x]`
immediately after completing that step and rewrite this sidecar in place so
progress remains resumable across sessions.

## Implementation Plan

1. [ ] Establish one shared file-encoding contract and dependency.
   - Add the direct `base64` dependency (using the already-resolved 0.22
     series) to `Cargo.toml` and let Cargo update `Cargo.lock`.
   - Define a shared `FileEncoding` value in the tool layer, with JSON/schema
     values `utf8` and `base64`, a default of `utf8`, and the usual
     `Deserialize`/`JsonSchema`/comparison derives. Use the standard Base64
     engine so read and write cannot drift in alphabet or padding behavior.
   - Put the input/result distinction in the `encoding` field comments on
     `ReadFileArgs` and `WriteFileArgs`, not in the shared enum itself.
     Preserve `schemars` property order and descriptions used by invocation
     titles and validation.

2. [ ] Extend the `read` tool with exact Base64 output while preserving text
   reads.
   - Add an optional, defaulted `encoding` field to `ReadFileArgs` and update
     `ReadMode`, argument descriptions, `ReadTool::description`, and the
     generated schema to state that Base64 requires `mode: "bytes"`.
   - Keep the existing raw-byte read and byte-range slicing. For `utf8`,
     retain the current `lines`, `bytes`, and `chars` behavior and line
     metadata. For `base64`, require that the caller explicitly selected
     `mode: "bytes"`, encode the selected raw bytes with the standard padded
     engine and no inserted line breaks, and return that string as the
     LLM-facing result.
   - Track the selected raw-byte count separately from the encoded string
     length. For Base64 results, expose conditional metadata containing only
     `read_bytes` (the selected raw-byte count), omit `read_lines`, and return
     no `ContentSection`; the Base64 result must still be sent to the model.
     Keep the current metadata and content-section behavior for UTF-8 results.
   - Override `ReadTool::validate_args`, call the existing schema validator,
     and reject `encoding: "base64"` with `lines` or `chars` before sandbox
     approval or file I/O. Any encoding or conversion failure must become a
     descriptive `ToolError`, never a panic.
   - Keep read logs limited to safe sizes/counts; add no content or decoded
     payload to debug/error fields.

3. [ ] Extend the `write` tool with Base64 input and safe result presentation.
   - Add an optional, defaulted `encoding` field to `WriteFileArgs`, and update
     its doc comment, `WriteTool::description`, generated schema, and default
     argument handling so omitted/`utf8` calls remain compatible.
   - Decode strict standard padded Base64 into bytes before writing when
     requested; keep the existing UTF-8 string path unchanged. Report decoded
     bytes in `WriteResult.bytes_written`, and return a
     `ToolError::failed("write", ...)` naming the target path and decoder
     reason before creating a parent directory or opening/truncating the
     target if decoding fails.
   - Preserve parent-directory creation, path normalization, sandbox checks,
     approval gating, timeout handling, and the existing confirmation text.
   - Centralize redaction used by both `WriteCall` and `WriteTool` so `content`
     is never present in a span/log. For Base64, use
     `REDACTED(bytes=<decoded length>)` when decoding succeeds and
     `REDACTED(base64_chars=<encoded length>)` when it does not; never include
     either the encoded string or decoded bytes.
   - Prevent Base64 payloads from being copied into pending or completed
     content previews, while retaining the current content sections for
     UTF-8 writes. Keep the binary result useful through the byte-count
     confirmation and metadata rather than rendering a large opaque string.
     Preserve the existing non-empty `content` requirement for all encodings.

4. [ ] Verify tool-contract and presentation wiring at the common boundaries.
   - Confirm the registry continues to expose the updated schemas without
     special-case registration changes and that `Tool::validate_args` rejects
     malformed/incompatible calls before approval or I/O.
   - Ensure `encoding` follows schema-defined argument ordering, is omitted
     when absent/defaulted where the existing invocation projection omits
     defaults, and never becomes a body/content argument.
   - Ensure `ReadResult` and `WriteResult` metadata schemas describe raw bytes
     rather than Base64 transport length where that distinction matters, and
     that Base64 result projections omit opaque content sections while
     persisted/replayed tool bubbles remain compatible with the existing
     `ToolCallResult`/`ContentSection` contract.

5. [ ] Add focused unit tests for read/write behavior and redaction.
   - In the module tests for `read.rs`, cover exact Base64 output for bytes
     containing NULs and invalid UTF-8, raw byte `offset`/`limit`, default
     UTF-8 text compatibility, explicit-mode validation, invalid
     mode/encoding combinations, conditional raw-byte-only metadata, omitted
     Base64 content sections, and schema descriptions/defaults.
   - In the module tests for `write.rs`, cover Base64 round-trip bytes,
     decoded byte counts, invalid Base64 with no created/truncated file,
     default UTF-8 compatibility, schema/default behavior, and the
     UTF-8-versus-Base64 content-preview policy, including the existing
     non-empty-content rejection.
   - Extend logging assertions to use distinctive payload sentinels and prove
     neither encoded nor decoded content appears in file-operation logs or
     `tool_args` spans. Use `capture_logs` for log assertions and
     `run_locked` for every async test that exercises tracing-instrumented
     `read_file`/`write_file`, as required by `docs/known-test-issues.md`.

6. [ ] Add an integration test through the public tool registry and call
   protocol.
   - Add a focused integration test file (for example,
     `tests/tools_binary_data.rs`) that creates a temporary workspace,
     constructs the normal `default_registry`, creates real `ToolCall`
     instances from JSON arguments, and runs `write` then `read` without
     reaching into private helper functions.
   - Assert that a Base64 write followed by a Base64 read preserves a
     representative binary fixture exactly, and that invalid Base64 returns
     the public `ToolError` shape without altering the destination.
   - Assert that binary read/write calls expose only size metadata and no
     opaque Base64 `ContentSection`, while the Base64 read text remains
     available through `ToolCallResult::text`.
   - Also exercise an omitted-encoding text write/read to guard the
     backwards-compatible wire path. Keep the integration test independent
     of ambient provider/configuration state and use the existing temporary
     directory and sandbox patterns.

7. [ ] Update the documentation that describes the affected contracts.
   - Update `docs/features.md` to replace the current UTF-8-only WIP wording
     with the `read`/`write` encoding arguments, byte-range/Base64 behavior,
     default text compatibility, the model-only binary result versus
     size-only user presentation, and the fact that configuration is per-call
     rather than a CLI/profile setting.
   - Update the matching typed-argument/tool-contract excerpt in
     `docs/blueprint.md` so its `read` schema includes the shared encoding
     contract and the explicit bytes-mode requirement.
   - Add the implementation-level Base64, raw-byte metadata, user-preview,
     redaction, validation, and no-size-limit rules to
     `docs/implementation-notes.md`; do not change
     `docs/configuration.md` because no configuration key is introduced.
   - Do not edit the two idea files; they remain the source requirements for
     this implementation plan.

8. [ ] Run the repository's required formatting, lint, and coverage gates.
   - Run `make format` so Rust and Markdown changes follow repository
     formatting, then run `make lint`.
   - Run the single mandatory test gate `make test-coverage`; it must pass
     the full suite and maintain the configured 80% line-coverage threshold.
   - If any gate exposes a regression, fix the implementation/tests/docs,
     keep the plan checkboxes current, and rerun the affected gate before
     marking this step complete.

## Proposed Improvements

- Keep the initial contract deliberately bounded to standard Base64 and
  existing in-memory tool execution. If large binary transfers become common,
  evaluate chunked/streamed tool results separately rather than adding hidden
  size limits or URL/copy semantics to this change.
- Consider adding a small shared helper for safe encoded-payload summaries if
  another future tool accepts binary strings; do not generalize beyond the
  `read`/`write` surfaces until a second caller exists.

## Implementation Hints

- `ReadFileArgs` and `WriteFileArgs` derive `JsonSchema` and `Deserialize`;
  `Tool::input_schema()` derives the wire schema directly, and the shared
  `Tool::argument_order()` derives title order from schema property order.
- `read_file` and `write_file` are private async helpers. Integration tests
  should use `ToolRegistry::create_call` and the public `ToolCall::run` /
  `ToolCallResult` traits rather than making the helpers public.
- `read_file` currently calls `tokio::fs::read`, slices raw bytes for
  `ReadMode::Bytes`, and lossy-decodes for all result modes. `ReadResult` stores
  a `String` plus path and starting-line information; metadata currently
  derives sizes from the returned string.
- `write_file` currently creates parent directories, calls
  `tokio::fs::write(&args.path, &args.content)`, and returns a `WriteResult`
  whose content section renders the input text. The binary path must pass a
  byte slice/vector instead while retaining the same filesystem setup.
- `WriteCall::non_default_redacted_arguments` and
  `WriteTool::non_default_redacted_arguments` currently duplicate the
  `REDACTED(size=...,lines=...)` policy. Both are span/UI-adjacent surfaces
  that must use the same encoding-aware redaction helper.
- `ToolCallResult` can only return an LLM-facing `String`; Base64 is therefore
  the lossless representation for binary read results. Base64 read text stays
  in `ToolCallResult::text` for the model, while binary read/write
  `ContentSection` previews are suppressed and only size metadata is shown to
  the user.
- The default registry wires `read` and `write` automatically; sandbox
  approval and path normalization are implemented outside the tool-specific
  encoding code and must remain unchanged.
- `capture_logs` and `run_locked` are in `src/test_support.rs`; the required
  rationale and locking rules are documented in `docs/known-test-issues.md`.
- `Cargo.lock` already contains Base64 0.22.1 transitively, but `Cargo.toml`
  does not declare it as a direct dependency; the manifest, not a manual lock
  edit, is the source of truth.
- `docs/architecture-design-records/` currently contains no ADR beyond
  `.gitkeep`, so there is no recorded architecture decision to reconcile.
- `make format` runs both `cargo fmt` and `rumdl fmt`; the required final
  validation commands are `make format`, `make lint`, and
  `make test-coverage`.
