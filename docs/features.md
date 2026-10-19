# Ylva agent-harness features

## General

- Rust-based terminal AI agent, runnable as a CLI or TUI
- Implements agent-harness functionality
- Unknown configuration keys are reported once at startup with file, key path
  and a *did you mean* suggestion: as a notice in the TUI conversation, on
  stderr in CLI mode, and in the log. `--strict-config` makes them fatal.

## Agent Skills

Agent Skills are reusable, task-specific instructions that agents load when
needed. Ylva discovers them from local user and workspace folders.

- Choose which skills each agent type can use.
- Skills provide guidance, not extra file, shell, or subagent permissions.
  Loading a skill does not execute scripts.
- Load-skill bubbles show the name and duration; expand to see the source and
  instructions, including in resumed conversations.
- Restart Ylva to pick up skill changes. Previously loaded instructions remain
  in saved conversations.

See [skill configuration](configuration.md#agent-skills) for setup and access
controls, and [skill commands](cli-options.md#skill-commands) for listing skills
and CLI options.

## Build Info

Use `ylva --version` to print build provenance (`Version`, `Commit`,
`Built`, `Profile`, `Features`), followed by the Ylva home (`YLVA_HOME`, or
`~/.ylva`) and the workspace root discovered from the current directory. Paths
under the operating-system home directory print with a leading `~`; other paths
print in full. An unresolved home or undiscovered workspace prints
`not resolved` or `not discovered`.

## Logging

Logging is opt-in and controlled entirely by CLI flags (there is no logging
configuration file).
See [CLI options](./cli-options.md) for usage of `--log-file`, `--log-level`
and `--log-format` options.

At startup (when logging is enabled), an `INFO`-level `"build info"` event is
logged before anything else, carrying `version`, `commit`, `built`, and
`profile` (the same values as `ylva --version`). It applies to every
invocation, including `session` and `profile` subcommands. TUI and CLI runs
then log a `"starting"` event carrying `app` (`"cli"` or `"tui"`, matching the
mode `main.rs` selected). This makes the exact build identifiable from the log
file alone, without cross-referencing a separate `--version` invocation.

At `DEBUG` level, `ylva session` logs one `"resolved session store"` event
with `cwd`, `chats_root`, and `project_key`. Session listings are per project,
so this event explains an empty `session list` run from an unexpected
directory.

Session-title generation emits structured lifecycle events when logging is
enabled. The main stages are recorded with `stage` values such as
`scheduled`, `request_started`, `response_received`, `normalized`,
`persist_started`, and `persisted`; failures and skipped attempts include a
reason or error stage. These events include the session id, selected utility
engine/provider/model, and character counts, but never the conversation
prompt, assistant reply, or generated title text.

At `DEBUG` level, every received `AgentCommand` emits one `"agent command
received"` event with a stable `command` variant field. Payloads are represented
by lengths or correlation IDs rather than their contents; tool approval/denial
events remain separate outcome events.

Session work is correlated through immutable `session` spans. Request, tool,
compaction, title and writer scopes retain the session that admitted their work;
after `/clear`, new work uses the replacement ID. Subagent spans additionally
identify their run and parent session, including completion and transcript
persistence. No new prompt, credential, or conversation-body fields are logged.

When a logfile is enabled, user-visible provider/stream failures, tool
failures, and fatal startup errors are also emitted through `tracing`. Fatal
startup paths flush the non-blocking logfile before exiting. A provider stream
failure is routed through the agent event loop once, so one failed request
produces one user-facing error even though lower layers may record their own
structured diagnostics.

## CLI mode

Non-interactive, one-shot session: the initial user turn is sent to the
agent, and the assistant response is streamed to stdout as it arrives. The
committed session is persisted incrementally.
CLI prints the identifier of the persisted session on exit via `stderr`.
Argument parsing is `clap`-backed (see [CLI options](./cli-options.md)):
`--help` prints usage to stdout and exits successfully; unknown flags and
missing values are usage errors. Exit codes are a contract for scripts and CI:
each run ends with one documented code (see [exit codes](./cli-options.md#exit-codes)).
Ctrl+C or SIGTERM stops a headless run cooperatively and exits with the interruption code.

Selected automatically when stdout is not a terminal, or forced via `--no-tui`.

When CLI stdout is a color-capable terminal, section headers and tool status
lines use the same semantic colors as their TUI bubbles. Colors are disabled
for non-terminal output, `TERM=dumb`, or `NO_COLOR` in `--color=auto` mode.
Use `--color=no` to force plain output or `--color=yes` to force colors;
these modes affect CLI output only and do not affect the TUI.
Tool-call status blocks are separated from preceding assistant output by a
blank line, while each call's completion status remains adjacent to its
invocation.

Initial user turn resolution: `--prompt <text>`, else piped stdin read to EOF
(only when stdin is not a terminal) — e.g. `echo '…' | ylva`.

Tool approval is host-driven: `read`/`glob` are auto-approved when the
sandbox abstains; `write` and `edit` require explicit user approval by
default; `bash` requires user approval by default but is subject to
structural sandbox rules (backgrounded commands, variable-assignment
prefixes, unquoted glob/brace expansion) and configurable `[tools.bash]`
rules — see "`bash` command rules" in
[configuration](./configuration.md#bash-command-rules). For ordinary path tools,
when no tool-specific rules are configured, a built-in floor applies: a path
outside the workspace root (the launch directory
when no workspace marker exists) — or a dot-prefixed
component anywhere in the resolved absolute path, except for `glob` enumeration,
which only lists paths — requires
user approval instead of being auto-approved or rejected outright. Path
tools can instead use configurable rule lists, including the opt-in
`cross_filesystem` rule that matches paths on a different filesystem (device)
than the sandbox root. A profile-level `[tools.<name>]` or higher-precedence
`[agents.<type>.tools.<name>]` table fully replaces, rather than supplements,
the default floor for that tool — see
[sandbox rule replacement and precedence](./configuration.md#sandbox-tool-approval-rules).
`load_skill` is an exception: without configured rules, it auto-approves eligible
skill instructions instead of applying the ordinary floor — see
[skill loader policy](./configuration.md#agent-skills).
`--auto-approve-write` and `--auto-approve-edit` append catch-all approval rules
after the applicable rule list, so these flags retain final precedence.

Unknown tool names return per-call errors to the model without interrupting
other calls in the batch. The next model request starts only after the batch's
known tools have completed, including any pending user approval.

Tool status lines render as `[tool] <title> …` (e.g. `[tool] Read docs/features.md [limit=32]`);
bracketed props come from the tool's title arguments ranked by schema field
order, so a labeled `bash` call renders `[tool] Bash echo hi [label=Greet the user]`.
The first-ranked argument is the call's payload (`terms`, `command`, `path`)
and is shown inline; both it and every bracketed prop are truncated with a
trailing `…` once they pass their display budget (80 and 40 characters)
instead of being replaced by a length placeholder, so a long query or command
still shows its head and the status line never wraps.

`--auto-approve-spawn` lets a spawned subagent (see
[Subagent delegation](#subagent-delegation)) run to completion before
the one-shot CLI process exits, instead of exiting as soon as the parent
turn finishes. Without this flag, `spawn_subagent` is denied outright in CLI
mode. See [implementation notes](./implementation-notes.md#cli-driver) for
details.

Provider configuration is loaded from the selected profile's optional `.env`
file and the process environment; see [CLI options — Environment
variables](./cli-options.md#environment-variables).

## Session Persistence

Every conversation is appended to a JSON Lines file under
`${YLVA_HOME:-${HOME}/.ylva}/chats/<project_key>/<session_id>.jsonl` as it happens
for TUI and CLI alike. A killed `ylva` process leaves everything already committed
intact.

- Sessions are grouped per project by a hash of the canonicalized launch
  directory, so the same checkout always lands in the same folder.
- `ylva session list` discovers raw and archived sessions, least recently used first.
  `--include-subagents` groups subagents beneath their parents with nested `+`
  markers; timestamps show the latest persisted activity, not creation time.
  Manual `delete`, `compress`, and `decompress` commands accept UUIDs,
  `--before`, or `--all` and support read-only previews. `--before` compares last
  use, preserving recently resumed sessions regardless of their creation age.
  There is no automatic
  maintenance. See [Session commands](cli-options.md#session-commands).
- Every start begins a **fresh** session: Ylva never auto-resumes and shows no
  startup picker.
- `--conversation-id <uuid> --clone` starts a **new** session that replays
  an existing one: the source file is left byte-for-byte unchanged, the clone
  gets its own session id with a `cloned_from` link in its header, and all new
  writes go to the clone. Only root sessions can be cloned, and a source
  another Ylva process holds open fails cleanly without creating anything.
  See the [clone contract](persistence.md#clone-contract).
- `--conversation-id <uuid>` re-attaches an existing session, replays its
  prior transcript into the next headless request and the TUI chat view, and
  appends the new turn; an unknown id is an error rather than a silent fresh
  session. Tool bubbles retain their persisted metadata, and completed or
  interrupted subagent runs remain drillable; a subagent's own transcript is
  loaded lazily on first drill-in. See the [resume contract](persistence.md#resume-contract)
  for compatibility and orphan-recovery details.
- A session is exclusively locked while open, so a second Ylva cannot
  interleave writes or maintain the same identity, even across title replacement
  and archive restoration.
- Lossless gzip archives remain discoverable, readable and cloneable. Explicit
  resume restores raw JSONL under lock. Deletion preserves descendants shared
  with surviving roots/clones. See
  [archive recovery](persistence.md#manual-maintenance-and-archive-recovery).
- Subagent runs record a start/result pair in the parent session and write
  their own transcript alongside it.
- Session titles are concise summaries generated by the configured utility
  engine. Roots use the first explicit user message and first non-empty completed
  assistant reply; tool-call envelopes alone do not count as the reply.
  Subagents can instead use their original task and completed `agent_finish`
  result or error, so tool-only runs can receive generated titles.
  The utility exchange is isolated from the visible conversation and is retried
  on resume if it fails or is interrupted. Existing titles remain unchanged;
  there is no task-label fallback or automatic backfilling.
- On exit, Ylva prints the final active session id, its persisted title when
  available, and a ready-to-paste
  `ylva --conversation-id <uuid>` command to stderr, so resuming never
  requires hunting through the chats directory.
- The root agent type is part of the session's metadata: a resume continues
  with the type the session last stored unless `--agent <type>` says
  otherwise, and an explicit override is persisted as the session's latest
  selection so the next resume keeps it.

See [persistence](./persistence.md) for the on-disk format and the full resume
contract.

## Context management and compaction

A long conversation must keep fitting the active model's context window
without losing anything the user can still read.

- **Summarizer capacity warnings.** CLI and TUI check the selected root and all
  configured agent types at startup, even before any user prompt or subagent
  spawn. Equivalent startup warnings are consolidated into one notice listing
  all affected agent types; different capacity conditions remain separate.
  They warn when a root or subagent's
  soft compaction trigger, including prompt and request overhead, can exceed
  its selected compaction engine's effective context window. `/models` switches
  recheck capacity immediately, even while idle, without repeating an unchanged
  warning on the next prompt. They also warn
  before an actual oversized summary request. Warnings are advisory: attempts
  still run and existing failure recovery is unchanged. Each agent type can
  select its own compaction engine. When neither the profile nor agent type
  explicitly selects one, the summarizer follows that agent's active
  conversation engine. An explicit summarizer retains its named engine's
  configured model and settings in an independent Engine runtime, even when
  the conversation uses the same engine name. See
  [per-agent configuration](./configuration.md#per-agent-configuration).
- **Transcript vs. model view.** The transcript is the durable, ordered
  record of every message and never loses one. The *model view* is what a
  request actually carries: the leading system block, an optional compaction
  summary/recovery handoff, latest task anchor, and retained tail with bounded
  model-only result overrides. Original tool bodies, metadata, and display
  sections remain canonical. The outer
  agent loop is the only writer of both; background summarization only ever
  receives an immutable snapshot of the prefix it summarizes.
- **Token estimates.** The compaction budget estimates the model view and
  active tool definitions using `tiktoken-rs` `cl100k_base`. The footer instead
  applies `cl100k_base` to the serialized JSON Chat Completions request body
  sent by OpenAI-compatible providers, including its roles, JSON structure,
  tools, and provider-specific fields. It counts the JSON body as text. It does
  not reproduce provider-side request processing or the selected model's
  tokenizer, so it is not the serving model's actual input-token count.
  The footer can also show a model-view estimate; see the
  [context-usage readout lifecycle](#tui-architecture) for request, startup,
  model-selection, compaction, and clear updates. The footer shows the
  token count and percentage without an estimate-source label. With
  `YLVA_TOKEN_USAGE_DUMP_PATH` set, Ylva appends a metadata-only JSONL record
  with the same local estimate, a UTC
  timestamp, its session ID when available, and any provider-reported
  prompt/input count received for that request. The provider count reflects
  serving-side accounting when available. Neither estimate is billed usage.
  The compaction estimate and serialized-body estimate can differ because they
  count different representations.
  Providers may omit usage, and interrupted streams retain an unavailable value
  rather than reporting zero. See [CLI options — Debug](./cli-options.md#debug).
- **Automatic compaction.** A completed turn or tool outcome at the soft
  threshold (default `0.7`) schedules one useful completed-prefix summary,
  including the prior summary/handoff as source. An open tool-call group is
  excluded; later admitted tools continue without soft-pressure cancellation.
  Results apply only at a safe boundary. Empty or task-only prefixes do not
  repeatedly schedule futile summaries. Generation/request IDs reject late
  outcomes. The root/subagent-scoped `· Compacting…` indicator remains active
  until application on success and clears on failure, timeout, stop, or clear.
- **Bounded continuation.** Every request is checked against the local safe
  ceiling: window minus 10% reply reserve, 5% safety margin, and 512 estimated
  tokens of overhead. At hard pressure (default `0.9`) or above that ceiling,
  an already-running summary gets only its remaining original timeout; commands
  remain responsive. Empty, failed, growing, or inadequate summaries lead to
  measured lossy recovery, not unbounded waits/retries. Recovery first clips
  older outcomes, removes older complete groups with a real-content handoff,
  clips retained outcomes, then shortens summary/handoff. The latest explicit
  request remains verbatim unless shortening is necessary to fit the ceiling.
  The target (default `0.5`) is best-effort, not permission to erase the task.
  Omission markers count toward the budget; tool IDs and matching outcomes remain
  paired. If immutable instructions/schemas plus a valid task cannot fit, the
  turn stops with an actionable error. A recognized provider context rejection
  can retry once with 25% extra request-local margin, only before any response
  text/tool-call deltas; completed or partial operations are never replayed.
- **Tool batches and results.** Only the first six requested positions are
  eligible, in their original order, across all tools and root/subagent runs.
  Unknown, invalid, denied, or budget-skipped positions are not backfilled.
  Remaining result allowance is shared across unresolved calls after reserved
  error framing; a call needs at least 256 estimated tokens to start, and approval
  rechecks the current allowance. Insufficient allowance can pause only for an
  existing summary within its remaining deadline, otherwise each call closes
  with one explicit not-executed error. The model must resubmit only skipped
  operations in smaller batches; the harness never retries them automatically.
  Skipped rounds still consume the tool-use iteration limit. All large tool
  results, partial errors, and subagent reports receive bounded model projections;
  source/network safety caps remain separate. Binary payloads and descriptors
  stay whole or are omitted together with an explicit textual notice.
- **Notices and diagnostics.** Scheduling, waiting, summary application, budget
  pauses, skipped calls, lossy reduction, and irreducible failure are distinguished
  without implying that clipping was successful summarization. Original content
  remains browsable. Applied summaries/recovery refresh the model-view estimate.
  Metadata-only reduction logs record trigger, request identity, safe boundary,
  stage, before/after estimates, projected/clipped counts, admitted/skipped calls,
  batch cap, remaining allowance, and terminal reason. Local estimates remain
  advisory, not provider-reported usage or billed tokens.
- **Summary generation.** Compaction summarizes earlier conversation to reduce
  model context while retaining the information needed to continue the work.
  Summary guidelines can be customized through
  [`COMPACTION.md`](./configuration.md#compaction-summary-prompt-compactionmd).
  See [engine configuration](./configuration.md#engine-configuration) for
  engine and timeout settings.
- **Scope.** Root and subagent contexts compact independently using the
  settings resolved for their agent type. Subagents wait for pending summaries
  before reporting completion and resume work after compaction changes their
  model view. In the TUI, `/compact` requests manual compaction of the root
  conversation only; see [slash commands](#slash-commands).
- **Persistence and resume.** Each applied snapshot appends a `compaction`
  event (never a rewrite); bounded overrides/handoffs append `model_projection`
  events, selected by revision even at the same boundary. Resume and clone
  restore the model view across rebuilt system blocks without duplicating
  synthetic transcript messages. See the
  [persistence format](./persistence.md#compaction).
- **In the TUI.** The transcript keeps every message and shows a
  `── compaction · N messages summarized ──` divider at the boundary (both
  on replay and when a summary is applied live), and the footer's
  key-hints row right-aligns the latest usage readout (`64.2K (45%)`).
  Compaction refreshes it immediately from the compacted
  model view; the next request replaces it with that serialized request's
  estimate.

## TUI mode

Interactive terminal UI launched automatically when stdout is a terminal
(see [TUI Architecture](#tui-architecture)). When stdout is not a terminal
(piped or redirected), Ylva falls back to the CLI driver.

The TUI window title is `<session title> - Ylva Agent Harness` once the
generated session title has been persisted. Untitled or newly cleared sessions
use `Ylva Agent Harness` until generation completes. The previous terminal
title is restored when the TUI exits.

## Model Provider

Currently the following AI model providers are supported:

- Ollama (OpenAI-compatible chat plus native model discovery endpoints)
- OpenRouter ( via OpenAI compatible endpoints )

Named provider connections are constructed independently, so multiple
connections can use the same implementation (for example, local Ollama and
Ollama Cloud). The conversation footer shows the active named engine and model
when a profile engine name is available; direct unnamed engines show only the
model. `/models` queries each connection once and lists configured named
engine shortcuts before raw `provider_connection/model` catalog entries.
Selecting a shortcut activates that exact engine's current Engine runtime settings,
even when another engine uses the same connection and model. A raw catalog
entry keeps provider-based routing: it changes the active engine for its
connection, or the deterministic representative engine for another connection.
The picker detail previews the provider-connection/model and effective context
window compaction will use for the highlighted selection, without repeating
the active conversation engine and model already shown in the TUI footer. It
uses the form `Compaction uses <provider_connection>/<model>, context: <window>`.
The detail turns orange when a fixed compaction engine would retain a
different effective context window than the highlighted choice, including
raw catalog models from the same provider connection. A different
provider connection or model alone does not indicate a context-window mismatch.
While the parameter popup is open, the detail uses the popup's edited context
length, so it previews the window before `Enter` confirms it; `Esc` restores
the stored window.
Compaction that follows the conversation selection shows the resulting pair
without a mismatch warning. Explicit compaction keeps its configured model and
window across selections, even with the same engine name. Switch notices describe the resulting
compaction-engine selection.

Model discovery also advertises optional reasoning-effort support. Ylva keeps
three provider-neutral states:

- `None`: the provider does not advertise reasoning support.
- `Some([])`: reasoning is supported, but the provider exposes no selectable
  effort alternatives; the provider's own default behavior is used.
- `Some(values)`: reasoning is supported and `values` are the selectable
  effort alternatives.

Ollama models that advertise the `/api/show` `thinking` metadata expose
concrete thinking levels as `Some(values)` (booleans map to `none`/`high`,
level names pass through); models that only advertise the `thinking`
capability keep `Some([])`.

When a provider also advertises a default effort (Ollama's `thinking.default`,
OpenRouter's `reasoning.default_effort`) that is one of the advertised efforts,
the picker marks that effort as `medium (default)` instead of adding a separate
default entry; stepping onto that entry selects the provider default (no effort
is sent). When the default is not among the advertised efforts, a leading
default slot reads `default (medium)`; without a default it reads `default`.

When alternatives are available, the `/models` parameter popup cycles
reasoning effort through the provider default and the advertised values for the
edited model, stopping at both ends rather than wrapping. Advertised values are
sorted weakest to strongest (`none`, `min`/`minimal`, `low`, `medium`/`med`,
`high`, `xhigh`/`extrahigh`, `max`) whatever order the provider returns; names
outside that ladder follow in provider order. Each picker row labels
reasoning as `unavailable`, its default label, or the current value. The parameter popup
shows whether it can be adjusted: `◂`/`▸` mark each direction that can still
move. While an overlay is open, the footer replaces the normal send
hint with the controls for that overlay.
`Enter` activates the exact named-engine shortcut or applies the raw catalog
model and selected effort to subsequent requests. On a named shortcut, the
engine keeps its current Engine runtime model/settings; only a reasoning-effort edit
in the picker is reapplied. The choice preview does not mutate an engine.
The selected effort is live Engine runtime state and is not added to the session's
provider/model metadata. A profile's `reasoning` value is validated against
the catalog once, lazily before the first chat: a value the model does not
advertise is dropped to the provider default and surfaced as a notice above
the first response — a transcript notice in the TUI, a `---[ Notice ]---`
marker in headless output — alongside a `tracing` warning in the log (see
[configuration](./configuration.md#profile-configuration)).

- Every outbound HTTP request (model listing, chat/SSE, and Ollama metadata
  calls) carries a dedicated `User-Agent: ylva/<version>` header so remote
  servers can identify the client.
- Outbound requests are capped at
  `MAX_CONCURRENT_PROVIDER_REQUESTS` concurrent requests per provider
  connection, shared across model discovery, metadata lookups, and chat. JSON
  catalog/detail GET and POST requests retry HTTP `429` and `5xx` responses
  with bounded exponential backoff and honor `Retry-After` delta-seconds or
  HTTP-date values when provided.
  Chat requests use the same retry policy before streaming begins; a provider
  error frame or a stream that closes without `[DONE]` is surfaced as a
  conversation error instead of a successful empty reply.

## Tools to be used by models

The following tools can be used by AI models during a conversation turn.

Each tool call succeeds or fails with a structured result (`ok`/`result`/
`error`, plus optional metadata) reported back to the model. Tool
descriptions sent to the model include their argument schema. See
[implementation notes](./implementation-notes.md#tool-result-wire-protocol)
for the wire format.

The initial tool protocol prompt and every model-facing tool description state
that Ylva accepts at most `MAX_TOOL_CALLS_PER_RESPONSE` (12) distinct tool calls
across all tools in one assistant response. If a response exceeds that limit,
Ylva discards the whole batch before any call executes, adds a concise hidden
harness notice to the next model request, and counts the rejection against the
per-turn `max_tool_iterations` budget. This bounds stream-side accumulation and
the UI work caused by a pathological tool-call batch. The iteration budget is
separate: it limits tool-use rounds over a turn, not calls within one response.
The 12-call accumulation backstop tolerates modest overshoot of the six-position
execution cap without accumulating dozens of skipped outcomes.

Independently, Ylva executes at most `MAX_EXECUTED_TOOL_CALLS_PER_BATCH` (6)
calls of one response, for root and subagent runs alike. Calls at later
requested positions — unknown or invalid calls count too — are not executed;
each gets one explicit not-executed error asking the model to resubmit only
those calls in smaller batches. A skipped round still consumes an iteration.
All tools except `agent_finish` share the remaining request allowance: a call whose share
falls below 256 estimated tokens is skipped with the same kind of error
instead of being started. Its committed output stays intact; a bounded textual
model projection uses its share, and whole binary descriptors/payloads can be
omitted together. Running tools are not cancelled for context pressure.

Path arguments given as absolute paths inside the current working directory
are displayed/executed in cwd-relative form for consistency; this does not
affect sandbox approval checks. A `~`/`~/` prefix or any
`$ENVVAR`/`${ENVVAR}` token in a path argument is expanded before use —
plain string substitution in Rust, never a shell — so a sentinel source such
as `$YLVA_HOME/AGENTS.md` can
be passed verbatim. Only path-like arguments (`path`, and `glob`'s
`pattern`) are expanded; free-form values such as `bash`'s `command` or
`write`'s `content` are passed through verbatim. Expansion happens before
sandbox evaluation and never
bypasses an approval check. See
[implementation notes](./implementation-notes.md#path-like-argument-normalization).

### `read`

Read file content from `path` using `mode`/`offset`/`limit` and the
per-call `encoding` argument. When `mode` is omitted, the read autodetects:
valid UTF-8 text is read by lines (so `offset`/`limit` keep their 0-based
line meaning), a whole binary file recognized as PNG/JPEG/WebP/GIF/PDF is
returned as Base64 with a typed attachment, and any other binary content —
or a ranged read of binary content — fails with a retry hint to use an
explicit `mode: "bytes"`. Set `mode` to `lines`/`chars`/`bytes` to force a
unit; explicit modes are honored as requested and fail when they cannot
apply. In `lines` mode, `offset` is a 0-based line index and `limit` is a
line count. Text-oriented `lines` and `chars` reads require valid UTF-8 and
fail with an error that suggests retrying with `mode: "bytes"` (and
`encoding: "base64"` for binary data) instead of inserting replacement
characters. `bytes` reads with the default `utf8` encoding retain the
existing lossy decoding behavior. `encoding` defaults to `utf8`; set
`encoding: "base64"` together with an explicit `mode: "bytes"` to return
the selected raw bytes exactly as standard padded Base64 with no line
breaks. Byte `offset`/`limit` slicing happens before Base64 encoding.
Base64 output remains available in the model-facing result text. Memory use
follows the selected window rather than the file size: `bytes` seeks to
`offset` and reads at most `limit` bytes, and the `lines`/`chars`/autodetect
paths stream fixed-size chunks while validating UTF-8 to EOF — whole-file
buffering remains only for results returned whole (a `bytes` selection
within the cap and the complete recognized media read that bypasses it).
When the selected decoded bytes are valid UTF-8, the user-facing tool bubble
uses the normal text metadata (`read_bytes` and `read_lines`) and a decoded
content preview; opaque or invalid UTF-8 bytes show only `read_bytes`
metadata and no content section. Output is capped at 262144 raw selected
bytes (256 KiB) (`truncated` field present in result metadata when capped):
content past the cap is discarded silently with no in-text marker and the
content section is labeled `Partial result`, the same vocabulary capped
`bash` and `search` output uses. The cap wins over a larger explicit
`limit`; complete recognized media reads (PNG/JPEG/WebP/GIF/PDF Base64
attachments) are exempt, because a truncated image or PDF is unusable and
`media_type` is only recognized for complete files. An exempt whole file is
still limited to 8388608 bytes (8 MiB): a larger complete media file fails
with an explicit size error instead of being truncated or loaded unbounded.
Per-call `limit` and byte caps bound the returned source output. The common
agent path separately bounds its model-only projection using advisory
`cl100k_base` accounting; canonical media stays complete even when its payload
and descriptors are omitted from the model view together.
The `read file` debug event records full-source `source_size_bytes` and
`source_lines` alongside `raw_selected_bytes`/`raw_selected_lines` after the
tool's byte cap. The later `read result size` event records
`returned_bytes`/`returned_lines` after tool-local caps, plus the
truncation flag. Source line counts are unavailable for binary reads and
partial byte ranges.
Continue a capped read with `offset`/`limit`. Reads auto-approve when the sandbox abstains; a path
outside the workspace root (the launch directory when no workspace marker exists)
or with a dot-prefixed component anywhere in its resolved absolute path requires user
approval instead. Any explicit verdict from a configured `[tools.read]`
rule list — including the opt-in `cross_filesystem` rule (a path on a
different filesystem than the sandbox root) — is honored as-is.

### `write`

Write `content` to `path`, with the per-call `encoding` defaulting
to `utf8`. Set `encoding: "base64"` to decode standard padded Base64 and
write the resulting arbitrary bytes; invalid input fails before any parent
directory or target-file change. Requires user approval before execution
(approval-gated). UTF-8 writes retain their content previews. Base64 writes
whose decoded bytes are valid UTF-8 also show the decoded text preview;
opaque or invalid UTF-8 bytes show only the byte-count confirmation and
metadata. Content payloads are redacted from logs and argument spans. There
is no CLI/profile configuration setting or size limit added by this feature.

### Binary attachments for `read`

Binary `read`/`write` support remains an exact transport and UI-safety layer,
with provider-aware input for explicit Base64 `read` results added on top:

- Complete Base64 byte reads are inspected after slicing with magic-number
  detection. Only PNG, JPEG, WebP, GIF, and PDF signatures become typed
  attachments; partial ranges and unrecognized bytes remain ordinary Base64.
- `ToolResult` stores a provider-neutral image or PDF/file descriptor that
  references the existing Base64 result. It does not own a second payload.
- Model discovery supplies conservative input capabilities. OpenRouter reads
  `architecture.input_modalities`; Ollama maps `/api/show`'s `vision`
  capability to image support only. The active model's capabilities are
  discovered automatically before the first chat request, so recognized
  attachments are projected without requiring a `/models` re-selection.
  Unknown and known text-only models retain text-only behavior.
- Capable Chat Completions requests keep tool responses contiguous, replace a
  supported result's long text with an `[image attached]` or `[file attached]`
  marker, then add a hidden wire-only user message with text before the media
  part. Images use `image_url`; PDFs use a `file` part and are never sent as
  `image_url`.
- Unsupported or unknown modalities keep the complete Base64 result in the
  tool message. Synthetic user messages are not persisted or rendered in the
  TUI/CLI. Request dumps redact Base64 and inline media payloads while retaining
  safe roles, IDs, media types, and filenames.

### `edit`

Patch an existing file in-place. The file must already exist (use `write`
to create new files). Two modes:

- **SearchReplace** — supply `old_string` and `new_string`; replaces the **first**
  occurrence by default. Set `replace_all: true` to replace every occurrence in one
  call (useful when renaming a symbol throughout a file). Returns an error if
  `old_string` is not found.
- **LineRange** — supply `start_line`, `end_line` (1-based, inclusive) and
  `new_string`; replaces those lines. Returns an error if the range is out of
  bounds or `start_line > end_line`.

A unified diff preview is shown before the user approves the call, with a
two-column line-number gutter (old file | new file) so each added, removed, and
context line is annotated with its position in the old and/or new file.
Requires user approval by default.

### `glob`

Search for paths using a glob `pattern`, including wildcards like `*`, `?`, `**`, `[...]`.
Patterns are resolved relative to cwd unless absolute; to search beneath a sub-directory, include
that directory in the pattern (for example, `docs/**/*.md`). There is no separate `path`
argument. Bare `**` matches directories; use `**/*` to match files and directories recursively.
The search root is never a match and is always excluded from returned paths and all
counts, regardless of the pattern or option flags; only descendants are eligible. Hidden files
(dot-prefixed names) are **included by default**, matching `find . -type f` behaviour. Optional
`dot_ignore` (default `false`); set to `true` to exclude any path component that starts with `.`.
Optional `git_ignore` (default `true`) skips paths matched by `.gitignore` rules; it does not
exclude the `.git` directory or other dot-prefixed paths. Set `dot_ignore` to `true` to exclude
them. Set `git_ignore` to `false` to use plain glob matching without gitignore rules. Optional
`only_files` / `only_dirs` filters restrict results to files or directories only (mutually
exclusive; both `true` returns an error). Optional `limit` caps the result count; omit it for
no result-count limit rather than using an arbitrary large value.
Result metadata contains flat `files`, `directories`, and `total` counts, where `total` is the
sum of the other two fields, plus a boolean `truncated`. `truncated` is `false` when all
matching paths are returned and `true` when `limit` omits additional paths; it does not report
an omission count. Optional `only_count` returns no paths and exposes the same counts; the result
text retains the matching total.
Returned paths obey the call's `limit`; source truncation is reported in metadata.
The common agent path separately bounds the model-only body without reducing
canonical paths or structured counts, using advisory `cl100k_base` estimates.
CLI/TUI summaries use a tool-provided display projection: `glob` omits zero-valued `files` and
`directories` and `truncated=false`. It omits `total` when either type count is zero, because
the total is then inferable, except that an empty result displays `total=0`; the complete
metadata remains available to the LLM/API.
Paths inside the workspace root (the launch directory when no workspace marker exists)
are auto-approved; paths outside require user
approval. A configured `cross_filesystem` rule is evaluated against the
pattern's root directory, not against each match.

### `bash`

Execute a shell command or multi-line script via `bash -c` from the
launch directory, which anchors tool execution even when the discovered
configuration workspace root is above it. Use relative paths directly; change
directories only when the command needs a different working directory. See
[sandbox roots and nested launches](configuration.md#sandbox-tool-approval-rules).
Requires explicit human approval before execution. An
optional `label` (3–6 word purpose of the command) describes what the
command will do: the collapsed bubble title and the CLI status line show
it as a `label=…` prop — including while the call is pending approval —
and the expanded bubble body leads with a `Label` section followed by a
`Command` section in every state (then a `Result` section with the
command output once completed — or `Partial result` when the output cap
truncated the capture — and a `Partial result` section after them when
the call fails with partial output). When those
sections carry both arguments the expanded args line is hidden, while
the collapsed title keeps them. An absent or blank `label` is omitted
everywhere, and approval behavior does not depend on it. stdout
and stderr are merged in true arrival order. Output is capped at 1000 whole
output lines or 65536 bytes, whichever is hit first (`truncated` field
present in result metadata when capped). Excess lines are discarded; a line
that alone exceeds the remaining byte budget is kept as a UTF-8-safe prefix
up to the byte cap so oversized first lines still yield non-empty output.
Default timeout: 30 s. On timeout or cancellation the entire
process (including any child processes it spawned) is killed. See
[implementation notes](./implementation-notes.md#bash-tool-process-group-cleanup).
The tool description explicitly tells the model never to use it to sleep,
poll, or otherwise wait for a spawned subagent — the harness delivers each
subagent's completion automatically.

### `search`

Search file contents using a regular-expression `pattern` within a
directory tree (defaults to cwd when `path` is omitted). Respects `.gitignore`.
Binary files are skipped. Results formatted in grep style: `filepath:lineno: content` for matches
and `filepath-lineno- content` for context lines. One structured result section per
matching file. Optional `glob` (e.g. `**/*.rs`, `*.txt`) restricts which files are searched,
matched against each file's path relative to the search root. Optional
`case_sensitive` (default `false`) and `context_lines` (default `0`). Optional
`files_with_matches: true` returns only matched file paths (no line content), like
`grep -l`. Optional `only_matches: true` returns only the matched substrings — one
entry per occurrence, ignoring context lines, like `grep -o`. `files_with_matches` and
`only_matches` are mutually exclusive. Result paths mirror the form of the input: a
relative `path` argument produces relative result paths; an absolute argument produces
absolute result paths. Output is capped at 1000 whole output lines (`truncated` field
present in result metadata when capped). Existing line/byte caps bound source
output; context pressure separately reduces its model-only body without changing
canonical output or metadata, using advisory `cl100k_base` estimates.
Paths inside the workspace root (the launch directory when no workspace marker exists)
are auto-approved unless another rule requires approval, such as `dot_path`
for a dot-prefixed component anywhere in the resolved absolute path; paths outside require
user approval.

### `web_search`

Search the web for `terms` and return ranked results from
the configured search backend (`docs/configuration.md`): `duckduckgo` (the
default — DuckDuckGo's HTML endpoint), `searxng`, which requires a
`[web_search.backends.searxng] base_url` naming the instance to query, or
`brave`, which requires a `[web_search.backends.brave] api_key` holding the
Brave Search API subscription token.
`text` lists each hit as a numbered title with its URL; the structured
result metadata carries one `{title, url, snippet}` entry per hit, with
snippets whitespace-normalized and truncated to 300 characters. Optional
`limit` caps the number of results (schema maximum 20; omit it for the
configured default — `[web_search] result_limit`, 15 unless overridden). A
search with no recognizable results returns an empty result list; a
backend block/challenge page or unparseable response fails with an error
instead of a silently-empty answer. Requires user approval before
execution unless a `[tools.web_search]` `network_request` rule (optionally
scoped to the backend endpoint's `scheme://host` with `target`, see
`docs/configuration.md`) or `--auto-approve-web-search` states a verdict.

### `web_fetch`

Fetch the web content at `url` over HTTP `GET` and return it
as text. Only absolute `http://`/`https://` URLs are accepted; other schemes
fail before any request is made. HTML is converted to markdown as a no-JS
reading: `<script>`/`<style>` payloads (inline JavaScript, JSON config
blobs, CSS) are dropped and `<noscript>` fallback text is kept as visible
content. Text/JSON/XML bodies (`text/*`, `application/json`,
`application/xml`, and
`+json`/`+xml` suffixes) pass through verbatim; the magic-number-sniffed
media types PNG/JPEG/WebP/GIF/PDF are returned as a Base64 result with a
typed attachment exactly like a whole-file `read` — complete response only,
up to 8388608 bytes (8 MiB); a larger response fails with an explicit size
error, never a truncated attachment. Other binary or non-UTF-8 bodies fail
with a clear error naming the content type. Returned text (markdown or
verbatim) is capped at 65536 bytes (64 KiB): longer output is truncated
with `truncated: true` result metadata and a `Partial result` content
section, the same vocabulary capped `read`/`bash`/`search` output uses.
Result metadata carries `url`, `final_url` (after redirects), `status`,
`content_type`, and `media_type` for media results. The done tool bubble
renders these response facts as a wrapped `Response` content section
(`final_url` only when a redirect was followed) instead of truncating
them into the one-line summary, which keeps only the `truncated` flag.
Redirects are followed by the backend under a non-interactive hop policy:
at most 10 hops, `http`/`https` only, and no loopback, private, link-local
or otherwise reserved destination address unless a `network_private_address`
rule permits it (checked on every hop, including
the first; the connection is pinned to the checked addresses). Approval and
host-scoped `network_request` `target` rules apply to the requested URL;
a hop to a different host is refused only by an explicit `deny` rule (see
`docs/configuration.md`). Requires user approval
before execution unless a `[tools.web_fetch]` `network_request` rule or
`--auto-approve-web-fetch` states a verdict.

### `clarify`

Ask the user a question in the TUI and wait for their answer.
Optional `context` adds supporting detail. Optional `choices` supplies up to
5 numbered answers; clicking a choice copies it into the input box,
and `Enter` submits it as the tool result. The TUI renders the question in a
yellow banner above the footer. This tool is not registered in headless CLI
mode, so it never appears in the headless `tools` array.

### `spawn_subagent`

Delegate a self-contained `task` (plus a required `label`
summarising the delegated task for display, and optional `briefing` and
`agent_type`) to a subagent that runs concurrently with the parent. `label`
is shown as the tool-call bubble's primary title text — including while
collapsed. An explicitly chosen `agent_type` qualifies the title instead of
appearing as a bracketed prop — `Spawn Subagent (local-discovery) …` in the
root conversation, `Subagent (local-discovery) …` in the drill-in header —
while the default type is omitted entirely. Only available to agents allowed
to spawn subagents (no nesting —
delegation is one level deep). Requires user approval unless
`--auto-approve-spawn` is set. The call resolves immediately with an
admission event containing the `run_id`, `tool_call_id`, `label`, and
outstanding-run count; this is not the subagent's outcome. A terminal
completion event later carries the same correlation fields plus
`status`, `result`/`error`, `sources`, `outstanding_count`, and `all_done`.
Completion order is not a reliable ordering signal. The profile-level
`max_concurrent_subagents` setting defaults to `4`; a positive value caps
simultaneous subagent runs, while `0` or a negative value means unlimited. Spawns
beyond the cap wait in FIFO order. The cap applies across the delegation tree
(delegation is currently one level deep; subagents cannot spawn further
subagents). Provider request concurrency is a separate limit. Its description
lists every configured agent type and its optional
model-facing `description`, so the parent can choose a suitable type instead
of guessing names.
`--max-concurrent-subagents <N>` overrides the profile value for the current
run; `N <= 0` means unlimited.
The description tells the parent to continue only genuinely independent work;
when none remains, it must end its response immediately without waiting
narration, timing speculation, or continued thinking about pending results.
Ending the response neither cancels subagents nor completes the user's task:
the harness resumes the parent after all outstanding subagents finish.
See
[Subagent delegation](#subagent-delegation).

### `agent_finish`

Only available to subagents (a root agent never sees
it). Reports `status` (`"success"` or `"failure"`), an optional `result` /
`error`, and optional `sources`, then ends the subagent's turn. `status` is
requested as mandatory but tolerated as missing: if a model omits it, it is
inferred as `"failure"` when `error` is set and `"success"` otherwise,
rather than the call being auto-denied for a schema violation. The parent
receives this through the `spawn_subagent` lifecycle event — it never sees
the subagent's own conversation, only the `result`/`error` string, so a subagent
should make its `result` self-contained rather than referencing
"earlier"/"above" content the parent cannot see. The handoff should be
concise and non-redundant, proportional to the task. It must not include
meta-commentary or self-review narration about the subagent's process (e.g.
"Self-Correction/Review:") — only the finished deliverable belongs there.
If a subagent goes idle
without calling `agent_finish`, the parent instead receives a synthesized
result built from the subagent's last assistant message; if the subagent hits
its max-tool-iterations cap first, the parent receives a clear failure
instead, so it isn't mistaken for the subagent's own conclusion. A subagent is
told its tool-use iteration budget up front so it can pace itself. Provider
and stream failures are also reported as clear failures rather than being
synthesized as empty or unstructured results.

## Subagent delegation

One level of agent delegation lets a root agent hand off a self-contained
task to a subagent that runs **concurrently** with the parent — the
parent keeps processing its own messages and tools while the subagent works.

- Up to the profile's `max_concurrent_subagents` limit run at once (default
  `4`); `0` or a negative value removes the subagent-scheduler cap. Additional
  calls wait in FIFO order and start as running subagents finish. Callers still
  correlate results by `run_id`, `tool_call_id`, and `label`, not completion
  order.
- `--max-concurrent-subagents <N>` overrides the profile setting for one run;
  `N <= 0` means unlimited.
- Each subagent's outcome (success, failure, or cancellation) is reported back
  to the parent as a structured hidden harness event. Intermediate events are
  recorded without forcing an acknowledgement turn; once `all_done` is true,
  the parent receives one continuation so it can aggregate the results without
  waiting for a new user message.
- Stopping the parent (`Ctrl+C`/`Esc`) or clearing its context (`/clear`)
  cancels every running and queued subagent, settling each spawn call as cancelled.
- A subagent's own tool calls go through the same approval flow as the
  parent's — a subagent is never auto-denied just for being a subagent. Approval
  prompts are presented one at a time in request order; a subagent waiting behind
  another prompt stays blocked while other subagents continue running.
- Use `--auto-approve-spawn` to skip the human-approval prompt for
  `spawn_subagent` calls (mirrors `--auto-approve-edit` / `--auto-approve-write`).
- Concurrent subagents share a filesystem. They may write the same file at the
  same time; the scheduler does not serialize or merge those writes.
- A subagent spawned after a root `/models` switch uses the root's active
  provider/model when it resolves the same named engine. An explicit engine
  override for that subagent uses its own named engine selection.
- Agent configuration selects a complete named engine; it cannot override
  individual engine properties. Define a specialized named engine when a
  subagent needs different model, temperature, context, reasoning, or connection
  settings.
- A change to a shared engine applies to the next LLM request, not to a
  request that is already streaming. A subagent can therefore use the new
  selection on a later request in the same run.

See [implementation notes](./implementation-notes.md#subagent-delegation-internals)
for the full mechanics.

### Drill-in TUI conversation view

The TUI presents a spawned subagent's own conversation as a first-class,
full-screen view — not just a summary bubble — while the parent keeps
streaming/running tools in the background:

- Double-clicking a `spawn_subagent` tool bubble's title switches to that
  specific subagent's transcript. `Esc` (with an empty draft) navigates back to
  the root conversation.
- Spawning a subagent never switches your view automatically — you stay on
  whatever you were looking at. The view only auto-switches when a tool
  approval or clarify question actually pauses for input, and it always
  switches to wherever that pause lives — the root conversation, the
  pausing subagent's own transcript, or a *different* subagent's transcript
  than the one you're currently drilled into — so an approval is never
  stranded behind an unrelated conversation; it auto-returns afterward only
  if the switch itself was automatic (a manual drill-in is never yanked
  away).
- Overlapping root and subagent tool approvals remain actionable: approving
  or denying one restores the controls and conversation view for another
  outstanding approval. The original input draft stays parked until all
  outstanding approvals are resolved.
- While viewing a subagent's transcript, a header above it shows the original
  `spawn_subagent` bubble (task/label/agent type) so context isn't lost.
  Completed subagent bubbles highlight `status=failure` in red in both the root
  conversation and this drill-in header.
  Typing while merely *watching* a running subagent is a no-op rather than
  silently steering the wrong agent (root vs. subagent) — the footer omits
  `enter send` in that state to make this clear.
- Both the header-to-chat divider and the chat-to-input divider, plus the
  conversation scrollbar (track/thumb/arrows), are tinted to match the
  `spawn_subagent` bubble's own border color while drilled in, instead of
  the root conversation's neutral divider/scrollbar color.
- A subagent's transcript remains browsable after it finishes — nothing is
  discarded when a subagent completes.
- The footer/header spinner keeps animating for as long as the subagent's
  tool call is running, including while the subagent itself is paused
  waiting for its own tool approval.
- Several subagent transcripts can stream independently. Each text and thinking
  fragment is assigned to its originating subagent run.
- If a tool approval is pending for a subagent while you've navigated back to
  the root view, the approval footer is prefixed `"Subagent tool '<name>'
  requests your approval:"` so it's clear the approval belongs to a
  backgrounded subagent, not the root agent.

See [implementation notes](./implementation-notes.md#drill-in-tui-conversation-view-internals)
for the full mechanics.

## TUI Architecture

- **Layout:** a scrollable chat-history area, a variable-height input box
  (capped at 1/3 of the terminal height), a `─` separator line, and a dynamic
  footer whose layout adapts to the available width. Key names in the key-hints row are
  bold, action text is normal, and multiple hints use a ` · ` separator. The
  mode/hint row also shows the
  active `named-engine → selected model-id` engine label for the transcript on
  screen, including an explicit reasoning effort in parentheses and the
  selected subagent while drilled in. The engine label is shortened to fit the row. In the main conversation
  it also shows separate counts for running and queued subagents in a compact
  `Subagents: <running> running,
  <queued> queued` label; finished subagents are excluded, zero-valued
  categories are omitted, and the counts are hidden while viewing a subagent
  transcript. No sidebar.
- **Compaction marker:** when a conversation compacts, the transcript keeps
  every message and a non-message divider (`── compaction · N messages
  summarized ──`) marks where the verbatim tail begins. It is inserted from
  the persisted snapshot on replay — so it survives resume in the same
  position — and added live when a summary is applied mid-session. It is a
  view-only entry: it is never sent to the model and never seeds input
  history. Live markers belong only to the compacted root or subagent
  transcript, regardless of which conversation is currently displayed;
  subagent compaction never adds a marker to the root transcript.
  After resume, a subagent's persisted marker is restored with its transcript
  on first drill-in and cached for subsequent drill-ins.
  See the [resume contract](persistence.md#resume-contract) for how
  the model view and the transcript stay separate.
- **Context-usage readout:** the key-hints row (or tool-approval button row)
  right-aligns the latest available local estimate against the active context window —
  e.g. `64.2K (45%)`. It updates as each request starts, before the provider
  responds, and retains that serialized-request estimate unless a startup,
  model-selection, or compaction update replaces it. Before any turn, a fresh
  or resumed session shows a baseline
  model-view estimate (system prompt, restored messages, and tool schemas),
  so the readout is populated immediately. A `/models` switch re-reports the
  same model-view estimate against the new active window, so the percentage
  follows the window change. When compaction applies a summary or drops context,
  the readout switches to the refreshed model-view estimate, including active
  tool definitions. `/clear` (alias `/reset`) removes the previous session's
  readout; it stays hidden until the new session reports an estimate, without
  an immediate startup baseline. While drilled into a subagent
  transcript, it shows that subagent's latest estimate and context
  window; subagent updates remain isolated from the root and other subagent
  readouts. A finished subagent's last readout remains
  available while its transcript is browsed. The estimate is not billed usage
  and can differ from the compaction budget's model-view estimate; the
  percentage is its fill of the window rounded to a whole percent and clamped
  at `100%`. It follows the dynamic footer layout and is dropped when there is no
  room. Headless output has no footer and ignores the update.
- **Input always active:** the input box is never disabled. `Enter` sends
  unconditionally — while the model is streaming, the message is queued as a
  pending steer (shown dimmed, then committed once the model has seen it),
  and `Ctrl+C`/`Esc` interrupt the current round. Interrupting marks any
  staged steers as cancelled (not sent), retaining the text in input history
  so it can be recovered with `↑`.
- **Tool approval sub-mode:** when a tool awaits approval the footer switches
  to `[ ✓ Approve (Enter) ]  [ ✗ Deny (Esc) ]` (mouse-clickable). With approval
  focus, `Enter` approves when the input is empty; typing a reason and pressing
  `Enter` denies with that reason. `Esc` or `Ctrl+C` denies immediately without
  a reason, even when text is present; neither uses double-press confirmation
  or interrupts the whole conversation. The user's draft is snapshotted when
  approval sub-mode starts and restored after all outstanding approvals resolve.
- **Double-press `Ctrl+C`/`Esc` to quit/cancel:** while pending or streaming,
  with normal root input focused and empty, the first press shows a per-key
  hint (`Esc`: cancel only; `Ctrl+C`: cancel & quit) and a second press within
  2 seconds confirms that key's action. A non-empty busy draft is cleared
  instead. Approval, clarify, and overlay controls take precedence; see
  [keybindings](keybindings.md). While idle, `Ctrl+C` double-press quits and
  `Esc` immediately clears the input. After an interrupt, the TUI waits for the
  agent's terminal idle update before applying the selected quit-or-stay-open
  behavior. An external
  SIGINT or SIGTERM skips the double-press: an idle TUI quits at once, and a
  running turn is stopped before the TUI quits with exit code `130`.
- **Help popup (`F1`):** toggles a centered popup with the full keybinding
  table, the selected profile and its config path, the resolved Ylva
  home, the discovered workspace root, and build-info values. An unavailable
  path is shown explicitly. Paths beneath the operating-system home directory
  use `~` notation when applicable; other paths remain absolute. Mouse-dragging
  across visible popup text copies the selection to the clipboard, including
  text reached after scrolling. The footer switches to popup controls while
  the popup is open.
- **Smart auto-scroll:** the chat follows the newest output by default; the
  mouse wheel and `PageUp`/`PageDown`/`End` scroll the history directly (no
  focus needed). Scrolling up disables auto-scroll; scrolling back down to
  the bottom (or pressing `End`) re-enables it. A completed response respects
  whatever scroll-lock state the user left it in, rather than jumping back to
  the bottom.
- **Wordmark:** while the conversation is empty, a centered ASCII-art "Ylva"
  wordmark is displayed with the version and git commit on one line and the
  selected profile on the next line. Mouse-dragging across those lines
  copies the selected text to the clipboard. Once the first message is sent,
  the wordmark disappears.
- **Input history:** `↑` on the first line loads the previous sent message
  (draft is saved); `↓` restores it; any edit while browsing copies the entry
  to a fresh draft. History includes explicit user messages from prior
  root sessions in the same project, ordered oldest to newest, up to the
  configured `input_history_limit` (default `50`); subagent prompts are
  excluded, and duplicate prompts are collapsed by exact text with the newest
  occurrence retained.
- **Line navigation:** `Fn+Left`/`Fn+Right` move the input cursor to the
  start/end of the current line, wrapping to the previous/next line.
- **Starts empty:** the TUI launches with an empty input box and no auto-sent
  turn. An explicit `--prompt` or piped stdin still auto-sends on startup.
- **Sending state:** after `Enter` (or an auto-sent initial prompt) the footer
  shows `"Sending…"` until the first stream event arrives.
- **Footer distinguishes thinking / responding / tool execution:** while
  streaming, the footer shows `"Thinking…"` when the model is emitting
  extended-thinking/reasoning tokens with no assistant text yet,
  `"Streaming…"` once assistant text is flowing, and `"Running tool…"` once a
  tool is actually executing. It keeps showing `"Running tool…"` while a
  spawned subagent is still working, even once the parent's own turn has
  settled, and shows a subagent-specific status while drilled into a running
  subagent's transcript (e.g. `"Subagent thinking…"`, `"Subagent
  streaming…"`, or `"Subagent done, parent working…"`) so it's never
  ambiguous which agent — parent or subagent — is doing what.
- **Queued subagent bubbles** show a static `≡` glyph instead of the animated
  spinner while waiting for an available scheduler slot under the profile's
  [`max_concurrent_subagents` setting](configuration.md#top-level-profile-fields). The
  internal `run_id` used to correlate subagent lifecycle events is omitted from
  the bubble title in both the root and subagent conversation views, and live
  `running`/`queued` state is communicated by the indicator and footer rather
  than a `status=...` title property.
- **Input as a message:** the input box renders like a sent user message — a
  colored left border, no full frame and no `"Input"` title. Long text
  word-wraps inside the box, and the box grows with its content up to the
  1/3 height cap.
- **Mouse editing:** a left-click in the input box places the cursor at the
  clicked character; a left-drag selects text. The same drag gesture selects
  visible text in the chat bubbles, subagent-view header bubble, and
  slash-command overlay (including the `/models` footer), copying it to the
  clipboard on release.
  `read`/`glob` tool status lines and approval buttons remain mouse-clickable.
- **Shift+Enter / Alt+Enter:** insert a newline through the terminal's
  modified-key encoding. Kitty keyboard protocol terminals (iTerm2, kitty,
  WezTerm) report the modifier directly; terminals that encode Shift+Enter as
  a line feed (such as Zed's terminal) are normalized to the same action.
- **Message dedenting:** submitted messages are automatically dedented —
  common leading whitespace is stripped and leading/trailing blank-only lines
  are removed. This makes pasting indented code snippets or quoted text send
  cleanly.
- **Multiline paste:** the TUI supports pasting multiline text via the
  terminal's bracketed-paste protocol. Pasted text is dedented before
  insertion.
- **Tool bubble titles:** a bubble's title line is the tool's own display
  label (`Tool::title()`) rather than a re-derivation from its name, so
  `web_search` and `web_fetch` name the backend the profile selects — e.g.
  `Web Search (Brave)` — exactly as the CLI status line prints it. A tool
  the registry does not know falls back to its title-cased name. A subtype
  the model picks per call (`Tool::subtype_argument()`, e.g.
  `spawn_subagent`'s `agent_type`) is folded into that label as
  `({value})` — `Spawn Subagent (local-discovery)` — and dropped from the
  bracketed props, so the variant is rendered once.
- **Tool bubble collapse/expand:** clicking a tool bubble's title line or its
  left-border bar toggles whether the bubble's arguments and result are shown
  (expanded, `▼`) or hidden (collapsed, `▶`). Collapsed bubbles display only
  the title line, useful for decluttering long conversations.
- **Home-relative tool paths:** TUI path labels use `~` for the home directory,
  without changing execution or saved data.
- **Expansion feedback:** expanding a tool or thinking bubble first shows
  `Expanding…` on its collapsed title, before rendering the body. This also
  applies to subagent transcripts and their header bubble. The static indicator
  disappears when the expanded content is displayed; cold layout still runs
  synchronously, so the indicator does not animate during that work.
- **Tool duration:** when a tool finishes, its execution time is shown in the
  bubble title. Running `spawn_subagent` bubbles also show a live elapsed timer
  in the root conversation and drill-in header, starting when the subagent
  actually begins execution (not while queued). Running timers display whole
  seconds, truncated rather than rounded, so subsecond redraws do not change
  the readout. Spinner and streamed-output updates keep their existing cadence.
  The timer continues while the subagent waits for approval and is replaced
  by the final duration with millisecond precision on completion.
- **Toast notifications:** transient messages appear in the bottom-right
  corner (e.g., `"Copied to clipboard"`) and auto-dismiss after ~1.5 seconds.
- **Scrollbar:** the chat scrollbar's thumb reaches the very bottom of the
  track at the end of the list (and the top at the start). Left-clicking its
  top/bottom arrow jumps to the start/end; clicking the track jumps to the
  corresponding position in the root or subagent conversation. Jumping to the
  end re-enables auto-scroll; other positions disable it.
- **Large expanded results:** root/subagent transcripts and the subagent header
  reuse unchanged entry layouts and paint only visible rows. Scrolling and
  selection do not repeatedly parse completed results; spinners and elapsed
  titles stay live. Full content, exact Markdown, selection/copy, collapse
  controls, and scroll heights are preserved, including documents exceeding
  the terminal library's narrow scroll-offset range. Cold expansion, changed
  stream content, resize, and view switches still require synchronous layout.
  Optional collapsed layouts share a code-owned memory allowance; active
  layouts are not evicted to satisfy it. No new configuration or keybinding
  is required.
  Selection/copy uses terminal columns and keeps wide Unicode and combining
  graphemes intact.
- **Markdown rendering:** assistant messages, user messages, the clarify
  banner's question/context text, and a user's clarify answer render Markdown
  instead of plain wrapped text. Supported: **bold**/*italic*, `inline code`
  (distinct color), fenced code blocks (dim, verbatim, syntax-highlighted
  when a language tag is given), bullet/numbered lists, headings,
  blockquotes, horizontal rules, and tables. Tables fit the bubble width,
  including by character-wrapping unbroken cell content when necessary.
  Styled Markdown rows use terminal display widths, including for wide
  Unicode glyphs, so content stays inside the bubble. Plain text with no
  Markdown syntax wraps as plain text. Unsupported syntax (links, images,
  HTML) degrades to plain text rather than crashing or dropping content.
  Markdown and syntax highlighting are applied live while a response streams.
- **Selectable syntax theme:** code blocks and file previews highlight with
  either the `dark` or the `light` theme, chosen with the `theme` key of the
  selected profile (see [configuration](./configuration.md#profile-file)).
- **Thinking bubble:** extended-thinking/reasoning output gets its own
  bubble: dim-blue border, Markdown rendering, shown before the assistant's
  reply. Starts expanded; title shows a live elapsed timer while streaming,
  truncated to whole seconds, then the total duration with millisecond
  precision once done. Click the title or left border to
  expand/collapse, same as tool bubbles. Depends on the model having
  extended-thinking/reasoning enabled, not on the provider. Interleaved
  thinking, replies, and tool calls each flush into their own bubble in
  order.

## Engine & Profile Configuration

### Named engine configuration

Backends are described by named engine entries, each specifying the provider,
model, sampling parameters, and connection details. Multiple engines can be defined
and referenced by name, for example a fast local Ollama engine for chat and
a more capable OpenRouter model for heavier tasks.

See [engine configuration](configuration.md#engine-configuration) for fields
and [profile file resolution](configuration.md#profile-file) for discovery,
precedence, and provider fallback behavior.

### Profile selection and resolution

A profile groups engine settings with optional per-agent overrides. Agent types
customize personas, tools, and delegation; their descriptions are advertised in
the `spawn_subagent` tool definition. The `default` agent type is always available.

See [profile file resolution](configuration.md#profile-file) for profile
selection and fallback rules,
[agent-type files](configuration.md#agent-type-files) for supported formats and
source precedence, and
[per-agent configuration](configuration.md#per-agent-configuration) for field
overrides, including the built-in default agent's spawning permission.

### Profile management and shims

`ylva profile` lists, creates, edits, and deletes home or workspace profiles.
Profile shims provide launcher scripts for named profiles.

See [profile commands](cli-options.md#profile-commands) for command behavior,
the [shim reference](cli-options.md#shims) for naming and restrictions, and
[managing profiles and shims](configuration.md#managing-profiles-and-shims)
for scope, environment, and `PATH` setup.

### Layered global persona and initialization

Root and subagent contexts share a global SOUL enriched by trusted `AGENTS.md`
and `CLAUDE.md` instructions. Each agent type can add a persona that extends
this baseline. Instruction content steers the model without overriding
code-enforced tool policy or sandbox constraints.

See [global persona](configuration.md#global-persona-soulmd) for SOUL resolution
and system-prompt composition, and
[trusted instruction files](configuration.md#trusted-instruction-files) for exact
discovery, precedence, framing, source-path handling, limits, and failure rules.

`ylva init` scaffolds home or workspace configuration, and
`ylva profile create <name>` scaffolds a named profile. See
[configuration](configuration.md#profile-file) for generated files and reseeding
behavior.

## Slash commands

Typing `/` as the **first character** of the draft-message input opens a
command overlay above the conversation; it narrows as the command is typed.
Canonical command names appear first, followed by aliases as separate selectable
entries. `/reset` appears alongside `/clear` and invokes the same action;
typing `/re` suggests `/reset`, and `Tab` preserves that spelling.
`↑`/`↓` highlight a command, `Tab` completes the highlighted command, and
`Enter` activates it even when only a partial command has been typed.
The command set is a fixed allowlist — a leading `/` that matches nothing in
it (e.g. `/foo`) remains ordinary chat text when no suggestion is selected
and is sent to the model unchanged, as is any `/` that is not the first
character of the input.

The `/models` picker displays provider-reported context lengths as
`context: <count>` labels using the same compact decimal token counts as the
TUI footer (`2048` appears as `context: 2.0K`; `131_072` appears as
`context: 131.1K`; `1_048_576` appears as `context: 1.0M`). These are token
counts, not byte sizes. Rows of models that
advertise a non-text input modality append a bracketed marker with one
upper-case letter per modality — `[I]` for image, `[F]` for file, `[IF]` for
both; known text-only models and models whose modalities the provider did
not advertise get no marker.
Submitting `/models` (with or without an initial argument, e.g.
`/models deepseek`) fetches the full catalog once and does *not* clear the
input — the draft stays as `/models <filter>` so the picker can be
narrowed by continuing to type. Every keystroke re-derives the filter from
the draft and re-applies it to the already-fetched engine names and
`provider_connection/model` identities without another provider round-trip.
A multi-word filter (e.g.
`/models deepseek v3`) matches when every word appears somewhere in the
engine name, provider connection, or model ID, case-insensitively and in any
order — not the joined phrase as one substring. Engine names match only their
named-engine shortcuts, not raw catalog rows for the same model. Display
metadata such as reasoning effort and context length is not searched.
A filter that matches nothing keeps the panel open with a
"Nothing matched your model filter." message rather than vanishing, so the
user can keep editing it. The detail area previews the compaction engine used
by the highlighted choice without duplicating the conversation engine/model
shown in the TUI footer. It shrinks away when the terminal cannot fit it,
leaving the picker controls usable. The picker's list, including the detail
area, is capped at two thirds of the space above the input; longer lists scroll.
The picker chooses its row layout from the visible models. While every visible
single-line label fits the panel, each model takes one row. As soon as one does
not fit, every visible model takes two rows: an identity row
(`engine → provider/model`, elided to fit) and an indented details row carrying
the context, reasoning and modality markers. Clicking either row highlights that
model.
The model currently serving the conversation is shown by its identity text in
bold green instead of a text suffix.
`Enter` on a highlighted model opens its parameter popup instead of switching.
The popup lists one row per parameter, each a label followed by its value: the
effective context length and, when the model advertises selectable efforts, the
reasoning effort. `↑`/`↓` move between the rows; `←`/`→` adjust the highlighted
value. The context length moves by one eighth of the current window, rounded
down to the fixed token-step multiple, and never exceeds the catalog's window
when one is reported. A `◂` before a value means it can move smaller and a `▸`
after it means it can move greater; a missing arrow marks a bound (for example
the minimum token-step floor or the advertised window). The step unit and floor
remain 32,768 tokens; decimal display formatting does not change the values.
Reasoning effort moves through the
advertised efforts in order (and the provider default's slot, see above), without
wrapping. When the advertised default is `medium`, that effort cannot be chosen
on its own; its slot selects the provider default.
Its edit is session-only and is not written to the profile. In the popup, `Enter`
confirms the model and parameters and switches; `Esc` returns to the model list
and discards the parameter edits. `Esc` in the list cancels and clears the input.

| Command | Effect | Result bubble |
| --- | --- | --- |
| `/models [filter]` | Queries every configured provider connection once, lists named-engine shortcuts before raw `provider_connection/model` catalog entries, and previews summarizer effects. A shortcut activates that exact engine's existing Engine runtime settings; a raw catalog row retains provider-based routing. `filter` narrows choices live without re-querying. `↑`/`↓`, the mouse wheel, and left-click select (left-click only highlights); page keys move by a page; `Enter` opens the parameter popup (context length and reasoning effort) for the highlighted model; `Esc` cancels. | Persisted — the provider connection/model is recorded; engine identity and reasoning effort remain live-only |
| `/tools` | Lists the tools the running agent may call, as resolved for its agent type. | Transient |
| `/clear` (alias `/reset`) | Drops the conversation and continues in a brand-new session file; earlier prompts stay recallable with `↑`. | Transient |
| `/session` | Opens the recent-session picker in either conversation view. Select Resume or Clone, then confirm a root session. | Transient; activation and subsequent work use the selected persisted identity |
| `/compact` | Requests a useful completed-prefix summary in the root conversation, regardless of automatic thresholds. | Transient outcomes; an applied summary is persisted |

`/session` and `/compact` are TUI-only; they do not change headless CLI parsing
or session maintenance commands. The session picker is available only within
a running TUI, not at startup.

`/session` lists recent **root sessions** in the active store's
launch-directory group, excluding the active session and subagent transcripts.
It includes archives, clones, and empty sessions.
Newest persisted activity comes first, with descending UUID as the tie-breaker.
Rows show local date/time with a UTC offset before the sanitized title;
empty titles display `Untitled`. The fixed limit adds no configuration key.
Browsing is allowed while busy, but activation requires root work, queued
steers, queued/running subagents, and running/pending compaction to be quiescent.

The popup title is `Sessions`; styled Resume/Clone buttons highlight the selected
action. Keybinding hints appear only in the TUI footer. The popup shows status
or errors only when relevant, without repeating footer controls.
Each open starts in **Resume**. `Left`/`Right` or clicking the action selects
Resume/Clone; arrows, page keys, and wheel navigate; clicking a row only
highlights it. `Enter` confirms. Loading and `Resuming`/`Cloning` remain
responsive to cancellation and quit; other mutations are blocked during
preparation. `Esc` dismisses and clears the draft without changing the
conversation. Selection errors keep the picker, chosen action, and old view.
Cancelled or superseded results never activate a target.

Activation restores the stored root agent type (`default` for legacy sessions)
and constructs fresh conversation engines from that type's **current profile**
configuration. The profile and workspace stay unchanged; launch `--agent` and
the previous session's live `/models` selection do not override this operation.
Missing types, locked/missing/invalid sources, and preparation failures are
explicit errors, not fresh-session fallbacks. Resume hydrates without
duplicating messages; Clone uses the existing [clone contract](persistence.md#clone-contract).
A clone durably created before cancellation or activation failure is retained
and reported as `Clone created but not activated: <id>`, including after
terminal restoration on quit.

`/compact` refuses explicitly from every subagent view. It immediately
snapshots an eligible completed prefix, even while the root is busy, retaining
the latest explicit task and every subsequent message verbatim. Queued steers
are not yet part of that snapshot. An unchanged/one-task/empty context produces
a no-op notice; an already-pending summary produces `Already compacting` without
queueing another pass or restarting its deadline.
The agent type's resolved compaction engine, prompt, capacity warnings, and
timeout still apply. At a safe boundary, application requires strictly lower
model-view token estimates and intact task/tool-pair boundaries. Failure,
timeout, empty summaries, and `No reduction` leave the view unchanged and
do not themselves trigger lossy fallback. Independent request-pressure recovery
and automatic subagent compaction are unchanged. Original messages and tool
bubbles remain browsable, including after resume, with the persisted marker
and refreshed footer estimate.
