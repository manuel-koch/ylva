# Session

## Export session

Add a read-only, provider-independent `ylva session export` command. It works
without initializing an engine or starting an agent. The examples below are
proposed syntax, not commands available today:

```shell
ylva session export -f md -o <PATH> <UUID>
ylva session export -f md --no-subagents --force -o <FILE> <UUID>
ylva session export -f md --no-subagents <UUID>
ylva session export -f jsonl --raw --no-subagents <UUID>
```

### Selection and output

- Export a selected session from the current launch directory's `project_key`.
  Support plain `.jsonl` and archived `.jsonl.gz` without restoring or changing
  either. Preserve the existing archive resolver's conflict and corruption
  checks; equivalent plain and archived copies are one session.
- Default to sanitized Markdown. Present messages, tool calls with their
  results, and subagent events without added source line numbers or call/run
  UUIDs. Render tool arguments as readable field lists, with shell commands
  in fenced blocks. Use the session title as the heading when present;
  otherwise use `Session`. Use short labels where needed for correlation, and
  show asynchronous subagent completion separately. Link bundled parent and
  subagent transcripts in both directions. Grouping may change the apparent
  order of interleaved events. Identify omitted media and withheld or omitted
  messages; routine hidden metadata needs no notice. Markdown is for reading,
  not lossless archival; HTML and PDF are other possible human-readable formats.
- Also support JSONL for machine-readable exports. Format and sanitization are
  independent. `--raw` explicitly disables redaction and warns on stderr, without
  requiring the scanner. Unpaged raw JSONL reproduces the selected logical
  transcript byte-for-byte, including unknown or malformed lines, incomplete
  trailing bytes, and original line endings, whether plain or archived.
- Include linked subagents recursively by default in both formats.
  `--no-subagents` exports only explicitly selected sessions while retaining
  the parent's recorded subagent calls and results. Directory output gives each
  root a portable bundle: `session.<format>` and indexed
  `subagent-<number>.<format>` files. Shared transcripts are copied into each
  relevant root bundle.
- One included session can be written to stdout or a file. Multiple selected
  or included IDs, including unavailable children, require an output directory;
  multiple roots get numbered bundle directories. A template containing
  project key, session UUID, or format tokens names each root's bundle
  directory, for example `exports/{project_key}/{session_id}`. Detect
  collisions and never silently overwrite an existing destination.
- Report each session's source identity, output path, snapshot, and status.
  Supply enough metadata to build a manifest relating selected roots to child
  session IDs and files, including missing children. The manifest contains no
  transcript content. Keep diagnostics and reports off payload stdout. See
  [`self-improve-reflect`](self-improvement.md#skill-1-self-improve-reflect-feedback-flywheel)
  for its use of that manifest.

For a proposed human-readable layout, see the
[Markdown output example](../plans/export-session-plan.md#markdown-output-example).

The `id` in a parent's `subagent_start` event is the subagent session UUID.
A recognized admission can identify a queued run before its start event. A
subagent file's `parent_session` identifies its original parent, but a cloned
root can refer to the same subagent file. Preserve every parent-event edge
without duplicating a transcript within one bundle or inferring relationships
solely from the child header. Capture one snapshot per unique source even when
it is copied to multiple bundles. A running or queued subagent may have no
persisted transcript yet; report it as unavailable instead of treating it as
empty.

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

Every generated bundle directory, even one containing a single transcript, must
be new or empty; `--force` cannot replace a nonempty bundle. Never delete
existing contents to make room. It can still replace an unchanged standalone
file or manifest outside a bundle.

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
an automatic rollback. Links in a partially published bundle may be broken.

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
  JSONL event with its original session and 1-based source line. Emit one
  complete JSON object per complete source line or valid final unterminated
  object, without fragmenting oversized records. Withhold malformed or
  otherwise unsanitizable content, omit binary/media payloads, and report
  omissions and their source references. Reports may include counts and
  detector names, never secret values.
- Omit known Base64 `write.content` in both sanitized formats, even when it
  encodes text: encoded secrets may escape scanning. Replace the field with an
  omission notice and report it. `--raw` retains the encoded content.
- Create export files with owner-only permissions where supported. Standalone
  exports persist at the user's chosen destination; do not automatically
  delete them. Warn that even sanitized exports can contain sensitive data.

### Acceptance scenarios

- Export plain and archived transcripts without changing the source. Raw JSONL
  reproduces the logical source exactly and needs no scanner; a conflicting or
  corrupt archive reports an error.
- Exercise default Markdown, sanitized JSONL, stdout and file output, optional
  subagent exclusion, bidirectional links, template collisions, queued or
  missing children, and cloned roots sharing a subagent. Confirm that every
  bundle output maps back to its source.
- Without `--force`, a known destination collision leaves all outputs
  unchanged. With it, replace standalone regular files atomically with
  owner-only permissions; reject nonempty bundles, symlink and non-regular
  destinations, and chat-store paths, including aliases. Verify stdout
  remains caller-controlled.
- Detect destinations changed after preflight, and report successful,
  failed, and uncertain files after a partial multi-file export. Do not claim
  protection against every race with external writers.
- Check scanner absence and failure, unmappable findings, generic credential
  assignments, unknown and malformed events, media and encoded-content
  omissions, and oversized events. Each sanitized JSONL record is a complete
  JSON object carrying its source reference; diagnostics contain no detected
  secret values.
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
