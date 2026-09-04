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

Implemented in `src/cli_driver.rs`; selected automatically by `main.rs` when
stdout is not a terminal or forced via cli flag `--no-tui`.

Initial user turn resolution: `--prompt <text>`, else piped stdin read to EOF
(only when stdin is not a terminal) — e.g. `echo '…' | ylva`.

Tool approval is host-driven: `read`/`glob` are auto-approved immediately;
`write` and `edit` require explicit user approval (sandbox-abstain → `UserApprovalRequired`);
`bash` always requires user approval.

Tool status lines render as `[tool] <title> …` (e.g. `[tool] Read docs/features.md [limit=32]`)

When `--auto-approve-spawn` is set and the assistant calls `spawn_subagent`, the
CLI keeps the process alive past the assistant's own final `StreamDone` until
every spawned/queued child call has reported a terminal outcome (`ToolDone` on
success/failure/cancellation, or `ToolError` in the structurally-unreachable
no-`ChildAgentManager` fail-safe) — otherwise the one-shot process would exit
as soon as the parent goes idle, silently discarding the child's result since
`spawn_subagent` resolves immediately with `{"status": "running"/"queued"}`
while the child keeps running concurrently (see
[Child-agent delegation](#child-agent-delegation)). Without `--auto-approve-spawn`,
`spawn_subagent` is denied outright in CLI mode (`UserApprovalRequired` has no
approval path headless), so this only matters when that flag is used.

**WIP:** Provider selection is from environment variables; see
[CLI options — Environment variables](./cli-options.md#environment-variables).

## TUI Runtime

Interactive terminal UI launched automatically when stdout is a terminal
(see [Phase 4: TUI skeleton](#phase-4-tui-skeleton)). When stdout is not a
terminal (piped or redirected), Ylva falls back to the CLI driver.

## Model Provider

Currently the following AI model providers are supported:

- Ollama ( via OpenAI compatible endpoints )
- OpenRouter ( via OpenAI compatible endpoints )

## Tools to be used by models

The following tools can be used by AI models during a conversation turn.

Tools report their results via **uniform tool-result envelope**:
Every `role:"tool"` message carries its `content` as a JSON struct derived from
`ToolEnvelope`. The shape on success:

```json
{
  "ok": true,
  "result": "...",
  "result_metadata": { "field": value, ... },
  "result_metadata_schema": { "field": { "type": "...", "description": "..." }, ... }
}
```

On failure:

```json
{ "ok": false, "error": "..." }
```

`result_metadata` and `result_metadata_schema` are omitted when a tool produces
no structured metadata. When present, `result_metadata_schema` is derived
automatically from the tool's typed metadata struct (via `schemars`) and covers
only the fields that are actually populated for that specific result — the schema
can never drift from the data.

Each tool description sent in the `tools` array includes its argument schema.

Path-like arguments (`read`/`write`/`edit`/`search`'s `path`, `glob`'s `pattern`)
are normalized before dispatch: an absolute value that falls inside the
process's current working directory is rewritten to its cwd-relative form —
this is what actually executes and what the UI renders for the call, not
merely cosmetic. Values that are already relative, or absolute but outside
the cwd, pass through unchanged. This is a display/consistency nicety, not a
security boundary — sandbox approval checks run independently on whichever
path form ends up being used. Each tool declares its own path-like argument
keys via `Tool::path_like_arguments()`.

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
    call (useful when renaming a symbol throughout a file). Returns `ToolError` if
    `old_string` is not found.
  - **LineRange** — supply `start_line`, `end_line` (1-based, inclusive) and
    `new_string`; replaces those lines. Returns `ToolError` if the range is out of
    bounds or `start_line > end_line`.
  A unified diff preview is shown before the user approves the call
  (`pending_content_sections()` returns a `Section::Diff`). The diff bubble renders
  a two-column line-number gutter (old file | new file) derived from the unified-diff
  hunk headers, so each added, removed, and context line is annotated with its
  position in the old and/or new file.
  Requires user approval (sandbox path check → `UserApprovalRequired` by default).
- **glob**: Search for paths using a glob `pattern`, including wildcards like `*`, `?`, `**`, `[...]`.
  Hidden files (dot-prefixed names) are **included by default**, matching `find . -type f` behaviour.
  Optional `dot_ignore` (default `false`); set to `true` to exclude any path component that starts
  with `.`. Optional `git_ignore` (default `true`) skips gitignored paths via `ignore::WalkBuilder`;
  set to `false` to use plain `glob` matching without gitignore rules. Optional `only_files` /
  `only_dirs` filters restrict results to files or directories only (mutually exclusive; both `true`
  returns `ToolError`). Optional `limit` caps the result count; when capped, `ignored_matches` and
  `matches` are set in result metadata. Optional `only_count`
  returns only the match count via `matches` in metadata; content is empty.
  Paths inside the workspace are auto-approved; paths outside are denied by the sandbox.
- **bash**: Execute a shell command via `bash -c`. Requires explicit human approval
  before execution (always `UserApprovalRequired`). stdout and stderr are merged in true
  arrival order (concurrent async reads). Output is capped at 100 KB
  (`truncated` / `truncated_at_bytes` fields present in result metadata when
  capped). Default timeout: 30 s (`DEFAULT_BASH_TIMEOUT_SECS`). On timeout or
  cancellation the entire process group is killed via `SIGKILL` (Unix) using
  `nix::sys::signal::killpg` — grandchildren spawned by the script are also cleaned up.
- **search**: Search file contents using a regular-expression `pattern` within a
  directory tree (defaults to cwd when `path` is omitted). Uses `ignore` crate for
  traversal (respects `.gitignore`). Binary files are skipped (null-byte heuristic on
  first 8 KB). Results formatted in grep style: `filepath:lineno: content` for matches
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
  collapsed — so the bubble hints at the child's purpose without expanding it.
  Only registered for `Role::Root` agents whose resolved `AgentConfig::can_spawn` is
  `true`; structurally absent from a child's own registry (no nesting — delegation
  is one level deep). Requires user approval unless `--auto-approve-spawn` is set.
  Resolves immediately with `{ "status": "running" | "queued", "run_id": "<uuid>" }` —
  `"queued"` when another child is already running (at most one runs at a time; further
  spawns queue in an unbounded FIFO drained automatically as each child finishes). The
  tool's description explicitly tells the model not to poll or `bash sleep`-wait for the
  child inline — some models (observed with DeepSeek) otherwise burn tool-call budget on
  sleep loops instead of trusting the automatic re-prompt described below. The
  child's eventual outcome arrives later as a trailing `system` message announcing
  the finish (referencing the same `tool_call_id`), once the child calls its own
  `agent_finish` tool (or is cancelled, hits the iteration cap, or otherwise goes idle
  without calling `agent_finish` — see below). The original provisional tool result is
  never rewritten or duplicated: the tool-calling wire protocol permits only one
  `role: "tool"` message per `tool_call_id`, so the real outcome is appended as a
  distinct `system` message instead of a second tool result.
- **agent_finish**: Only registered for `Role::Child` agents (never appears in a parent's
  registry). Reports `status` (`"success"` or `"failure"`), an optional `result` /
  `error`, and optional `sources`, then unconditionally ends the child's loop — exempt
  from the max-tool-iterations cap. The parent receives this as the `spawn_subagent`
  call's outcome. The parent never sees the child's own conversation (prior turns, tool
  calls, or reasoning) — only the `result`/`error` string — so the tool's description and
  the `result` field's schema description both instruct the child to make `result`
  self-contained (e.g. inline the full requested table/findings) rather than referencing
  "earlier"/"above" content the parent cannot see. If a child goes idle (final assistant
  text with no tool call, a killed tool, or the iteration cap) without ever calling
  `agent_finish`, the parent instead receives a synthesized `status: "unstructured"`
  result built from the child's last assistant message.

## Child-agent delegation

One level of agent delegation lets a `Role::Root` agent hand off a self-contained task
to a `Role::Child` agent that runs **concurrently** with the parent — the parent keeps
processing its own messages and tools while the child works.

- `AgentFactory::create` wires a `ChildAgentManager` onto any `Role::Root` `AgentLoop`
  whose resolved `AgentConfig::can_spawn` is `true`; children are constructed via the
  same factory (`Role::Child(ChildRoleConfig { .. })`) and never receive `spawn_subagent`
  themselves.
- At most one child runs at a time; further `spawn_subagent` calls are queued in an
  unbounded FIFO and started automatically as each preceding child finishes.
- A child's outcome — success, failure, or an "unstructured" fallback — is injected into
  the parent's conversation as a new trailing `system` message referencing the original
  `spawn_subagent` call's `tool_call_id`, never by rewriting or duplicating that call's
  provisional tool result; the tool-calling wire protocol only permits one tool-result
  message per `tool_call_id`. That system message also tells the root that the result is
  already shown to the user verbatim (the tool bubble's Result/Error/Sources sections),
  so it should not repeat/quote it back — only add new analysis, a decision, or a next
  step — keeping the visible reply free of duplicated noise and wasted output tokens. If
  the root agent is otherwise idle at that moment (no stream or tool in flight), this
  immediately re-prompts the root LLM with the updated context so it reacts to the result
  without waiting for a new user message (essential for one-shot headless sessions, which
  never get a "next turn" otherwise); if the root is still busy with unrelated work
  (mid-stream or running its own tool round), the result is picked up once that work's own
  queue-drain/`StreamDone` re-prompt happens next.
- `AgentCommand::Stop` on the parent cancels the running child (and drains the queue)
  via `ChildAgentManager::cancel_all`; each cancelled/queued call is resolved to a
  terminal `ToolDone` (`status: "cancelled"`) and logged (`tracing::info!`) — durable
  persistence of child outcomes is deferred to a later phase. Unlike a natural finish,
  this never re-prompts the root (the user explicitly asked to stop).
- Use `--auto-approve-spawn` to skip the human-approval prompt for `spawn_subagent`
  calls (mirrors `--auto-approve-edit` / `--auto-approve-write`).
- A child's own tool calls go through the exact same `ToolPending` → approve/deny
  pause as the root's — a child is never auto-denied just for being a child. Each
  `AgentLoop` stamps its own `run_id` (`None` for the root, `Some(child_run_id)` for a
  `Role::Child`) onto every `UiUpdate::ToolPending`/`ClarifyPending` it emits. Drivers
  (`cli_driver`, `tui_driver`) track that `run_id` alongside the pending call and, when
  answering, wrap the verdict in `AgentCommand::ForChild { run_id, command }` whenever
  it's `Some`. The root's command loop resolves `run_id` to the still-running child's
  own command channel via `ChildAgentManager::running_command_sender` and forwards the
  inner command verbatim; a `run_id` with no matching running child (already finished,
  or unknown) is logged (`tracing::warn!`) and dropped rather than panicking or stalling
  the root. This is what lets a spawned child safely pause on `bash`/`write`/etc. without
  ever hanging the whole session.

## Phase 4: TUI skeleton

The TUI is a **rendering layer** on top of the headless `AgentLoop`; the agent
architecture is unchanged. It is selected automatically in `main.rs` via
`std::io::stdout().is_terminal()`.

### TUI module layout

The TUI follows a **reducer/MVP** split across source modules:

#### Model

`src/tui_reducer_state.rs` — pure view-model with no I/O. Defines `AppState`,
`TuiMode`, `Focus`, `AppLayout`, `Effect`, `InputEvent`, and `DomainEvent`.

#### Presenter

`src/presenter.rs` — pure `update(state, event) → Vec<Effect>` reducer. No
ratatui imports, no channel sends; fully unit-testable without a live terminal.

`route_domain` (the `DomainEvent` dispatch entry point) logs a `DEBUG`
`"domain event handled"` event for every `DomainEvent`, carrying only the
event's variant tag (`event`), its tool/clarify `id` when it has one
(`correlation_id`), and the `TuiMode` transition (`mode_before`/`mode_after`)
— never chunk text, tool arguments, or tool output. This is enough to follow
the TUI's state-transition flow and match up a call's
`ToolStart`/`ToolProgress`/`ToolDone` (or `ClarifyPending`) sequence across
log lines, without risking sensitive payloads in the log file.

#### View

`src/render.rs` + `src/ui.rs` — `render(frame, state)` entry point. Writes
computed geometry back to `state.layout`; delegates widget rendering to helpers
in `ui.rs`.

#### Driver

`src/tui_driver.rs` and `src/cli_driver.rs` — async event loops, one per
runtime mode, selected by `main.rs` based on whether stdout is a terminal.

`tui_driver.rs` owns `TextArea` and channel handles, converts
`AppAction → InputEvent` and `UiUpdate → DomainEvent`, calls `update()`, then
executes the returned `Vec<Effect>` via `execute_effects()`.

`cli_driver.rs` consumes `UiUpdate` events from the agent, streams the
assistant response to stdout, and prints the final conversation as JSONL on
exit.

- **Layout:** a scrollable chat-history area, a variable-height input box
  (capped at 1/3 of the terminal height), a `─` separator line, and a 2-row
  footer (mode/hint row and key-hints row). No sidebar.
- **Input always active:** the input box is never disabled. `Enter` sends
  unconditionally — while the model is streaming the message is queued as a
  pending steer, and `Ctrl+C`/`Esc` interrupt the current round.
- **Steer bubble lifecycle:** a steer (a message typed while the agent is
  busy) is rendered `Queued` (dimmed green, same hue as a normal sent
  message) immediately, near the bottom of the
  chat, but is only *staged*, not yet committed to the conversation log —
  this avoids fixing its position ahead of thinking/tool-call content for
  the in-flight round that hasn't been flushed yet. It becomes `Steered`
  and is committed to the log — after everything already flushed for that
  round — once `SteerApplied` arrives, at which point it renders identically
  to a normal `Sent` message (same solid green): once injected, the model has
  seen it like any other user message, so the UI only distinguishes
  "queued" from "seen", not *why* a message was seen.
- **Cancelled steers:** interrupting commits all staged steers as `Cancelled`
  (grey) with a dimmed "Not sent." line. The text is retained in input history
  so it can be recovered with `↑`.
- **Tool approval sub-mode:** when a tool awaits approval the footer switches
  to `[ ✓ Approve (Enter) ]  [ ✗ Deny (Esc) ]` (mouse-clickable). `Enter`
  sends `ApproveTool`, `Esc` sends `DenyTool` immediately; `Ctrl+C` still uses
  the double-press confirm. The input field is available to type a reason for
  denial (a dimmed `"Enter reason for denial here…"` placeholder appears when
  the field is empty). The user's draft is snapshotted when approval sub-mode
  starts and restored automatically after the tool is approved or denied.
- **Double-press `Ctrl+C`/`Esc`:** each key has its own 2-second confirm
  window (`CONFIRM_TIMEOUT`); only a second press of the *same* key confirms
  — pressing the other key starts its own fresh window instead, so e.g.
  `Ctrl+C` then `Esc` never quits on that `Esc`. While streaming/tool-pending
  the first press shows a per-key hint (`Esc`: cancel only; `Ctrl+C`: cancel
  & quit) and the second confirms that key's action; while idle `Ctrl+C`
  double-press quits and `Esc` immediately clears the input (single press,
  never quits — and dismisses a pending `Ctrl+C` quit confirm). A natural
  transition to `Idle` clears confirm state. OS SIGINT stops immediately (no
  double-press).
- **Help popup (`F1`):** toggles a centered popup with the full keybinding
  table and `BuildInfo` values.
- **Smart auto-scroll:** the chat follows the newest output by default; the
  mouse wheel and `PageUp`/`PageDown`/`End` scroll the history directly (no
  focus needed, so the wheel works anywhere over the TUI). Scrolling up
  disables auto-scroll; scrolling back down to the bottom (or pressing `End`)
  re-enables it. The scroll offset is clamped to the content height, so
  scrolling past either end never silently drifts.
- **Wordmark:** while the conversation is empty (no messages submitted yet), a
  horizontally centered ASCII-art "Ylva" wordmark is displayed with the version
  and git commit shown below it in DarkGray. Once the first message is sent,
  the wordmark disappears and the conversation fills the chat area.
- **Input history:** `↑` on the first line loads the previous sent message
  (draft is saved); `↓` restores it; any edit while browsing copies the entry
  to a fresh draft.
- **Line navigation:** `Fn+Left`/`Fn+Right` move the input cursor to the
  start/end of the current line, wrapping to the previous/next line.
- **Starts empty:** the TUI launches with an empty input box and no auto-sent
  turn — the hardcoded demo prompt is headless-only. An explicit `--prompt` or
  piped stdin still auto-sends on startup.
- **Sending state:** after `Enter` (or an auto-sent initial prompt) the footer
  shows `"Sending…"` until the first stream event arrives, instead of `"Idle"`.
- **Footer distinguishes thinking / responding / tool execution:** while
  streaming, the footer shows `"Thinking…"` when the model is emitting
  extended-thinking/reasoning tokens with no assistant text yet,
  `"Streaming…"` once assistant text (or tool-call argument deltas with no
  visible text) is flowing, and `"Running tool…"` once a tool bubble has
  actually started executing (`ToolCallState::Running`) — rather than a
  single generic `"Streaming…"` for the whole in-flight turn.
- **Footer stays "Running tool…" while a spawned child is still working:**
  the root loop's own turn can settle to idle (e.g. right after
  `spawn_subagent` acks with `"running"`) before the child finishes; the
  footer keeps showing `"Running tool…"` — driven by the `spawn_subagent`
  tool bubble still being `ToolCallState::Running` — instead of `"Idle"`,
  so an in-progress subagent never looks finished/stuck.
- **Queued subagent bubbles show a static glyph, not the spinner:** the
  animated Braille spinner (`ToolCallState::Running`) means "actively
  executing" for every other tool, but a `spawn_subagent` call queued behind
  another running child (`status=queued`) isn't doing anything yet — it's
  waiting for the running slot to free up. That bubble shows a static `≡`
  instead; the spinner only appears once `status=running` (the child has
  actually started).
- **Input as a message:** the input box renders like a sent user message — a
  colored left border (dimmed Green, matching the sent user-message bubble's
  border), no full frame and no `"Input"` title. Long text
  **word-wraps** inside the box (no horizontal scrollbar), and the box grows
  with its content up to the 1/3 height cap.
- **Mouse editing:** a left-click in the input box places the cursor at the
  clicked character (word-wrapping honored); a left-drag selects text. `read`/
  `glob` tool status lines and approval buttons remain mouse-clickable.
- **Shift+Enter / Alt+Enter:** insert a newline via the kitty keyboard protocol
  (`DISAMBIGUATE_ESCAPE_CODES`), so terminals that support it (ZED, iTerm2,
  kitty, WezTerm) report the modifier and the newline is not mistaken for send.
- **Message dedenting:** submitted messages are automatically dedented —
  common leading whitespace is stripped (via `textwrap::dedent`) and leading
  and trailing blank-only lines are removed. This makes pasting indented code
  snippets or quoted text send cleanly.
- **Multiline paste:** the TUI supports pasting multiline text via the
  terminal's bracketed-paste protocol (enabled automatically by crossterm).
  Pasted text is dedented before insertion.
- **Tool bubble collapse/expand:** clicking a tool bubble's title line or its
  left-border bar toggles whether the bubble's arguments and result are shown
  (expanded, `▼`) or hidden (collapsed, `▶`). Collapsed bubbles display only
  the title line, useful for decluttering long conversations.
- **Tool duration:** when a tool finishes, its execution time is shown in the
  bubble title.
- **Toast notifications:** transient messages appear in the bottom-right corner
  (e.g., `"Copied to clipboard"`) and auto-dismiss after ~1.5 seconds.
- **Scrollbar:** the chat scrollbar's thumb reaches the very bottom of the
  track at the end of the list (and the top at the start).
- **Markdown rendering:** assistant messages, user messages, the clarify
  banner's question/context text, and a user's clarify answer (shown in the
  "User answered:" result of the clarify tool bubble) render Markdown instead
  of plain wrapped text. Supported: **bold**/*italic*,
  `inline code` (distinct color), fenced code blocks (dim, verbatim — no
  word-boundary reflow — syntax-highlighted when a language tag is given),
  bullet/numbered lists, headings, blockquotes (`│` prefix), horizontal
  rules, and tables (column-aligned, bold header row, `│` column
  separators). Plain text with no Markdown syntax wraps as plain text.
  A fenced code block line that is still too long to fit after the
  line-number gutter is character-wrapped onto blank-gutter continuation
  rows (never clipped), so long code lines never render past the message
  bubble's right edge.
  Unsupported syntax (links, images, HTML) degrades to plain text — inner
  text is preserved — rather than crashing or dropping content.
- **Thinking bubble:** extended-thinking/reasoning output (Anthropic
  `thinking_delta`, OpenAI `reasoning_content`, or bare `reasoning` from
  Ollama/vLLM) gets its own bubble: dim-blue border, Markdown rendering,
  shown before the assistant's reply (even on thinking-only turns that go
  straight into a tool call). Starts expanded; title shows a live elapsed
  timer while streaming, then the total duration once done. Click the title
  or left border to expand/collapse, same as tool bubbles. Depends on the
  model having extended-thinking/reasoning enabled, not on the provider.
  Interleaved thinking, replies, and tool calls each flush into their own
  bubble in order.

## Phase 5: Named engines and profiles

### Named engine configuration

Backends are described by named engine entries, each specifying the provider,
model, sampling parameters, and connection details. Multiple engines can be defined
and referenced by name, for example a fast local Ollama engine for chat and
a more capable OpenRouter model for heavier tasks.

See [configuration](./configuration.md) for all available fields.

### Runtime profiles

A profile groups a default engine with optional per-agent overrides. This is the
groundwork for Phase 6 child agents, which will let different agent types run against
different engines automatically.

**WIP:** Profile and engine definitions are constructed in code; TOML file loading
is introduced in Phase 7.

### `/models` command (coming soon)

The groundwork for a `/models` slash command is in place. When implemented it will
list the models available from the active provider directly inside the conversation.
