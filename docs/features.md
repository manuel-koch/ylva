# Ylva agent-harness features

## General

- Rust-based terminal AI agent, runnable as a CLI or TUI
- Implements agent-harness functionality

## Build Info

Use `ylva --version` to print build provenance (`Version`, `Commit`,
`Built`, `Profile`).

## Logging

Logging is opt-in and controlled entirely by CLI flags (there is no logging
configuration file).
See [CLI options](./cli-options.md) for usage of `--log-file`, `--log-level`
and `--log-format` options.

At startup (when logging is enabled), two `INFO`-level events are logged
before anything else: a `"build info"` event carrying `version`, `commit`,
`built`, and `profile` (the same values as `ylva --version`), and a
`"starting"` event carrying `app` (`"cli"` or `"tui"`, matching the mode
`main.rs` selected). This makes the running app and exact build identifiable
from the log file alone, without cross-referencing a separate `--version`
invocation.

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

When a logfile is enabled, user-visible provider/stream failures, tool
failures, and fatal startup errors are also emitted through `tracing`. Fatal
startup paths flush the non-blocking logfile before exiting. A provider stream
failure is routed through the agent event loop once, so one failed request
produces one user-facing error even though lower layers may record their own
structured diagnostics.

## CLI Runtime

Non-interactive, one-shot session: the initial user turn is sent to the
agent, and the assistant response is streamed to stdout as it arrives. The
committed session is persisted incrementally.
CLI prints the identifier of the persisted session on exit via `stderr`.

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

Tool approval is host-driven: `read`/`glob` are auto-approved immediately;
`write` and `edit` require explicit user approval by default; `bash`
requires user approval by default but is subject to structural sandbox
rules (backgrounded commands, variable-assignment prefixes, unquoted glob/
brace expansion) and configurable `[tool.bash]` rules — see "`bash` command
rules" in [configuration](./configuration.md#bash-command-rules).

Tool status lines render as `[tool] <title> …` (e.g. `[tool] Read docs/features.md [limit=32]`)

`--auto-approve-spawn` lets a spawned child agent (see
[Child-agent delegation](#child-agent-delegation)) run to completion before
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
- Every start begins a **fresh** session: Ylva never auto-resumes and shows no
  startup picker.
- `--conversation-id <uuid>` re-attaches an existing session, replays its
  prior transcript into the next headless request and the TUI chat view, and
  appends the new turn; an unknown id is an error rather than a silent fresh
  session. Tool bubbles retain their persisted metadata, and completed or
  interrupted subagent runs remain drillable; a child's own transcript is
  loaded lazily on first drill-in. See the [resume contract](persistence.md#resume-contract)
  for compatibility and orphan-recovery details.
- A session is exclusively locked while open, so a second Ylva cannot
  interleave writes into the same file.
- Child-agent runs record a start/result pair in the parent session and write
  their own transcript alongside it.
- Session titles are concise summaries generated by the configured utility
  engine from the first explicit user message and first non-empty completed
  assistant reply. Assistant messages that only contain tool calls do not
  count as the reply. The utility exchange is isolated from the visible
  conversation and is retried on resume if it fails or is interrupted; child
  transcripts use the same title flow.
- On exit, Ylva prints the session id and a ready-to-paste
  `ylva --conversation-id <uuid>` command to stderr, so resuming never
  requires hunting through the chats directory.

See [persistence](./persistence.md) for the on-disk format and the full resume
contract.

## TUI Runtime

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
model. `/models` queries each connection once and lists
`provider_connection/model` entries so a selection can change both connection
and model.

Model discovery also advertises optional reasoning-effort support. Ylva keeps
three provider-neutral states:

- `None`: the provider does not advertise reasoning support.
- `Some([])`: reasoning is supported, but the provider exposes no selectable
  effort alternatives; the provider's own default behavior is used.
- `Some(values)`: reasoning is supported and `values` are the selectable
  effort alternatives.

When alternatives are available, `←`/`→` in the `/models` picker cycles
between the provider default and the advertised values for the highlighted
model. Each picker row labels reasoning as `unavailable`, `default`, or the
current value followed by `⚙` when it can be adjusted, so models that do not
respond to `←`/`→` are distinguishable. While an overlay is open, the footer
replaces the normal send hint with the controls for that overlay.
`Enter` applies both the model and the selected effort to subsequent requests.
The selected effort is runtime state and is not added to the session's
provider/model metadata.

## Tools to be used by models

The following tools can be used by AI models during a conversation turn.

Each tool call succeeds or fails with a structured result (`ok`/`result`/
`error`, plus optional metadata) reported back to the model. Tool
descriptions sent to the model include their argument schema. See
[implementation notes](./implementation-notes.md#tool-result-wire-protocol)
for the wire format.

Path arguments given as absolute paths inside the current working directory
are displayed/executed in cwd-relative form for consistency; this does not
affect sandbox approval checks. See
[implementation notes](./implementation-notes.md#path-like-argument-normalization).

**WIP:** `read` and `write` currently only support UTF-8 code points via lossy decode.

- **read**: Read file content from given `path` using `mode`/`offset`/`limit`.
  Mode can be `bytes`/`chars`/`lines` and defaults to `lines` for every call.
  In `lines` mode, `offset` is a 0-based line index and `limit` is a line
  count. Use `chars` for character counts or set `bytes` explicitly for byte
  ranges and binary data. Offset and limit are 0-based within the selected mode.
- **write**: Write `content` to file at given `path`. Requires user approval before
  execution (approval-gated). Use to create new files or overwrite existing ones.
- **edit**: Patch an existing file in-place. The file must already exist (use `write`
  to create new files). Two modes:
  - **SearchReplace** — supply `old_string` and `new_string`; replaces the **first**
    occurrence by default. Set `replace_all: true` to replace every occurrence in one
    call (useful when renaming a symbol throughout a file). Returns an error if
    `old_string` is not found.
  - **LineRange** — supply `start_line`, `end_line` (1-based, inclusive) and
    `new_string`; replaces those lines. Returns an error if the range is out of
    bounds or `start_line > end_line`.
  A unified diff preview is shown before the user approves the call, with a
  two-column line-number gutter (old file | new file) so each added, removed,
  and context line is annotated with its position in the old and/or new file.
  Requires user approval by default.
- **glob**: Search for paths using a glob `pattern`, including wildcards like `*`, `?`, `**`, `[...]`.
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
  CLI/TUI summaries use a tool-provided display projection: `glob` omits zero-valued `files` and
  `directories` and `truncated=false`. It omits `total` when either type count is zero, because
  the total is then inferable, except that an empty result displays `total=0`; the complete
  metadata remains available to the LLM/API.
  Paths inside the workspace are auto-approved; paths outside are denied by the sandbox.
- **bash**: Execute a shell command or multi-line script via `bash -c` from the
  current working directory, which is the workspace root in normal runtime.
  Use relative paths directly; do not prefix commands with a redundant `cd` to
  the workspace root. Requires explicit human approval before execution. stdout
  and stderr are merged in true arrival order. Output is capped at 1000 whole output
  lines (`truncated` field present in result metadata when
  capped). Default timeout: 30 s. On timeout or cancellation the entire
  process (including any child processes it spawned) is killed. See
  [implementation notes](./implementation-notes.md#bash-tool-process-group-cleanup).
  The tool description explicitly tells the model never to use it to sleep,
  poll, or otherwise wait for a spawned subagent — the harness delivers each
  subagent's completion automatically.
- **search**: Search file contents using a regular-expression `pattern` within a
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
  present in result metadata when capped). Paths inside the
  workspace are auto-approved; paths outside are auto-denied.
- **clarify**: Ask the user a question in the TUI and wait for their answer.
  Optional `context` adds supporting detail. Optional `choices` supplies up to
  5 numbered answers; clicking a choice copies it into the input box,
  and `Enter` submits it as the tool result. The TUI renders the question in a
  yellow banner above the footer. This tool is not registered in headless CLI
  mode, so it never appears in the headless `tools` array.
- **spawn_subagent**: Delegate a self-contained `task` (plus a required `label`
  summarising the delegated task for display, and optional `briefing` and
  `agent_type`) to a child agent that runs concurrently with the parent. `label`
  is shown as the tool-call bubble's primary title text — including while
  collapsed. Only available to agents allowed to spawn children (no nesting —
  delegation is one level deep). Requires user approval unless
  `--auto-approve-spawn` is set. The call resolves immediately with an
  admission event containing the `run_id`, `tool_call_id`, `label`, and
  outstanding-run count; this is not the child's outcome. A terminal
  completion event later carries the same correlation fields plus
  `status`, `result`/`error`, `sources`, `outstanding_count`, and `all_done`.
  Completion order is not a reliable ordering signal. The current scheduler
  runs at most one child at a time and queues further spawns, but that is an
  implementation constraint rather than part of the model-facing contract. Its
  description lists every configured agent type and its optional
  model-facing `description`, so the parent can choose a suitable type instead
  of guessing names.
  See
  [Child-agent delegation](#child-agent-delegation).
- **agent_finish**: Only available to child agents (a root agent never sees
  it). Reports `status` (`"success"` or `"failure"`), an optional `result` /
  `error`, and optional `sources`, then ends the child's turn. The parent
  receives this through the `spawn_subagent` lifecycle event — it never sees
  the child's own conversation, only the `result`/`error` string, so a child
  should make its `result` self-contained rather than referencing
  "earlier"/"above" content the parent cannot see, and must not fold in
  meta-commentary or self-review narration about its own process (e.g.
  "Self-Correction/Review:") — only the finished deliverable belongs there.
  If a child goes idle
  without calling `agent_finish`, the parent instead receives a synthesized
  result built from the child's last assistant message; if the child hits
  its max-tool-iterations cap first, the parent receives a clear failure
  instead, so it isn't mistaken for the child's own conclusion. A child is
  told its tool-use iteration budget up front so it can pace itself. Provider
  and stream failures are also reported as clear failures rather than being
  synthesized as empty or unstructured results.

## Child-agent delegation

One level of agent delegation lets a root agent hand off a self-contained
task to a child agent that runs **concurrently** with the parent — the
parent keeps processing its own messages and tools while the child works.

- The current scheduler runs at most one child at a time; further
  `spawn_subagent` calls queue and start automatically as each preceding child
  finishes. This is not a model-facing ordering guarantee, so callers correlate
  results by `run_id`, `tool_call_id`, and `label`.
- Each child's outcome (success, failure, or cancellation) is reported back
  to the parent as a structured hidden harness event. Intermediate events are
  recorded without forcing an acknowledgement turn; once `all_done` is true,
  the parent receives one continuation so it can aggregate the results without
  waiting for a new user message.
- Stopping the parent (`Ctrl+C`/`Esc`) also cancels any running/queued
  children.
- A child's own tool calls go through the same approval flow as the
  parent's — a child is never auto-denied just for being a child.
- Use `--auto-approve-spawn` to skip the human-approval prompt for
  `spawn_subagent` calls (mirrors `--auto-approve-edit` / `--auto-approve-write`).
- A child spawned after a root `/models` switch uses the root's active
  provider/model when it resolves the same named engine. An explicit engine
  override for that child uses its own named engine selection.
- Agent configuration selects a complete named engine; it cannot override
  individual engine properties. Define a specialized named engine when a
  child needs different model, temperature, context, reasoning, or connection
  settings.
- A change to a shared engine applies to the next LLM request, not to a
  request that is already streaming. A child can therefore use the new
  selection on a later request in the same run.

See [implementation notes](./implementation-notes.md#child-agent-delegation-internals)
for the full mechanics.

### Drill-in TUI conversation view

The TUI presents a spawned child's own conversation as a first-class,
full-screen view — not just a summary bubble — while the parent keeps
streaming/running tools in the background:

- Double-clicking a `spawn_subagent` tool bubble's title switches to that
  specific child's transcript. `Esc` (with an empty draft) navigates back to
  the root conversation.
- Spawning a child never switches your view automatically — you stay on
  whatever you were looking at. The view only auto-switches to a child when
  that specific child pauses for its own tool approval or clarify question,
  so you don't miss an interactive prompt from a backgrounded child; it
  auto-returns afterward only if the switch itself was automatic (a manual
  drill-in is never yanked away).
- While viewing a child's transcript, a header above it shows the original
  `spawn_subagent` bubble (task/label/agent type) so context isn't lost.
  Completed child bubbles highlight `status=failure` in red in both the root
  conversation and this drill-in header.
  Typing while merely *watching* a running child is a no-op rather than
  silently steering the wrong agent (root vs. child) — the footer omits
  `enter send` in that state to make this clear.
- Both the header-to-chat divider and the chat-to-input divider, plus the
  conversation scrollbar (track/thumb/arrows), are tinted to match the
  `spawn_subagent` bubble's own border color while drilled in, instead of
  the root conversation's neutral divider/scrollbar color.
- A child's transcript remains browsable after it finishes — nothing is
  discarded when a subagent completes.
- The footer/header spinner keeps animating for as long as the subagent's
  tool call is running, including while the subagent itself is paused
  waiting for its own tool approval.
- If a tool approval is pending for a child while you've navigated back to
  the root view, the approval footer is prefixed `"Subagent tool '<name>'
  requests your approval:"` so it's clear the approval belongs to a
  backgrounded child, not the root agent.

See [implementation notes](./implementation-notes.md#drill-in-tui-conversation-view-internals)
for the full mechanics.

## TUI Architecture

The TUI is a rendering layer on top of the headless agent loop; the agent
architecture itself is unchanged. See
[implementation notes](./implementation-notes.md#tui-module-layout) for the
module structure (model/presenter/view/driver split).

- **Layout:** a scrollable chat-history area, a variable-height input box
  (capped at 1/3 of the terminal height), a `─` separator line, and a 2-row
  footer (mode/hint row and key-hints row). Key names in the key-hints row are
  bold, action text is normal, and multiple hints use a ` · ` separator. The
  mode/hint row also shows the
  active `named-engine → selected model-id` identity for the transcript on
  screen, including an explicit reasoning effort in parentheses and the
  selected child while drilled in. In the main conversation it also shows separate counts
  for running and queued subagents in a compact `Subagents: <running> running,
  <queued> queued` label; finished subagents are excluded, zero-valued
  categories are omitted, and the counts are hidden while viewing a child
  transcript. No sidebar.
- **Input always active:** the input box is never disabled. `Enter` sends
  unconditionally — while the model is streaming, the message is queued as a
  pending steer (shown dimmed, then committed once the model has seen it),
  and `Ctrl+C`/`Esc` interrupt the current round. Interrupting marks any
  staged steers as cancelled (not sent), retaining the text in input history
  so it can be recovered with `↑`.
- **Tool approval sub-mode:** when a tool awaits approval the footer switches
  to `[ ✓ Approve (Enter) ]  [ ✗ Deny (Esc) ]` (mouse-clickable). `Enter`
  approves, `Esc` denies immediately; `Ctrl+C` still uses the double-press
  confirm. The input field is available to type a reason for denial. The
  user's draft is snapshotted when approval sub-mode starts and restored
  automatically after the tool is approved or denied.
- **Double-press `Ctrl+C`/`Esc` to quit/cancel:** while streaming/tool-pending
  the first press shows a per-key hint (`Esc`: cancel only; `Ctrl+C`: cancel
  & quit) and a second press within 2 seconds confirms that key's action;
  while idle, `Ctrl+C` double-press quits and `Esc` immediately clears the
  input. After an interrupt, the TUI waits for the agent's terminal idle
  update before applying the selected quit-or-stay-open behavior. OS SIGINT
  stops immediately (no double-press).
- **Help popup (`F1`):** toggles a centered popup with the full keybinding
  table, the selected runtime profile and its config path, the resolved Ylva
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
  selected runtime profile on the next line. Mouse-dragging across those lines
  copies the selected text to the clipboard. Once the first message is sent,
  the wordmark disappears.
- **Input history:** `↑` on the first line loads the previous sent message
  (draft is saved); `↓` restores it; any edit while browsing copies the entry
  to a fresh draft. History includes explicit user messages from prior
  top-level sessions in the same project, ordered oldest to newest, up to the
  configured `input_history_limit` (default `50`); child-agent prompts are
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
  child's transcript (e.g. `"Subagent thinking…"`, `"Subagent
  streaming…"`, or `"Subagent done, parent working…"`) so it's never
  ambiguous which agent — parent or child — is doing what.
- **Queued subagent bubbles** show a static `≡` glyph instead of the animated
  spinner while waiting for the one running-child slot to free up. The
  internal `run_id` used to correlate child lifecycle events is omitted from
  the bubble title in both the root and child conversation views, and live
  `running`/`queued` state is communicated by the indicator and footer rather
  than a `status=...` title property.
- **Input as a message:** the input box renders like a sent user message — a
  colored left border, no full frame and no `"Input"` title. Long text
  word-wraps inside the box, and the box grows with its content up to the
  1/3 height cap.
- **Mouse editing:** a left-click in the input box places the cursor at the
  clicked character; a left-drag selects text. The same drag gesture selects
  visible text in the chat bubbles, child-view header bubble, and
  slash-command overlay, copying it to the clipboard on release.
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
- **Tool bubble collapse/expand:** clicking a tool bubble's title line or its
  left-border bar toggles whether the bubble's arguments and result are shown
  (expanded, `▼`) or hidden (collapsed, `▶`). Collapsed bubbles display only
  the title line, useful for decluttering long conversations.
- **Tool duration:** when a tool finishes, its execution time is shown in the
  bubble title.
- **Toast notifications:** transient messages appear in the bottom-right
  corner (e.g., `"Copied to clipboard"`) and auto-dismiss after ~1.5 seconds.
- **Scrollbar:** the chat scrollbar's thumb reaches the very bottom of the
  track at the end of the list (and the top at the start).
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
  runtime profile (see [configuration](./configuration.md#profile-file)).
- **Thinking bubble:** extended-thinking/reasoning output gets its own
  bubble: dim-blue border, Markdown rendering, shown before the assistant's
  reply. Starts expanded; title shows a live elapsed timer while streaming,
  then the total duration once done. Click the title or left border to
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

Engines are read from the resolved profile file:
`<workspace-root>/.ylva/profiles/<name>/config.toml`, then
`~/.ylva/profiles/<name>/config.toml`, then the code-owned defaults for the
implicit `default` profile. The `--profile <name>` value selects the profile
name. With no disk file for the implicit default profile, the code defaults
leave provider discovery to the environment variables as before.

See [configuration](./configuration.md) for all available fields.

### Runtime profiles

A profile groups a default engine with optional per-agent overrides. Each agent
type may also define a model-facing `description`; root agents that can spawn
children receive all configured agent types and descriptions in the
`spawn_subagent` tool definition.

Profiles use the project/home/built-in resolution chain described in
[configuration](./configuration.md#profile-file) for the supported keys.
The `default` agent type is always available; when `[agents.default]` is
omitted, it uses the built-in root configuration with child spawning enabled.
Defining other agent types, such as `local-discovery`, does not disable the
default agent. If `[agents.default]` is defined, its settings replace the
built-in defaults.

Agent types can also be authored as standalone `.md` or `.toml` files in the
global `~/.ylva/agents/`, selected profile `agents/`, or project
`.ylva/agents/` directories. Project files shadow profile files, and profile
files shadow global files for the same type; file definitions are replaced
whole before inline profile fields are applied. A Markdown file uses strict
`+++` TOML or `---` YAML frontmatter followed by its system-prompt body.
Inline `persona` content always precedes that body.

### Layered global persona and initialization

The top-level conversation's global persona comes from `SOUL.md`, resolved
from the project `.ylva/`, home `~/.ylva/`, and the code-owned built-in value
in that order. The resolved SOUL is inherited exactly once by both root and
child contexts. Agent-type persona and file body are then appended, followed
by the tool protocol; a child additionally receives its task briefing and
runtime-budget constraints.

`ylva init` writes `SOUL.md`, `profiles/default/config.toml`, and an annotated
`profiles/default/.env` starter; it does not seed agent-type files.
`ylva init --workspace` seeds the default profile config and `.env` starters
plus `.gitignore` in the workspace. Both support write-if-missing and
`--force` reseeding. Richer agent-type and profile examples are documented
rather than shipped as repository templates.

## Slash commands

Typing `/` as the **first character** of the draft-message input opens a
command overlay above the conversation; it narrows as the command is typed.
`↑`/`↓` highlight a command, `Tab` completes the highlighted command, and
`Enter` activates it even when only a partial command has been typed.
The command set is a fixed allowlist — a leading `/` that matches nothing in
it (e.g. `/foo`) remains ordinary chat text when no suggestion is selected
and is sent to the model unchanged, as is any `/` that is not the first
character of the input.

The `/models` picker displays provider-reported context lengths as
`context: <size>` labels using integer binary sizes (`2048` appears as
`context: 2kb`; `1_048_576` appears as `context: 1mb`).
Submitting `/models` (with or without an initial argument, e.g.
`/models deepseek`) fetches the full catalog once and does *not* clear the
input — the draft stays as `/models <filter>` so the picker can be
narrowed by continuing to type. Every keystroke re-derives the filter from
the draft and re-applies it to the already-fetched `provider_connection/model`
identities without another provider round-trip. A multi-word filter (e.g.
`/models deepseek v3`) matches when every word appears somewhere in the
identity, case-insensitively and in any order — not the joined phrase as one
substring. A filter that matches nothing keeps the panel open with a
"Nothing matched your model filter." message rather than vanishing, so the
user can keep editing it. `Enter` confirms the highlighted model and clears
the input; `Esc` cancels and clears it too.

| Command | Effect | Result bubble |
| --- | --- | --- |
| `/models [filter]` | Queries every configured provider connection once, lists them as `provider_connection/model`, and switches the engine to the picked connection/model. `filter` (typed after the command, and refined live afterwards) narrows the displayed/selectable choices to those matching every filter word, case-insensitively and in any order, without re-querying. `↑`/`↓`, the mouse wheel, and left-click select, with left-click only highlighting and preserving the current popup window when the row is already visible; `page-up`/`fn-up` and `page-down`/`fn-down` move by a page, `←`/`→` cycle reasoning effort when available, `Enter` confirms, `Esc` cancels. | Persisted — the switch changes which provider connection/model later turns use; the reasoning effort applies to the current runtime |
| `/tools` | Lists the tools the running agent may call, as resolved for its agent type. | Transient |
| `/clear` (alias `/reset`) | Drops the conversation and continues in a brand-new session file; earlier prompts stay recallable with `↑`. | Transient |
