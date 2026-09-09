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
{"type":"meta","v":1,"schema_version":1,"app_version":"0.7.0","created_at":"2026-01-08T09:41:07Z","project_key":"7f3c…","provider_connection":"openrouter","model":"gpt-4o","session_id":"6f1e…","title":"explain the store layout"}
{"type":"model_changed","v":1,"provider_connection":"openrouter","model":"gpt-4o","changed_at":"2026-01-08T09:41:07Z"}
{"type":"message","v":1,"message":{"role":"user","content":"explain the store layout","kind":"explicit"}}
{"type":"message","v":1,"message":{"role":"assistant","content":"…","status":"complete","tool_calls":[]}}
{"type":"child_start","v":1,"id":"…","parent_session":"6f1e…","agent_type":"explorer","task":"…","created_at":"…"}
{"type":"child_result","v":1,"id":"…","parent_session":"6f1e…","status":"success","result":"…","sources":[],"completed_at":"…"}
```

A child-agent's own session file carries `parent_session` in its own `meta`
line too:

```json
{"type":"meta","v":1,"schema_version":1,"app_version":"0.7.0","created_at":"…","project_key":"7f3c…","provider_connection":"ollama","model":"llama3","session_id":"9a2d…","title":"…","parent_session":"6f1e…"}
```

### `meta`

| Field | Description |
| --- | --- |
| `schema_version` | Version marker governing the whole file. |
| `app_version` | Ylva version that created the session. |
| `created_at` | RFC 3339 UTC timestamp, second precision (sorts lexicographically). |
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

### `message`

Wraps one **committed** conversation message: an explicit or implicit user
turn, a completed or interrupted assistant turn, a tool result, or a tool
error. Live streaming deltas are ephemeral UI state and never reach disk; a
message is written once, at the moment the agent loop commits it to its
`Context`, so the on-disk order is the commit order.

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
engine — it never needs to cross-reference the parent's. For compatibility,

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
per-event `v` field lets a reader take an event-type-specific path even without
consulting the header.

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

In Phase 7 a resumed session appends to the existing file but does **not**
replay the earlier turns back into the in-memory conversation — restoring a
transcript into the running UI is Phase 8 scope.

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
