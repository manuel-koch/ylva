# Ylva agent-harness persistence

Reference for what Ylva stores on disk and how to resume, clone and maintain
your sessions.

Ylva writes every conversation to a plain [JSON Lines](https://jsonlines.org)
file as it happens. If Ylva is killed, the lines already written stay intact.
You can read a session file with `cat`, `grep` or `jq`. No Ylva tooling is
needed.

Normal sessions always persist. Startup fails if Ylva cannot find its home
directory, or cannot create or open the chats directory or the session file. Set
`YLVA_HOME` to a writable directory when your operating system home directory is
unavailable.

## On-disk layout

```text
~/.ylva/chats/<project_key>/<session_id>.jsonl
```

| Segment | Meaning |
| --- | --- |
| `~/.ylva/chats` | Root of all stored sessions. `~/.ylva` is the value of `YLVA_HOME` when it is set and non-empty, otherwise `.ylva` in your operating system home directory. |
| `<project_key>` | Groups all sessions started from the same physical directory. See below. |
| `<session_id>` | The session's UUID. `--conversation-id <session_id>` re-attaches it. |

A subagent's transcript is a session file of its own in the same
`<project_key>` directory. Ylva writes it once, when the subagent finishes.

Manual compression replaces `<session_id>.jsonl` with a lossless
`<session_id>.jsonl.gz` archive and a `<session_id>.meta.json` file holding the
header fields. `<session_id>.lock` and `.project.lock` files coordinate writers
and maintenance. They stay in place and hold no transcript data. Listing
sessions ignores temporary files left by an interrupted operation.

On Unix, Ylva creates session and lock files with mode `0600` and the chats and
project directories with mode `0700`, so other local users cannot read them.
Existing files and directories keep their permissions. Transcripts are not a
secure store. Ylva does not guarantee they are free of secrets, so keep secrets
out of conversations.

### Workspace trust store

`~/.ylva/trusted-workspaces.toml` (mode `0600`) records the approvals you give
with `ylva trust`. For each workspace path it maps every approved profile name to
a fingerprint of the workspace files the approval covers. See
[workspace trust](configuration.md#workspace-trust).

### Relocating the Ylva home with `YLVA_HOME`

`YLVA_HOME=/some/dir` makes Ylva read and write `/some/dir` directly, for chats
and profiles, instead of `~/.ylva`. It names the Ylva home itself, like
`CARGO_HOME` or `GOPATH`. Use it for an isolated setup such as a test harness or
a throwaway sandbox. An unset or empty `YLVA_HOME` falls back to `~/.ylva`. A
workspace-local `YLVA_HOME` (for example `$(PWD)` from `.ylva/.envrc`) keeps
chats under `<workspace>/.ylva/chats/`, which the workspace `.gitignore` ignores.
See [configuration](configuration.md).

### `project_key` derivation

`project_key` is the SHA-256 hex digest of the launch directory after Ylva
resolves symlinks and `..`. Paths that lead to the same directory share a key,
for example a symlink and its target, or macOS's `/var` and `/private/var`. Two
different directories never share one.

The key does not use the git root. A plain, non-git directory is a project too.

## File format

Each line is one JSON object with a `type` field. The first line of every file is
the `meta` header. Every later line is an event.

```json
{"type":"meta","version":1,"created_at":"2026-01-08T09:41:07.123Z","schema_version":2,"app_version":"0.7.0","project_key":"7f3c…","provider_connection":"openrouter","model":"gpt-4o","session_id":"6f1e…","title":"Store Layout Summary"}
{"type":"model_changed","version":1,"changed_at":"2026-01-08T09:41:07.123Z","provider_connection":"openrouter","model":"gpt-4o"}
{"type":"message","version":1,"timestamp":"2026-01-08T09:41:07.456Z","message":{"role":"user","content":"explain the store layout","kind":"explicit"}}
{"type":"message","version":1,"timestamp":"2026-01-08T09:41:09.012Z","message":{"role":"assistant","content":"…","status":"complete","tool_calls":[]}}
{"type":"subagent_start","version":1,"created_at":"…","id":"…","parent_session":"6f1e…","agent_type":"explorer","task":"…"}
{"type":"subagent_result","version":1,"completed_at":"…","id":"…","parent_session":"6f1e…","status":"success","result":"…","sources":[],"origin":"agent"}
```

A subagent's own session file also carries `parent_session` in its header:

```json
{"type":"meta","version":1,"created_at":"…","schema_version":2,"app_version":"0.7.0","project_key":"7f3c…","provider_connection":"ollama","model":"llama3","session_id":"9a2d…","title":"…","parent_session":"6f1e…"}
```

### `meta`

| Field | Description |
| --- | --- |
| `version` | Version of this line's own shape. [Schema tolerance](#schema-tolerance) explains how it differs from `schema_version`. |
| `created_at` | Session creation time in UTC, millisecond precision. Resuming never changes it. |
| `schema_version` | Version of the file format. Ylva writes it once when it creates the session and never changes it, even when a newer Ylva resumes the session. It records the format the file started with. See [Schema tolerance](#schema-tolerance). |
| `app_version` | Ylva version that created the session. |
| `project_key` | The [`project_key`](#project_key-derivation) of the launch directory. |
| `provider_connection` / `model` | The provider connection and model chosen at creation. Ylva never updates them. `model_changed` events record later changes. |
| `session_id` | The session's UUID. It matches the file name. |
| `title` | A short title that your configured `utility_engine` generates from the conversation. See [Session titles](#session-titles). |
| `parent_session` | Only in a subagent's own session file. The `session_id` of the session that started it. Absent for root sessions and for sessions that `/clear` starts. |
| `agent_type` | The agent type the session started with. A `/clear` replacement session gets the current type, and a clone copies its source's type. Sessions created before this field existed count as the `default` agent type. `agent_type_changed` events record later selections. Absent while unset. |
| `allowed_tools` | Only in a subagent's own session file. The tools that subagent could use. Absent in older transcripts. |
| `cloned_from` | Only in a session created with `--clone`. The `session_id` of the source. The link goes one way, and the source never records its clones. Absent otherwise. |

Ylva writes the header as soon as it creates the session, with an empty `title`,
so everything recorded afterwards is on disk the moment it is written.

A session where you never sent a message is not kept. Ylva removes its file when
you exit, run `/clear` or switch to another session. After a crash, an empty file
like this can remain.

#### Session titles

After the first non-empty, complete assistant reply in a root session, Ylva asks
the `utility_engine` for a title. The request uses no tools and shows nothing in
the UI. Ylva keeps the first non-empty line of the answer, removes surrounding
quotes, cuts it to 80 characters and stores it in the header. Only the header
line changes. If title generation fails or is interrupted, the title stays empty
and the next process that resumes the session tries again.

For a finished subagent, the title comes from its original task and its final
outcome. That outcome is the `result` of a successful `agent_finish` call, or the
`error` of a failed one. An intended call, a failed tool run or acknowledgement
text does not count. Without an outcome, Ylva uses the subagent's text reply.
Ylva adds no message to the transcript, keeps existing titles, and never
generates titles or contacts a provider while listing sessions.

Apart from the title, resuming never changes the header.

### `message`

One committed conversation message. It is a user turn, a completed or
interrupted assistant turn, a tool result or a tool error. Ylva does not store
live streaming output. It writes a message once, when the agent finishes it, so
the file order is the conversation order.

`timestamp` is that moment in UTC with millisecond precision. It is when you
sent a user message, or when Ylva received the complete assistant message. Stream start and
first-token times are not stored, so you cannot derive response latency from a
transcript. Lines written before this field existed have an empty timestamp.

Ylva does not store the two system prompts that start every conversation. The
running Ylva rebuilds them on every start, and a stored copy would go stale.

### `agent_type_changed`

Ylva writes it when you resume a session with `--agent <type>` and that type
differs from the one the session already uses. Later resumes without the flag
keep the new type. `meta.agent_type` holds only the type from creation, so this
event is the history of every later selection. The last one wins. A clone needs
no such event, because its header already holds its type.

### `model_changed`

Records the provider connection and model that serve the session. Ylva writes it
on every start, for a new session and for a resume, because a different
`--profile` or an edited config can select another model. It writes it again
after every successful `/models` switch. Together these events are the full
history of the models a conversation used, including the first. A subagent
session has its own history, starting with its own model.

The event also records the model settings in effect at that moment. All of them
are optional: `engine_name`, `reasoning_effort`, `temperature`,
`max_output_tokens` and `context_length`. Older sessions lack them. Resuming does
not restore these settings. It starts from the engine your current profile
configures.

### `subagent_start` / `subagent_result`

Ylva writes them to the parent session when a subagent starts and when it
finishes. The result's `origin` is `agent` when the subagent reported its outcome
with `agent_finish`, and `runtime` when Ylva produced the outcome itself. Both
events name the parent in `parent_session`. Ylva saves the subagent's own
transcript separately when it finishes, and that file names its parent too. You
can follow the link from either file.

### `compaction`

Ylva writes it when a background summary replaces older conversation in the
model's view. Ylva only appends these events. A later event that cuts further
into the conversation supersedes an earlier one, and resume uses the newest.
Every summarized message stays in the file. Ylva never shortens the transcript.

| Field | Description |
| --- | --- |
| `compacted_at` | When Ylva applied the compaction, in UTC with millisecond precision. |
| `boundary` | Position of the first message kept verbatim, counted in the model's view including the system prompts. |
| `transcript_boundary` | The same position counted in the stored messages, so it points at a `message` line. The TUI uses it to show the compaction marker. Absent in older sessions, which show no marker. |
| `estimated_tokens_before` / `estimated_tokens_after` | Estimated token count of the conversation sent to the model before and after the compaction. Absent in older sessions. |
| `summary` | The text that stands in for everything before the boundary. On resume the model sees it as one marked user message ahead of the verbatim messages. Ylva never adds it to the transcript. |

A subagent's transcript also lists the compactions it performed, the model
settings it used and its agent type.

### `model_projection`

Ylva writes it whenever the trimmed view it sends to the model changes. It never
replaces the stored messages or the visible conversation. On resume, Ylva uses
the update with the highest `projection_revision`. Cloning copies these events
unchanged.

The `projection` object holds `projection_revision`, `transcript_start`,
`message_overrides` (pairs of message position and shortened message),
`summary_override`, and optional `handoff` and `anchor` messages. Overrides hold
only the shortened text for the model, not the rich display content. On resume,
Ylva checks them against the conversation and ignores invalid data with a log
message. A newer compaction supersedes older overrides and recovery handoffs.
Synthetic summaries and anchors exist only in the model's view. Resuming never
copies them into the transcript.

## Schema tolerance

Readers ignore unknown fields in a line. An unknown event `type` makes only that
line unreadable, not the file. A reader can load the events it knows across Ylva
versions, but that does not guarantee a complete replay after a breaking change.
The per-line `version` lets a reader treat an event type differently by age
without reading the header.

`version` and `schema_version` track different things and can differ within one
file.

- `schema_version` lives only in the `meta` header and describes the whole file.
  Ylva writes it once at creation and never changes it, even when a newer Ylva
  resumes the session.
- `version` is on every event line, `meta` included, except `model_projection`,
  which uses only `projection_revision`. It describes that line's own shape, and
  the Ylva version that writes the line sets it.

A session that an old Ylva created and a newer one resumed can therefore keep its
original `schema_version` while newer lines carry a higher `version`. Ylva bumps
`schema_version` only for a hard break that requires migrating or discarding
data. Ordinary changes to single events use `version`.

The current schema is 2. Delegation events are now `subagent_start` and
`subagent_result`. They replace schema 1's `child_start` and `child_result`.
There is no alias and no automatic migration. The header version is informational
and does not reject a file. Replay skips old delegation events, so an old root
transcript can look readable while it misses its delegation links, outcomes and
drill-in state.

Before you use sessions created before this rename, discard the affected root
sessions, including clones that contain the old events, and their subagent
transcripts, raw or archived. Cascading deletion cannot find links stored under
the old event names. Sessions that never used delegation stay readable. Profiles,
credentials, skills and other configuration need no changes. Ylva never deletes
any of this automatically.

## Locking

While a session is open, Ylva holds an exclusive lock on its
`<session_id>.lock` file. A second Ylva that opens the same session fails at once
with "session … is open in another Ylva process". It does not wait or mix its
writes into the file. The lock is released when the session closes. Ylva keeps
lock files and never deletes them during maintenance. Writing the title does not
release the lock.

Listing sessions takes no lock.

A per-project lock stops cloning and subagent publication from racing with
deletion, so nobody can remove a subagent while another session records a
reference to it. Ylva holds it only during those operations, not for the life of
a conversation. Compression and decompression do not take it. Creating a session
or clone and publishing a subagent reference can wait briefly for a running
deletion.

Other sessions in the project can stay open during maintenance. Ylva refuses only
the selected transcripts that are open, or deletion groups that contain one.
Other targets continue.

## Manual maintenance and archive recovery

See [Session commands](cli-options.md#session-commands) for syntax and
selectors. Ylva has no automatic cleanup, no age or size settings and no
automatic compression. New sessions stay plain JSONL until you archive them.

Compression and decompression report `compressed <uuid>` and
`decompressed <uuid>`. A dry run reports `would compress <uuid>` and
`would uncompress <uuid>`. Neither lists storage files. A session already in the
requested state reports `already compressed <uuid>` or
`already uncompressed <uuid>`, also in a dry run.

Both operations survive interruption. Compression writes the metadata file and
the verified archive first and removes the plain transcript last. Decompression
restores the plain transcript, then removes the archive and the redundant
metadata file. An interruption always leaves a complete copy. Running the same
command again finishes the job.

Every feature that reads sessions understands plain and archived transcripts,
including links between roots and subagents. That covers listing, resume, clone,
titles, agent selection, compaction, projections, attachments and input history.
Reading never decompresses, repairs or migrates files.

- If a plain copy and an archived copy are equivalent, the session is listed once.
- If they differ, Ylva reports a diagnostic and refuses maintenance, resume and
  cloning for that session.
- The metadata file must agree with the file name, the project and the archive
  header. A metadata file alone is not a transcript.
- A corrupt archive or inconsistent metadata is an error.
- A matching metadata file next to a plain transcript with no archive is an
  interrupted compression. You can complete it.

Deleting a root session also deletes its subagent transcripts, unless surviving
sessions or clones still refer to them. Ylva keeps those and reports them.
Deleting a referenced subagent transcript explicitly fails. `cloned_from` is
history only, so deleting a source never deletes its clones. An open root session
blocks deletion of its whole group. Independent groups continue when another
group fails. A partly failed batch exits with the general-error code. See
[exit codes](./cli-options.md#exit-codes). Deletion does not roll back across
files.

Ylva reports each deleted session by UUID as `deleted <uuid>`, subagents
included. A dry run reports `would delete <uuid>`. Neither lists storage files.
Kept subagents and failures stay visible, and a failure on a file names its path.

Maintenance works on Unix only. Other platforms refuse maintenance and refuse to
resume archived sessions that need restoring. Plain sessions work everywhere.

## Resume contract

- Without `--conversation-id`, every start begins a new session, in the TUI and
  headless alike. Ylva never resumes automatically and shows no startup picker.
- `--conversation-id <uuid>` re-attaches that session and appends to it. The
  existing transcript stays. Ylva restores an archived session first.
- An unknown id is a hard error. Ylva never falls back to a new session.
- **Repair on resume.** If the last line of the file is incomplete or unreadable,
  for example after a power loss, Ylva cuts it off before the first new line. The
  file never holds damage in the middle. Only resuming repairs a file. Reading
  never changes one.
- **Finding the id.** On exit, Ylva prints the final session id, its title when
  available and a `ylva --conversation-id <uuid>` command to stderr. You do not
  need to search the chats directory. See
  [Resuming a session](cli-options.md#resuming-a-session).
- **Replay.** Ylva loads the stored messages after the two system prompts that
  the running Ylva rebuilds. Loading never writes messages back, so resuming
  duplicates nothing.
- **TUI input history.** On startup the TUI loads your explicit prompts from
  earlier root sessions of the same project. It takes the newest first until it
  reaches `input_history_limit`, then offers them oldest to newest for recall.
  The lookup skips subagent transcripts and the current session. Ylva replays the
  current session separately. Identical prompts appear once, at their newest
  position. These prompts fill only the input history. They never enter the
  visible conversation or the model's context, so Ylva still starts a new
  session.
- **Tool results.** Tool results store their summary, display sections, duration
  and result type. Tool errors store their display sections and duration. A
  resumed session shows the same tool output as the live one. Sessions written
  before this was added show plainer tool output.
- **Attachments.** A tool result can carry attachments such as images. Ylva
  stores only provider-independent metadata that refers to the Base64 data
  already in the result, so the data is not duplicated. Older sessions have no
  attachments. When a provider accepts media, Ylva can add an extra user message
  to the request. Ylva only sends that message to the provider. It never stores
  or shows it, and resuming rebuilds the same media input from the stored data.
- **Tool call titles.** Ylva stores the arguments a tool shows in its title with
  the call. A tool such as `write` never shows file content in its title after
  resuming.
- **Subagent runs.** `subagent_start` and `subagent_result` store the id of the
  tool call that started the run, and `subagent_start` also stores its label. On
  resume these events bring the subagent's entry in the conversation to its real
  final state, or to an error if the subagent has no result. Ylva reads the
  subagent's own transcript only when you first open it, and restores its latest
  compaction marker there. Opening it again reuses the loaded content. Records
  without a call id are legacy data. Their entry keeps the stored running or
  queued text and cannot be opened.
- **Interrupted tools.** A tool call without a stored answer, other than a
  subagent start, shows as the error "Tool call did not complete before the
  session ended." Ylva sends the same error to the provider, so the next request
  stays valid. It never writes this message to disk.

## Clone contract

The TUI `/session` picker offers Resume and Clone for other sessions, under the
same storage rules. It lists archived and cloned root sessions, leaves out the
active session and subagent transcripts, and shows the 32 most recently used.
Dates show in local time with a UTC offset. Empty titles show as `Untitled`.

Before it switches, Ylva checks that the session is usable. That includes
checking that the agent type last stored for it exists in your current profile.
Ylva checks again once it opens the session. The type falls back to `default`
only for older sessions without one. Neither the launch `--agent` nor an earlier
`/models` choice overrides your picker selection. The new session gets freshly
configured engines. Its transcript, model view, subagent runs, input history,
title and final exit message all follow it. Ylva shuts down and releases the
previous session, and loading never adds historical messages twice.

`esc` cancels the switch. It does not cancel an archive restoration that is
already running, the repair on resume, or a clone that is already being created.
If Ylva created a clone but did not activate it, Ylva keeps it, reports its ID as
`Clone created but not activated`, and you can resume it later. Cancelling a
clone never falls back to resuming the source. After activation, all new work
goes to the clone. The source and the shared subagent transcripts stay unchanged.

`/compact` on a root session uses the same stored compactions and never shortens
the transcript. A failed, empty or non-shrinking summary writes no compaction and
changes nothing in the model's view. Recovery from a too-large request works
independently. See [slash commands](features.md#slash-commands) for the controls
and how an ongoing task is handled.

- `--conversation-id <uuid> --clone` starts a brand-new session that replays the
  source's conversation. Without `--clone`, the same id resumes and appends. Both
  modes work in the TUI and in headless runs.
- The clone gets a new `session_id` and file name, and its own header. The header
  has `cloned_from` naming the source and no `parent_session`. Ylva copies the
  source's `title`, `provider_connection`, `model` and `agent_type`. An explicit
  `--agent` overrides the type on the clone only.
- Ylva copies everything after the source header: messages, `model_changed` and
  `agent_type_changed` events, compactions and subagent run events. It re-points
  the subagent events to the clone's id, so resuming the clone finds its own
  subagent runs. It does not copy the subagent transcript files. They stay shared
  read-only history.
- Ylva never modifies the source. It reads the source under a lock held only for
  the read, and never repairs, rewrites or appends to it. New messages from the
  clone run go to the clone's file.
- Cloning is a hard error and creates no session if the source is a subagent
  transcript, is open in another Ylva process, does not exist or has no readable
  header.
- The copied turns appear as an ordinary resumed conversation under the new
  session id.

## Listing sessions

Listing returns every session of one project, most recently used first, with the
UUID as tie-breaker. It reads the full transcripts, takes no lock and changes
nothing. It reads archives in memory and never restores them. Invalid headers,
file representations or non-empty activity timestamps produce diagnostics. Ylva
never treats such a session as old enough for age-based maintenance.

Ylva calculates the last-use time and does not store it in the header. It is the
latest `timestamp`, `created_at`, `changed_at`, `completed_at` or `compacted_at`
among the readable event lines, starting from the header's `created_at`. Ylva
compares real instants, not text and not file modification time, so out-of-order
events and time zones work. Older messages can have no timestamp or an empty one,
and `model_projection` events have none. These do not advance the last use.
Ylva skips unreadable JSON lines and logs a diagnostic.

Every start or resume records a timestamped `model_changed` event. Messages,
subagent events, selection changes and compactions can advance the last use.
Listing, title updates, compression, decompression and cloning the source cannot.
A clone starts from its own creation time, so copied historical timestamps never
make a new clone look older than it is.

The CLI shows the last use and lists the oldest first. It groups included
subagents beneath their parent. All maintenance `--before` selectors use this
same time and check it again under the session's lock before they change
anything. Ylva skips a session or group that you used again and that no longer
qualifies. See [Session commands](cli-options.md#session-commands).

## Durability

Ylva writes in the background, so the conversation never waits for the disk. It
writes and flushes every line at once but does not sync it to physical storage.
A sync after every turn costs too much for an interactive chat, and a flushed
line survives a crash of Ylva itself. A power loss can still lose recent lines.

The title in the header is the one exception. Ylva replaces it once, after
writing it empty, as [Session titles](#session-titles) describes. The replacement writes
a complete temporary file, syncs it, swaps it into place and syncs the
directory. See [implementation-notes](./implementation-notes.md). A crash in the
middle never corrupts the file. Afterwards you see either the old empty-titled
header or the new one, never a mix. The new title is durable once Ylva reports it
as stored.
