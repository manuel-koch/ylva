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
| `~/.ylva/chats` | Root of all persisted sessions. `~/.ylva` is resolved by `ylva_home::ylva_root()`: the `YLVA_HOME` environment variable, used verbatim as the root when it is set and non-empty, otherwise `<operating-system-home>/.ylva` (`dirs::home_dir()`). |
| `<project_key>` | SHA-256 hex digest grouping every session launched from the same physical directory — see below. |
| `<session_id>` | The session's UUID v4. `--conversation-id <session_id>` re-attaches it. |

A child agent's transcript is a session file of its own in the **same**
`<project_key>` directory, written once when the child finishes.

### Relocating the root with `YLVA_HOME`

Setting `YLVA_HOME=/some/dir` makes Ylva read and write `/some/dir` directly
— chats *and* profiles — instead of `~/.ylva`. `YLVA_HOME` names the root
directly, matching the convention of `CARGO_HOME`/`RUSTUP_HOME`/`GOPATH`/
`XDG_DATA_HOME`: it is *the* Ylva home. The dedicated variable exists so
an isolated setup (a test harness, a throwaway sandbox) can redirect Ylva's own
state. An unset or empty `YLVA_HOME` falls back to
`~/.ylva`.

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
{"type":"meta","version":1,"created_at":"2026-01-08T09:41:07.123Z","schema_version":1,"app_version":"0.7.0","project_key":"7f3c…","provider_connection":"openrouter","model":"gpt-4o","session_id":"6f1e…","title":"explain the store layout"}
{"type":"model_changed","version":1,"changed_at":"2026-01-08T09:41:07.123Z","provider_connection":"openrouter","model":"gpt-4o"}
{"type":"message","version":1,"timestamp":"2026-01-08T09:41:07.456Z","message":{"role":"user","content":"explain the store layout","kind":"explicit"}}
{"type":"message","version":1,"timestamp":"2026-01-08T09:41:09.012Z","message":{"role":"assistant","content":"…","status":"complete","tool_calls":[]}}
{"type":"child_start","version":1,"created_at":"…","id":"…","parent_session":"6f1e…","agent_type":"explorer","task":"…"}
{"type":"child_result","version":1,"completed_at":"…","id":"…","parent_session":"6f1e…","status":"success","result":"…","sources":[]}
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
| `created_at` | RFC 3339 UTC timestamp, millisecond precision (sorts lexicographically). |
| `schema_version` | Version marker governing the whole file. Written **once**, when the session is created, and never rewritten afterwards — not even across a later resume under a newer Ylva build. A session created by an old build and resumed for months by newer ones keeps that original `schema_version` forever; it is not a "current shape" indicator, only a record of what the file's format was when it was created (see [Schema tolerance](#schema-tolerance)). |
| `app_version` | Ylva version that created the session. |
| `project_key` | The [`project_key`](#project_key-derivation) of the launch directory. |
| `provider_connection` / `model` | The provider-connection/model resolved for this session at creation time — mirrors the very first `model_changed` record (see below) so it's readable from the header alone. Never rewritten after creation; a later `/models` switch or a different resolution on a fresh process start is tracked only by `model_changed`. |
| `session_id` | The session's UUID — matches the file name. |
| `title` | Auto-generated from the first explicit user message: whitespace-flattened to one line and truncated to 80 characters. Empty until the first conversation turn completes (the first committed assistant reply). If a turn was interrupted before completing, the title stays empty until the session is next opened (resumed) **and** that run's writer shuts down — which finalizes the title from the already-committed user message even if no new turn ran during that resume. Empty forever for a session that never commits a user turn. |
| `parent_session` | Present **only** on a child-agent's own session file, naming the parent's `session_id` — mirrors the parent-side `child_start`/`child_result` events so the link is discoverable from either file. Omitted entirely (not even `null`) for every top-level session and for `/clear` rollovers, which start a fresh session but are not a child run. |

The header is written to disk **immediately** when the session is created —
with an empty `title` — so every event committed after it is durable the
instant it is written; no event is ever held only in memory waiting on a
title. Once the first conversation turn completes (the first committed
assistant reply), the `title` is filled in by atomically rewriting just that
first line (temp file plus `rename()`, same pattern as a schema-upgrade
rewrite) — every other line is untouched and its byte offsets are stable
across the rewrite's lifetime (the rewrite only ever happens once). If a turn
is killed before the reply arrives, the title stays empty and a later resume
re-derives it from the transcript; the rewrite then happens as soon as either
a new turn completes or that resume's session is closed, whichever comes
first — closing alone is enough once the title source has been re-derived.
`Store::resume` never reconstructs `SessionMeta` from scratch — the only
in-place rewrite of the `meta` line mutates `title` on the already-parsed
struct, leaving `schema_version`, `app_version`, and every other field
byte-for-byte as they were on disk before the rewrite.

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

### `child_start` / `child_result`

Written to the **parent** session when a child agent run begins and when it
finishes. Both carry `parent_session`, so a child's records are always
attributable. The child's own transcript is written separately, once, at
completion, and its own `meta` line carries `parent_session` too (see above) —
so the link is discoverable from either file, not just top-down from the
parent.

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
- `version` lives on **every** line (`meta` included) and describes just that
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
its file for the lifetime of the process. A second Ylva that tries to open the
same session fails immediately with "session … is open in another Ylva
process" instead of blocking or silently interleaving writes. The lock is
released when the session is closed.

Listing sessions takes no lock — it is a pure read.

## Resume contract

- Without `--conversation-id`, every start — TUI and headless alike — begins a
  **fresh** session. Ylva never auto-resumes and shows no startup picker.
- `--conversation-id <uuid>` re-attaches that session for **append**: the
  existing file is never truncated or rewritten.
- An unknown id is a **hard error**, never a silent fresh-session fallback.
- **Repair-on-open:** if the last line of a resumed file is incomplete or
  unparseable (a write cut short by a power loss), it is truncated before the
  first append, so the repaired file never carries mid-file corruption. Only
  resume repairs; reads never mutate.
- **Discoverability:** on exit, Ylva prints the session id and a
  `ylva --conversation-id <uuid>` command to stderr so the id never has to be
  hunted down in the chats directory. See [Resuming a session](cli-options.md#resuming-a-session).
- **Replay:** when a store is attached, `Context::set_store` reads the
  committed messages and `Context::hydrate` appends them after the two system
  prompts rebuilt by the current binary. Hydration bypasses `Context::commit`,
  so reading a session never writes its messages back or duplicates them on a
  later flush. The rule is unconditional for fresh and resumed sessions; a
  new file simply has no messages to hydrate.
- **Full-fidelity tool bubbles:** `Message::ToolResult` persists `summary`,
  `content_sections`, `duration`, and specialized result-section identifiers;
  `Message::ToolError` persists `content_sections` in addition to its
  duration. These additive fields use `#[serde(default)]`, so files written
  before replay support deserialize and show silently degraded (blanker) tool
  bubbles.
- Assistant tool calls also persist their tool-specific `title_arguments`,
  captured from the created tool call before execution. Replay uses this
  exact projection, so tools such as `write` do not show body content in the
  bubble title. Calls without title arguments persist an empty list.
- **Child-run replay:** `ChildStart` and `ChildResult` persist `call_id`, and
  `ChildStart` also persists `label`. On resume, lifecycle records repair a
  `spawn_subagent` bubble to the child's actual completed state or to a
  terminal error when the child has no result. Replay discovers child
  metadata eagerly, but reads the child's own transcript from the stable
  run-id session path only on first drill-in. A `ChildStart`/`ChildResult`
  record without a `call_id` is legacy data: its bubble keeps the persisted
  running/queued text and is not drillable.
- **Interrupted tools:** an unanswered non-`spawn_subagent` tool call is
  synthesized as an in-memory-only `Message::ToolError` with
  `"Tool call did not complete before the session ended."`. This keeps the
  next provider payload protocol-valid and renders a visible terminal Error
  bubble; the synthetic message is never appended to disk.

## Listing sessions

`Store::list_sessions(chats_root, project_key)` returns every session of one
project, newest first. It parses only each file's first (`meta`) line — never
the transcript — takes no lock, and mutates nothing: a file whose header does
not parse is skipped with a warning rather than failing the whole listing.

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
