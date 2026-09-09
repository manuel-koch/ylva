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

## CLI Runtime

Non-interactive, one-shot session: the initial user turn is sent to the
agent, the assistant response is streamed to stdout as it arrives, and the
final committed conversation history is printed as JSONL on exit — one JSON
object per message, in the same wire shape the provider sees.

Selected automatically when stdout is not a terminal, or forced via `--no-tui`.

Initial user turn resolution: `--prompt <text>`, else piped stdin read to EOF
(only when stdin is not a terminal) — e.g. `echo '…' | ylva`.

Tool approval is host-driven: `read`/`glob` are auto-approved immediately;
`write` and `edit` require explicit user approval; `bash` always requires
user approval.

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
- `--conversation-id <uuid>` re-attaches an existing session and appends to it;
  an unknown id is an error rather than a silent fresh session.
- A session is exclusively locked while open, so a second Ylva cannot
  interleave writes into the same file.
- Child-agent runs record a start/result pair in the parent session and write
  their own transcript alongside it.
- On exit, Ylva prints the session id and a ready-to-paste
  `ylva --conversation-id <uuid>` command to stderr, so resuming never
  requires hunting through the chats directory.

See [persistence](./persistence.md) for the on-disk format and the full resume
contract.

## TUI Runtime

Interactive terminal UI launched automatically when stdout is a terminal
(see [TUI Architecture](#tui-architecture)). When stdout is not a terminal
(piped or redirected), Ylva falls back to the CLI driver.

## Model Provider

Currently the following AI model providers are supported:

- Ollama (OpenAI-compatible chat plus native model discovery endpoints)
- OpenRouter ( via OpenAI compatible endpoints )

Named provider connections are constructed independently, so multiple
connections can use the same implementation (for example, local Ollama and
Ollama Cloud). The active connection is shown in the conversation footer, and
`/models` queries each connection once and lists
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
  Mode can be `bytes`/`chars`/`lines`.
  Offset and limit are 0-based!
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
  Hidden files (dot-prefixed names) are **included by default**, matching `find . -type f` behaviour.
  Optional `dot_ignore` (default `false`); set to `true` to exclude any path component that starts
  with `.`. Optional `git_ignore` (default `true`) skips gitignored paths;
  set to `false` to use plain glob matching without gitignore rules. Optional `only_files` /
  `only_dirs` filters restrict results to files or directories only (mutually exclusive; both `true`
  returns an error). Optional `limit` caps the result count; when capped, `ignored_matches` and
  `matches` are set in result metadata. Optional `only_count`
  returns only the match count via `matches` in metadata; content is empty.
  Paths inside the workspace are auto-approved; paths outside are denied by the sandbox.
- **bash**: Execute a shell command via `bash -c`. Requires explicit human approval
  before execution. stdout and stderr are merged in true
  arrival order. Output is capped at 100 KB
  (`truncated` / `truncated_at_bytes` fields present in result metadata when
  capped). Default timeout: 30 s. On timeout or cancellation the entire
  process (including any child processes it spawned) is killed. See
  [implementation notes](./implementation-notes.md#bash-tool-process-group-cleanup).
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
  absolute result paths. Output is capped at 100 KB (`truncated` / `truncated_at_bytes` fields present
  in result metadata when capped). Paths inside the
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
  `--auto-approve-spawn` is set. The call resolves immediately with a
  `running`/`queued` status — at most one child runs at a time, further
  spawns queue automatically. The child's eventual outcome (success, failure,
  or cancellation) arrives later as a separate message once the child
  finishes, is cancelled, or hits its iteration cap. See
  [Child-agent delegation](#child-agent-delegation).
- **agent_finish**: Only available to child agents (a root agent never sees
  it). Reports `status` (`"success"` or `"failure"`), an optional `result` /
  `error`, and optional `sources`, then ends the child's turn. The parent
  receives this as the `spawn_subagent` call's outcome — it never sees the
  child's own conversation, only the `result`/`error` string, so a child
  should make its `result` self-contained rather than referencing
  "earlier"/"above" content the parent cannot see. If a child goes idle
  without calling `agent_finish`, the parent instead receives a synthesized
  result built from the child's last assistant message; if the child hits
  its max-tool-iterations cap first, the parent receives a clear failure
  instead, so it isn't mistaken for the child's own conclusion. A child is
  told its tool-use iteration budget up front so it can pace itself.

## Child-agent delegation

One level of agent delegation lets a root agent hand off a self-contained
task to a child agent that runs **concurrently** with the parent — the
parent keeps processing its own messages and tools while the child works.

- At most one child runs at a time; further `spawn_subagent` calls queue and
  start automatically as each preceding child finishes.
- The child's outcome (success, failure, or cancellation) is reported back
  to the parent as a new message once it's known, re-prompting the parent
  immediately if it was otherwise idle, so it reacts without waiting for a
  new user message.
- Stopping the parent (`Ctrl+C`/`Esc`) also cancels any running/queued
  children.
- A child's own tool calls go through the same approval flow as the
  parent's — a child is never auto-denied just for being a child.
- Use `--auto-approve-spawn` to skip the human-approval prompt for
  `spawn_subagent` calls (mirrors `--auto-approve-edit` / `--auto-approve-write`).

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
  Typing while merely *watching* a running child is a no-op rather than
  silently steering the wrong agent (root vs. child) — the footer omits
  `enter: send` in that state to make this clear.
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
  footer (mode/hint row and key-hints row). The mode/hint row also shows the
  active `provider-connection/model` identity for the transcript on screen,
  including an explicit reasoning effort in parentheses and the selected child
  while drilled in. No sidebar.
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
  input. OS SIGINT stops immediately (no double-press).
- **Help popup (`F1`):** toggles a centered popup with the full keybinding
  table and build-info values.
- **Smart auto-scroll:** the chat follows the newest output by default; the
  mouse wheel and `PageUp`/`PageDown`/`End` scroll the history directly (no
  focus needed). Scrolling up disables auto-scroll; scrolling back down to
  the bottom (or pressing `End`) re-enables it. A completed response respects
  whatever scroll-lock state the user left it in, rather than jumping back to
  the bottom.
- **Wordmark:** while the conversation is empty, a centered ASCII-art "Ylva"
  wordmark is displayed with the version and git commit shown below it. Once
  the first message is sent, the wordmark disappears.
- **Input history:** `↑` on the first line loads the previous sent message
  (draft is saved); `↓` restores it; any edit while browsing copies the entry
  to a fresh draft.
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
  spinner while waiting for the one running-child slot to free up.
- **Input as a message:** the input box renders like a sent user message — a
  colored left border, no full frame and no `"Input"` title. Long text
  word-wraps inside the box, and the box grows with its content up to the
  1/3 height cap.
- **Mouse editing:** a left-click in the input box places the cursor at the
  clicked character; a left-drag selects text. `read`/`glob` tool status
  lines and approval buttons remain mouse-clickable.
- **Shift+Enter / Alt+Enter:** insert a newline in terminals that support the
  kitty keyboard protocol (ZED, iTerm2, kitty, WezTerm), so the modifier is
  not mistaken for send.
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
  blockquotes, horizontal rules, and tables. Plain text with no Markdown
  syntax wraps as plain text. Unsupported syntax (links, images, HTML)
  degrades to plain text rather than crashing or dropping content. Markdown
  and syntax highlighting are applied live while a response streams.
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

Engines are read from one flat profile file,
`~/.ylva/profiles/<name>/config.toml` (`--profile <name>` selects it). Without
such a file, the backend comes from environment variables as before.

See [configuration](./configuration.md) for all available fields.

### Runtime profiles

A profile groups a default engine with optional per-agent overrides. This is the
groundwork for Phase 6 child agents, which will let different agent types run against
different engines automatically.

Profiles are read from `~/.ylva/profiles/<name>/config.toml`; see
[configuration](./configuration.md#profile-file) for the supported keys.

## Slash commands

Typing `/` as the **first character** of the draft-message input opens a
command overlay above the conversation; it narrows as the command is typed.
`↑`/`↓` highlight a command and `Tab` completes the highlighted command.
The command set is a fixed allowlist — a leading `/` that matches nothing in
it (e.g. `/foo`) is ordinary chat text and is sent to the model unchanged, as
is any `/` that is not the first character of the input.

The `/models` picker displays provider-reported context lengths as
`context: <size>` labels using integer binary sizes (`2048` appears as
`context: 2kb`; `1_048_576` appears as `context: 1mb`).

| Command | Effect | Result bubble |
| --- | --- | --- |
| `/models` | Queries every configured provider connection, lists successful results as `provider_connection/model`, and switches the engine to the picked connection/model. `↑`/`↓` select, `page-up`/`fn-up` and `page-down`/`fn-down` move by a page, `←`/`→` cycle reasoning effort when available, `Enter` confirms, `Esc` cancels. | Persisted — the switch changes which provider connection/model later turns use; the reasoning effort applies to the current runtime |
| `/tools` | Lists the tools the running agent may call, as resolved for its agent type. | Transient |
| `/clear` (alias `/reset`) | Drops the conversation and continues in a brand-new session file; earlier prompts stay recallable with `↑`. | Transient |
