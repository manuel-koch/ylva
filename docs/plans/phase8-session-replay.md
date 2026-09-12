# Phase 8: Session Replay on Resume

## Context

Ylva is a Rust TUI agent harness (`manuel-koch/ylva`). `docs/blueprint.md` §7
("Phased Build Plan") tracks feature phases; this plan implements **Phase 8:
Session Replay on Resume** (blueprint.md lines ~2795-2853), building on the
persistence design in blueprint §4H and the `Store`/`Context` machinery Phase
7 shipped.

**Goal:** resuming a session via `--conversation-id <uuid>` must reconstruct
its prior turns into the running conversation instead of starting blank, so
both the model (headless and TUI) and the user see continuous history. Phase
7 wired `--conversation-id` to *append* to the existing session file
(`Store::resume`, `src/store.rs`) but explicitly deferred replaying those
prior lines back into the in-memory `Context` or the TUI transcript — that
gap is what this phase closes.

**Why this matters:** today, resuming a session only continues writing to
the same file; the model has zero visibility into what was said before this
process started, and the TUI shows an empty chat pane even though the
session file already holds a full transcript. This defeats the stated
purpose of `--conversation-id` (see `docs/persistence.md` "Resume contract"
and `docs/features.md`'s Session Persistence section, both of which
currently say resume does **not** replay).

**Scope decision — full-fidelity replay, schema extended (superseding the
original blueprint framing):** blueprint's Phase 8 entry says "Key crates:
none new (uses the existing `Store::read_messages`, §4H)", implying a
purely in-memory projection with no persisted-schema changes. Investigation
showed that would force replayed tool-call bubbles to look visibly blanker
than live ones (no `summary`/`content_sections`, and no `duration` for
`ToolResult`) because those fields are today only ever carried on an
in-memory `ToolDone` UI event, never persisted. **Decision: persist what's
needed for full-fidelity replay instead of accepting degraded bubbles.**
This is an additive, `#[serde(default)]`-tolerant schema extension per §4H
decision 4 (no `schema_version` bump, no migration/upgrader needed) — see
Implementation Plan steps 1-2 and Implementation Hints for the exact fields.
`docs/blueprint.md`'s Phase 8 bullet 2 must be amended in the same commit to
describe this (step 10 below).

**Scope decision — child-run replay is now in scope (refinement round,
supersedes the original "out of scope, follow-up" framing below):** a
grilling-skill refinement session on this plan (see conversation history)
established that leaving `spawn_subagent` bubbles/child transcripts entirely
unreplayed produces two concrete defects, not just a missed enhancement:

1. A finished child's root `spawn_subagent` bubble would replay as
   permanently stuck **"Running"** forever, because its persisted
   `Message::ToolResult` is written once at spawn time
   (`{"status":"running"/"queued","run_id":...}`, see
   `resolve_spawn_subagent`, `src/agent.rs` ~line 2115) and is **never
   updated** when the child actually finishes — the real outcome only ever
   reaches the model as an invisible `Message::User { kind:
   ImplicitToolNote }` harness notice (`push_async_harness_notice`), and the
   live `UiUpdate::ToolDone` for that bubble is never persisted at all. This
   directly contradicts "full-fidelity replay" and must be fixed regardless
   of whether child drill-in is replayed.
2. The root's own session file already carries enough data
   (`StoreEvent::ChildStart`/`ChildResult`, `src/store.rs` ~lines 113-133) to
   both fix defect 1 and to let a user drill into a finished child's full
   conversation on a resumed session exactly like a live one — so this is
   implemented as new steps 18-24 below, using **lazy** per-child transcript
   loading (only the root file is read eagerly; a specific child's own
   transcript is read from disk on first drill-in) to avoid an eager
   N-file-read at every resume.

This also surfaced a related, previously-unhandled defect in the *original*
step 8 projection: a generic (non-`spawn_subagent`) tool call left
unanswered by the file (process died mid-call) has no persisted
`ToolResult`/`ToolError` at all, which — left alone — would hydrate an
`Assistant` message whose `tool_calls` entry has no matching `tool`-role
response, violating most LLM providers' wire protocol on the very next
request after resume. Step 22 below fixes this by synthesizing an in-memory
`Message::ToolError` during `Context::hydrate`, mirroring the existing
`AgentCommand::Stop` cancellation-resolution pattern
(`src/agent.rs` ~line 1816) that already does the same thing for children
cancelled on live shutdown.

**Related ideas (`docs/ideas/`):** `docs/ideas/timing.md` proposes a broader,
session-wide duration-tracking feature (request/thinking/tool/idle phases);
this plan's `duration` field stays narrowly scoped to tool-bubble replay
fidelity and is not a step toward that idea, though a future timing effort
could revisit its shape. `docs/ideas/slash-commands.md`'s `/agent` overlay
(sub-agent conversation-list UI) will eventually depend on the same
child-transcript-replay machinery steps 18-24 introduce — flagged here so
that future work doesn't rediscover the dependency independently.

**Acceptance criteria:**

- A resumed headless (`ConsoleApp`) run includes every previously committed
  message from the session file in the very next LLM request payload — the
  model "remembers" the prior conversation on turn 1 of the new process.
- A resumed TUI (`TuiApp`) run shows the prior turns in `chat_entries` before
  the first frame is drawn, scrolled to the bottom of that history, with
  replayed tool-call bubbles showing the same `summary`/`content_sections`/
  `duration` a live bubble would have shown (for sessions written after this
  change; see the "legacy sessions" note below for older files).
- Hydrating prior messages must **never** re-commit them to the store (no
  double-recording on the next flush/resume cycle).
- Hydration runs unconditionally whenever a `Store` is attached to a
  `Context` (no separate "was this a resume" flag) — a fresh session's file
  has nothing to hydrate yet, so this is a safe no-op there.
- The root/parent transcript is eagerly replayed (as above); a resumed
  `spawn_subagent` bubble whose child already finished shows its true
  terminal state (`Done` with the real `status`/`result`/`sources`-derived
  `content_sections` and a `duration` derived from
  `ChildStart.created_at`/`ChildResult.completed_at`) instead of the stale
  `"running"`/`"queued"` text its `Message::ToolResult` was written with at
  spawn time — never a permanently-stuck `Running` bubble (see steps 18-20).
- A resumed `spawn_subagent` bubble whose child has a `ChildStart` but **no**
  matching `ChildResult` (process died while the child was still running)
  replays as a terminal **Error/Interrupted** state with a synthetic note —
  never as `Running` (step 20).
- Drilling into any resumed `spawn_subagent` bubble (finished or
  interrupted) shows that child's full own conversation, exactly like a
  live session, **lazily loaded**: the child's transcript is read from disk
  (`Store::session_path` + `Store::read_messages`) only the first time the
  user drills into it, not eagerly for every past child at resume time
  (steps 21-23).
- A generic (non-`spawn_subagent`) tool call left unanswered by the file
  (process died mid-call, no persisted `Message::ToolResult`/`ToolError` at
  all) is **not** silently dropped: `Context::hydrate` synthesizes an
  in-memory-only `Message::ToolError` ("Tool call did not complete before
  the session ended") so the next LLM request stays protocol-valid, and this
  synthesized error is **displayed** as a visible Error tool bubble in the
  TUI — never reconstructed as `Pending`/`Running` (step 22).
- **Legacy sessions** (files written before this change) lack the new
  `summary`/`content_sections`/`duration` fields, and lack `call_id`/`label`
  on `ChildStart`/`ChildResult`; they deserialize those as empty/zero/absent
  via `#[serde(default)]`. A tool bubble replays with visibly blanker
  content in the first case; a `spawn_subagent` bubble whose `ChildStart`
  predates this change cannot be fixed up or drilled into (no `call_id` to
  correlate it by) and keeps showing its originally-persisted
  `"running"`/`"queued"` text, undrillable, in the second case. Both are
  accepted, silent, degraded behavior — no UI marker distinguishes either
  (consistent with the project's existing old-reader/new-writer tolerance
  philosophy, §4H decision 4).
- `docs/blueprint.md`, `docs/persistence.md`, `docs/features.md`, and
  `docs/implementation-notes.md` are updated to describe the new hydration
  behavior and schema additions in the same commit as the code (per this
  repo's "docs are part of the change set" rule).

**Starting point:** `src/agent.rs` — `Context` (struct at line ~452), its
`commit`/`push_tool_result`/`push_tool_error`/`messages()` methods, and the
`Message` enum (line ~374); `src/agent/tools.rs` — `Section` and its variant
structs (line ~142-198); `src/store.rs` — `Store::read_messages` (line ~690)
and `Store::resume` (line ~406); `src/llm.rs` — `serialize_messages` (line
~199), which already destructures every `Message` variant with `..` so new
fields are compile-safe there with no change needed. For the child-run
replay extension (steps 18-24): `src/store.rs` — `StoreEvent::ChildStart`/
`ChildResult` (~lines 113-133), `record_child_start`/`record_child_result`
(~lines 564-591), `session_path` (~line 238, already public — builds a
session's file path from `chats_root`/`project_key`/`session_id` directly,
no directory scan needed); `src/agent/child_agent_manager.rs` —
`ChildAgentManager::spawn`/`start` (~lines 312-365, where `record_child_start`
is called and `call_id`/`label` are available locally but currently
discarded), `FinishResult` and its `to_json`/`content_sections` methods
(~lines 64-127), the `AgentCommand::Stop` cancellation-resolution pattern
(`src/agent.rs` ~line 1816) that already synthesizes a terminal
`Message::ToolError`/content-section for a child cancelled on live shutdown
— the pattern step 22 reuses for orphaned generic tool calls; `src/agent.rs`
`resolve_spawn_subagent` (~line 2050-2120) and the `finish_rx` arm (~lines
1310-1415) for how the root's `spawn_subagent` bubble's live "running" →
"done" transition currently happens only in `UiUpdate`s, never persisted;
`src/tui_reducer_state.rs` — `ChildRunInfo`/`ChildConversation`
(~lines 372-408), `AppState.child_conversations`, `start_child_run`
(~line 1656), `switch_to_child_view` (~line 1660, today a pure in-memory
reducer with no I/O).

## Implementation Plan

Each implementation step is marked `[x]` after completion and this file is
rewritten in place, so progress is resumable across sessions.

1. [x] **Make tool content sections serializable.** In
       `src/agent/tools.rs`, add `serde::Serialize, serde::Deserialize` to the
       derive list of `Section` (line ~196), `ContentSection` (line ~142),
       `DiffSection` (line ~162), `SearchResultLine` (line ~171), and
       `SearchResultSection` (line ~182). All fields on these types are plain
       data (`String`, `Option<String>`, `Option<usize>`, `bool`,
       `Vec<SearchResultLine>`) — no custom `Serialize`/`Deserialize` impl is
       needed, this is a mechanical derive addition. Verify with `cargo build`
       that no other trait bound conflicts arise.

2. [x] **Extend the persisted `Message` schema.** In `src/agent.rs`'s
       `Message` enum (line ~374):
       - Add to `Message::ToolResult`: `summary: Vec<(String, String)>`,
       `content_sections: Vec<crate::agent::tools::Section>`,
       `duration: std::time::Duration` — each annotated
       `#[serde(default)]` so old session files (written before this
       change, lacking these fields) still deserialize.
       `ToolResult` also carries an optional result-section identifier so
       specialized headings (for example `clarify`'s `user_answer`) survive
       replay; absent identifiers use the standard `result` section.
       Assistant `ToolCallRequest` records persist the tool-specific
       `title_arguments` captured from the created call. Calls without title
       arguments persist an empty list.
       - Add to `Message::ToolError`: `content_sections:
       Vec<crate::agent::tools::Section>`, also `#[serde(default)]`.
       `Message::ToolError` already has a `duration` field; no `summary`
       field is added here since `ToolCallState::Error`
       (`src/tui_reducer_state.rs` line ~165) has no `summary` field to
       populate.
       - `std::time::Duration` already round-trips through `serde_json`
       without extra crates or `#[serde(with = "...")]` (confirmed: it is
       already used unannotated on `Message::ToolError.duration` today and
       the crate builds).

3. [x] **Thread the new data through `Context::push_tool_result` /
       `push_tool_error`.** In `src/agent.rs`:
       - Define a canonical serializable `ToolResult` payload containing the
       tool id, name, result text, metadata, summary, content sections, and
       duration. Store it as the flattened value of `Message::ToolResult`.
       `push_tool_result` accepts and forwards this payload unchanged rather
       than expanding it into another positional argument list.
       - Extend `push_tool_error`'s signature (line ~579) to accept
       `content_sections: Vec<crate::agent::tools::Section>` and include it
       in the constructed `Message::ToolError`.
       - **Fix the pre-existing `Duration::ZERO` hardcoding** in
       `push_tool_error`: it currently discards the real duration
       unconditionally. Change it to accept the real `duration` as a
       parameter and persist that, instead of always zero. This is tightly
       coupled to this change (duration only starts mattering once it's
       used for replay) and must be fixed here, not left as a latent bug.
       - Update every call site of `push_tool_result`/`push_tool_error` in
       `src/agent.rs` (found at lines ~1227, ~1266, ~1652, ~1708, ~1768,
       ~1882, ~1903, ~1923, ~2102, ~2120, ~2371, plus the
       `resolve_spawn_subagent` call at ~2120, and test call sites ~2812,
       ~2853, ~3276) to pass through the real `summary`/`content_sections`/
       `duration` already in scope at that call site (e.g. the
       `AgentEvent::ToolDone`/`AgentEvent::ToolError` handlers at ~1213 and
       ~1266 already have all three values available locally — they are
       currently only forwarded to the `UiUpdate::ToolDone`/`ToolError`
       channel send, not to `context.push_tool_result`/`push_tool_error`).
       Call sites that fail before any tool execution starts (e.g. "Unknown
       tool" at ~1186, `resolve_spawn_subagent`'s no-`ChildAgentManager`
       branch) legitimately have no elapsed duration or summary and should
       pass `Duration::ZERO`/`vec![]`, mirroring what they already send on
       the `UiUpdate` side.

4. [x] **Add `Context::hydrate`.** In `src/agent.rs`, add a method
       `pub fn hydrate(&mut self, messages: Vec<Message>)` that appends each
       message directly via `self.messages.push(..)` — **never** through
       `self.commit(..)`, since `commit` (line ~578) is what forwards to the
       store and would duplicate every line on the next resume/flush.
       Document that it must be called after `Context::new()`'s two default
       system prompts are already installed and before any other message is
       pushed, and that it is safe to call with an empty `Vec` (a no-op).

5. [x] **Expose the session file path from `Store`.** Add
       `pub fn path(&self) -> &Path` to `Store` (`src/store.rs`, near line
       ~346), backed by the existing `StoreInner.path` field (line ~331), so
       callers outside `store.rs` can call `Store::read_messages(store.path())`.

6. [x] **Call hydration unconditionally wherever a `Store` is attached to a
       `Context`.** No "resumed vs. fresh" flag is threaded through
       `AgentFactory`/`AgentLoop` — a fresh session's file has no prior
       committed messages when hydration runs, so `Store::read_messages`
       returns an empty `Vec` there and hydration is a safe no-op. Update:
       - `AgentLoop::run`'s `tokio::spawn` closure (`src/agent.rs` ~line
       1028-1031), where `Context::new()` + `context.set_store(store)` runs
       for the root loop: immediately after `set_store`, read
       `Store::read_messages(store.path())` and call
       `context.hydrate(messages)`. On a read error, log a warning
       (`tracing::warn!`) and continue with an empty hydration rather than
       failing the whole run — a corrupt/unreadable file should not block
       startup (mirrors the read-tolerance philosophy of §4H decision 6).
       - The `/clear`/`/reset` fresh-context path (`src/agent.rs` ~line
       1943-1951, `fresh_context.set_store(new_store)`): apply the same
       hydrate-after-set_store call for consistency, even though this path
       always targets a brand-new file (harmless no-op, keeps one uniform
       rule instead of a special case).
       - Child-agent contexts are unaffected: children are always started via
       `start_new_session` (§4H decision 9), never resumed from an existing
       file, so hydration there is always a no-op in practice; no change to
       the child-context construction path is required beyond whatever
       `set_store` call it already makes, if a uniform helper is
       introduced (see Implementation Hints).

7. [x] **Headless verification.** `ConsoleApp` (`src/cli_driver.rs`) needs no
       structural change beyond step 6 — the hydrated `Context` is already
       what gets sent as the request payload on the next turn — but add an
       assertion-level integration test (see step 12) confirming the behavior
       end-to-end via the real binary, not just the unit level.

8. [x] **Project hydrated messages into TUI `chat_entries` before first
       render.** In `src/tui_reducer_state.rs`, add a function (following the
       existing naming/visibility conventions of nearby `AppState` methods
       such as `push_user_message`/`flush_assistant_message` around lines
       1490-1540) that maps each `Message` to a `ChatEntry`:
       - `Message::User { content, kind: UserMessageKind::Explicit }` →
       `ChatEntry::UserMessage { text: content, state:
       UserMessageState::Sent }`.
       - `Message::User { kind: UserMessageKind::ImplicitToolNote, .. }` →
       **skipped**, no `ChatEntry` produced — these are hidden from the
       live UI too (see `Context::push_implicit_user`'s doc comment,
       `src/agent.rs`), so replay must preserve that invisibility. They are
       still hydrated into `Context.messages` by step 4/6 so the *model*
       retains them — only the TUI projection drops them.
       - `Message::Assistant { content, status, .. }` → `ChatEntry::
       AssistantMessage { text: content, interrupted: matches!(status,
       AssistantStatus::Interrupted) }` (check `AssistantStatus`'s exact
       variants near the `Message` enum, `src/agent.rs` ~line 374, and
       match whatever mapping `flush_assistant_message` already uses live).
       - `Message::ToolResult { .. }` → a `ChatEntry::ToolBubble` with
       `state: ToolCallState::Done { arguments, result_section:
       ResultSection::plain(result), content_sections, summary, duration,
       title_arguments }`. `content_sections`/`summary`/`duration` come
       straight from the (now-extended) `Message::ToolResult`; `arguments`/
       `title_arguments` come from matching the preceding
       `Message::Assistant.tool_calls` entry with the same `id` (see next
       bullet).
       - `Message::ToolError { .. }` → similarly `ToolCallState::Error {
       error, title_arguments, duration, content_sections }`.
       - Assistant messages carrying `tool_calls`
       (`Message::Assistant.tool_calls: Vec<ToolCallRequest>`) are the
       source of each subsequent `ToolResult`/`ToolError`'s `arguments`/
       `title_arguments` — match by `ToolCallRequest.id` ==
       `Message::ToolResult.id`/`Message::ToolError.id`, using
       `ToolCallRequest::title_arguments()` (`src/agent.rs` ~line 365).
       Production tool-call handling captures the tool-specific projection
       before execution and persists it on the request, so replay uses the
       same title arguments as the live path. Calls without title arguments
       use an empty projection.
       `ToolResult` also persists an optional result-section identifier, with
       the standard `result` section as the legacy fallback; this preserves
       specialized headings such as the `clarify` tool's `user_answer`
       section.
       - A tool call with no matching `ToolResult`/`ToolError` in the file
       (process died mid-call) never reconstructs a `Pending`/`Running`
       state. **Superseded by the child-run replay extension** (steps
       18-22): `spawn_subagent` calls are cross-referenced against
       `ChildStart`/`ChildResult` records instead (steps 19-20, `Done` or
       `Error` depending on whether the child actually finished); every
       other tool gets an in-memory-only synthesized `Message::ToolError`
       during `Context::hydrate` (step 22), which this projection then
       renders as a normal, visible Error bubble like any other
       `ToolError` — no bare tool call is ever left with no `ChatEntry`.
       - `Message::System` is never projected to a `ChatEntry` (system
       prompts/briefings are not shown in the chat transcript live
       either).

9. [x] **Call the TUI projection before the first frame.** Find where
       `TuiApp::new`/`TuiApp::run` (`src/app.rs`) constructs its initial
       `AppState` and call the new projection function from step 8
       immediately after construction — driven off `Context::messages()`
       after hydration has already run in step 6 — and before the render
       loop's first `draw_frame` call, then set the initial scroll position
       to the bottom of `chat_entries` (reuse whatever existing
       auto-scroll-to-bottom mechanism already fires when new entries are
       pushed live — do not add a second scrolling mechanism). This runs
       unconditionally like step 6's hydration — a fresh session yields an
       empty `chat_entries` seed, which is exactly today's starting state.

10. [x] **Update `docs/blueprint.md`'s Phase 8 section** (lines ~2795-2853):
        amend bullet 2 (currently describing only `Context::hydrate` as a
        pure in-memory seam with "no new crates") to state that
        `Message::ToolResult`/`ToolError` are additively extended with
        `summary`/`content_sections`/`duration` (per §4H decision 4, no
        schema-version bump) so replayed tool bubbles match live ones; update
        the "Key crates: none new" line's surrounding text if it now reads
        as misleading (the line itself remains literally true — no new
        crate is added — but the paragraph should no longer imply zero
        schema change). Also amend the bullet stating child-agent transcripts
        are out of scope: they are now replayed (lazily, on first drill-in),
        with `StoreEvent::ChildStart`/`ChildResult` additively extended with
        `call_id`/`label` for the correlation this requires (steps 18-23).

11. [x] **Update `docs/persistence.md`.** Remove the "In Phase 7 a resumed
        session appends to the existing file but does **not** replay the
        earlier turns..." paragraph at the end of the "Resume contract"
        section (~line 198) and replace it with a description of the new
        hydration behavior: `Context::hydrate` appends persisted messages
        without re-committing them; the two leading system prompts are
        always rebuilt fresh, never replayed from disk; `Message::ToolResult`/
        `ToolError` now additionally persist `summary`/`content_sections`/
        `duration` so replay can reconstruct a full-fidelity tool bubble;
        sessions written before this change lack those fields and replay
        with degraded (blanker) tool bubbles, silently, via `#[serde(default)]`;
        `StoreEvent::ChildStart`/`ChildResult` now additionally persist
        `call_id`/`label` so a resumed `spawn_subagent` bubble can be
        fixed up to its true terminal state and drilled into; a child's own
        transcript is read from disk lazily, on first drill-in, not eagerly
        at resume; an orphaned child (`ChildStart` with no `ChildResult`)
        replays as an Error bubble, never stuck `Running`; any other
        unanswered tool call is synthesized into an in-memory-only
        `Message::ToolError` during hydration so the model-facing payload
        stays protocol-valid and the bubble is shown as an Error, never
        silently dropped; legacy `ChildStart`/`ChildResult` records lacking
        `call_id` cannot be correlated and that bubble keeps its originally-
        persisted `running`/`queued` text, undrillable.

12. [x] **Update `docs/features.md`.** In the Session Persistence section
        (~line 66-78), update the "Every start begins a fresh session..."
        area and the resume description to state that resuming now restores
        the prior transcript into both the headless request payload and the
        TUI chat view, including a finished or interrupted subagent's own
        conversation (loaded lazily on first drill-in) — linking to
        `docs/persistence.md`'s updated Resume contract for detail.

13. [x] **Update `docs/implementation-notes.md`.** Add a "Session replay"
        subsection (near the existing `Store::resume`-related notes around
        lines 206-247) describing: the `Context::hydrate` seam and why it
        bypasses `commit`; that hydration runs unconditionally (no
        resume/fresh flag) because a fresh session's file yields an empty
        read; the `Message` → `ChatEntry` projection function and its file
        location; the "no Pending/Running reconstruction" rule for orphaned
        tool calls, now satisfied by synthesizing a terminal `Message::
        ToolError` in `hydrate` (generic tools) or by cross-referencing
        `ChildStart`/`ChildResult` (`spawn_subagent`); the new persisted
        `summary`/`content_sections`/`duration` fields and the fixed
        (no-longer-hardcoded-zero) `push_tool_error` duration; the new
        `call_id`/`label` fields on `ChildStart`/`ChildResult` and why they
        exist (bubble correlation for replay); the lazy per-child transcript
        loading design (`needs_lazy_load`, `Store::session_path`, why it's
        lazy rather than eager); and the legacy-session degraded-replay
        behavior for both the tool-bubble fields and the child-run fields.

14. [x] **Tests — schema round-trip.** Add/extend unit tests in
        `src/agent.rs`'s test module (near `push_tool_result_and_error_append_messages`,
        line ~2812) asserting that a `Message::ToolResult`/`ToolError` built
        with non-empty `summary`/`content_sections`/`duration` serializes to
        JSON and deserializes back losslessly, and that an old-format JSON
        line (hand-written, missing these fields) still deserializes
        successfully with empty/zero defaults (proves the `#[serde(default)]`
        backward-compatibility contract).

15. [x] **Tests — `Context` hydration round-trip.** Add a unit test (near
        `messages_returns_immutable_read_back_slice`, `src/agent.rs` ~line
        2904, or alongside the store's own `Context`-based tests in
        `src/store.rs` ~lines 1157/1201/1230) asserting: (a) hydrated
        messages appear in `context.messages()` in the given order, appended
        after the two default system prompts; (b) hydrating does not call
        `Store::record_message` (using whatever test double/mock pattern the
        existing store tests already use) — a second flush/resume must not
        double every line.

16. [x] **Tests — end-to-end replay.** Extend
        `tests/store_persistence_e2e.rs` (which already spawns the real
        compiled binary, see `spawn_ylva` at line ~94) with a test that: (a)
        runs the binary once to produce a session with one completed turn
        including at least one tool call; (b) resumes that same
        `--conversation-id` for a second run with a new user message; (c)
        asserts, via the mock LLM server's captured request body (see
        `start_mock_llm_server`, line ~205, and how
        `conversation_id_appends_to_the_existing_session_instead_of_overwriting_it`,
        line ~230, already inspects the persisted file), that the **second**
        run's outgoing request payload includes the first run's user/
        assistant/tool content. Also assert that after the second run's
        session file is reread, each line from the first run still appears
        exactly once (no duplication from hydration).

17. [x] **Tests — TUI projection.** Add a presenter/reducer-level unit test
        in `tests/tui_render_state.rs` (or alongside existing `AppState`
        tests in `src/tui_reducer_state.rs`'s test module — check which
        location the existing convention for this kind of state-shape test
        favors before adding) that builds a small `Vec<Message>` fixture
        covering a user turn, an assistant turn, and a completed tool
        call/result pair (with non-empty `summary`/`content_sections`/
        `duration` on the `ToolResult`), runs it through the step-8
        projection function directly, and asserts the resulting
        `chat_entries` sequence and content (including the tool bubble's
        `summary`/`content_sections`/`duration`) match expectations —
        without spinning up the real TUI event loop.

### Child-run replay extension (steps 18-24)

Added by a grilling-skill refinement round after the original 17 steps
above were drafted (see "Scope decision — child-run replay is now in
scope" in Context). These steps must land together with steps 1-17 in the
same phase — step 19's fix depends on step 18's schema addition, and steps
21-23 depend on step 19's corrected bubble state existing first.

18. [x] **Persist `call_id` and `label` on `ChildStart`/`ChildResult`.** In
        `src/store.rs`: add `call_id: Uuid` to `StoreEvent::ChildStart` and
        `StoreEvent::ChildResult`, and `label: String` to `ChildStart` only
        (mirrors where `SpawnRequest.label` already lives) — both
        `#[serde(default)]` (a random/nil default `Uuid` and empty `String`
        are fine for legacy lines; correlation for those records is simply
        unavailable, per the "legacy sessions" acceptance criterion).
        Thread the new values through `record_child_start`/
        `record_child_result` (`src/store.rs` ~lines 564-591) and their
        callers in `src/agent/child_agent_manager.rs`'s `spawn`/`start`
        (~lines 312-365, `call_id` is `request.call_id`, already in scope)
        and `record_child_result` (~line 246, `finish_result.call_id`,
        already in scope) — this is a mechanical plumbing change, no new
        control flow.

19. [x] **Fix up the root `spawn_subagent` bubble's terminal state on
        replay.** Add a `Store` read function (e.g.
        `Store::read_child_runs(path) -> Vec<ChildRunRecord>`, alongside
        `read_messages`) that re-parses the file's raw `StoreEvent`s (not
        just `Message`s) and pairs each `ChildStart` with its `ChildResult`
        by `id` (the run_id), producing one record per child run:
        `{ run_id, call_id, label, agent_type, task, status: Option<String>,
        result: Option<String>, sources: Vec<String>, duration:
        Option<Duration> }` (`duration` computed from
        `ChildResult.completed_at - ChildStart.created_at` when a result
        exists, `None` otherwise). In the step 8/9 TUI projection: for any
        `Message::ToolResult` whose `name` is
        `tools::spawn_subagent::SpawnSubagentTool::NAME`, look up its
        `run_id` (parsed from the persisted JSON `result` text,
        `{"status":...,"run_id":...}`) in the `read_child_runs` output
        instead of trusting the stale persisted `status` text, and project:
        - a matching record **with** a `status`/`result` (i.e. `ChildResult`
        existed) → `ToolCallState::Done` with `summary: [("status",
        status)]`, `content_sections` rebuilt the same way
        `FinishResult::content_sections` renders them (reuse that method
        by constructing an equivalent `FinishResult`-shaped value from the
        record, or factor a shared helper), and the computed `duration`;
        - a matching record **without** a `ChildResult` (orphaned — process
        died mid-child) → `ToolCallState::Error` (step 20) instead of
        `Done`;
        - no matching record at all (e.g. a legacy file predating step 18's
        `call_id`, so correlation is impossible) → fall back to today's
        plain projection of the persisted (stale) `Message::ToolResult`
        content, per the legacy-degrade acceptance criterion.

20. [x] **Orphaned child → Error, not Running.** In the same projection
        branch as step 19: when a `spawn_subagent`'s `run_id` has a
        `ChildStart` but no `ChildResult`, project it as
        `ToolCallState::Error { error: "Subagent did not finish before the \
        session ended.", title_arguments, duration: <ChildStart.created_at \
        to hydration time, or zero>, content_sections: vec![] }` — never
        `Pending`/`Running`, consistent with the plan's existing rule for
        orphaned tool calls in general.

21. [x] **Eagerly populate `ChildRunInfo` (not transcripts) at root hydration
        time.** In the step 9 call site (before the first frame), for every
        record `read_child_runs` returned with a resolvable `call_id`,
        insert a `ChildConversation` into `AppState.child_conversations`
        keyed by `run_id`, with `info: ChildRunInfo { run_id, bubble_id:
        call_id, agent_type, task, label }`, empty `messages`, and
        `needs_lazy_load: true` (the flag itself is added in step 23). This
        is free — it only reads data steps 19-20 already loaded
        from the root file, no child file is opened here. A `ChildRunInfo`
        is only insertable when `call_id` resolved (see step 19's
        "no matching record" fallback); otherwise no entry is added and that
        bubble is simply not drillable, per the legacy-degrade acceptance
        criterion.

22. [x] **Synthesize a visible `Message::ToolError` for orphaned generic
        tool calls during `Context::hydrate`.** After appending all
        hydrated messages (step 4), scan for any `Message::Assistant.tool_calls`
        entry whose `id` has no later `Message::ToolResult`/`ToolError` in
        the hydrated list, **excluding** `spawn_subagent` calls (handled by
        steps 19-20 instead, which always have at least the spawn-time
        `ToolResult`) — for each, append an in-memory-only
        `Message::ToolError { id, name, error: "Tool call did not complete \
        before the session ended.".to_string(), duration:
        Duration::ZERO, content_sections: vec![] }` (never persisted —
        `hydrate` already bypasses `commit`, so this falls out naturally).
        Mirrors the existing `AgentCommand::Stop` cancellation-resolution
        pattern (`src/agent.rs` ~line 1816) that does the same thing for
        children cancelled on live shutdown. Update the step 8 projection's
        "no `ChatEntry` at all" rule for orphaned tool calls: now that
        `Context::hydrate` always synthesizes a terminal `Message::ToolError`
        for these, the projection naturally produces a visible Error bubble
        for them like any other `ToolError` — no separate special-case is
        needed in the projection itself, only in `hydrate`.

23. [x] **Lazy per-child transcript loading on first drill-in.** Add
        `needs_lazy_load: bool` to `ChildConversation`
        (`src/tui_reducer_state.rs`): `false` for `start_child_run`'s
        live-creation path (unconditionally — live children never need
        lazy loading, the flag is simply inapplicable there, never
        "already loaded"), `true` for step 21's replay-discovered entries
        until first read. At the call site that handles a drill-in gesture
        (before invoking `switch_to_child_view`, ~line 1660 — the reducer
        itself stays pure/synchronous with no I/O, so do this in the
        surrounding presenter/dispatch glue that already has access to
        `chats_root`/`project_key`), check `needs_lazy_load`; if `true`,
        synchronously read `Store::session_path(chats_root, project_key,
        run_id)` via `Store::read_messages`, run it through the step 8
        projection function, populate `ChildConversation.messages`, and set
        `needs_lazy_load = false` — then proceed with the existing
        `switch_to_child_view` call unchanged. On a read error (corrupt/
        missing child file), `tracing::warn!` and treat as an empty
        transcript rather than failing the click, mirroring step 6's
        read-tolerance for root hydration.

24. [x] **Tests — child-run replay extension.** Extend the step 14-17 test
        additions with: (a) a `Store`-level unit test for
        `read_child_runs` pairing `ChildStart`/`ChildResult` by id,
        including the no-`ChildResult` (orphaned) case; (b) a TUI-projection
        unit test asserting a finished `spawn_subagent`'s bubble replays as
        `Done` with the child's real `status`/`content_sections`/`duration`
        (not the stale spawn-time text), and an orphaned one replays as
        `Error`; (c) a unit test on the new `Context::hydrate` orphaned-tool-
        call synthesis (step 22), asserting the synthesized
        `Message::ToolError` is in-memory only (never reaches
        `Store::record_message`) and that the model-facing
        `serialize_messages` payload has a valid `tool` response for every
        `tool_call_id`; (d) a reducer-level test for `needs_lazy_load`:
        a replay-discovered `ChildConversation` starts with
        `needs_lazy_load: true` and empty `messages`, and after the
        drill-in call site's lazy-load path runs once, `needs_lazy_load` is
        `false` and `messages` reflects that child's own transcript,
        without a second read happening on a repeat drill-in (assert via a
        call counter or similar on the test double, following whatever
        pattern step 15's store test double already uses); (e) extend the
        `tests/store_persistence_e2e.rs` end-to-end test (step 16) with a
        run that spawns a real child via the mock LLM server, lets it
        finish, resumes the parent, and asserts (via the compiled binary,
        not unit-level) that the resumed session's `spawn_subagent` bubble
        and its drill-in transcript are both fully populated.

## Implementation Hints

- `Store::read_messages(path: &Path) -> Result<Vec<Message>, StoreError>`
  (`src/store.rs` ~line 690) already filters a session file down to
  committed `Message`s in append order; it is a pure read (never repairs or
  mutates the file), matching §4H decision 6.
- `Store` currently exposes no public path accessor; `StoreInner.path`
  (`src/store.rs` ~line 331) holds the session file path used internally by
  `read_messages`'s callers today (e.g. `first_explicit_user_message`, ~line
  300).
- `Context::commit` (`src/agent.rs` ~line 578) is the **only** place that
  forwards messages to `Store::record_message`; `Context::hydrate` must push
  onto `self.messages` directly, bypassing `commit` entirely, or every
  hydrated line will be re-appended to the file on the very same run.
  `Context::new()` (`src/agent.rs` ~line 453) installs exactly two
  `Message::System` entries (`DEFAULT_SYSTEM_PROMPT` and
  `TOOL_PROTOCOL_SYSTEM_PROMPT`) before any hydration runs; hydrated
  messages must be appended after those two, never replacing them, per
  blueprint's explicit "two leading system prompts stay sourced fresh from
  the running binary" instruction.
- `AgentLoop::run`'s `tokio::spawn` closure (`src/agent.rs` ~line 1028) is
  where `Context::new()` and `context.set_store(store)` currently run for
  the root loop; the `/clear` reset path is at ~line 1943-1951
  (`fresh_context.set_store(new_store)`). Both are the two production call
  sites needing the new hydrate-after-set_store call; consider a small
  private helper (e.g. `hydrate_context_from_its_store(&mut Context)`) if
  the two call sites would otherwise duplicate the same three lines
  (read path, call `Store::read_messages`, call `context.hydrate`, log on
  error).
- `AgentEvent::ToolDone`'s handler (`src/agent.rs` ~line 1213) and
  `AgentEvent::ToolError`'s handler (~line 1266) already have `summary`,
  `duration`, and `content_sections` in scope locally (they are currently
  sent only to the `ui_tx`/`UiUpdate` channel, not to
  `context.push_tool_result`/`push_tool_error`) — this is the main place the
  new parameters get real values instead of placeholders.
- `ChatEntry` (`src/tui_reducer_state.rs` ~line 215) and `ToolCallState`
  (~line 165)/`UserMessageState` (~line 120) are the TUI's transcript view
  types; `AppState.chat_entries: Vec<ChatEntry>` (~line 1243) is the flat,
  session-lifetime list the presenter renders.
- `ToolCallRequest` (`src/agent.rs` ~line 346: `id: Uuid, name: String,
  arguments: Value, persisted_title_arguments`) is carried on
  `Message::Assistant.tool_calls`; its `title_arguments()` method
  (`src/agent.rs` ~line 365) returns the exact tool-specific projection
  captured when the call was created. Calls without title arguments carry an
  empty persisted projection. Reuse it rather than re-deriving title fields
  ad hoc.
- `src/llm.rs`'s `serialize_messages` (~line 199) already destructures every
  `Message` variant with `..`, so the new `Message::ToolResult`/`ToolError`
  fields need no change there and never leak into the LLM wire payload.
- `docs/blueprint.md`'s Phase 8 section is at lines ~2795-2853.
  `docs/persistence.md`'s "Resume contract" section is ~lines 184-199.
  `docs/features.md`'s Session Persistence section is ~lines 60-78.
  `docs/implementation-notes.md` already has a cluster of `Store::resume`-
  related notes around lines 200-247 to extend.
- Test conventions: `tests/store_persistence_e2e.rs` spawns the real
  compiled binary (`Command::new(env!("CARGO_BIN_EXE_ylva"))`, ~line 99)
  against an in-process mock LLM server (`start_mock_llm_server`, ~line 205)
  and already has a resume-focused test
  (`conversation_id_appends_to_the_existing_session_instead_of_overwriting_it`,
  ~line 230) whose pattern (spawn, resume, inspect persisted JSONL) the new
  hydration e2e test should follow. `tests/tui_render_state.rs` exists for
  TUI-state-shape assertions without a live terminal (`render.rs`/`main.rs`/
  `tui_widgets.rs` are excluded from coverage requirements per
  `docs/development-workflow.md` precisely because they need a live
  terminal — keep the new projection function itself terminal-free and
  testable, per that same convention).
- `make test` / `make lint` / `make test-coverage` (`docs/development-workflow.md`)
  are the verification commands to run before reporting this phase done;
  ≥80% line coverage is enforced on testable files.

### Child-run replay extension hints (steps 18-24)

- `Store::session_path(chats_root, project_key, session_id) -> PathBuf`
  (`src/store.rs` ~line 238) already builds a session's exact file path with
  no directory scan — a specific child's transcript is always at
  `session_path(chats_root, project_key, run_id)` once `run_id` is known
  from `ChildStart`, so lazy loading (step 23) needs no `list_sessions`-style
  discovery at all.
- `ChildAgentManager::spawn` (`src/agent/child_agent_manager.rs` ~line 312)
  already has `call_id`/`label` in scope at the exact point it calls
  `store.record_child_start(run_id, ...)` (~line 357) — step 18 is purely
  about passing those two extra values through, not deriving them from
  anywhere new. Likewise `record_child_result` (~line 246) already has
  `finish_result.call_id` in scope.
- `FinishResult::content_sections()` (`src/agent/child_agent_manager.rs`
  ~line 96) is the existing renderer for a child's `status`/`result`/
  `sources` into Markdown `Section`s — step 19's bubble fix-up should reuse
  this rendering logic (via a shared helper or by constructing an
  equivalent value from the persisted `ChildResult` record) rather than
  re-deriving it.
- The `AgentCommand::Stop` handler's cancellation loop (`src/agent.rs`
  ~line 1816) is the existing precedent for "synthesize a terminal
  `Message::ToolError`-shaped outcome for a tool call that would otherwise
  be left stuck mid-flight" — step 22's orphaned-generic-tool-call handling
  in `Context::hydrate` follows the same shape, just triggered by hydration
  instead of a live stop command.
- `switch_to_child_view` (`src/tui_reducer_state.rs` ~line 1660) is a pure,
  synchronous, in-memory reducer function today with no I/O — step 23's
  lazy read must happen in the calling/dispatching code *before*
  `switch_to_child_view` is invoked, not inside the reducer itself, to keep
  that function's existing purity/testability intact.
