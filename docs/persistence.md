# Ylva agent-harness persistence

Reference for Ylva's persisted data.

Every conversation is written to a plain [JSON Lines](https://jsonlines.org)
file as it happens, so a killed process leaves every already-committed line
intact and a session file can be inspected with `cat`, `grep`, or `jq` without
any Ylva-specific tooling.

Persistence is mandatory for normal sessions. Startup fails if the Ylva home
cannot be resolved, or if the chats root or session file cannot be created or
opened. Set `YLVA_HOME` to a writable state directory when the operating-system
home directory is unavailable.

## On-disk layout

```text
~/.ylva/chats/<project_key>/<session_id>.jsonl
```

| Segment | Meaning |
| --- | --- |
| `~/.ylva/chats` | Root of all persisted sessions. `~/.ylva` is resolved by `ylva_home::ylva_home()`: the `YLVA_HOME` environment variable, used verbatim as the Ylva home when it is set and non-empty, otherwise `<operating-system-home>/.ylva` (`dirs::home_dir()`). |
| `<project_key>` | SHA-256 hex digest grouping every session launched from the same physical directory — see below. |
| `<session_id>` | The session's UUID v4. `--conversation-id <session_id>` re-attaches it. |

A child agent's transcript is a session file of its own in the **same**
`<project_key>` directory, written once when the child finishes.

Manual compression replaces `<session_id>.jsonl` with a lossless
`<session_id>.jsonl.gz` archive and a `<session_id>.meta.json` sidecar containing
the metadata header fields. Stable `<session_id>.lock` files and
`.project.lock` coordinate writers and maintenance; they are retained and are
not transcripts. Temporary transition/rewrite files are ignored by discovery.

### Relocating the Ylva home with `YLVA_HOME`

Setting `YLVA_HOME=/some/dir` makes Ylva read and write `/some/dir` directly
— chats *and* profiles — instead of `~/.ylva`. `YLVA_HOME` names the Ylva home
directly, matching the convention of `CARGO_HOME`/`RUSTUP_HOME`/`GOPATH`/
`XDG_DATA_HOME`: it is *the* Ylva home. The dedicated variable exists so
an isolated setup (a test harness, a throwaway sandbox) can redirect Ylva's own
state. An unset or empty `YLVA_HOME` falls back to
`~/.ylva`. A workspace-local `YLVA_HOME` (for example `$(PWD)` from
`.ylva/.envrc`) keeps chats under `<workspace>/.ylva/chats/`, which the workspace
`.gitignore` ignores (see [configuration](configuration.md)).

### `project_key` derivation

`project_key` is the SHA-256 hex digest of the **canonicalized** launch
directory (symlinks resolved, `..` collapsed, using the same
`SandboxContext::canonicalize_best_effort` helper the sandbox uses for its
workspace root). Two paths that resolve to one directory — a symlink and its
target, or macOS's `/var` and `/private/var` — therefore share one key, while
two genuinely different directories never collide.

It is deliberately **not** the git root: a plain, non-git directory is a
first-class project.

## File format

Each line is one JSON object with a `type` tag. The **first line of every file
is the `meta` header**; every following line is an event.

```json
{"type":"meta","version":1,"created_at":"2026-01-08T09:41:07.123Z","schema_version":1,"app_version":"0.7.0","project_key":"7f3c…","provider_connection":"openrouter","model":"gpt-4o","session_id":"6f1e…","title":"Store Layout Summary"}
{"type":"model_changed","version":1,"changed_at":"2026-01-08T09:41:07.123Z","provider_connection":"openrouter","model":"gpt-4o"}
{"type":"message","version":1,"timestamp":"2026-01-08T09:41:07.456Z","message":{"role":"user","content":"explain the store layout","kind":"explicit"}}
{"type":"message","version":1,"timestamp":"2026-01-08T09:41:09.012Z","message":{"role":"assistant","content":"…","status":"complete","tool_calls":[]}}
{"type":"child_start","version":1,"created_at":"…","id":"…","parent_session":"6f1e…","agent_type":"explorer","task":"…"}
{"type":"child_result","version":1,"completed_at":"…","id":"…","parent_session":"6f1e…","status":"success","result":"…","sources":[],"origin":"agent"}
```

A child-agent's own session file carries `parent_session` in its own `meta`
line too:

```json
{"type":"meta","version":1,"created_at":"…","schema_version":1,"app_version":"0.7.0","project_key":"7f3c…","provider_connection":"ollama","model":"llama3","session_id":"9a2d…","title":"…","parent_session":"6f1e…"}
```

### `meta`

| Field | Description |
| --- | --- |
| `version` | Per-line version of the `meta` event's own shape — see [Schema tolerance](#schema-tolerance) below for how this differs from `schema_version`. |
| `created_at` | Session creation time in RFC 3339 UTC, millisecond precision. Never updated by resume; it is the baseline for derived last-use time, not the maintenance age by itself. |
| `schema_version` | Version marker governing the whole file. Written **once**, when the session is created, and never rewritten afterwards — not even across a later resume under a newer Ylva build. A session created by an old build and resumed for months by newer ones keeps that original `schema_version` forever; it is not a "current shape" indicator, only a record of what the file's format was when it was created (see [Schema tolerance](#schema-tolerance)). |
| `app_version` | Ylva version that created the session. |
| `project_key` | The [`project_key`](#project_key-derivation) of the launch directory. |
| `provider_connection` / `model` | The provider-connection/model resolved for this session at creation time — mirrors the very first `model_changed` record (see below) so it's readable from the header alone. Never rewritten after creation; a later `/models` switch or a different resolution on a fresh process start is tracked only by `model_changed`. |
| `session_id` | The session's UUID — matches the file name. |
| `title` | Generated by the configured `utility_engine`. Roots use the first explicit user message and first non-empty `complete` assistant reply. Subagents prefer their original task plus the completed structured `agent_finish` result or error; without such an outcome, the existing text-reply flow remains available. The response is normalized to the first non-empty line, matching surrounding quote characters are removed, and the result is limited to 80 characters. It remains empty when no eligible outcome exists or generation fails; it is never replaced with a task label or copied user message. |
| `parent_session` | Present **only** on a child-agent's own session file, naming the parent's `session_id` — mirrors the parent-side `child_start`/`child_result` events so the link is discoverable from either file. Omitted entirely (not even `null`) for every root session and for `/clear` rollovers, which start a fresh session but are not a child run. |
| `agent_type` | The root agent type resolved for this session at creation (or when `/clear` started its replacement session, or when a `--clone` copied the source's type). `None` for sessions written before the field existed — those read back as the `default` agent type. Later selections are tracked only by `agent_type_changed` below; the header is never rewritten. Omitted entirely while unset. |
| `cloned_from` | Present **only** on a session created by `--clone`, naming the source session's `session_id`. The link is one-way: the source header is never rewritten, so a clone cannot be discovered from the source file. Omitted entirely (not even `null`) for every session that was not cloned. |

The header is written to disk **immediately** when the session is created —
with an empty `title` — so every event committed after it is durable the
instant it is written; no event is held in memory waiting on an LLM call.
After a root's first non-empty complete assistant reply is committed, the CLI or TUI driver
builds the same prompt returned by `Store::title_prompt()` and sends it to the
raw configured utility engine with no tools, store, or visible UI updates.
Successful output is supplied to `Store::set_title()`, which atomically
rewrites only the first line (temporary file plus `rename()`); all transcript
events remain byte-for-byte in their original order. A failed or interrupted
utility task leaves the title empty. `Store::resume` re-derives the first
explicit user message and first non-empty complete assistant reply from the persisted
transcript, allowing the next process to retry the same flow.

Finished subagent transcripts use the same utility request and title-persistence
flow, but also accept a structured terminal outcome. An `agent_finish` call must
have a matching successful tool-completion record by call ID and tool name;
an intended call, failed tool execution, or acknowledgement text is not an outcome.
Successful tasks use the nonempty `result`; failed tasks use the nonempty `error`,
with the same status inference as the tool itself. The terminal outcome is
preferred over intermediate assistant commentary. Original messages stay
unchanged: no synthetic assistant reply is inserted. Resume reconstructs these
inputs from the persisted call arguments and completion record. Existing titles
are preserved. No automatic backfilling or provider requests occur during listing.

`Store::resume` never reconstructs `SessionMeta` from scratch — the only
rewrite of the `meta` line mutates `title` on the already-parsed struct,
leaving `schema_version`, `app_version`, and every other field unchanged.

### `message`

Wraps one **committed** conversation message: an explicit or implicit user
turn, a completed or interrupted assistant turn, a tool result, or a tool
error. Live streaming deltas are ephemeral UI state and never reach disk; a
message is written once, at the moment the agent loop commits it to its
`Context`, so the on-disk order is the commit order.

`timestamp` (RFC 3339 UTC, millisecond precision) records that same commit
instant — effectively "sent" for a user message and "response fully
assembled" for an assistant message, since a message has exactly one commit
point. It is *not* a stream-start or first-token instant: no such event is
persisted, so per-message generation latency (e.g. time-to-first-token) can't
be derived from the transcript today. Lines written before this field existed
read back with `timestamp: ""`.

The two system prompts every context starts with are deliberately not
persisted: they are rebuilt from the running binary on every start, so storing
them would pin a stale copy into the transcript.

### `agent_type_changed`

Written when an explicit `--agent <type>` override on a resumed session
differs from the type that session was already using, so later resumes
without the flag continue under it. `meta.agent_type` only captures the type
resolved at creation; this event is the authoritative, replayable history of
every later selection and the last one wins. A `--clone` run needs no such
record: the clone's header already carries its effective type.

### `model_changed`

Records the actual `provider_connection`/`model` serving the session — written once on
**every process start** (fresh session or resume alike, since resuming under
a different `--profile` or an edited config can resolve to a different
provider connection or model than last time) and again after every successful
`/models` switch. `meta.provider_connection`/`meta.model` only capture the pair resolved at
creation and are never rewritten afterwards, so `model_changed` is the only
authoritative, replayable history of every value that has actually served the
conversation, including that first one. A child-agent session gets its own
independent `model_changed` history, starting with the child's own resolved
engine — it never needs to cross-reference the parent's.

Named-engine identity and reasoning effort are not persisted. Resuming resolves
the current profile and starts from its configured agent/default engine; a
same-provider/model shortcut activation may therefore have no distinguishable
engine-only record. Engine runtime settings are not restored from the session
history.

### `child_start` / `child_result`

Written to the **parent** session when a child agent run begins and when it
finishes. The result's `origin` is `agent` when the child called
`agent_finish`, or the serialized value `runtime` when the harness produced
the outcome. Here `runtime` means Ylva's harness, not an Engine runtime. Both
carry `parent_session`, so a child's records are always
attributable. The child's own transcript is written separately, once, at
completion, and its own `meta` line carries `parent_session` too (see above) —
so the link is discoverable from either file, not just top-down from the
parent.

### `compaction`

Written by the outer loop when a background summary is applied to the model
view. Appended, never rewritten: a later event with a larger `boundary`
supersedes an earlier one, and replay keeps the newest snapshot while every
summarized message stays in the file (the transcript is never pruned).

| Field | Description |
| --- | --- |
| `compacted_at` | RFC 3339 UTC timestamp, millisecond precision. |
| `boundary` | First retained tail index in the **live model context**, including the leading system block. Current restore rebases `transcript_boundary` against rebuilt systems rather than assuming this live coordinate stays unchanged. |
| `transcript_boundary` | The same cut in **persisted-transcript coordinates**: the system prompts excluded, so it indexes this file's `message` records directly. The TUI reads this to place its compaction marker without building a model context. `None` (omitted) for events written before the marker existed — those snapshots replay without a marker. |
| `summary` | The compact-engine text representing `[..boundary]`. On resume it is projected as one marked user message ahead of the verbatim tail; it is never inserted into the transcript itself. |

### `model_projection`

Written whenever the bounded model view changes, before dispatch or after a safe
compaction sweep. Canonical `message` records and UI sections are never replaced.
Replay chooses the highest `projection_revision`, including updates at an
unchanged boundary. Clone copies these events unchanged.

The `projection` object carries `projection_revision`, `transcript_start`,
`message_overrides` (transcript-relative index/message pairs), `summary_override`,
and optional `handoff`/`anchor` messages. Overrides contain bounded model bodies,
not duplicate rich UI sections. Tool IDs/names and safe boundaries are validated
on restore; invalid state is logged and ignored. Coordinates exclude leading
system messages, so rebuilt system blocks may have different lengths. A newer
summary boundary supersedes obsolete overrides and recovery handoffs. Synthetic
summaries/anchors are projected only; hydration never appends transcript copies.

## Schema tolerance

Events are `#[serde(tag = "type")]` and **never**
`#[serde(deny_unknown_fields)]`: unknown fields are ignored and an unknown
`type` fails only that one line. Old readers load new files and vice versa. The
per-event `version` field lets a reader take an event-type-specific path even
without consulting the header.

`version` and `schema_version` deliberately track two different things, at
two different scopes, and can drift apart within a single file:

- `schema_version` lives only on the `meta` header and describes the file as
  a whole. It is written once, at creation, and — as noted above — never
  rewritten, not even across a resume by a newer Ylva build.
- `version` lives on event lines (`meta` included), except `model_projection`,
  which uses its nested `projection_revision` only, and describes just that
  one line's own shape. It is stamped fresh, from the *currently running*
  binary, every time a line is written.

Consequently, a session file created under an old build and resumed later
under a newer one can end up with a `meta.schema_version` frozen at its
original value while later lines in the very same file carry a higher
`version` than that — the newer binary's per-event shape moved on even though
nothing rewrote the header to say so. Only a hard, whole-file semantic break
(needing a real migration/upgrader) is meant to ever bump `schema_version`;
ordinary per-event evolution is exactly what the per-line `version` field
exists to tolerate without that.

## Locking

An open session holds a **non-blocking exclusive advisory lock** (`flock`) on
a stable `<session_id>.lock` identity file until its writer drains. Title
replacement cannot release this lock. Identity lock files are retained, never
unlinked or recreated during maintenance. A second Ylva that tries to open the
same session fails immediately with "session … is open in another Ylva
process" instead of blocking or silently interleaving writes. The lock is
released when the session is closed.

Listing sessions takes no lock — it is a pure read.

A project coordination lock serializes clone/child publication with reference
discovery and deletion, preventing removal of a child while another session
publishes a reference to it. It is held only during coordinated operations, not
for the lifetime of a conversation. Compression and decompression do not take
this project lock. Session/clone creation and child-reference publication may
wait while deletion holds it. These locks contain no transcript data.

Other sessions in the project may remain open during maintenance. Only selected
open transcripts, or deletion groups containing them, are refused; independent
unlocked targets continue.

## Manual maintenance and archive recovery

See [Session commands](cli-options.md#session-commands) for syntax and selectors.
There are no automatic sweeps, age/size settings, or automatic compression.
New sessions remain raw JSONL until explicitly archived.
Compression and decompression report `compressed <uuid>` and
`decompressed <uuid>`; dry-run reports `would compress <uuid>` and
`would uncompress <uuid>`. Neither lists storage files. Already-transformed
targets report `already compressed <uuid>` or `already uncompressed <uuid>`,
including in previews.

Compression writes and synchronizes a complete temporary metadata sidecar,
publishes it atomically, then publishes a verified gzip archive in the same
directory. Both destinations and the directory are synchronized before removing
the raw source. Decompression publishes and synchronizes raw JSONL before
removing the archive, then removes redundant metadata. An interruption leaves a
complete recoverable copy. Repeating the manual operation under its identity
lock finishes an interrupted transition.

Discovery and every replay reader recognize raw and archived transcripts,
including root/subagent links, clones, titles, agent selection, compaction,
model projections, attachments, and input history. Reads never decompress,
repair, or migrate files. Equivalent raw/archive copies appear once; differing
copies produce diagnostics and block maintenance, resume, and cloning. Sidecars
must agree with the filename, project, and archive header. A sidecar alone is
not a transcript; corrupt gzip or inconsistent metadata is an explicit error.
A matching sidecar beside raw JSONL without an archive is a recoverable
interrupted transition.

Deletion coordinates reference discovery with clone and delegation publication.
Deleting a root includes unshared descendants, but descendants still referenced
by surviving roots or clones are retained and reported. Explicit deletion of a
referenced subagent transcript fails. `cloned_from` is historical provenance:
deleting the source never cascades to its clones. A locked root blocks deletion
of its entire descendant group. Independent groups continue after runtime
failures; a partial batch exits with the general-error code (see
[exit codes](./cli-options.md#exit-codes)). Deletion is not a multi-file
transaction. Normal deletion reports each deleted session by UUID, including
descendants, as `deleted <uuid>`; dry-run reports the same selected sessions as
`would delete <uuid>`. Neither lists storage files. Retained descendants and
failures remain visible, with file-level failures naming the path.

Maintenance is supported on Unix only. Other platforms explicitly refuse
maintenance and archived resume requiring restoration; ordinary raw sessions
remain supported.

## Resume contract

- Without `--conversation-id`, every start — TUI and headless alike — begins a
  **fresh** session. Ylva never auto-resumes and shows no startup picker.
- `--conversation-id <uuid>` re-attaches that session for **append**: the
  existing transcript is preserved. Archived sessions are restored under the
  identity lock before append-open and trailing-line repair.
- An unknown id is a **hard error**, never a silent fresh-session fallback.
- **Repair-on-open:** if the last line of a resumed file is incomplete or
  unparseable (a write cut short by a power loss), it is truncated before the
  first append, so the repaired file never carries mid-file corruption. Only
  resume repairs; reads never mutate.
- **Discoverability:** on exit, Ylva prints the final active session id, its
  persisted title when available, and a
  `ylva --conversation-id <uuid>` command to stderr so the id never has to be
  hunted down in the chats directory. See [Resuming a session](cli-options.md#resuming-a-session).
- **Replay:** when a store is attached, `Context::set_store` reads the
  committed messages and `Context::hydrate` appends them after the two system
  prompts rebuilt by the current binary. Hydration bypasses `Context::commit`,
  so reading a session never writes its messages back or duplicates them on a
  later flush. The rule is unconditional for fresh and resumed sessions; a
  new file simply has no messages to hydrate.
- **TUI input history:** On startup, the TUI also reads explicit user messages
  from prior root sessions in the same project, newest prompts first until
  the configured `input_history_limit` is reached, then presents those
  messages oldest to newest. Child-agent transcripts and the current session
  are excluded from this lookup; the current session is replayed separately.
  Exact duplicate prompts are collapsed across sessions and only the newest
  occurrence is retained. These messages seed only the draft input history and
  never hydrate the visible transcript or model context, so this does not
  change the fresh-session/no-auto-resume contract.
- **Full-fidelity tool bubbles:** `Message::ToolResult` persists `summary`,
  `content_sections`, `duration`, and specialized result-section identifiers;
  `Message::ToolError` persists `content_sections` in addition to its
  duration. These additive fields use `#[serde(default)]`, so files written
  before replay support deserialize and show silently degraded (blanker) tool
  bubbles.
- `Message::ToolResult` also has an additive `attachments` field. It defaults
  to an empty list when an older JSONL line omits it. Each
  `ToolAttachment` stores only provider-neutral media metadata and references
  the containing result's canonical Base64 `result`; it does not duplicate the
  payload in the session file.
- Attachment-bearing results remain ordinary persisted tool messages. During a
  capable provider request, serialization may add a synthetic user message
  after the contiguous tool-response group, but that message is wire-only: it
  is never committed, hydrated, rendered, or written to JSONL. Resume therefore
  reconstructs the same media projection from the persisted descriptor and
  Base64 result without creating a duplicate message.
- Assistant tool calls also persist their tool-specific `title_arguments`,
  captured from the created tool call before execution. Replay uses this
  exact projection, so tools such as `write` do not show body content in the
  bubble title. Calls without title arguments persist an empty list.
- **Child-run replay:** `ChildStart` and `ChildResult` persist `call_id`, and
  `ChildStart` also persists `label`. On resume, lifecycle records repair a
  `spawn_subagent` bubble to the child's actual completed state or to a
  terminal error when the child has no result. Replay discovers child
  metadata eagerly, but reads the child's own transcript from the stable
  run-id session path only on first drill-in. That read also restores the
  latest persisted compaction marker at its transcript-relative boundary;
  subsequent drill-ins reuse the loaded entries without duplicating the marker.
  Snapshots without a transcript-relative boundary replay without a marker.
  A `ChildStart`/`ChildResult`
  record without a `call_id` is legacy data: its bubble keeps the persisted
  running/queued text and is not drillable.
- **Interrupted tools:** an unanswered non-`spawn_subagent` tool call is
  synthesized as an in-memory-only `Message::ToolError` with
  `"Tool call did not complete before the session ended."`. This keeps the
  next provider payload protocol-valid and renders a visible terminal Error
  bubble; the synthetic message is never appended to disk.

## Clone contract

The TUI `/session` picker provides explicit same-process Resume and Clone
actions under these same storage contracts. It includes archived and cloned
roots, excludes the active root and child transcripts, and lists the newest
32 eligible roots by persisted activity (UUID tie-breaker). Dates display local
time with a UTC offset; empty titles display `Untitled`.

Preparation validates root eligibility and the source's latest stored agent
type against the current profile before opening where possible, then rechecks
the opened target. The type falls back to `default` only for legacy missing
metadata; neither launch `--agent` nor the old live `/models` choice overrides
picker selection. Fresh configured engines, transcript/model projections,
child-run replay, input history, title handling, and final exit identity all
follow the activated store. The old loop is joined and its obsolete stores
released; hydration never appends historical messages again.

`Esc` cancels publication, not an already-running blocking archive restoration,
normal repair-on-resume, or durable clone creation. If a clone exists but is
not activated, it is retained and its ID explicitly reported as
`Clone created but not activated`; it remains eligible for later resume.
Clone cancellation never resumes/appends to the source as a fallback. All
new work after activation writes exclusively to the clone; source bytes and
shared historical child transcripts remain unchanged.

Root `/compact` uses the same persisted compaction snapshots, without pruning
the transcript. A failed, empty, or non-shrinking manual summary writes no
compaction event and changes no model view; separate request-pressure recovery
is unaffected. See [slash commands](features.md#slash-commands) for controls
and retained-task semantics.

- `--conversation-id <uuid> --clone` starts a **brand-new** session that
  replays the source's logical event stream. Without `--clone`, the same id
  resumes for append as usual; both modes work in the TUI and headless runs.
- The clone gets a fresh `session_id` (its own file name), its own `meta`
  header with `cloned_from` naming the source, `parent_session` unset, and the
  source's `title`, `provider_connection`, `model`, and `agent_type` copied
  over — an explicit `--agent` overrides the type **on the clone only**.
- Every logical event after the source header is copied: messages,
  `model_changed`/`agent_type_changed` selection records, compaction
  snapshots, and child-run lifecycle records. `child_start`/`child_result`
  records are rebased onto the clone's id so a later resume of the clone
  resolves its child runs from its own session; the child transcript files
  themselves are untouched and stay shared read-only history.
- **The source is never modified.** It is read under a nonblocking exclusive
  lock held only for the read, and it is never repaired, rewritten, or
  appended to: the clone run's new messages land only in the clone's file.
- A source that is a **child transcript** (`parent_session` set), a source
  another Ylva process holds open, an unknown id, or a file without a readable
  `meta` header is a hard error that creates no session at all.
- Hydration and TUI replay read the clone's own file, so the copied turns
  appear as an ordinary resumed conversation on the fresh session id.

## Listing sessions

`Store::list_sessions(chats_root, project_key)` returns every session of one
project, most recently used first, with UUID tie-breakers. It reads the full
resolved transcript, takes no lock, and mutates nothing. Archives are decoded
in memory, never restored as a side effect. Invalid headers, representations,
or nonempty activity timestamps produce diagnostics rather than qualifying
silently for age-based maintenance.

`SessionSummary.last_used_at` is derived, not added to the persisted header.
It is the latest top-level `timestamp`, `created_at`, `changed_at`, `completed_at`,
or `compacted_at` found in readable event lines, using the header's `created_at`
as the baseline. Comparison uses RFC 3339 instants, not string order or file
modification time. Out-of-order events and timestamps with offsets are supported.
Legacy messages can omit timestamps or contain empty ones; model-projection
entries have no timestamp. Such entries do not advance last use. Malformed JSON
event lines remain tolerated and are skipped with a logging diagnostic.

Every application start/resume records a timestamped `model_changed` event.
Messages, child lifecycle events, selection changes, and compaction can advance
last use. Listing, title rewrites, compression/decompression, and cloning the
source do not. A clone's own creation time is its baseline, so copied historical
timestamps cannot make the new clone appear older than its creation.

The CLI displays last use and reverses the library's order; included children
are grouped beneath their stored parent. All maintenance `--before` selectors
use the same last-use timestamp, revalidated under the identity lock before
mutation. A target/group that was used again and no longer qualifies is skipped.
See [Session commands](cli-options.md#session-commands).

## Durability

Writes go through a dedicated writer task fed by a channel, so the agent loop
never waits on disk. Every line is written and flushed immediately, but **not**
`fsync`ed: an interactive chat is not worth an `fsync` per turn, and a flushed
line already survives a process crash. The one exception is the `meta` line's
`title`, which is intentionally rewritten once, in place, after it is first
written empty — see "The header is written to disk immediately…" above; the
rewrite itself is a temp-file-plus-`rename()` swap, so a crash mid-rewrite
never corrupts the file (the reader either still sees the old, empty-titled
line or the new one, never a torn mix of both).
