# Session

## Export session

Add a read-only, provider-independent `ylva session export` command. It works
without initializing an engine or starting an agent. The examples below are
proposed syntax, not commands available today:

```shell
ylva session export -f md -o <PATH> <UUID>
ylva session export -f md --force -o <PATH> <UUID>
ylva session export -f md <UUID>
ylva session export -f jsonl --raw <UUID>
```

### Selection and output

- Export a selected session from the current launch directory's `project_key`.
  Support plain `.jsonl` and archived `.jsonl.gz` without restoring or changing
  either. Preserve the existing archive resolver's conflict and corruption
  checks; equivalent plain and archived copies are one session.
- Default to sanitized Markdown. It presents user and assistant messages, tool
  calls and results, and subagent events chronologically with source references.
  Identify omitted internal metadata and media. Markdown is for reading, not
  lossless archival; HTML and PDF are other possible human-readable formats.
- Also support JSONL for machine-readable exports. Format and sanitization are
  independent. `--raw` explicitly disables redaction and warns on stderr, without
  requiring the scanner. Unpaged raw JSONL reproduces the selected logical
  transcript byte-for-byte, including unknown or malformed lines, incomplete
  trailing bytes, and original line endings, whether plain or archived.
- Export the selected session alone by default. Including linked subagents is
  opt-in; each root or subagent has a separate file. A multi-file export needs
  an output directory or path template rather than mixing sessions on stdout.
  Offer a configurable template with project key, session UUID, and format
  information; for example, `exports/{project_key}/{session_id}.jsonl`.
  Detect collisions and never silently overwrite an existing destination.
- Report each session's source identity, output path, snapshot, and status.
  Supply enough metadata to build a manifest relating selected roots to child
  session IDs and files, including missing children. The manifest contains no
  transcript content. Keep diagnostics and reports off payload stdout. See
  [`self-improve-reflect`](self-improvement.md#skill-1-self-improve-reflect-feedback-flywheel)
  for its use of that manifest.

The `id` in a parent's `subagent_start` event is the subagent session UUID.
A subagent file's `parent_session` identifies its original parent, but a cloned
root can refer to the same subagent file. Preserve every parent-event edge
without duplicating the shared transcript or inferring relationships solely
from the child header. A running subagent may have no persisted transcript yet;
report it as unavailable instead of treating it as empty.

When an agent invokes the exporter through Bash, the Bash approval policy
governs the command; file-tool `read` rules do not constrain the program's
internal reads. This is not an OS isolation boundary or a new agent tool.

Capture a stable snapshot so later appends do not leak into an export.
Sanitized reflection snapshots end at a complete source event and report any
omitted partial line; a full raw export instead preserves every source byte at
its captured boundary. Inventory and export should stream large transcripts
while preserving the archive conflict checks; the current resolver reads whole
transcripts into memory. There is no aggregate export-byte limit or CLI paging
in v1, although individual events and available system resources still impose
practical limits.

### Output safety and overwrites

Standalone file exports fail if a destination exists unless the user
explicitly supplies `--force`. Reflection never uses `--force`. Even with the
flag, replace only regular files: reject a final destination that is a
symlink, including a dangling one, or another type of filesystem entry.
Symlinked parent directories are allowed, but resolve them when checking
destinations. Never write into the Ylva chat store, including through an alias,
so exports cannot replace session files or their metadata.

Preflight all known destinations and collisions before writing. Without
`--force`, a known existing destination fails the export before any output is
changed. For each file, write an owner-only temporary file and publish it
atomically rather than truncating an existing file in place. A replacement
remains owner-only where supported, even if the old file had broader
permissions.

Record each destination's state during preflight. If it appears, disappears, or
changes before publication, report a conflict rather than knowingly
overwriting the changed file. Serialize Ylva's own competing writes; an
uncooperative external writer can still race the final check and atomic rename.
Multi-file publication is not a transaction: on failure or interruption,
report which files were published and any uncertain outcomes, without claiming
an automatic rollback.

These checks cover paths the exporter opens, including `-o` and template
destinations. A shell can redirect stdout and truncate its target before Ylva
starts; the caller is responsible for that destination.

### Sanitization

Sanitized export requires a separately installed local
[TruffleHog](https://github.com/trufflesecurity/trufflehog) executable. Never
install it automatically or link it into Ylva; it is an AGPL-3.0 external
program. Disable secret verification and update checks, and do not rotate
credentials. Record the scanner version used for each export; it need not
remain fixed throughout a run.

TruffleHog discovers possible secrets but does not sanitize the transcript.
Its output can itself contain secret values, so capture it privately and do not
put raw findings in logs, prompts, reports, or model-readable export
directories. Map detected values back to the source and redact them locally.
Add deterministic redaction for common
credential assignments: a local synthetic check found that the scanner missed
some generic assignments. Detection remains **best effort**; a sanitized
export is not proof that no secret remains.

- If the scanner is missing or fails, report the error; never silently fall
  back to raw output. If a detected value cannot be mapped and redacted safely,
  withhold the affected event or batch.
- Preserve safely sanitizable unknown JSON events. Associate every sanitized
  event with its original session and 1-based source line, either in the
  output or a separate index; output line numbers alone are insufficient after
  omissions and fragmentation. Withhold malformed or otherwise unsanitizable
  content, omit binary/media payloads, and report omissions and their source
  references. Reports may include counts and detector names, never secret
  values.
- Scan and redact before splitting an oversized event. When necessary, encode
  it as multiple complete JSONL fragment objects, each identifying the source
  session, 1-based source line, field, part, and offset. All fragments remain
  in that session's single file.
- Create export files with owner-only permissions where supported. Standalone
  exports persist at the user's chosen destination; do not automatically
  delete them. Warn that even sanitized exports can contain sensitive data.

### Acceptance scenarios

- Export plain and archived transcripts without changing the source. Raw JSONL
  reproduces the logical source exactly and needs no scanner; a conflicting or
  corrupt archive reports an error.
- Exercise default Markdown, sanitized JSONL, stdout and file output, optional
  subagents, template collisions, missing children, and cloned roots sharing a
  subagent. Confirm that every output maps back to its source.
- Without `--force`, a known destination collision leaves all outputs
  unchanged. With it, replace regular files atomically with owner-only
  permissions; reject symlink and non-regular destinations and chat-store
  paths, including aliases. Verify stdout remains caller-controlled.
- Detect destinations changed after preflight, and report successful,
  failed, and uncertain files after a partial multi-file export. Do not claim
  protection against every race with external writers.
- Check scanner absence and failure, unmappable findings, generic credential
  assignments, unknown and malformed events, media omissions, and oversized
  events. Every emitted fragment is valid JSON and carries its source
  reference; diagnostics contain no detected secret values.
- Verify snapshot boundaries during appends and title updates, streaming of
  large sessions, restrictive permissions where supported, and the absence of
  automatic installation, verification, or update checks.

## Whole-file schema migrations

**Deferred**: Ylva does not automatically migrate whole-file schemas.
[Schema tolerance](../persistence.md#schema-tolerance) describes the current
format and what happens to incompatible older events. Keep additive changes on
the existing per-event evolution path.

If preserving session semantics across a past or future whole-file break
requires migration, add a chain of small, idempotent upgraders. Acquire the
writer lock before migrating; write a complete temporary file and atomically
replace the original only after conversion succeeds. A failed or interrupted
migration must leave the original readable, and read-only discovery must never
rewrite sessions. Test repeated upgrades, interrupted writes, unsupported
versions, and preservation of messages and root/subagent linkage.
