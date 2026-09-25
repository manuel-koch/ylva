# Bounded-memory `read`: streaming/seek I/O and a 256 KiB output cap

An implementing agent must flip `[ ]` to `[x]` immediately after completing each
step and rewrite this file in place, so progress is resumable across sessions.

## Context

The `read` tool (`src/agent/tools/read.rs`) resolves every invocation — all
three explicit modes (`lines`, `chars`, `bytes`) and the omitted-`mode`
autodetect path — by loading the entire file with a single
`tokio::fs::read(&args.path)` and slicing the resulting `Vec<u8>` afterwards.
The `TODO` at `read_file()` (src/agent/tools/read.rs:460-462) calls this out:
reading 10 lines or 100 bytes of a multi-gigabyte file still allocates the
whole file, and `chars` mode additionally materializes a `Vec<char>` of the
entire text (up to 4× file size). Separately, a `limit`-less read can return
arbitrarily large content straight into the LLM transcript.

**Goal:** two coordinated changes to `read_file()` and its helpers:

1. **Bounded memory** — peak memory proportional to the selected output window
   (`offset`/`limit` slice, or the whole file when no range is given) instead
   of the file size: seek+range reads for `bytes` mode, chunked streaming with
   discard for `lines`/`chars`/autodetect.
2. **Bounded output** — a hard cap of **256 KiB of raw selected bytes** on
   every text-oriented result: truncate silently at the cap, report
   `truncated: true` in result metadata, and label the UI content section
   `Partial result` — the same truncation vocabulary bash and search already
   use (no in-text marker). Complete recognized media reads
   (PNG/JPEG/WebP/GIF/PDF Base64 attachments) are exempt — a truncated image
   or PDF is unusable and `media_type` is only recognized for complete files.

**Behavior otherwise unchanged:** identical results, metadata
(`read_bytes`, `read_lines`, `encoding`, `media_type`), attachments,
`starting_line`, log messages, error strings, schema (except the additive
`truncated` field), and approval handling — for everything under the cap.
UTF-8 validation semantics stay whole-file (streamed, not buffered). All
existing unit and integration tests must pass unchanged.

**Acceptance criteria:**

- `bytes` mode seeks to `offset` and reads at most `limit` bytes; unselected
  bytes are never materialized.
- `lines`/`chars` modes stream in fixed-size chunks, retain only the selected
  window, and keep validating UTF-8 to EOF (discarding chunks past the
  window); no whole-file `Vec<u8>` or `Vec<char>` is built.
- Autodetect keeps its classification semantics (valid UTF-8 → lines result;
  whole supported media file → Base64 attachment; otherwise the existing
  retry hint) with O(chunk) buffering for text results; the media sniff runs
  against the first chunk only.
- Text results are capped at 256 KiB raw selected bytes: content is cut
  cleanly at the cap with no in-text marker, truncation is reported as
  `truncated: true` in `ReadResultMetadata` (pruned from the metadata schema
  when `false`), and `content_sections()` labels the cut section
  `Partial result` like bash; the cap wins even when an explicit `limit` is
  larger; complete recognized media reads bypass the cap.
- Tool `description()` and schema docs disclose the cap so the model knows to
  use `offset`/`limit`.
- The `TODO` is removed; module docs describe the seek/stream design, the cap,
  and the media exemption.
- Tests: range-boundary characterization tests, chunk-boundary tests
  (test-only chunk-size knob), and cap/truncation/exemption tests — no
  large-file or timing-based tests.
- Docs updated in the same change set: `docs/features.md`,
  `docs/implementation-notes.md`.
- Verification gate passes: `make format lint` and `make test-coverage`
  (≥80% line coverage), plus `rumdl fmt` on touched Markdown files.

**Key constraints:**

- Module doc mandates `tokio::fs` for all file I/O (src/agent/tools/read.rs:17);
  `tokio` is built with `features = ["full"]` (Cargo.toml:15), so
  `tokio::io::{AsyncBufReadExt, AsyncReadExt, SeekExt, BufReader}` are available.
- Every read is bounded upstream by `DEFAULT_TOOL_TIMEOUT_SECS` (5s)
  (src/agent/tools/read.rs:18-25); whole-file validation scans and exempt
  media reads rely on this timeout as their backstop.
- `encoding: "base64"` requires explicit `mode: "bytes"` (cross-field
  validation before any I/O); media-type detection (`infer`) applies only when
  the selection is the complete file.
- Text paths keep strict-UTF-8 errors with the existing remedy hints;
  `bytes` + `utf8` keeps `String::from_utf8_lossy`.
- `docs/architecture-design-records/` is empty — no recorded decision
  constrains this change.

## Implementation Plan

1. [x] Add characterization tests for range boundaries in
   `src/agent/tools/read.rs` (`mod tests`), pinning *current* behavior before
   refactoring: `offset` past EOF, `limit: 0`, `offset + limit` crossing EOF,
   and `offset` past EOF with no `limit`, for `bytes` (both encodings),
   `lines`, `chars`, and autodetect. Assert content, `read_bytes`,
   `starting_line`, and error/emptiness outcomes. Verify:
   `cargo test --lib agent::tools::read`.

2. [x] Rework the `bytes` branch of `read_file()` to a seek-and-range read:
   open `tokio::fs::File`, `seek(Start(offset))`, `take(limit)`,
   `read_to_end`, so only the selected byte range is loaded; obtain the file
   length from metadata to keep the "selection == complete file" check that
   gates `infer` media-type detection (currently
   `start == 0 && slice.len() == full_bytes.len()` in `base64_read_result`).
   Preserve: lossy decode for `utf8`, `read_bytes` (Base64 = raw slice length,
   `utf8` = content length), `starting_line: Some(0)`, log lines, and the
   exact `failed to read '{path}': {error}` message. Adjust
   `base64_read_result` to take the selected slice plus completeness
   information instead of the full byte vector. Tests: step 1 tests plus
   existing `read_bytes_honors_offset_and_limit`,
   `read_base64_preserves_raw_bytes_and_range_metadata`, and
   `read_base64_recognizes_supported_complete_media_signatures` stay green.

3. [x] Implement a streaming `lines` reader used by `ReadMode::Lines`:
   `tokio::io::BufReader<tokio::fs::File>` reading fixed-size chunks (chunk
   size a `const`, overridable in tests), decoding complete lines, skipping
   `offset` lines and collecting up to `limit` lines, stripping exactly one
   trailing `'\n'` per line so results match the current
   `text.split_terminator('\n')` + `join("\n")` semantics (empty file → 0
   lines; `"\n"` → one empty line; CRLF preserved). After the window is
   collected, **continue scanning to EOF, discarding chunks**, so whole-file
   UTF-8 validation is preserved; stop early at the first invalid byte.
   Map decode errors to the existing `decode_text` message (path, mode,
   "invalid UTF-8", `retry with mode: "bytes"` remedy). Reproduce
   `starting_line` as `Some(min(offset, total_lines))` and keep the
   `size=`/`lines=` debug log. Tests: step 1 tests plus existing
   `read_lines_honors_offset_and_limit` and
   `read_text_modes_reject_invalid_utf8` stay green.

4. [x] Implement a streaming `chars` reader used by `ReadMode::Chars`:
   read fixed-size chunks, decode incrementally (`str::from_utf8` on chunk +
   carried incomplete tail; `error_len: Some` → strict UTF-8 error), count
   code points from the start of the file, retain only characters with index
   in `[offset, offset + limit)`, and keep scanning to EOF for whole-file
   validation (stop at first invalid byte) — never collect a whole-file
   `Vec<char>`. Same `starting_line: Some(0)` and log contract as step 3.
   Tests: step 1 tests plus existing `read_chars_counts_utf8_code_points`
   stay green.

5. [x] Rework `autodetect_read()` to stream: validate UTF-8 over the whole
   file in chunks while collecting the lines window (discarding unselected
   lines, O(chunk) memory), classifying as soon as the file is fully valid or
   the first invalid byte appears. On full validity return the lines result;
   on invalid UTF-8 fall back to the current classification — no
   `offset`/`limit`: sniff the media type **from the first chunk only** and,
   if supported, read the whole file for the Base64 result (whole-file
   materialization is inherent there), otherwise return the existing
   ranged/binary retry-hint error. Preserve the UTF-8-first ordering of the
   current implementation (an ASCII `%PDF…` file stays a text result).
   Tests: existing `read_omitted_mode_*` tests stay green; add a test that a
   valid UTF-8 file with `offset`/`limit` returns only the window and that a
   binary file's classification completes from the first chunk.

6. [x] Reuse the already-open file handle in the autodetect invalid-UTF-8
   fallback: for the no-range supported-media path, seek the open handle back
   to the start and read the whole file from it (the buffered first chunk
   still feeds the `infer` sniff) instead of re-opening the path or calling
   `tokio::fs::read` a second time. Output stays byte-identical; the existing
   `read_omitted_mode_*` tests must stay green.

7. [x] Add chunk-boundary correctness tests using the test-only chunk-size
   knob: a line split across chunks, a multi-byte UTF-8 sequence (`é`,
   emoji) split across chunks, invalid UTF-8 exactly at a chunk boundary,
   and a `limit` window ending mid-chunk. Fast, deterministic, no
   timing/memory/large-file assertions (per plan decision: no sparse-file
   tests).

8. [x] Implement the 256 KiB hard cap:
   - Add `pub const READ_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;` in
     `read.rs` (precedent: `SEARCH_OUTPUT_LIMIT_LINES`/`_BYTES` at
     search.rs:41/49).
   - Enforce it on the raw selected bytes (before decode/Base64 encoding) in
     every text path: `bytes`+`utf8`, `bytes`+`base64` (non-media),
     `lines`, `chars`, and autodetect text. The cap wins over a larger
     explicit `limit`.
   - On hitting the cap: stop reading and truncate cleanly at the cap — no
     in-text marker (bash and search truncate silently too); for
     `bytes`+`utf8`, back off the cut to a UTF-8 char boundary before lossy
     decoding.
   - Add `truncated: bool` to `ReadResultMetadata`, omitted when `false`
     (follow the existing `metadata()`/`metadata_schema()` pruning pattern at
     read.rs:200-223); this flag is the model-facing statement that output
     was cut (`ToolEnvelope` embeds `result_metadata`, see Implementation
     Hints).
   - Label the truncated content section `Partial result` instead of the
     usual no-label section, mirroring bash (`bash.rs:165-169`).
   - Exempt complete recognized media reads (PNG/JPEG/WebP/GIF/PDF whole-file
     Base64): they bypass the cap entirely; truncated results are by
     definition not complete files and therefore never carry `media_type`.
   - Keep the log contract (`reading file` / `read file` with `size=`/
     `lines=`) reporting the actually returned content.
   Tests: silent truncation with `truncated: true` metadata and the
   `Partial result` label for each capped path (use ~257 KiB fixtures),
   `truncated` pruned from the metadata schema and the unchanged no-label
   section for untruncated reads, cap beating an oversized explicit `limit`,
   base64 truncation, and a recognized media file just over the cap staying
   complete/attached.

9. [x] Disclose the cap in the model-facing contract: extend
   `ReadTool::description()` (read.rs:360-370) and the `ReadFileArgs`/output
   docs with the 256 KiB cap, the fact that capped results are flagged
   `truncated` in result metadata, and the hint to continue via
   `offset`/`limit`. Keep the existing schema-description assertions
   green (`read_schema_explains_autodetect_and_explicit_modes`,
   `read_input_args_all_properties_have_descriptions`).

10. [x] Remove the `TODO` at src/agent/tools/read.rs:460-462 and update the
   module-level doc comment (src/agent/tools/read.rs:15-25) to document the
   seek/stream design, the chunked validation scan, the 256 KiB cap with the
   complete-media exemption, and where whole-file buffering remains
   (Base64 whole-file/media reads). No other public API or schema changes
   beyond the additive `truncated` metadata field.

11. [x] Documentation step — update every affected doc in the same change:
    - `docs/features.md` (**read** bullet, lines ~211-235): streaming/seek
      memory behavior; the 256 KiB cap with silent truncation, the
      `truncated` metadata field and `Partial result` section label (same
      vocabulary as the bash/search bullets at features.md:315-325/344-345),
      and the complete-media exemption.
    - `docs/implementation-notes.md` (§ "Exact binary data in `read` and
      `write`"): **rewrite the stale whole-file claim** at line ~324 ("It
      reads the complete file into memory as before") so it describes
      range-only reads plus chunked streaming validation, with whole-file
      buffering limited to complete Base64/media reads — this sentence is
      the documented inconsistency this task resolves, and leaving it stale
      is a bug. Add the cap/exemption paragraph. Do **not** touch the
      "no binary-transfer size limit" sentence (~352-353) — it documents
      `write`, not `read`.
    - Inline doc comments in `src/agent/tools/read.rs` (module doc from
      step 10; `ReadMode`/`ReadFileArgs` docs if stale).
    - Run `rumdl fmt` on the touched Markdown files
      (`.rumdl.toml` configuration).

12. [x] Verification gate: run `make format lint` and the single mandatory
    test gate `make test-coverage` (≥80% line coverage enforced; do not also
    run `make test`). Fix any lint/coverage failures, then mark all
    checkboxes in this file `[x]` and rewrite it in place.

## Implementation Hints

- Single choke point: `async fn read_file(args: ReadFileArgs)` at
  src/agent/tools/read.rs:452; whole-file read at line 463; `TODO` at 460-462.
  Helpers: `decode_text` (72), `autodetect_read` (542), `lines_read_result`
  (565), `base64_read_result` (600), `supported_media_type` (629).
- Line semantics are defined by `text.split_terminator('\n')` +
  `join("\n")` (read.rs:571-577): a trailing newline does not create a final
  empty line, `"\n"` yields one empty line, `"\r\n"` lines keep the `\r`.
  A chunk/line stream must strip exactly one trailing `'\n'` to match.
- Autodetect order today: whole-file `std::str::from_utf8` first, then
  `offset.is_none() && limit.is_none() && supported_media_type(bytes)`
  (read.rs:548-552), else the retry-hint error (554-561). Keep UTF-8
  classification first — e.g. ASCII-only `%PDF…` files are currently returned
  as text, not Base64. Whole-file validity is the decision input, but a
  single invalid byte settles it — validation may exit at the first invalid
  byte (semantically identical, just faster).
- Media sniffing needs only the first chunk: `infer` 0.22 is magic-byte
  based and inspects leading bytes alone (verified in crate source: PNG needs
  4, JPEG 3, WebP ~12, GIF 6, PDF 4 bytes). An 8 KiB first chunk is ample.
  Keep it buffered for the fallback sniff after an early-invalid exit; the
  rest of the file is read only because a complete-file Base64 *output*
  needs it.
- Whole-file UTF-8 validation is a settled decision for **all** text paths
  (explicit `lines`/`chars` and autodetect): stream to EOF after collecting
  the window, discarding chunks; stop at the first invalid byte. Memory stays
  bounded; I/O is backstopped by the 5s tool timeout. Existing wording in
  `docs/features.md:220-223` and `docs/implementation-notes.md:308-312`
  ("require valid UTF-8") therefore stays accurate — no rewording needed for
  validation scope.
- `read_bytes` differs by path: Base64 → raw selected slice length;
  `utf8` paths → `content.len()` (lossy replacement chars can make this
  differ from raw byte count). Preserve per-path computation; after
  truncation `read_bytes` reflects the content actually returned.
- `starting_line`: lines → `Some(min(offset, total))`; bytes/chars →
  `Some(0)`; Base64 → `Some(0)` only when a text preview exists. It feeds
  `ContentSection.line_number_offset`; empty content produces no sections, so
  clamp precision only matters for non-empty windows.
- Media type is recognized only for a complete-file selection; with
  range-based reads this is checked against the file's metadata length, not
  against bytes held in memory. Truncated results are never complete files,
  so cap and attachment rules compose without extra checks.
- Cap mechanics: `pub const READ_OUTPUT_LIMIT_BYTES: usize = 262_144;`
  applied to raw selected bytes pre-encoding; Base64 output inflates ~4/3.
  Truncation is signaled only by `truncated: true` metadata plus the
  `Partial result` content-section label — never by an in-text marker —
  matching bash (`bash.rs:552-583`, label at `bash.rs:165-169`) and search
  (`search.rs:168`). The flag is model-visible: `serialize_tool_result`
  embeds `result_metadata` + schema in the `ToolEnvelope` JSON string sent as
  the `role:"tool"` content (`openai_compatible_messages.rs:147-164`,
  `llm.rs:240-258`), so the model can continue with `offset`/`limit`.
  `display_text()` already returns `None` for Base64, and `text_preview` is
  derived from the truncated raw slice, not from `content`.
- Metadata pattern: `ReadResultMetadata` + `metadata_schema()` prune
  optional properties when absent (read.rs:200-223) — add `truncated` the
  same way (skip-serialize when `false`, remove from schema when `false`).
- Error strings are asserted verbatim by tests: `"invalid UTF-8"`,
  `` retry with mode: "bytes" ``, `` encoding: "base64" ``,
  `` autodetect supports UTF-8 text and whole image/PDF files only ``.
- Log contract asserted by `read_file_logs_start_and_result_size`: start log
  `"reading file"`, result log `"read file"` containing `size=` and `lines=`.
- Any test invoking `read_file` must use `crate::test_support::run_locked`
  (docs/known-test-issues.md); `capture_logs` tests hold `SUBSCRIBER_LOCK`
  via the same helper. Cap fixtures (~257 KiB) are written with
  `std::fs::write` in temp dirs — no sparse/large-file tests by decision.
- Integration coverage that must stay green: `tests/tools_binary_data.rs`
  (whole-file/ranged Base64 and text reads, metadata, error names),
  `tests/tools_agent_loop.rs` (text read metadata/sections). None of the
  existing fixtures exceed 256 KiB, so the cap does not alter them.
- Test-only chunk-size knob: make the chunk size a parameter or
  `#[cfg(test)]`-overridable const so boundary tests stay fast and
  deterministic.
- Verification policy (docs/development-workflow.md): `make format lint` +
  `make test-coverage` only; Markdown lint/format via `rumdl fmt` with
  `.rumdl.toml`. Plans live in `docs/plans/<topic>.md` and must include a
  documentation step naming every `docs/*` file touched.
- Precedent for streaming reads: `search.rs` uses `BufReader` probing
  (`is_binary_file`, search.rs:375-387), output caps
  (search.rs:41/49, `truncated` flag), and `spawn_blocking` for its sync
  walk (search.rs:459); `read` should stay async on `tokio::fs` per its
  module doc instead of moving to `spawn_blocking`.
- Module path for test filters: crate `ylva`, `agent::tools::read`; run with
  `cargo test --lib agent::tools::read`.
