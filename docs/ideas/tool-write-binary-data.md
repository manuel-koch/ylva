# WriteTool — binary data

## Context

- The current `WriteFileArgs` interface (`src/agent/tools/write.rs`) only lets a
  model write string data:

  ```rust
  pub struct WriteFileArgs {
      pub path: String,
      pub content: String,
  }
  ```

- That makes it **impossible for a model to write binary data** (e.g. an
  image) to a file: LLM tool calls are transported as JSON, and JSON has no
  binary type — a `String` field can only carry UTF-8 text.
- The I/O layer is not the blocker: `tokio::fs::write` already accepts
  `&[u8]`, and `write_file` could write raw bytes if the arguments carried
  them. The constraint is purely the JSON/text boundary of the tool contract.

## Goal

Extend the `write` tool so a model can write binary files to disk while
keeping text writing unchanged (backwards compatible).

## Options

1. **Base64-encode the content** — add an optional `encoding` argument
   (default: `utf8`); the model sends the payload base64-encoded and the tool
   decodes it to bytes before writing.

   - Pros: single tool, transport-safe ASCII, works offline.
   - Cons: ~33% size overhead; large payloads cost many tokens; the model must
     produce valid base64 (decode errors possible for large binary files).

2. **Copy from an existing source path** — an additional `src` argument; the
   tool copies `src` to `path`.

   - Pros: no encoding, cheap for local data.
   - Cons: only works when the source file already exists on the machine.

3. **Fetch from URL** — an additional `url` argument; the tool downloads the
   payload.

   - Pros: no token cost for the payload.
   - Cons: requires network access, introduces untrusted input / SSRF
     considerations, only works when the file is already reachable somewhere.

## Proposal

Adopt **option 1 (base64)** as the baseline — it is the least invasive for a
JSON-based tool protocol, keeps a single `write` tool, and works offline.

- Extend `WriteFileArgs` with
  `encoding: String` defaulting to `utf8` (`utf8` | `base64`);
  when `base64`, `write_file` decodes `content` (`base64` crate) into
  `Vec<u8>` and writes the raw bytes.
- Decode failures surface as a `ToolError::failed("write", …)` naming the
  reason, never a panic.
- Keep the string/UTF-8 path for the default `utf8` encoding (unchanged
  behaviour, backwards compatible).

**Deliberately out of scope** for the baseline: option 2 (copy) and option 3
(URL fetch) stay future extensions — copy is trivial to add later, and URL
fetch needs a security review (SSRF) before it can be considered.

## Logging / redaction

- The base64 payload is still arbitrary caller-supplied data: it must **not**
  be logged verbatim. `WriteTool::non_default_redacted_arguments` keeps
  redacting `content` regardless of encoding.
- `lines` in the existing `REDACTED(size=…,lines=…)` summary is meaningless
  for encoded binary payloads. When `encoding` is `base64`, prefer summary
  with byte size only (e.g. `REDACTED(bytes=…)`), or report the *decoded*
  byte size as the meaningful size.

## Acceptance criteria / definition of done

1. A model can write a binary file (e.g. an image) via `encoding: "base64"` —
   the written bytes decode exactly to the model-supplied payload.
2. The default `utf8` path behaves exactly as today (no behavioural change for
   existing text writes).
3. Invalid base64 produces a `ToolError`, never a panic.
4. No payload bytes (encoded or decoded) ever appear in a log/span; the
   redacted argument summary remains in place.
5. Unit + integration tests cover: base64 round-trip, invalid base64,
   backwards compatibility, and no payload leakage into logs.

## Docs to update

- `docs/features.md` — write tool description: `encoding` argument.
- `docs/configuration.md` — only if `encoding` becomes configurable at the
  CLI level (not planned; it is a per-call argument, so **no change**).
- Relevant source files: `src/agent/tools/write.rs` (`WriteFileArgs`,
  `WriteTool::description`/`schema`, `write_file`, redacted arguments).
