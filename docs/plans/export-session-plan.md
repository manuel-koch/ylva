# Session export implementation plan

## Context

Add a read-only, provider-independent `ylva session export` command so people
and tools can read Ylva's persisted conversations without starting or resuming
a conversation. It must run before profile, engine, agent, or prompt
initialization and select sessions only from the launch directory's existing
`project_key`. Resolve the configured chat root, but refuse symlinked project
directories and symlinked or non-regular transcript and sidecar entries below
it. Export itself must not restore, repair, rewrite, lock, or otherwise change
the source store. Other store readers keep their existing behavior.

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
redaction for either format and always warns on stderr. Only raw JSONL
reproduces the captured logical transcript bytes exactly, including unknown
or malformed lines, original line endings, and incomplete trailing bytes.
Plain and gzip representations have the same behavior; retain the existing
archive conflict, identity, sidecar, and corruption checks. Refuse export
if a readable `meta` explicitly identifies a different
session or project, even for plain-only raw JSONL. An absent, unreadable, or
ambiguous header does not by itself block raw recovery of a plain-only
transcript without a sidecar.

Sanitized output is best effort, not proof that secrets are absent. Resolve
one trusted, separately installed local *TruffleHog* executable from absolute
entries on `PATH` and check its required interface at startup. Use that
executable throughout the invocation, with verification and update checks
disabled. Ylva must neither install nor link it, switch scanners mid-run, nor
fall back to raw output. Scan each unique snapshot once and reuse the sanitized
event stream and its outcome for all copies. Redact findings locally, supplement
them with deterministic credential-assignment redaction, and withhold content
that cannot be handled safely. Never echo scanner findings, detected values, or
scanner stderr in reports or logs. Warn that sanitized exports can still contain
sensitive data and that Markdown can contain active HTML, links, or remote images.

Sanitized output omits typed media and known Base64 payloads from `write`
arguments and `read` results, including encoded text. Markdown identifies the
omissions and withheld or omitted messages, but does not announce routinely
hidden metadata. Raw Markdown retains non-media encoded content but omits
typed media; raw JSONL retains all payloads.

Markdown is a readable view: group tool calls with unambiguous results, omit
added source line numbers and call/run UUIDs, and use short labels when needed
for correlation. Do not remove identifiers from ordinary conversation or tool
content merely because they resemble IDs; known subagent lifecycle fields are
internal metadata. Grouping can change the apparent order of interleaved
events; show asynchronous subagent completion separately. Link bundled parent
and subagent files to each other using relative, indexed filenames.
Sanitized JSONL and manifest edge/omission records retain source references.
Use the session title as a safely rendered heading when present; otherwise use
`Session`. Scan the title in sanitized exports. If an unsafe title is withheld,
report it as a partial failure.

Capture a stable snapshot independently for each source, including active
sessions, without claiming a globally simultaneous snapshot. For a plain
transcript, measure the byte boundary once on the opened file and read the
header and bounded prefix from that same handle. If a title replacement races
the open, either title is acceptable; never reopen to refresh it or include
appends beyond the captured boundary. Validate gzip through EOF on the pinned
representation. Retry observed archive representation changes a bounded
number of times before reporting an unstable source, distinct from stable
corruption. Once captured, reuse the snapshot across the invocation.

Include a complete, valid final JSON object even without its newline. Report
an unparseable partial tail and omit it except in raw JSONL. An ambiguous
duplicate-key object must not silently become an ordinary event or determine
an edge; withhold it in sanitized output and mark recursive discovery
incomplete. There is no aggregate export-size limit, CLI paging, or fragment
record format. Available memory and scratch disk still limit the work; use
bounded open handles and private spooling rather than holding all sources open
or buffering transcripts in memory.

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
For multiple distinct roots, use `session-1/`, `session-2/`, and so on
under the output directory, in first-requested order. Reserve numbers even
when no output stages for a root. A path containing
`{project_key}`, `{session_id}`, or `{format}` is instead a template for each
root's bundle directory, even if it contains only one transcript; tokens use
the root's identity. Reject malformed templates as usage errors.

Within each bundle, name the root `session.<format>` and each non-root
transcript `subagent-<number>.<format>`. Assign bundle-local numbers by a
breadth-first traversal: inspect each parent's recorded edges in source order
and number each distinct referenced child on first encounter, including an
unavailable child. A cycle back to the root reuses `session.<format>`; no
source is copied twice within one bundle. With `--no-subagents`, number only
root-side child references in source order for display; those unrequested
children do not create output paths or affect stdout selection. All files for a
bundle are siblings.

`--manifest PATH` optionally writes a [versioned metadata manifest](#machine-readable-records).
The manifest must be outside every actual bundle. It may be a sibling of a
standalone file or of the bundle directories. Treat the manifest and reports as
sensitive: source paths, session IDs, and hashes can disclose information
without containing transcript text. Diagnostics and per-output reports go to
stderr, never payload stdout.

#### Staging and publication

Inventory fixes the layout and numbering before rendering. For file exports,
first render privately, then preflight the final destinations of successfully
staged transcripts and the requested manifest as a set. Use owner-only
scratch outside the chat store and export tree; promptly unlink snapshot
spools and clean up scratch on cooperative exit. No final output path or
directory is created or changed during inventory, scanning, or private
rendering. Do not renumber after a failure or check hypothetical files.
Create a bundle only if it has staged transcripts; an available child can
still be published if its root failed to render. Never create an empty
substitute file. Stdout remains streaming: if a manifest is requested in this
mode, preflight it before emitting any payload bytes.

Preflight every exporter-opened final destination. Reject existing files
without `--force`, final symlinks and non-regular entries even with it, and
paths inside the Ylva chat store, including aliases. Resolve permitted
symlinked parent directories. Detect file/directory, aliased, and nested
bundle collisions. Compare existing paths by filesystem identity and reject
case- or Unicode-equivalent spellings if their distinctness on the destination
filesystem cannot be established. Each actual bundle directory, even a
single-transcript templated bundle, must be new or empty. Any entry in it
causes refusal, including a hypothetical unavailable child's filename.
`--force` does not permit replacing a nonempty bundle or deleting its
contents.

After preflight, create missing directories with owner-only permissions where
supported; do not change existing directory permissions. Report created
directories and leave them in place after failure. Pin and recheck resolved
parents and recheck bundle emptiness before the first destination-side
temporary file. Copy from scratch to an owner-only adjacent temporary file as
needed; never depend on an atomic move across filesystems. Sync the staged
file, then publish atomically with a final preflight-state comparison and
serialization of competing Ylva exporters. Use no-clobber publication for an
initially absent destination. With `--force`, only an unchanged standalone
file or manifest may be replaced. Sync the containing directory and any new
directory entries required for publication before reporting success. If a
required sync fails after rename, report publication as uncertain.

Do not claim protection against every race with external writers or
power-loss guarantees beyond the filesystem's sync semantics. Multi-file
publication is not a transaction: isolate late failures to the affected
destination or bundle, continue independent outputs, and report published,
failed, unattempted, and uncertain outcomes without rollback. Links can be
broken after partial failure. Existing global `--log-file` handling runs
before and outside export preflight; the caller must keep it disjoint from
source and destination paths. The caller likewise owns inherited stdout and
stderr redirects, which can truncate files before Ylva starts.

#### Results and exit codes

Exit `0` when the export succeeds, including when there are only intentional
media or encoded-content omissions or an incomplete-tail warning. Exit `1`
for an unavailable, unstable, or corrupt included source, a withheld unsafe
or malformed complete event in sanitized output, an unrenderable complete
event in raw Markdown, or an execution failure such as a late destination
conflict or uncertain manifest publication. Continue independent outputs. With recursive
inclusion enabled, malformed lines, ambiguous duplicate keys, known legacy
`child_start`/`child_result` records, and invalid current delegation records
make discovery incomplete and cause exit `1`. Do not migrate them or infer
edges from them. Raw JSONL still copies those bytes; with `--no-subagents`,
such copied records remain warnings. A safely displayable unknown event is
not by itself a discovery failure.

Invalid arguments, a missing or incompatible scanner at startup, and known
preflight conflicts cause exit `2` before touching final destinations. At
runtime, withhold an event or batch when a finding cannot be mapped safely.
A scanner process or protocol failure, including a timeout, fails the whole
source export: discard affected staged copies, continue other sources, and
exit `1`. Scan and map each batch before emitting its bytes. Stdout can
contain a partial payload if reading, scanning, or writing fails; report
that on stderr.

Cooperatively handled SIGINT and SIGTERM take precedence and exit `130`.
Stop and reap any active scanner. An interruption before successful
destination preflight discards scratch and reports on stderr only, without
writing a manifest. Once preflight succeeds, make a best-effort attempt to
publish the requested manifest even if staging failed for some or all outputs,
or a later failure or interruption occurs. Stderr is authoritative if manifest
publication fails or is uncertain. SIGKILL and power loss cannot produce a final
report.

#### Reflection handoff

Reflection is out of scope and does not block standalone export. It uses
fresh run-owned paths, never `--force`. A changed or missing export requires
a new current snapshot and invalidation and re-mining of affected evidence;
the exporter does not recreate a past snapshot. Deduplicate shared evidence
by source identity and snapshot digest, preserving every bundle edge and
flagging differing sanitized JSONL hashes for copies of the same captured
source. Report a record that exceeds the mining context as incomplete
coverage rather than assuming it can be mined by paging.

Leave `session list` and local title inventory unchanged. Before sending raw
title inventory to a model, reflection obtains explicit consent for that
inventory, separate from consent to share selected transcripts. Titles in
sanitized exports are scanned like other fields. Standalone exports persist
at the chosen destinations; only reflection manages its run cleanup.
Sanitized inventory, whole-file schema migration, HTML, and PDF are also
out of scope.

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
  `[Session](session.md#ylva-start:subagent-1)`. If no start heading is rendered,
  link to the parent file without a fragment. List unique direct parents, root
  first and then in bundle-number order, separated by commas. Derive these
  relationships from parent events, not only the child's header. Do not link
  to unrequested or unavailable files, or across bundles.
- Emit `<a name="ylva-start:subagent-<number>"></a>` followed by a blank line
  before the first rendered start for each child in a parent file. The number
  is the bundle-local reference number. Use this explicit anchor, not a slug
  derived from heading text. Resolve backlinks from the starts that will
  actually render in the chosen mode before finalizing bundle Markdown.
  Repeated starts and unrelated authored headings do not change the anchor.
  Preserve streaming; cyclic parent relationships must not depend on
  publication order. Viewers that do not honor custom HTML anchors may not
  follow these fragments.
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
  so on in call order. Match results or errors to calls only if the stored
  IDs give an unambiguous pairing; do not print the IDs or repeat a matched
  result at its original position. Show unpaired results separately rather
  than guessing.
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
- Classify encoded `read` results from the source before redaction:
  `message.result_metadata.encoding` must be `base64` on a `read` tool
  result. Don't infer encoding from legacy results without the marker. In
  sanitized JSONL, replace the known encoded payload in `message.result`
  with `[Encoded content omitted.]` and omit any persisted preview of that
  payload in `message.content_sections`. Report each omitted location.
  Typed attachments also identify an omitted payload; use the media omission
  in preference to the generic encoded-content omission for the same field.
- Render an ordinary tool result in a `text` fence; for an error, emit
  `> Tool error:` followed by the fenced error text. Under a call, emit
  `> Result not recorded.` when no matching result was recorded,
  `> Result not paired.` when matching is ambiguous, and `> Content withheld.`
  when the identified result was withheld. For a typed media attachment in
  either Markdown mode, emit `> Media omitted: <media type>.` instead of
  its bytes. Use the sanitized media type in sanitized Markdown. For an
  omitted non-media Base64 `read` result, emit `> Encoded content omitted.`;
  raw Markdown instead fences that result as text. Do not remove UUID-like
  text from ordinary tool content.
- A `spawn_subagent` admission result with a valid `subagent_accepted` event,
  run ID, and `running` or `queued` status is rendered as
  ``Subagent #<number> accepted (`<status>`).`` rather than printing lifecycle
  IDs. If that result is not recognizable, render its text normally when safe
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
  Preserve well-formed authored Markdown and fenced content, including active
  links, images, and HTML. Close or escape malformed blocks that would swallow
  generated headings or anchors, such as an unclosed fence or HTML comment.
  Add a structural newline before a generated closing fence only when needed.
  Escape Markdown punctuation and line breaks in generated single-line
  headings and media notices. In both Markdown modes, visibly escape terminal
  control characters other than line breaks and tabs; raw JSONL remains
  byte-exact.

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

For sanitized JSONL, emit one complete JSON object per complete source line
or valid final unterminated object, without a maximum record size or fragments.
Report an unparseable partial tail without emitting a record for it. An emitted
event has this shape:

```json
{"export_version":1,"source":{"session_id":"UUID","line":42},"event":{"type":"message"},"omissions":[]}
```

`source.line` is the original 1-based source line, not the output line.
Preserve safely sanitizable unknown JSON objects inside `event`; use
`event: null` when a complete event must be withheld. Use fixed reason codes
in `omissions` for withheld events and for omitted media or encoded content.
Include a schema-known field location where possible, using only schema field
names and array indices. Otherwise refer to the source line alone. Reports
and manifest omissions must likewise never include arbitrary source keys.
Ordinary line-oriented reads of an oversized record can truncate it; consumers
must read character ranges or Base64 byte ranges, continue to EOF, and
reassemble the record before parsing. A deliberately limited page need not
set the read tool's `truncated` flag.

The manifest's version 1 object contains `version`, `source_home`, `project_key`,
`selected_ids`, `sessions`, `bundles`, `outputs`, `edges`, and overall
`status`. A session record identifies one source, its representation, source
paths, availability, snapshot boundary, source digests, and whether recursive
discovery was complete, incomplete, or not requested. A bundle record
identifies its requested root, directory, standalone file or stdout, and
status. An output record relates a root and source session to its bundle-local
number (`null` for the root), destination or stdout, publication status,
content completeness, output digest, scanner version, and omission references
and counts. Record unavailable or unstaged included members with no destination.
Publication and completeness are independent: a file may be `published` even
when records were withheld or a byte-exact raw copy had incomplete subagent
discovery. Report a definitive output digest only for confirmed file
publication or a complete, flushed stdout stream. Copies of one sanitized
JSONL snapshot have equal digests within the invocation; Markdown copies
can differ because of bundle-local links.

Each edge contains the parent UUID, child UUID, source line, and event kind,
including recognized admissions. Use `null` for unavailable information.
Do not include transcript text, titles, tasks, arbitrary source keys, or
scanner findings in the manifest. Record enough snapshot metadata to compare
the title-independent header separately from the complete-event prefix after
a title change or plain/archive conversion. The manifest describes transcript
outcomes; its own publication result is reported on stderr rather than
recursively recorded in itself.

Use lowercase hexadecimal SHA-256 values. Record the captured logical byte
length and digest, plus a digest and byte length for complete source lines
after the header, including malformed complete lines but excluding an
unparseable partial tail. Include a valid unterminated final event in that
prefix. For the header digest, parse its JSON object, remove only `title`,
recursively sort object keys, serialize compactly as UTF-8, and hash the
resulting bytes. This preserves unknown fields while allowing a title-only
rewrite; the event-prefix length is relative to the end of the header so the
header's changed length does not shift the comparison. If the header is absent,
unreadable, or ambiguous (including duplicate keys), its normalized digest is
`null`. This is not a requirement for a readable header in raw plain-only
exports; a clear identity mismatch is still refused.

## Implementation Plan

Complete the steps in order. Update affected documentation alongside each
behavioral step, and mark its checkbox `[x]` in this file immediately after
the step and its tests and documentation are complete. Write focused failing
tests first where practical. Add executable tests as each CLI behavior lands
in `tests/session_commands.rs`, using isolated temporary homes and an offline
fake scanner on a controlled `PATH`. Run the relevant owner-local and
executable tests and `make lint` before marking each code-bearing step done.
Do not stage, commit, or merge branches without explicit user consent.

1. [ ] Establish the baseline and implementation boundary. Start with
   `src/session_command.rs`, `src/cli.rs`, and `src/store/archive.rs`; run
   `cargo test store::tests::archive` and `cargo test --test session_commands`.
   Review the recorded decisions and test-isolation rules in the references
   below before changing code.
2. [ ] Add a streaming, read-only snapshot/representation reader through the
   store facade. Pin the chat root and open regular source entries within
   the selected project without following entry symlinks. Capture one fixed
   prefix of an opened plain file; bound the number of handles with private
   spooling as needed. Do not take the active writer's exclusive identity lock
   or create store locks. Validate gzip to EOF, sidecars, and identities, and
   compare simultaneous plain/archive copies incrementally. Retry observable
   concurrent representation changes without relaxing stable corruption checks.
   Preserve the resolver's behavior for other readers. Test equal/divergent
   copies, corrupt archives and sidecars, source aliases, plain-only identity
   mismatches and unreadable headers, maintenance races, appends and title
   replacement, and large snapshots with bounded handles and no whole-file
   buffering. Run `cargo test store::tests::archive`.
3. [ ] Exercise the real early `session` dispatcher and clap model with a
   single-session `--no-subagents -f jsonl --raw` slice. Stream stdout and
   produce a standalone file using private staging, final destination
   preflight, and atomic publication. Establish the helpers used by later
   modes. Test provider-independent startup, open stdin, byte-exact plain and
   gzip payloads, stdout/stderr separation, a known output conflict, and
   unchanged source files. Keep this slice internal to the implementation
   work until the full CLI contract is ready; never default to raw output.
   Run `cargo test --test session_commands`.
4. [ ] Build a streaming inventory of explicit roots and, by default, their
   linked subagents. Follow parent-side `subagent_start`, `subagent_result`,
   and recognized `spawn_subagent` admission records, retaining each source
   line and kind; never infer clone relationships solely from child headers.
   Recognize an admission in a `spawn_subagent` tool result when its JSON
   has event `subagent_accepted`, a valid run UUID, and `running` or `queued`
   status; if `tool_call_id` is present, it must match the result's call ID.
   An accepted queued run can lack a start record or transcript; include its
   ID and report it unavailable. Detect duplicate keys before parsing can
   collapse them, including in the header or a serialized admission.

   Capture each unique source once, guard cycles, and build the deterministic
   breadth-first numbering and membership separately for each root bundle.
   Distinguish unrequested, unavailable, unstable, and corrupt sources.
   Compute per-source boundaries and digests, including the title-independent
   header and complete-event prefix. Test shared children across clone bundles,
   repeated edges, nested/cyclic records, admission-only queued subagents,
   missing and corrupt sources, legacy or malformed delegation records, and
   duplicate keys. Exercise recursive and `--no-subagents` discovery through
   the CLI, including warnings versus failure in byte-exact raw copies.
5. [ ] Implement local sanitization through TruffleHog's JSON stdin interface
   with `--no-verification`, `--no-update`, `--no-ignore-tag`, and
   `--fail-on-scan-errors`. Check scanner compatibility before starting work;
   record the version without echoing scanner output. Set a finite deadline
   per scanner job; terminate and reap it on timeout or interruption.
   Capture findings and stderr privately, never in logs, reports, or
   model-readable directories.

   Treat the entire finding as sensitive. Account for all secret-bearing
   fields, not just `Raw`, and redact safely mapped occurrences in JSON keys
   and values. A literal match elsewhere does not establish a safe mapping
   for a transformed finding. Withhold the affected batch for an unresolvable
   finding or key collision; classify invalid scanner protocol separately as
   a whole-source failure. Define a finite, case-insensitive list of common
   credential names (such as `password`, `api_key`, `api_token`, `access_token`,
   and `authorization`) and apply it to structured JSON values and textual
   assignments even without scanner findings.

   Classify media, known Base64 writes, reads, and their previews before
   redaction. Process unknown objects safely without the closed `StoreEvent`
   enum. Reuse the sanitized stream and verdict for all copies of the source.
   Test with fake scanners: missing/incompatible scanners, multiple finding
   values, transformed findings with an unrelated literal match, key
   collisions, generic assignments, media and encoded text/binary, raw bypass,
   and secret-free scanner diagnostics. Test invalid scanner output and timeout
   discarding one source's staged copies while other sources continue.
6. [ ] Implement streaming Markdown and sanitized JSONL on the captured event
   stream, reusing the raw JSONL copier for raw output. Use the rendering and
   record rules above, including explicit start anchors and independent
   per-bundle backlinks before publication. Group unambiguous calls with
   results, keeping asynchronous completion separate. Retain parent-side
   results without child files when traversal is disabled. Render oversized
   JSONL records without fragments; raw JSONL remains independent of parsing.

   Use `docs/plans/export-session-markdown-fixture.json` and the expected
   parent and child in `docs/plans/export-session-markdown-example.md.txt` and
   `docs/plans/export-session-markdown-subagent-example.md.txt` as a rendering
   test. Cover clone-specific backlinks, shared and unavailable children,
   multiple parents and cycles, repeated starts, missing or withheld starts,
   and authored headings that repeat generated start headings. Also cover
   overlapping calls, ambiguous pairing, errors, withheld and missing results,
   asynchronous completion, Markdown omissions and hidden lifecycle
   identifiers, UUIDs preserved in ordinary content, JSONL source-line
   references after omissions, field-aware arguments, multiline Markdown and
   literal strings, nested objects and arrays, unknown tools and fields,
   non-object roots, null and empty values, encoded-content omissions, and
   source text matching the omission marker. Cover present, missing, and
   unsafe titles, embedded fence delimiters, unclosed Markdown blocks,
   terminal controls, malformed lines, a valid unterminated object, media in
   both Markdown modes, large events, CRLF, and incomplete trailing bytes.
   Exercise CLI selection of both formats with and without `--raw`.
7. [ ] Complete the destination planner and preflight for all output modes.
   Resolve layout and template syntax from the inventory. After rendering to
   private scratch, preflight only staged transcript destinations and the
   requested manifest outside the bundles. Resolve existing ancestors and
   preserve their and each final destination's preflight identity and state.
   In stdout mode preflight a requested manifest before the stream. Test
   single-file versus bundled output, numbered roots, untouched hypothetical
   paths, a staged child with a failed root render, nonempty bundles,
   templates, aliased or nested directories, case/Unicode aliases, permitted
   parent symlinks, chat-store aliases, final symlinks, and stdout selection.
8. [ ] Complete publication and cooperative interruption for all outputs.
   Create private directories after preflight, recheck pinned parents and
   bundle emptiness, and sync owner-only adjacent temporary files before
   atomic publication. Serialize competing Ylva exporters and revalidate
   destination state before publishing; publish no-clobber for an initially
   absent file. Sync the required directory entries, and report uncertainty
   if sync fails after rename. Test `--force`, permissions, destinations that
   appear, disappear, or change, concurrent Ylva writers, copying from scratch
   across filesystems, and late failure without rollback. Verify that created
   directories remain and are reported, unaffected outputs continue, and
   cooperative SIGINT and SIGTERM clean up scratch and stop active scanner work.
9. [ ] Implement the versioned manifest and stderr reporting from the same
   inventory and publication results, without transcript content. Compute
   source and per-output hashes while streaming; associate each duplicate
   output with its root bundle and shared source snapshot. Distinguish
   publication, content completeness, and discovery completeness. Include
   final output digests only for confirmed success and use only fixed
   omission reason codes and schema-known paths or source lines.
   Apply destination safety to the manifest itself. Test metadata privacy,
   shared-snapshot hashes, scanner versions, title/archive verification
   digests, missing children, partial publication, and failed or partial
   stdout. Check exit codes and manifest behavior on both sides of final
   preflight, including all outputs failing during staging, cooperative
   interruption, and an uncertain manifest.
   Run `cargo test --test session_commands` and the owner-local export tests.
10. [ ] Update `docs/cli-options.md`, `docs/features.md`, `docs/persistence.md`,
    `docs/ideas/session.md`, and `README.md` for the actual CLI, output schema,
    snapshot, safety, and limitations. Keep
    `docs/plans/export-session-markdown-template.md` and
    `docs/plans/export-session-markdown-fixture.json`,
    `docs/plans/export-session-markdown-example.md.txt`, and
    `docs/plans/export-session-markdown-subagent-example.md.txt` aligned with
    the accepted output contract and rendering test. Leave
    `docs/ideas/self-improvement.md` to the separate reflection change; its
    reconciliation is not an exporter completion gate.
    Update `docs/implementation-notes.md` only for non-obvious cross-module
    rationale, following its opening guideline. Run `rumdl check` on changed
    Markdown.
11. [ ] Run `make format lint` and `make test-coverage`; the latter is the
    single mandatory full-suite gate and must retain at least 80% aggregate
    line coverage. Run `make audit` only if `Cargo.toml` or `Cargo.lock`
    changes. Report any failed or unavailable checks.

## Consistency Issues

- [low] The newer reflection idea assumes fragments, sanitized title inventory,
  shared files instead of root-specific copies, and historical-snapshot
  regeneration. Reconcile it with the reflection handoff before implementing
  reflection. Do not overwrite that branch's design or block standalone export
  on its integration.

## Implementation Hints

- `src/main.rs` dispatches `CliCommand::Session` before starting the Tokio
  runtime or loading providers; `src/session_command.rs` resolves
  `default_chats_root()` and `project_key()` for the canonical launch
  directory. ADR-0003 keeps session commands independent of workspace trust;
  ADR-0004 shares the equivalent project key across linked worktrees.
- Global logging starts before session dispatch in `src/main.rs`;
  `src/logging.rs` opens the supplied log file with truncation. This is
  separate from exporter-managed destinations.
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
  descriptors alongside a Base64 tool-result string. For Base64 reads,
  `src/agent/tools/read.rs` reports `result_metadata.encoding` and can supply
  a decoded text preview in the persisted `content_sections`. The request
  dump redactor in `src/llm/openai_compatible_provider.rs` is media-specific,
  not a general transcript secret scanner.
- `src/agent/tools/bash/result.rs` identifies Bash commands as shell content;
  `src/agent/tools/spawn_subagent.rs`, `clarify.rs`, and `agent_finish.rs`
  identify the specified prose fields as Markdown. `src/agent/tools/write.rs`
  accepts Base64 file content, including non-media bytes and encoded text.
- `Cargo.toml` already includes `pulldown-cmark`. GitHub supports
  [custom HTML anchors](https://docs.github.com/en/get-started/writing-on-github/getting-started-with-writing-and-formatting-on-github/basic-writing-and-formatting-syntax#custom-anchors)
  independently of generated heading IDs.
- `src/private_fs.rs::replace_file` buffers supplied contents and unconditionally
  renames over the target, syncing both file and parent directory. It does not
  provide streaming, no-clobber publication, or destination-state revalidation.
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
