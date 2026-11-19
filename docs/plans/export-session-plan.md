# Session export implementation plan

## Context

Add a read-only, provider-independent `ylva session export` command for Ylva's
persisted conversations. It must run before profile, engine, agent, or prompt
initialization and select sessions only from the launch directory's existing
`project_key`. It must not restore, repair, rewrite, or otherwise change the
source store.

The command accepts one or more explicit session UUIDs, including subagent
UUIDs. Each distinct requested session is a root. By default, recursively
include its recorded subagents; `--no-subagents` exports only the explicit
roots. In either mode retain each parent's recorded subagent calls and eventual
results. Record observed parent-event edges even when traversal is disabled;
unrequested children do not count as included outputs or failures.

Each root gets a self-contained bundle when multiple files are needed. Include
each available transcript once per bundle, but copy a shared subagent into
every root bundle that references it, including clone bundles. Pin each unique
source snapshot once per invocation and reuse it across those copies. Preserve
every parent-event occurrence and its source line; report unavailable
subagents rather than inventing empty files. Bulk `--all` and `--before`
selectors are outside this change.

Default output is sanitized Markdown; `-f jsonl` selects machine-readable
JSONL. Format and sanitization are independent: `--raw` disables scanning and
redaction for either format and always warns on stderr. In particular,
unpaged raw JSONL must reproduce the selected logical transcript bytes exactly,
including unknown or malformed lines, original line endings, and incomplete
trailing bytes. Plain and gzip representations have the same behavior; retain
all existing archive conflict, identity, sidecar, and corruption checks.

Sanitized output is best effort, not proof that secrets are absent. It requires
a separately installed local *TruffleHog* executable, with verification and
update checks disabled; Ylva must neither install nor link it. Redact findings
locally, supplement them with deterministic credential-assignment redaction,
and withhold content that cannot be handled safely. Scanner findings and
diagnostics must never expose detected secret values. Warn that sanitized
exports may still contain sensitive data. Media and known Base64
`write.content` are omitted from sanitized output, including encoded text;
Markdown identifies these omissions and withheld or omitted messages, but
does not announce routinely hidden metadata. Raw exports retain encoded write
content.

Markdown is a readable view: group each tool call's arguments and result, omit
added source line numbers and call/run UUIDs, and use short labels when needed
for correlation. Do not remove identifiers from ordinary conversation or tool
content merely because they resemble IDs; known subagent lifecycle fields are
internal metadata. Grouping can change the apparent order of interleaved
events; show asynchronous subagent completion separately. Link bundled parent
and subagent Markdown files to each other using relative, indexed filenames.
Sanitized JSONL and manifest edge/omission records retain source references.
Use the session title as a safely rendered heading when present; otherwise use
`Session`. Scan the title in sanitized exports. If an unsafe title is withheld,
report it and apply the existing partial-failure policy.

Capture a stable snapshot independently for each source, including active
sessions, without claiming a globally simultaneous snapshot. Later appends and
title replacement must not leak into a captured snapshot. Include a complete,
valid final JSON object even without its newline; omit and report an
unparseable partial tail. There is no aggregate export-size limit, CLI paging,
or fragment-record format. A single event may still be limited by available
system resources.

### CLI and output contract

```shell
ylva session export --no-subagents <UUID>
ylva session export --no-subagents -f jsonl --raw -o transcript.jsonl <UUID>
ylva session export -f jsonl -o 'exports/{session_id}' <UUID>
ylva session export --manifest manifest.json \
  -o 'exports/{project_key}/{session_id}' <UUID> <UUID>
```

`-f`/`--format` accepts `md` and `jsonl`, defaulting to `md`. `-o`/`--output`
is interpreted after inventory. Without placeholders it names a file for one
unique selected or included ID, or a directory for multiple IDs. Count
unavailable included IDs when choosing the mode; do not count unrequested
children when `--no-subagents` is set. Without `-o`, stdout is permitted only
for one ID.

For one root with multiple IDs, the output directory is that root's bundle.
For multiple distinct roots, create `session-1/`, `session-2/`, and so on
under the output directory, in first-requested order. Reserve an unavailable
root's number without creating its hypothetical directory. A path containing
`{project_key}`, `{session_id}`, or `{format}` is instead a template for each
root's bundle directory, even if it contains only one transcript; tokens use
the root's identity. Reject malformed templates and destination collisions,
including aliased or nested bundle directories, before creating paths.

Within each bundle, name the root `session.<format>` and each non-root
transcript `subagent-<number>.<format>`. Assign bundle-local numbers by a
breadth-first traversal: inspect each parent's recorded edges in source order
and number each distinct referenced child on first encounter, including an
unavailable child. A cycle back to the root reuses `session.<format>`; no
source is copied twice within one bundle. With `--no-subagents`, number only
root-side child references in source order for display; those unrequested
children do not create output paths or affect stdout selection. All files for a
bundle are siblings.

`--manifest PATH` optionally writes a versioned, metadata-only JSON manifest.
It covers the selected roots, every parent-event edge, each source identity and
snapshot, and every root-specific output path or stdout, scanner version,
omission and publication status, and unavailable child. Include source and
per-output SHA-256 digests so future consumers can identify the evidence
without copying transcript text. Diagnostics and per-output reports go to
stderr, never payload stdout.

Every exporter-opened destination, including the manifest, is subject to the
same checks: preflight the complete destination set before writing; reject
existing files without `--force`, final symlinks and non-regular entries even
with it, and all paths inside the Ylva chat store, including aliases. Resolve
allowed symlinked parent directories. Each actual bundle directory, even a
single-transcript templated bundle, must be new or empty; `--force` cannot
replace a nonempty bundle. It may still replace an unchanged standalone file
or manifest outside a bundle. Never delete existing bundle contents.

Preflight only files that will actually be written, plus the requested
manifest; an unavailable session has no output destination, and an unavailable
root has no bundle directory. The general empty-directory check still rejects
any entry in an actual bundle, including a hypothetical unavailable child's
filename. After preflight, create missing directories needed for outputs with
owner-only permissions where supported; do not change existing directory
permissions. Report created directories and leave them in place after failure.
Recheck resolved paths and bundle emptiness before the first staged file.
Publish owner-only temporary files atomically, recheck the preflight state, and
serialize Ylva's competing writers. Do not claim protection against every race
with external writers. Multi-file publication is not a transaction: report
published, failed, unattempted, and uncertain outcomes without claiming
rollback. Links can be broken after partial failure. A shell may truncate a
redirected stdout target before Ylva starts; the caller owns that risk.

Unavailable or corrupt explicitly selected or included sessions, and unsafe
or malformed complete events in sanitized exports, result in exit code `1`;
export other available sessions and never create empty substitutes. An
unrenderable complete event in raw Markdown is also an error. For raw JSONL,
successfully copying malformed complete lines is a warning, not a failure;
when recursive inclusion is enabled, malformed lines make discovery incomplete
and result in exit code `1` without altering the copied bytes. With
`--no-subagents`, those copied lines remain warnings.
Invalid arguments, a missing scanner at startup, and known preflight conflicts
abort before writing with exit code `2`. Intentional media and encoded-content
omissions and an incomplete trailing line are reported warnings, not failures.

Scanner failure is an error, never a fallback to raw output. Scan and safely
map each event or batch before publishing its bytes; on scanner execution
failure, discard the affected session's staged output, continue with the other
available outputs, and exit `1`. Stdout remains streaming and may already
contain a partial payload when reading or scanning fails; report this on
stderr. After preflight, make a best-effort attempt to publish a requested
manifest on execution failure or cooperative interruption. Stderr is
authoritative if manifest publication fails or is uncertain. Cooperatively
handled SIGINT and SIGTERM take precedence and use exit code `130`; SIGKILL
and power loss cannot produce a final report.

The future reflection consumer uses fresh run-owned paths, never `--force`.
When an export changes or is missing, it takes a new current snapshot and
invalidates and re-mines affected evidence; this command does not re-export a
past snapshot. Session-list/inventory titles need no special sanitization,
though they can be sensitive; titles inside sanitized transcripts are scanned
like other fields. Implementing reflection, sanitized inventory, whole-file
schema migration, HTML, and PDF is out of scope. Standalone exports persist at
the chosen destinations; only reflection manages its own run cleanup.

### Markdown output example

The [annotated pseudotemplate](export-session-markdown-template.md) describes
renderer behavior, not a configurable runtime template. The
[parent](export-session-markdown-example.md.txt) and
[subagent](export-session-markdown-subagent-example.md.txt) Markdown files are
the exact expected output for the [source fixture](export-session-markdown-fixture.json).
They are stored as Markdown text to preserve repeated role headings without
changing the lint configuration.

#### Rendering rules

- Start with `# <heading>`, where `heading` is the sanitized `meta.title`, or
  the literal `Session` when the title is empty, missing, or withheld. Escape
  Markdown punctuation and replace line breaks with spaces so the title
  remains one heading.
- Immediately below the heading, show bundled parents when present:
  `Parent: <reference>` for one, or `Parents: <references>` for several.
  Label a bundled root `Session` and another bundled parent
  `Subagent #<number>`. In each parent's file, link to its first rendered
  start heading for this child, for example
  `[Session](session.md#subagent-1-started)`. If no start heading is rendered,
  link to the parent file without a fragment. List unique direct parents, root
  first and then in bundle-number order, separated by commas. Derive these
  relationships from parent events, not only the child's header. Do not link
  to unrequested or unavailable files, or across bundles.
- Derive each start fragment from the target parent's actual rendered heading
  sequence using GitHub-style heading anchors, including suffixes for duplicate
  headings. Account for all earlier rendered headings, including those from
  conversation Markdown; for example, an earlier `Subagent #1 started` heading
  makes the next such heading's fragment `subagent-1-started-1`. Resolve
  fragments from the same sanitized or raw content used for the final output,
  before finalizing bundle backlinks. Preserve streaming and use owner-only
  staging if needed; cyclic parent relationships must not depend on publication
  order. Markdown viewers with different heading-anchor rules may not follow
  these fragments.
- Process displayable events in source order. Introduce each explicit message
  with `## <role>`, where `role` is the literal `User`, `Assistant`, or `System`
  selected from the persisted role, followed by its sanitized content rendered
  as Markdown. Append `> Assistant response interrupted.` if an assistant is
  interrupted. An implicit harness user message is omitted and replaced by
  `> Internal harness notice omitted.`.
- Render each assistant tool call after its message under
  `### Tool: <tool_name><call_label>`, followed by the literal labels
  `**Arguments:**` and `**Result:**`. Render `tool_name` as a sanitized inline
  code span. `call_label` is empty unless one assistant event has multiple
  calls with the same name; in that case append a space and `#1`, `#2`, and
  so on in call order. Match results or errors to calls by their stored IDs
  without printing those IDs; do not render a matched result again at its
  original position.
- Render the call's arguments after sanitization, or the original arguments
  with `--raw`. For an object, render every field in recorded order as a
  Markdown list item `- **<key>:** <value>`. Render nonempty nested objects as
  nested field lists and nonempty arrays as ordered lists numbered from `1`,
  retaining array order. A non-object root uses the same value rules without
  inventing a field name. Separate nested blocks from their labels with a
  blank line; for a container inside an array, put its nested list immediately
  after the array marker on the next line. Indent every continuation line under
  its list item. Choose exactly one variant per field: an encoded-content
  omission takes precedence, followed by null/empty values, nonempty classified
  strings, nested containers, and other scalars.
- Render numbers and booleans as text. Render null, the empty string, the
  empty array, and the empty object as the literal inline-code markers
  `` `null` ``, `` `""` ``, `` `[]` ``, and `` `{}` ``, respectively,
  including inside arrays or at the root. Escape field names as needed to
  render them literally; represent an empty key as `""` and control characters
  with JSON-style escapes. Escape literal backslashes so they remain
  distinguishable from those control-character escapes.
- Classify nonempty string fields by tool and top-level field, without guessing
  from their contents. Render `bash.command` in a `bash` fence; render
  `write.content` and `edit.old_string`/`new_string` in `text` fences. Render
  `spawn_subagent.task`/`briefing`, `clarify.question`/`context`, and
  `agent_finish.result`/`error` as authored Markdown in an indented block after
  their field label. Render all other strings, including unknown tools and
  nested values, as escaped literal text; encode each logical line break as a
  backslash followed by a newline for a Markdown hard break. Normalize CRLF
  and CR to line breaks in Markdown, while raw JSONL remains byte-exact. Do
  not invoke tools or discard unknown fields to classify arguments.
- In sanitized output, replace each string-valued `write.content` whose
  `encoding` is `base64` with the literal `[Encoded content omitted.]`, even
  when it encodes text. Determine the field from the source before redaction,
  retain its omission location through rendering, and record it in the source
  event's omissions and output reports. Render the marker as a field value,
  escaping its brackets in Markdown; never infer an omission merely from
  matching text. With `--raw`, retain the encoded string and apply the normal
  fenced-content or empty-string rule.
- Render an ordinary sanitized tool result in a `text` fence; for an error,
  emit `> Tool error:` followed by the fenced error text. Emit
  `> Result not recorded.` if the snapshot contains no matching result. For a
  media attachment, emit `> Media omitted: <media type>.` using its sanitized
  media type instead of its encoded bytes. Do not remove UUID-like text from
  ordinary tool content.
- A `spawn_subagent` admission result with a valid `subagent_accepted` event,
  run ID, and `running` or `queued` status is rendered as
  ``Subagent #<number> accepted (`<status>`).`` rather than printing lifecycle
  IDs. If that result is not recognizable, render its sanitized text normally
  instead of inventing a status. An unmatched tool result is shown separately
  under `## Tool result: <tool name>` using the same result rules.
- Use the bundle-local number in every `Subagent #<number>` label. If that
  transcript has a planned output in the same bundle, render the label in
  admissions, starts, and completions as a relative Markdown link to its
  `subagent-<number>.md` file; a cycle to the root links to `session.md`.
  Otherwise render the label as plain text. Link targets contain no session
  UUIDs. A target that fails later may leave a broken link; report the partial
  publication instead of claiming the bundle is complete.
- Show a start as `## <subagent reference> started` followed by `Task: <task>`.
  Show completion separately as `## <subagent reference> completed`, followed
  by `- Status: <status>` and, when present, `- Result: <result>`. The task,
  status, and optional result come from the corresponding sanitized subagent
  events; render status as an inline code span. Keep the recorded result or
  error even when `--no-subagents` omits the child transcript. Preserve
  multiline values, indenting continuation lines inside list items.
- Show a safely displayable unknown event under `## Other event` in a `json`
  fence, serialized as compact JSON. A withheld unsafe or malformed complete
  event gets the fixed notice `> Content withheld.`; an unparseable partial
  tail gets `> Incomplete trailing content omitted.`. Reports and exit codes
  retain the distinctions described above.
- For inline code, use a backtick delimiter longer than any run in the value
  and pad it as needed for CommonMark. For every fenced block, use at least
  three backticks and one more than the longest backtick run in the body.
  Preserve message and argument Markdown and fenced content, adding a
  structural newline before a closing fence only when needed. Escape Markdown
  punctuation and line breaks in generated single-line headings and media
  notices.

#### Matching source fixture

The fictional fixture contains a parent that reads configuration and media,
checks a port with Bash, then asks a subagent to verify it. Its `session` array
and each array in `subagents` represent separate source transcripts. Serialize
each object as a source JSONL line in `<project_key>/<session_id>.jsonl`. The
fixture has synthetic, already-redacted values and a dummy media payload to
exercise omission; `[REDACTED]` is illustrative, not proof of scanner behavior
or redaction provenance. Executable tests must substitute the launch
directory's computed `project_key` in both headers and the directory name; the
expected Markdown does not change.

The expected parent is `session.md` and the child is `subagent-1.md`; the
sidecars contain their exact bytes. Labels and links correlate calls and
subagents without displaying IDs. Grouping need not reproduce exact event
order. Markdown is not a machine-readable provenance contract: source content
can itself resemble generated headings or notices. The example is not a
guarantee that sanitization found every secret.

### Machine-readable records

For sanitized JSONL, emit one complete JSON object per source line, without a
maximum record size or fragments. An emitted event has this shape:

```json
{"export_version":1,"source":{"session_id":"UUID","line":42},"event":{"type":"message"},"omissions":[]}
```

`source.line` is the original 1-based source line, not the output line.
Preserve safely sanitizable unknown JSON objects inside `event`; use
`event: null` plus a non-secret omission reason when the source line must be
withheld. Record media and encoded-write-content omissions in the corresponding
event's `omissions`, including the affected field's location.
Ordinary line-oriented reads of an oversized record can truncate it; consumers
must read character ranges or Base64 byte ranges, continue to EOF, and
reassemble the record before parsing. A deliberately limited page need not
set the read tool's `truncated` flag.

The manifest's version 1 object contains `version`, `source_home`, `project_key`,
`selected_ids`, `sessions`, `bundles`, `outputs`, `edges`, and overall
`status`. A session record identifies one source, its representation, source
paths, availability, snapshot boundary, and source digests. A bundle record
identifies its requested root, directory, standalone file or stdout, and status.
An output record relates a root and source session to its bundle-local number
(`null` for the root), destination or stdout, publication status, output
digest, scanner versions, and omission references/counts. Record unavailable
included members with no destination. This represents multiple outputs from
one source snapshot without conflating their potentially different Markdown
links or hashes.

Each edge contains the parent UUID, child UUID, source line, and event kind,
including recognized admissions. Use `null` for unavailable information;
never include transcript text, titles, tasks, findings, or secret values in
the manifest. Record enough snapshot metadata to compare an immutable header
separately from the complete-event prefix after a title change or plain/archive
conversion. The manifest describes transcript outcomes; its own publication
result is reported on stderr rather than recursively recorded in itself.

Use lowercase hexadecimal SHA-256 values. Record the captured logical byte
length and digest, plus a digest and byte length for complete source lines
after the header, including malformed complete lines but excluding an
unparseable partial tail. Include a valid unterminated final event in that
prefix. For the header digest, parse its JSON object, remove only `title`,
recursively sort object keys, serialize compactly as UTF-8, and hash the
resulting bytes. This preserves unknown fields while allowing a title-only
rewrite; the event-prefix length is relative to the end of the header so the
header's changed length does not shift the comparison. If the header cannot be
parsed, record the unavailable header digest as `null`; do not add a stronger
header requirement to raw plain-only exports than the existing archive
resolver imposes.

## Implementation Plan

Complete the steps in order. Update affected documentation alongside each
behavioral step, and mark its checkbox `[x]` in this file immediately after
the step and its tests and documentation are complete. Write focused failing
tests first where practical; run the relevant owner-local tests and `make lint`
before marking each code-bearing step done. Do not stage, commit, or merge
branches without explicit user consent.

1. [ ] Establish the baseline and implementation boundary. Start with
   `src/session_command.rs`, `src/cli.rs`, and `src/store/archive.rs`; run
   `cargo test store::tests::archive` and `cargo test --test session_commands`.
   Review the recorded decisions and test-isolation rules in the references
   below before changing code.
2. [ ] Add a streaming, read-only snapshot/representation reader through the
   store facade. Pin opened source handles and their captured boundaries; do not
   take the active writer's exclusive identity lock or create store locks.
   Validate gzip to EOF, sidecars and identities, and compare simultaneous
   plain/archive copies incrementally before using them. Preserve the existing
   resolver's behavior for other readers. Test raw and archived snapshots,
   equivalent and divergent copies, corrupt archives and sidecars, appends,
   title replacement, and generated large transcripts without whole-file
   buffering; run `cargo test store::tests::archive`.
3. [ ] Build a streaming inventory of explicit roots and, by default, their
   linked subagents. Follow parent-side `subagent_start`, `subagent_result`,
   and recognized `spawn_subagent` admission records, retaining each source
   line and kind; never infer clone relationships solely from child headers.
   Recognize an admission in a `spawn_subagent` tool result when its JSON
   has event `subagent_accepted`, a valid run UUID, and `running` or `queued`
   status; if `tool_call_id` is present, it must match the result's call ID.
   An accepted queued run can lack a start record or transcript; include its
   ID and report it unavailable. With `--no-subagents`, record root-side
   edges but do not traverse or fail for their unrequested children.

   Pin each unique source once, guard cycles, and build the deterministic
   breadth-first numbering and membership separately for each root bundle.
   Distinguish unrequested, unavailable, and corrupt sources; report when
   invalid source data prevents complete discovery. Capture per-source
   boundaries and digests, including a title-independent header identity and
   complete-event prefix. Test shared children across clone bundles, repeated
   edges, nested/cyclic records, queued admissions without starts, missing
   running subagents, explicitly selected missing/corrupt sessions, changed
   titles, later appends, and equivalent archives.
4. [ ] Implement local sanitization using TruffleHog's JSON stdin interface and
   explicit no-verification, no-update, no-ignore-tag, and scan-error flags.
   Record every scanner version used per session; capture findings and scanner
   stderr privately, never in logs, reports, or model-readable directories.
   Treat the entire finding as sensitive and map every detected secret value,
   not only `Raw`; redact safely mapped values and common credential assignments
   in JSON keys and values. Withhold an event or batch if mapping is unsafe or
   redacting keys would cause a collision. Omit known Base64 `write.content`
   and media payloads as specified above. Handle safely sanitizable unknown
   JSON objects without deserializing them through the closed `StoreEvent`
   enum. Test with an injected fake scanner, including missing/failing
   scanners, multiple finding values, encoded/unmappable values, generic
   assignments, Base64 write content encoding text and binary, raw bypass,
   media, continuing other sessions after a scan failure, and secret-free
   diagnostics and logs.
5. [ ] Implement streaming renderers for Markdown, sanitized JSONL, and raw
   JSONL. Markdown presents user/assistant messages, tool calls/results,
   subagent events, and safely representable unknown events as specified
   above, grouping calls with results while keeping asynchronous completion
   separate. Render bidirectional bundle-local links using the inventory's
   numbering and relationships; target the first rendered parent start heading
   where available and otherwise the parent file. Resolve fragments across
   each parent's complete rendered heading sequence before finalizing links,
   including for cycles and duplicate headings. Retain parent-side results
   without child files when traversal is disabled. Sanitized JSONL follows the
   record contract above, including oversized single records; raw JSONL copies
   captured logical bytes without filtering or normalization, independently of
   inventory parsing. Test both formats with and without `--raw`.

   Use `docs/plans/export-session-markdown-fixture.json` and the expected
   parent and child in `docs/plans/export-session-markdown-example.md.txt` and
   `docs/plans/export-session-markdown-subagent-example.md.txt` as a rendering
   test. Cover clone-specific backlinks, shared and unavailable children,
   multiple parents and cycles, repeated starts, missing or withheld starts,
   and headings in source Markdown that collide with generated start headings.
   Also cover grouped overlapping calls, errors and missing results,
   asynchronous completion, Markdown omissions and hidden lifecycle
   identifiers, UUIDs preserved in ordinary content, JSONL source-line
   references after omissions, field-aware arguments, multiline Markdown and
   literal strings, nested objects and arrays, unknown tools and fields,
   non-object roots, null and empty values, encoded-content omissions, and
   source text matching the omission marker. Cover present, missing, and
   unsafe titles, headings with Markdown syntax or newlines, embedded
   code-fence delimiters, a valid unterminated object, malformed lines, media,
   large events, CRLF, and incomplete trailing bytes. Verify that raw malformed
   lines cause failure when recursive discovery is incomplete and remain
   warnings with `--no-subagents`.
6. [ ] Resolve the `-o` and manifest destinations, expand the supported
   root-bundle template tokens, and preflight all known outputs and collisions
   before changing any output. Resolve existing ancestors, create missing
   directories privately after preflight, and recheck and pin the resolved
   parents. Require every actual bundle directory to be new or empty, even
   with `--force`. Reject chat-store aliases, final symlinks including dangling
   ones, and non-regular file destinations; preserve each destination's
   initial identity and state. Test single-file versus bundled output,
   numbered roots, unavailable roots with untouched hypothetical paths,
   nonempty bundles including hypothetical child filenames, template
   collisions and overlapping bundle directories, newly created and symlinked
   parents, chat-store aliases, and stdout selection. Verify created
   directories remain and are reported after a later failure.
7. [ ] Publish each file through an owner-only adjacent temporary file and an
   atomic operation, with cooperative Ylva-writer locking and a final comparison
   against its preflight state. Use an atomic no-clobber publication for an
   initially absent destination; `--force` may replace only an unchanged
   standalone regular file or manifest, never a nonempty bundle. Pin the
   resolved parent while publishing. Test permissions, appearing/disappearing/
   replaced destinations, concurrent Ylva writers, partial multi-file
   failure with potentially broken links, cleanup, and cooperative
   SIGINT/SIGTERM. Report uncertain outcomes and document the residual
   external-writer race.
8. [ ] Implement the versioned manifest and stderr reporting from the same
   inventory and publication results, without transcript content. Compute
   source and per-output hashes while streaming; associate each duplicate
   output with its root bundle and shared source snapshot. Represent stdout,
   unavailable, omitted, failed, unattempted, and uncertain outcomes
   explicitly. Apply destination safety to the manifest itself. Test
   metadata-only reports, clone bundles with shared sources, snapshot
   verification data across title/archive changes, scanner-version changes,
   missing children, partial publication, and attempted manifest publication
   after failure or cooperative interruption.
9. [ ] Wire the command into the existing early `session` dispatcher and clap
   model. Map argument/preflight errors, partial execution errors, and
   cooperative interruption to the documented exit codes. Add executable tests
   in `tests/session_commands.rs` for provider-independent startup, open stdin,
   stdout/stderr separation, default recursive inclusion and `--no-subagents`
   in both formats, all output modes, scanner absence, streaming stdout
   failures, `--force`, and unchanged source files. Use isolated temporary
   homes and a fake scanner on a controlled `PATH`; keep the tests offline.
   Run `cargo test --test session_commands` and the new owner-local export
   tests.
10. [ ] Update `docs/cli-options.md`, `docs/features.md`, `docs/persistence.md`,
    `docs/ideas/session.md`, and `README.md` for the actual CLI, output schema,
    snapshot, safety, and limitations. Keep
    `docs/plans/export-session-markdown-template.md` and
    `docs/plans/export-session-markdown-fixture.json`,
    `docs/plans/export-session-markdown-example.md.txt`, and
    `docs/plans/export-session-markdown-subagent-example.md.txt` aligned with
    the accepted output contract and rendering test. Reconcile
    `docs/ideas/self-improvement.md` with the approved unfragmented, fresh-path,
    title-inventory decisions only after its newer branch version has been
    integrated; do not overwrite that newer design. If integration is still
    pending, report the dependency rather than marking this step complete.
    Update `docs/implementation-notes.md` only for non-obvious cross-module
    rationale, following its opening guideline. Run `rumdl check` on changed
    Markdown.
11. [ ] Run `make format lint` and `make test-coverage`; the latter is the
    single mandatory full-suite gate and must retain at least 80% aggregate
    line coverage. Run `make audit` only if `Cargo.toml` or `Cargo.lock`
    changes. Report any failed or unavailable checks.

## Consistency Issues

- [low] The newer reflection idea still describes fragment records and requires
  sanitized title inventory and historical-snapshot regeneration. Those
  descriptions conflict with the approved decisions above and must be
  reconciled when that documentation is integrated.

## Implementation Hints

- `src/main.rs` dispatches `CliCommand::Session` before starting the Tokio
  runtime or loading providers; `src/session_command.rs` resolves
  `default_chats_root()` and `project_key()` for the canonical launch
  directory. ADR-0003 keeps session commands independent of workspace trust;
  ADR-0004 shares the equivalent project key across linked worktrees.
- `src/store/archive.rs::read_bytes` currently buffers complete logical
  transcripts. `Store::read_events` in `src/store/replay.rs` also materializes
  events and drops unknown or malformed lines, so it cannot implement raw
  export or lossless source-line mapping. Existing archive regression tests
  are in `src/store/tests/archive.rs`. The resolver does not require a valid
  header for a plain-only transcript without a sidecar.
- `src/store/writer.rs` appends event bytes and the newline separately, and
  replaces the header inode for title updates while holding a separate
  identity lock. `src/store/subagent.rs` writes completed subagent transcripts
  under the run UUID named by the parent's events; clone-shared transcripts
  retain their original `parent_session`.
- `src/agent/tool_queue/spawn.rs` records the immediate `subagent_accepted`
  tool result. `src/agent/subagent_manager.rs` records `subagent_start` only
  when a queued run actually starts, so an accepted queued run can have no
  start event or child transcript at the parent's snapshot boundary.
- `src/agent/message.rs` stores tool calls inside assistant messages and media
  descriptors alongside a Base64 tool-result string. The provider request
  dump redactor in `src/llm/openai_compatible_provider.rs` is media-specific;
  it is not a general transcript secret scanner.
- `src/agent/tools/bash/result.rs` identifies Bash commands as shell content;
  `src/agent/tools/spawn_subagent.rs`, `clarify.rs`, and `agent_finish.rs`
  identify the specified prose fields as Markdown. `src/agent/tools/write.rs`
  accepts Base64 file content, including non-media bytes and encoded text.
- `Cargo.toml` already includes `pulldown-cmark`. GitHub documents its
  [heading-anchor rules](https://docs.github.com/en/get-started/writing-on-github/getting-started-with-writing-and-formatting-on-github/basic-writing-and-formatting-syntax#section-links),
  including duplicate-heading suffixes.
- `src/private_fs.rs::replace_file` buffers supplied contents and unconditionally
  renames over the target. Its private-file and directory helpers are useful
  prior art, but that replacement function alone does not provide streaming,
  no-clobber publication, or destination-state revalidation.
- The installed TruffleHog 3.99.7 supports `stdin`, `--json`,
  `--no-verification`, `--no-update`, `--no-ignore-tag`, and
  `--fail-on-scan-errors`. Its JSON findings can contain sensitive `Raw`,
  `RawV2`, `SecretParts`, `ExtraData`, and other fields. This observed version
  is not a required pin.
- `serde_json` enables `preserve_order` in `Cargo.toml`, so stable header
  normalization requires explicit recursive key sorting rather than relying
  on the JSON map's iteration order.
- `src/agent/tools/read.rs` caps text-oriented reads at 256 KiB and may apply a
  smaller model token budget. `mode: chars` and `mode: bytes` use different
  offset units; Base64 byte ranges preserve bytes across UTF-8 boundaries.
- `docs/known-test-issues.md` documents the shared tracing/cwd lock and the
  asynchronously released session-writer lock. `tests/session_commands.rs`
  clears subprocess environments, including `PATH`.
- The newer reflection design is on `feature/self-improvement` at commit
  `8466228`; the checked-out version is older. It describes metadata-only
  manifests, source/export digests, restricted readers, and run cleanup.
  Reflection owns mining ranges, progress, consent, and cleanup.
- `CHANGELOG.md` is generated by release-plz under ADR-0002.
