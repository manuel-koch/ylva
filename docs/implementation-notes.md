# Implementation Notes

Internal mechanics, module/struct/field references, and design rationale that
back the user-facing behavior described in [features](./features.md). This
document is for contributors working on Ylva's internals, not end users.

## Engine/provider runtime configuration

`EngineConfig` contains the engine's immutable connection settings and the
initial values for its model and sampling policy. `EngineRuntimeConfig` is the
shared mutable snapshot for request-level settings (`model`, `temperature`,
`context_length`, and `reasoning_effort`).

`OpenAiCompatibleProvider` and `EngineImpl` share one `EngineRuntimeConfigHandle` rather
than maintaining independent mutable copies. The provider clones the snapshot
before starting a request and releases the lock before network I/O, so a
runtime update cannot produce a request with mixed settings. Updates apply to
subsequent requests; an already-running stream keeps the snapshot it started
with. `context_length` remains available to the engine's context-budgeting
logic and is not sent as an OpenAI request parameter. `reasoning_effort` is
translated into the provider-specific request field when it is explicit;
`None` leaves the provider's own default behavior in place.

`OpenRouterProvider` reuses that transport for both chat and OpenRouter's
metadata-rich `/models` catalog. Its selected effort is sent as
`reasoning.effort`. `OllamaProvider` accepts the Ollama server origin as its
base URL, derives `/v1/chat/completions` for chat, lists models from
`/api/tags`, and enriches each entry with `/api/show` model metadata when
available. An explicit Ollama effort is sent as the OpenAI-compatible
`reasoning_effort` field; models that only advertise the `thinking`
capability use Ollama's own default because they have no selectable effort
alternatives. The Ollama base URL must be the server origin and must not
include the `/v1` path.

`OpenRouterProvider` likewise accepts only the OpenRouter origin and derives
`/api/v1` before constructing the shared OpenAI-compatible transport. A
configured OpenRouter path is not rewritten or supported.

The shared chat transport receives a request-body customization function from
the concrete provider wrapper. This keeps provider-specific wire fields in
`OpenRouterProvider` and `OllamaProvider` rather than centralizing them in a
format-dispatch enum.

`ModelInfo.reasoning_efforts` normalizes provider metadata into three states:
`None` means no advertised reasoning support, `Some([])` means supported
reasoning without selectable alternatives, and `Some(values)` means supported
reasoning with those alternatives. The `/models` picker adds an implicit
provider-default choice before `values`; the chosen explicit value is carried
in `ProviderModelOption` and applied when the selection is confirmed. Picker
rows render available context lengths as `context: <size>` labels using
integer binary `kb`/`mb` sizes.

`EngineRouter` owns the configured engine set when a profile or environment
provides more than one provider connection. It delegates chat and runtime
configuration to the active engine, groups engines by their named provider
connection, queries each connection once and concurrently for `/models`, and
switches the active connection only after it successfully accepts the
selected model. Connection settings remain owned by each underlying engine;
switching connections does not mutate another connection's configuration.

The `/models` slash command normalizes the draft to `/models <filter>` and
fetches the full catalog once via `Effect::ListModels { filter }`, storing
every fetched `ModelChoice` in `SlashOverlay::ModelPicker { choices, filter,
selected }` — `choices` is never re-filtered out of the vector. Mouse-wheel
events move the highlighted choice through the same bounded selection helper
as keyboard navigation, so the rendered window follows the pointer without
changing the chat scroll position. A left-click on a rendered row updates the
highlight through the same row mapping, preserves the current popup window
when that row is already visible, and never emits `SwitchModel`; only Enter
confirms the choice. The input is
deliberately *not* cleared: `AppState::refresh_slash_overlay` re-derives
`filter` from the live draft on every keystroke while the picker is open (a
prior no-op) and `matching_model_indices` — shared by selection movement,
`selected_model`, reasoning-effort cycling, and rendering — recomputes which
indices are currently displayed/selectable from `choices` + `filter` without
touching the network. `matching_model_indices` splits `filter` on whitespace
and requires every word to appear somewhere in the choice's
`provider_connection/model` identity, case-insensitively — an AND match
across words in any order, not a single joined-phrase substring, so
`/models deepseek v3` and `/models v3 deepseek` both match `stub/deepseek-v3`.
Confirming (`Enter`) or cancelling (`Esc`) the picker
clears the input. When `matching_model_indices` returns no indices,
`tui_widgets::draw_slash_overlay` keeps the panel open and renders a single
"Nothing matched your model filter." row (selection `None`) instead of
hiding the overlay, so the filter stays editable. When the rendered rows
overflow the popup's capped height, the view window is anchored to the
selected row and `render_chat_scrollbar` draws a one-column scrollbar in the
popup's right interior gutter.

`ProviderRegistry` centralizes provider-name validation, environment-based
selection, and construction. A profile separates
`provider_connections.<name>` (connection identity and endpoint) from its
`provider` field (implementation kind), and an engine references the
connection with `provider_connection`. Multiple connections may therefore use
the same implementation. Engine-level endpoint and credential values override
connection values. A missing API key may be filled from the matching profile
environment variable.

Environment discovery loads the selected profile's `.env` plus exported
process variables, and the merged snapshot is used both for provider
discovery and `${ENV_VAR}` interpolation in every TOML string. Process
variables win over `.env` values, and missing interpolation variables fail
profile loading. The merged snapshot is never logged.

## Tool-result wire protocol

Every `role:"tool"` message carries its `content` as a JSON struct derived
from `ToolEnvelope`. Success shape:

```json
{
  "ok": true,
  "result": "...",
  "result_metadata": { "field": value, ... },
  "result_metadata_schema": { "field": { "type": "...", "description": "..." }, ... }
}
```

Failure shape:

```json
{ "ok": false, "error": "..." }
```

`result_metadata` and `result_metadata_schema` are omitted when a tool
produces no structured metadata. When present, `result_metadata_schema` is
derived automatically from the tool's typed metadata struct (via `schemars`)
and covers only the fields actually populated for that result — the schema
can never drift from the data. Each tool description sent in the `tools`
array includes its argument schema.

## Path-like argument normalization

Path-like arguments (`read`/`write`/`edit`/`search`'s `path`, `glob`'s
`pattern`) are normalized before dispatch: an absolute value that falls
inside the process's current working directory is rewritten to its
cwd-relative form — this is what actually executes and what the UI renders
for the call, not merely cosmetic. Values that are already relative, or
absolute but outside the cwd, pass through unchanged. This is a
display/consistency nicety, not a security boundary — sandbox approval
checks run independently on whichever path form ends up being used. Each
tool declares its own path-like argument keys via
`Tool::path_like_arguments()`.

## `bash` tool process-group cleanup

The shell inherits the process's current working directory, which is the
workspace launch directory in normal runtime. Bash commands should therefore
use relative paths directly rather than starting with a redundant `cd` to the
workspace root.

On timeout or cancellation the entire process group is killed via `SIGKILL`
(Unix) using `nix::sys::signal::killpg` — grandchildren spawned by the script
are also cleaned up. Default timeout is `DEFAULT_BASH_TIMEOUT_SECS` (30s).

## CLI driver

Implemented in `src/cli_driver.rs`; selected automatically by `main.rs` when
stdout is not a terminal or forced via `--no-tui`.

When `--auto-approve-spawn` is set and the assistant calls `spawn_subagent`,
the CLI keeps the process alive past the assistant's own final `StreamDone`
until every spawned/queued child call has reported a terminal outcome
(`ToolDone` on success/failure/cancellation, or `ToolError` in the
structurally-unreachable no-`ChildAgentManager` fail-safe) — otherwise the
one-shot process would exit as soon as the parent goes idle, silently
discarding the child's result since `spawn_subagent` resolves immediately
with an admission event (`event: "subagent_accepted"`) while the child keeps
running concurrently. Without `--auto-approve-spawn`, `spawn_subagent` is denied
outright in CLI mode (`UserApprovalRequired` has no approval path headless),
so this only matters when that flag is used.

## Store & persistence

Implemented in `src/store.rs`. See [persistence](./persistence.md) for the
user-facing on-disk contract; this section is the contributor view.

- **One handle, one writer.** `Store` is a cheap `Arc` handle. Every clone
  shares one `flock`ed file and one writer task, so append order is the commit
  order regardless of who holds the clone. The advisory lock is released once
  the last clone drops and the writer drains.
- **Off the hot path.** `Store::record_*` enqueue onto an `mpsc` channel and
  never block; the writer task appends one line per event, `flush`ing but never
  `fsync`ing. `Store::flush().await` is a barrier used by tests and shutdown
  paths — it is fed by a second channel of `oneshot` acks, and the writer
  drains all pending events before acknowledging.
- **One commit point.** `Context::commit` is the only place a message enters
  the conversation, and therefore the only place a `message` event is produced.
  This upholds the blueprint §4C ownership rule: only the outer loop mutates
  `Context`, so no streaming task can ever write. `AgentLoop::with_store`
  attaches the store to the `Context` the loop creates.
- **Session replay.** `Context::set_store` reads the attached session through
  `Store::read_messages` and calls `Context::hydrate`, which appends directly
  to the in-memory message list instead of using `commit`; this prevents
  already-persisted lines from being written again. The rule is unconditional
  for fresh and resumed stores because a fresh file reads as an empty
  transcript. `hydrate` also synthesizes an in-memory `ToolError` for an
  unanswered generic tool call, keeping the next provider payload valid
  without creating a new disk event.
- **Replay projection.** `tui_reducer_state::project_messages` maps persisted
  `Message` values to terminal-free `ChatEntry` values before the first TUI
  frame. `ToolResult`/`ToolError` carry persisted `summary`,
  `content_sections`, and `duration`, and `ToolResult` also carries an
  optional result-section identifier for tools such as `clarify`. This keeps
  the resumed result heading identical to the live bubble; absent identifiers
  fall back to the standard result section. `push_tool_error` preserves the
  actual duration instead of hard-coding zero. Assistant tool calls persist the
  tool-specific `title_arguments` captured when the call is created, so
  `clarify` keeps only its question inline while its context and choices remain
  body sections, and
  replayed bubbles use the same title projection as live bubbles; calls
  without title arguments use an empty title projection.
  System messages and implicit harness notes remain model-only.
- **Tool-result payload.** `Message::ToolResult` carries a serializable
  `ToolResult` payload as its flattened value, and
  `Context::push_tool_result` forwards that value unchanged. This keeps the
  persisted JSON shape stable while avoiding a positional argument list for
  the complete result.
- **Child replay and lazy loading.** `ChildStart`/`ChildResult` carry
  `call_id` (plus `label` on `ChildStart`) so `Store::read_child_runs` can
  repair a replayed `spawn_subagent` bubble to a finished or interrupted
  state. `replay_messages` creates `ChildConversation` metadata with
  `needs_lazy_load`; `tui_driver` reads
  `Store::session_path(store.chats_root(), store.project_key(), run_id)` only
  on the first drill-in, then caches the projected child messages. Production
  child transcripts use the run id as their session id through
  `Store::write_child_transcript_for_run`, avoiding a second path-correlation
  field. Legacy records missing correlation fields remain readable but
  undrillable/degraded.
- **Every event carries a millisecond-precision timestamp.** `now_timestamp()`
  formats RFC 3339 at `SecondsFormat::Millis`, and every `StoreEvent` variant
  has a timestamp field: `meta.created_at`, `message.timestamp`,
  `child_start.created_at`, `child_result.completed_at`, and
  `model_changed.changed_at`. For a `message` event this is the instant
  `Context::commit` accepted it — effectively "sent" for a user message and
  "response fully assembled" for an assistant message, since only one commit
  point exists per message today; it is *not* a stream-start or
  first-token instant (no such event is persisted). `#[serde(default)]`
  reads a `message` line written before this field existed back as `""`
  rather than failing.
- **Immediate `meta` header, deferred title.** The header is written to disk
  right away (empty `title`) when the session is created, so every subsequent
  event is durable the instant it is written — nothing is ever buffered only
  in memory waiting on a title. Once the first conversation turn completes
  (the first committed `Message::Assistant`), `SessionWriter::finalize_title_if_pending`
  rewrites just that first line via `rewrite_meta_line` (temp file plus
  `rename()`, the same pattern as a schema-upgrade rewrite), then reopens and
  re-`flock`s the file at its new inode so later appends keep landing at the
  same path. `Store::resume` re-derives the title source from the transcript
  (`first_explicit_user_message`) when the on-disk title is still empty, so a
  session killed mid-turn still gets titled once a later run completes a
  turn.
- **`model_changed`, not `meta.provider_connection`/`meta.model`.**
  `meta.provider_connection`/`meta.model` capture only the
  connection/model pair resolved at session creation and are
  never rewritten afterwards. `StoreEvent::ModelChanged` carries the actual
  `provider_connection`/`model` (read off `EngineConfig`, never a
  config-table name) and
  is recorded via `Store::record_model_changed` on every process start (fresh
  or resumed) and again after every successful `/models` switch
  (`Effect::SwitchModel` in `tui_driver.rs` → `AgentCommand::RecordModelChanged`
  → `Context::store()`, so it follows `/clear` rollovers correctly) — it is
  the only authoritative, replayable history of what has actually served the
  session, including that first value. A child's own session file gets its
  own independent `model_changed` history from its resolved engine, passed
  through `ChildTranscriptTarget::engine_config` into
  `Store::write_child_transcript`.
- **Repair-on-open.** `Store::resume` truncates a trailing incomplete or
  unparseable line before the first append. `list_sessions` and any other read
  path never mutate, not even a file with a corrupt tail.
- **Child agents.** `ChildAgentManager::with_store` holds the *parent's* store
  and writes `child_start` (from `start`) and `child_result` (from
  `record_child_result`, called by `AgentLoop::run`'s `finish_rx` arm). The
  child's own transcript is written once at completion by
  `agent_factory::write_child_transcript` via
  `Store::write_child_transcript_for_run`, which opens a fresh sub-session
  under the same project directory using the child run id and passes the
  parent's session id as `meta.parent_session`. The parent link is therefore
  discoverable from the child file too, not just top-down via the parent's
  lifecycle events. The older general-purpose
  `Store::write_child_transcript` helper remains available for callers that
  need a newly generated child session id. `Store::start_new_session` takes an
  explicit `parent_session: Option<Uuid>` for exactly this reason — `/clear`
  passes `None` (a rollover is a fresh session, not a child run).
- **Testing seam.** `chats_root` is an explicit parameter on `Store::create`,
  `Store::resume`, and `Store::list_sessions`; only `main.rs` resolves the real
  `~/.ylva/chats` through `default_chats_root()`. Unit tests pass a `TempDir`
  directly, and the end-to-end test sets `YLVA_HOME` for the spawned binary —
  no `#[cfg(test)]` seam in library code.
- **Mandatory startup persistence.** `main.rs` fails startup when
  `default_chats_root()` cannot resolve the Ylva home or when the session file
  cannot be created or resumed. The CLI has no transient, non-persisted mode;
  `Option<Store>` remains available only to lower-level APIs and tests that
  intentionally run without persistence.
- **`~/.ylva` resolution.** `src/ylva_home.rs` is the single source of truth for
  the per-user root shared by `chats/` and `profiles/`: `ylva_root()` returns a
  non-empty `YLVA_HOME` verbatim as the root, else `dirs::home_dir()` with
  `.ylva` appended. Its pure core,
  `resolve_ylva_root(override_value, home_directory)`, takes both inputs as
  arguments so it is unit-testable without mutating the process environment.
  `store::default_chats_root()` and `config::default_profiles_root()` both build
  on it and map a missing root to their own domain error.

## Child-agent delegation internals

- `AgentFactory::create` wires a `ChildAgentManager` onto any `Role::Root`
  `AgentLoop` whose resolved `AgentConfig::can_spawn` is `true`; children are
  constructed via the same factory (`Role::Child(ChildRoleConfig { .. })`)
  and never receive `spawn_subagent` themselves.
- The current scheduler runs at most one child at a time; further
  `spawn_subagent` calls are queued in an unbounded FIFO and started
  automatically as each preceding child finishes. The model-facing lifecycle
  contract does not expose that scheduling detail, so a future concurrent
  scheduler can deliver completions out of order without changing the
  protocol.
- The immediate `spawn_subagent` tool result is an admission event, not a child
  outcome. It includes `event: "subagent_accepted"`, `run_id`, `tool_call_id`,
  `label`, `status`, `outstanding_count`, and `all_done: false`.
- A child's outcome — success, failure, cancellation, or an "unstructured"
  fallback — is injected into the parent's conversation as a hidden harness
  notice containing `event: "subagent_completed"`, the correlation fields,
  `status`, `result`/`error`, `sources`, `outstanding_count`, and `all_done`.
  It never rewrites or duplicates the provisional tool result; the
  tool-calling wire protocol only permits one tool-result message per
  `tool_call_id`. The notice also tells the root that the result is already
  shown to the user verbatim in the tool bubble's Result/Error/Sources
  sections, so it should not repeat or quote it back. Intermediate completion
  notices do not force an acknowledgement turn. When `all_done` becomes true,
  the root is re-prompted once (immediately if idle, or after its current
  stream/tool round) so it can aggregate the results without waiting for a
  new user message.
- `AgentCommand::Stop` on the parent cancels the running child (and drains
  the queue) via `ChildAgentManager::cancel_all`; each cancelled/queued call
  is resolved to a terminal `ToolDone` (`status: "cancelled"`) and logged
  (`tracing::info!`) — durable persistence of child outcomes is deferred to
  a later phase. Unlike a natural finish, this never re-prompts the root
  (the user explicitly asked to stop).
- A child's own tool calls go through the exact same `ToolPending` →
  approve/deny pause as the root's — a child is never auto-denied just for
  being a child. Each `AgentLoop` stamps its own `run_id` (`None` for the
  root, `Some(child_run_id)` for a `Role::Child`) onto every
  `UiUpdate::ToolPending`/`ClarifyPending` it emits. Drivers (`cli_driver`,
  `tui_driver`) track that `run_id` alongside the pending call and, when
  answering, wrap the verdict in `AgentCommand::ForChild { run_id, command }`
  whenever it's `Some`. The root's command loop resolves `run_id` to the
  still-running child's own command channel via
  `ChildAgentManager::running_command_sender` and forwards the inner command
  verbatim; a `run_id` with no matching running child (already finished, or
  unknown) is logged (`tracing::warn!`) and dropped rather than panicking or
  stalling the root.

### Drill-in TUI conversation view internals

- `ChildAgentManager::start` emits `UiUpdate::ChildAgentStarted { run_id,
  call_id, agent_type, task, label }` immediately before its usual
  `ToolProgress`; `call_id` is the `spawn_subagent` tool call's own id (the
  root `ToolBubble`'s `id`), threaded through purely to correlate a `run_id`
  with its bubble (`ChildRunInfo::bubble_id`). The child's own streamed
  text/thinking tokens are forwarded as `UiUpdate::ChildAgentToken { run_id,
  text }` (merged text and thinking into one buffer — no separate thinking
  bubble for the child), and `UiUpdate::ChildAgentDone { run_id, status,
  result }` marks that child's conversation as finished. The child's own
  `StreamStarted`/`StreamDone`/`StreamCancelled` lifecycle events are
  swallowed, not forwarded — they would otherwise corrupt the parent's own
  `TuiMode`.
- `AppState` tracks `active_view: ConversationView` (`Root` or `Child {
  run_id, agent_type, task, label }`, default `Root`), `child_conversations:
  HashMap<Uuid, ChildConversation>` keyed by `run_id` (each entry holds
  `info: ChildRunInfo { run_id, bubble_id, agent_type, task, label }`,
  `messages: Vec<ChatEntry>`, and `partial_text: String`), `running_child:
  Option<ChildRunInfo>` (the child currently *executing* — cleared on
  `ChildAgentDone`, but its `child_conversations` entry is never removed, so
  a finished child's transcript stays drillable), and `auto_switched_to_child:
  bool`. A `run_id`'s entry is created once, on `ChildAgentStarted`, and
  persists for the lifetime of the TUI session — this is the fix for a bug
  where the entire drill-in model tracked only a single global child
  conversation, which got silently overwritten by whichever child started
  most recently while the user was still viewing an older one's transcript
  (including the "queued child starts while an active one's view is open"
  case). `running_child` (*which child is executing*), `active_view` (*which
  child the user is viewing*), and `child_conversations` (*every child ever
  seen*) are deliberately three independent concerns — conflating
  "executing" with "viewable" was the root cause. The model tracks each
  child by its own map entry rather than a single shared slot specifically
  so it stays correct if child execution ever becomes truly parallel
  (`ChildAgentManager` today still runs one child at a time, but nothing in
  the TUI's viewing/storage layer assumes that).
- Spawning a child never changes `active_view` — the user stays on whatever
  they were looking at. Only an *interactive pause belonging to the
  currently-executing child* (`ToolPending`/`ClarifyPending` tagged with
  that child's `run_id`) auto-switches `active_view` to `Child` and arms
  `auto_switched_to_child` — unless the user is already viewing that exact
  running child's view. This auto-switch fires regardless of what
  `active_view` was beforehand: `Root`, or a different (e.g.
  already-finished) child's view the user had manually drilled into. The
  view active immediately before the auto-switch is snapshotted into
  `AppState.pre_pause_view` (`Root` or the other child's `Child { .. }`) so
  it can be restored precisely. When that pause resolves, the view
  auto-returns to `pre_pause_view` (falling back to `Root` if none was
  recorded) **only if** the switch that got there was automatic — a user who
  navigated to `Child` manually (double-clicking the spawn bubble) is never
  yanked back automatically.
  - Concurrent root-pending-approval and child-interactive-pause precedence
    is not implemented — `TuiMode`/`Focus` remain a single, global pause
    state (matching the "one active child at a time" delegation-depth-1
    constraint), so this is a known, deferred edge case.
- Double-clicking a `spawn_subagent` tool bubble's title resolves the
  clicked bubble's id to its owning `run_id` via
  `AppState::run_id_for_bubble` (a reverse lookup over `child_conversations`
  matching `info.bubble_id`) and switches to that specific child's view
  (`switch_to_child_view(run_id)`) — never "whichever child is currently
  running" or "whichever child started most recently". This is what makes
  drilling into an *older*, already-finished subagent's bubble correct even
  while a newer one is running or queued. This is deliberately distinct from
  the single-click collapse/expand toggle every bubble (including spawn
  bubbles) always supports, so the two gestures never collide. A first click
  never toggles immediately: it only arms `last_bubble_click` `(Uuid,
  Instant)`; the actual collapse/expand toggle is deferred until
  `DomainEvent::BubbleClickWindowExpired` fires (a `tokio::time::sleep`
  future polled in `tui_driver`'s `select!`, mirroring the existing
  `confirm_timeout`/`ConfirmExpired` double-press-abort pattern) with no
  confirming second click having arrived within `DOUBLE_CLICK_WINDOW`
  (250 ms). This keeps the bubble's rendered height — and therefore any
  bottom-pinned auto-scroll — unchanged between the two clicks of a
  double-click, so the second click's row reliably resolves back to the same
  bubble; toggling immediately on the first click was found to break this
  correlation in practice. Because a child's `child_conversations` entry
  isn't removed on completion, drilling in still works after the subagent
  has finished — a lone single click on a finished bubble only
  expands/collapses it once the window expires.
- While viewing `Child { run_id, .. }`, the main chat area renders that
  specific `child_conversations[run_id].messages` instead of `chat_entries`.
  A non-scrolling header above it re-renders the *real* root `spawn_subagent`
  `ToolBubble` (same chrome, same collapsed/expanded state, same
  `agent_type`/`label`/`task` content the user saw before drilling in)
  rather than a separately-maintained header string — the bubble is found
  via `ChildRunInfo::bubble_id`, an exact, permanent correlation captured at
  `ChildAgentStarted` time (the spawn_subagent tool call's own id), not by
  guessing "the most recently started spawn bubble". The header's title is
  shortened to `"Subagent"` instead of the plain title-cased `"Spawn
  Subagent"` shown for the same bubble in the root conversation (via
  `ToolBubbleArgs::display_title_override` in `tui_widgets.rs`). A child's
  transcript is **not** seeded with a synthetic task message — the header
  bubble already shows the task. If the child is paused for its own
  approval/clarify, the input box behaves normally (`Enter` routes to the
  child's flow via `Focus::ToolApproval`/`ClarifyPending`, unaffected by
  `active_view`); if the child is merely running, `Enter` with a non-empty
  draft is a no-op instead of silently steering the *root* agent — the
  previous behavior queued a steer for `Root` while the user was looking at
  the child's transcript, with no visual indication of which conversation
  the message actually went to. This is called out in the footer (`enter
  send` is omitted entirely while viewing a running child), not via a
  separate overlay on the input box, since keystrokes are still buffered
  normally either way.
- The spawn-bubble uses the same `Color::Magenta` border as every other tool
  bubble — no separate color styling was introduced for it.
- The footer/header spinner glyph is driven by `AppState.spinner_frame`,
  incremented once per tick in `tui_driver`'s event loop — gated by
  `spinner_should_tick()`, which checks `agent_mode() != AgentMode::Idle`
  rather than raw `TuiMode`. This matters specifically for a spawned
  subagent: the root's own `TuiMode` settles to `Idle` almost immediately
  after `spawn_subagent` is called (the root's own turn is done; it's just
  waiting on the tool), so gating on raw `TuiMode` alone would freeze the
  spinner glyph on both the footer and the child-view header bubble for the
  subagent's entire run. `agent_mode()` already reports `ToolRunning`
  whenever a tool bubble (including `spawn_subagent`) is still `Running`,
  keeping the spinner animating — including while the subagent itself is
  paused for its own inner tool approval, since that only sets
  `Focus::ToolApproval` and never touches the root `spawn_subagent` bubble's
  `Running` state.
- The footer's right-aligned `provider-connection/model` identity and explicit
  reasoning effort are projected from the live root engine or the configured
  child engine, so they follow the transcript currently on screen. The effort
  is shown in parentheses only when it is not the provider default. Root model
  switches refresh the label before the next frame.
- Footer keybinding hints are represented as structured key/action entries
  rather than one formatted string, so `tui_widgets` can render the key in
  bold, the action in normal text, and ` · ` between entries.
- A thin separator line (the same dim `─` rule used between the chat area
  and the input/footer panel) is rendered directly below the child-view
  header bubble, so it never reads as part of the scrollable
  conversation-list beneath it.
- The header bubble scrolls independently of the chat area when its
  rendered content exceeds the clamped max height (40% of the terminal):
  `AppState.child_view.header_bubble_scroll_offset` drives a
  `Paragraph::scroll` offset, clamped each frame in `tui_widgets::draw` to
  the header's actual overflow, and a scrollbar (reusing
  `render_chat_scrollbar`) appears in a 1-column gutter to its right only
  when the content overflows. Mouse-wheel events over the header's rect
  (`AppState.layout.header_bubble`, hit-tested in `presenter::route_mouse`
  ahead of the clarify-banner/chat-area checks) scroll the header instead of
  the chat area. The offset resets to `0` whenever a fresh child view is
  entered (`switch_to_child_view` / `ChildAgentStarted`).
- `switch_to_child_view` (manual drill-in via double-click) also re-pins the
  main chat area's scroll to the bottom (`conversation.scroll_offset = 0`,
  `conversation.scroll_locked = true`) — these fields are shared between the
  root and child views (not per-view state), so a non-bottom position left
  over from scrolling up while viewing `Root` would otherwise carry straight
  into the child transcript. Before doing so, it saves the root's own
  `(conversation.scroll_offset, conversation.scroll_locked)` into
  `AppState.saved_root_scroll` (an immediate best-effort fallback), and also
  captures the drilled-into `spawn_subagent` bubble's `Uuid` into
  `AppState.saved_root_scroll_bubble`. Manual "back to parent" navigation
  (`switch_to_root_view`, triggered by `Esc` with an empty draft) restores
  `saved_root_scroll` for the current frame, and moves
  `saved_root_scroll_bubble` into `AppState.pending_scroll_to_bubble` for
  precise resolution: once `active_view` has flipped back to `Root` and the
  next frame's `AppState.conversation.bubble_title_rows` reflects root's
  transcript again, `render::sync_from_draw` looks up that bubble's
  *current* row and scrolls there, unlocking scroll — this stays correct
  even if the root conversation grew while the child view was open. The net
  effect: the user lands back exactly at the root `spawn_subagent` bubble
  they drilled in from, not just near it. `note_child_interactive_pause`'s
  auto-switch into `Child` (an interactive pause, not manual navigation)
  never populates `saved_root_scroll`/`saved_root_scroll_bubble`, so its
  later auto-return via `switch_to_root_view` is a no-op on scroll.
- When a tool-approval is pending for the tracked subagent and the user is
  looking at `Root` (having manually navigated back via `Esc` while the
  subagent's own tool call is still awaiting approval), the approval
  footer's title is prefixed with `"Subagent tool '<name>' requests your
  approval:"` instead of the plain `"Tool '<name>' requests your approval:"`
  — otherwise the pending approval is invisible from the root transcript.
  The distinction is derived from whether `Focus::ToolApproval`'s `tool_id`
  lives in any `child_conversations` entry's `messages` (subagent) or
  `chat_entries` (root), independent of `active_view`.
- Clicking anywhere on the child-view header bubble toggles its collapsed
  state — unlike a normal chat bubble, this needs no click-vs-double-click
  debounce, since the drill-in gesture only applies from the root
  transcript. `AppState::header_bubble_id()` is the single source of truth
  for which real `chat_entries` bubble the header re-renders (used by both
  `render::project_for_draw` and `presenter::route_mouse`), and
  `AppState::toggle_header_bubble_collapsed()` flips that bubble's
  `collapsed` flag directly — deliberately not reusing
  `toggle_tool_bubble_collapsed`, which also clamps
  `conversation.scroll_offset`/`conversation.scroll_locked` against
  `conversation.bubble_title_rows`; those fields describe the
  currently-visible chat area (the child's own transcript), not the header,
  so touching them here would be a side effect on the wrong view's scroll
  state.
- The `collapsed` flag toggled above lives on the same `chat_entries`
  `ToolBubble` entry that represents the `spawn_subagent` call itself, so it
  survives the subagent finishing: neither `DomainEvent::ToolDone` (which
  only patches the entry's `state`) nor `ChildAgentDone` (which only flushes
  pending partial text and may switch `active_view` back to `Root`) touch
  `collapsed`. A manual expand is therefore preserved when the subagent
  completes, and re-entering the drilled-in view later (e.g. by
  double-clicking the now-finished bubble from `Root`) still shows it in
  whichever state it was last left. Like any other tool bubble, a
  `spawn_subagent` bubble still *starts* `collapsed` by default until the
  user toggles it once.

## TUI module layout

The TUI follows a **reducer/MVP** split across source modules. It is selected
automatically in `main.rs` via `std::io::stdout().is_terminal()`; the TUI is
a rendering layer on top of the headless `AgentLoop` — the agent architecture
itself is unchanged.

### Model

`src/tui_reducer_state.rs` — pure view-model with no I/O. Defines `AppState`,
`TuiMode`, `Focus`, `AppLayout`, `Effect`, `InputEvent`, and `DomainEvent`.

### Presenter

`src/presenter.rs` — pure `update(state, event) → Vec<Effect>` reducer. No
ratatui imports, no channel sends; fully unit-testable without a live
terminal.

`route_domain` (the `DomainEvent` dispatch entry point) logs a `DEBUG`
`"domain event handled"` event for every `DomainEvent`, carrying only the
event's variant tag (`event`), its tool/clarify `id` when it has one
(`correlation_id`), and the `TuiMode` transition (`mode_before`/
`mode_after`) — never chunk text, tool arguments, or tool output. This is
enough to follow the TUI's state-transition flow and match up a call's
`ToolStart`/`ToolProgress`/`ToolDone` (or `ClarifyPending`) sequence across
log lines, without risking sensitive payloads in the log file.

### View

`src/render.rs` + `src/tui_widgets.rs` — `render(frame, state)` entry point.
Writes computed geometry back to `state.layout`; delegates widget rendering
to helpers in `tui_widgets.rs`.

### Driver

`src/tui_driver.rs` and `src/cli_driver.rs` — async event loops, one per
runtime mode, selected by `main.rs` based on whether stdout is a terminal.

`tui_driver.rs` owns `TextArea` and channel handles, converts `AppAction →
InputEvent` and `UiUpdate → DomainEvent`, calls `update()`, then executes the
returned `Vec<Effect>` via `execute_effects()`.

`cli_driver.rs` consumes `UiUpdate` events from the agent, streams the
assistant response to stdout, and prints the final conversation as JSONL on
exit.

## Slash commands

`src/slash_command.rs` is the whole parser: a closed allowlist
(`SLASH_COMMANDS`) plus `parse_slash_command` (exact match on the raw draft)
and `slash_command_suggestions` (prefix match, drives the overlay). It is
pure and has no knowledge of the TUI.

The Presenter (`presenter.rs`) checks the **raw**, un-dedented draft on
`Enter`, so only a `/` in the input's first cell can start a command. While
suggestions are visible, the selected index is kept in `AppState`; `Tab` emits
`Effect::SetInputText` with the selected command's canonical name, and plain
`Enter` activates the selected command even when the draft is only a prefix.
The `/models` picker queries provider-connection-qualified
options from `EngineRouter`, displays `provider_connection/model`, has the
same bounded `↑`/`↓` and mouse-wheel movement semantics, maps left-clicks to
highlight-only selection without unnecessary window repositioning, and uses
`page-up`/`fn-up` and
`page-down`/`fn-down` to move the highlighted option by three entries, uses
`←`/`→` to cycle reasoning efforts when available, and `Enter` activates both
the selected connection, model, and effort;
`Esc` closes any open overlay and clears the draft (before the usual
draft-clear/abort ladder), since leaving the draft in place would make the
next per-action `refresh_slash_overlay` recompute reopen the very overlay
just dismissed.
Model rows identify reasoning as unavailable, at the default, or adjustable
with a `⚙` marker. The user-facing fixed value is rendered as `default`. The
footer switches from the normal `enter send` hint to the controls for the
active command overlay, including the model picker's reasoning, switch, and
cancel keys.
Left-dragging within any visible slash-command overlay row selects its
rendered text, highlights the range while dragging, and copies the selected
text to the clipboard on release without changing the command or model
selection.

Slash effects (`Effect::ClearConversation`/`ShowTools`/`ListModels`/
`SwitchModel`) are the only ones that need the engine, so
`tui_driver::split_slash_effects` peels them out of each batch and
`execute_slash_effects` handles the immediate ones; the runtime driver hands
`ListModels` to a background task, while `execute_effects` matches all of
these engine effects as no-ops.
The driver sets `SlashOverlay::LoadingModels` after the `/models` draft is
normalized and starts provider catalog discovery in a cancellable
driver-local task. The next normal loop render displays the loading state.
The task's result is handled by a separate `select!` branch, so the main loop
continues consuming agent updates, input, timeouts, and spinner ticks while
discovery is pending. Dismissing the loading overlay cancels the pending
request; completed results are ignored unless the loading overlay is still
active.

Two rules keep this consistent with blueprint §4C:

- Nothing on the Driver side writes to the session store. `/models`'
  switch is persisted by sending `AgentCommand::RecordModelChanged` (a
  structured `model_changed` store event only — never a conversation
  message, since a mid-conversation `role:"system"` entry is rejected by
  some providers/chat templates), and `/clear` rolls the session file over
  by sending `AgentCommand::ClearContext` — both handled inside the agent
  loop, the sole `Context` mutator. The human-readable confirmation is a
  TUI-only notice (`AppState::push_notice`) that is never added to
  `Context`.
- `AgentCommand::ClearContext`'s handler reads the *live* `engine.config()`
  (not a value cached from session creation) and passes its
  `provider_connection`/
  `model` to `Store::start_new_session`, so the successor session's `meta`
  header reflects whatever backend actually served the conversation up to
  that point — including a `/models` switch that happened since the session
  was created — rather than reusing whatever
  `meta.provider_connection`/`meta.model` were
  written at creation.

`AppState::available_tool_names` is captured once, at TUI startup, from the
resolved `AgentLoop::registry()` (sorted, since the registry is a `HashMap`),
which is why `/tools` needs no I/O.

## TUI behavior internals

- **Steer bubble lifecycle:** a steer (a message typed while the agent is
  busy) is rendered `Queued` immediately, near the bottom of the chat, but is
  only *staged*, not yet committed to the conversation log — this avoids
  fixing its position ahead of thinking/tool-call content for the in-flight
  round that hasn't been flushed yet. It becomes `Steered` and is committed
  to the log — after everything already flushed for that round — once
  `SteerApplied` arrives, at which point it renders identically to a normal
  `Sent` message: once injected, the model has seen it like any other user
  message, so the UI only distinguishes "queued" from "seen", not *why* a
  message was seen.
- **Double-press `Ctrl+C`/`Esc`:** each key has its own 2-second confirm
  window (`CONFIRM_TIMEOUT`); only a second press of the *same* key confirms
  — pressing the other key starts its own fresh window instead, so e.g.
  `Ctrl+C` then `Esc` never quits on that `Esc`.
- **Smart auto-scroll:** `DomainEvent::StreamDone`/`StreamCancelled` do
  **not** force auto-scroll back on — a completed response respects
  whatever scroll-lock state the user left it in, the same way in-flight
  `StreamChunk`/`StreamThinkingChunk` updates already did; otherwise
  scrolling up to view an earlier subagent bubble (or a plain scroll-back in
  a long conversation) would get yanked back to the bottom the moment the
  current response finished.
- **Footer mode-word disambiguation:** `TuiState::agent_mode` follows the
  transcript currently shown, while `TuiState::parent_agent_mode` retains the
  root loop's status. Once a viewed child's `spawn_subagent` bubble reaches a
  terminal state while the root is still busy producing its final reply or
  running another child, the footer overrides the word with `"Subagent done,
  parent working…"` instead of the misleading `"Idle"`/root status. While
  drilled into a live child, the root's `agent_mode()` would report
  `"Running tool…"` for as long as the `spawn_subagent` bubble is `Running`
  — correct for the root footer but misleading for the child's own
  transcript — so the footer instead inspects the child's own `TurnBuffer`
  (projected per-child via `render::project_for_draw`) and shows `"Subagent
  thinking…"`/`"Subagent streaming…"`/`"Subagent working…"` as appropriate.
- **Queued subagent bubbles:** show a static `≡` glyph instead of the
  animated spinner while `status=queued` (waiting for the running slot to
  free up); the spinner only appears once `status=running`.
- **Shift+Enter / Alt+Enter:** insert a newline via the kitty keyboard
  protocol (`DISAMBIGUATE_ESCAPE_CODES`), so terminals that support it (ZED,
  iTerm2, kitty, WezTerm) report the modifier and the newline is not
  mistaken for send.
- **Message dedenting:** submitted messages are automatically dedented —
  common leading whitespace is stripped (via `textwrap::dedent`) and
  leading/trailing blank-only lines are removed.
