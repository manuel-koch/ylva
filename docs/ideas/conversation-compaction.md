# Conversation compaction

Planned in [the blueprint finalization plan](../plans/blueprint-finalization-plan.md).
The design choices below are settled. Implementation must preserve the full
session transcript while giving the model a compacted view.

## Problem

Every committed message stays in `Context.messages` for the life of the
session and is re-sent on every request. Consequences:

- Long conversations grow past `context_length`; the provider rejects the
  request (400/413) and the turn dies (blueprint §8 pitfall 3).
- Every old message is paid for on every later request.
- Tool results dominate: they are re-sent verbatim (the per-result backstop
  is `tool-result-size-guard.md`; the aggregate is this idea).

What exists today:

- `Context` is a plain `Vec<Message>` — no tiers, no token counts, no
  compaction; the blueprint explicitly defers all of it to Phase 12.
- `compact_engine` in the profile resolves (falling back to `default_engine`)
  and is validated at startup, but has no runtime consumer yet.
- `context_length` is already resolved per engine: profile value, then the
  provider `/models` catalog, then the 128 000 default
  (`docs/configuration.md`).
- The session file is append-only: resume never rewrites it
  (`docs/persistence.md`, *Resume contract*).
- The only way to shed context today is `/clear`, which discards everything
  and starts a fresh session with no link back.

## Selected design

### Token estimates and thresholds

- Cache an approximate token count per message when it enters the context.
  Use `tiktoken-rs` for OpenAI-compatible models and a four-characters-per-token
  estimate for Ollama. These counts are for budgeting, not billing.
- Start background summarization after a completed turn when usage reaches
  about 70% of the active engine's context window.
- Apply a completed summary at the next safe turn boundary. At 90%, do not wait
  for the background task. Drop old complete turns or oversized old tool
  results synchronously until usage is at or below the target.
- The default target is 50% of the active engine's context length. Expose
  `compact_target_ratio` per engine. Keep the 90% hard trigger fixed.
- Re-evaluate the budget when `/models` switches to a model with a different
  context length.

The profile already resolves `context_length` from the engine configuration,
the provider catalog, or the 128,000-token default. `compact_engine` already
selects the engine for summary requests.

### Summary and live context

The summarizer receives a snapshot of an old context prefix. It must preserve
decisions, outstanding work, file paths, identifiers, user constraints, and
where the verbatim tail begins. It has no tools and never mutates `Context`.
The outer agent loop applies the result at a natural boundary. If summarization
fails or times out, the loop logs the failure and uses the synchronous drop
fallback.

Only root conversations compact in this implementation. Child runs are
short-lived and one-shot. There is no manual `/compact` command yet.

### Persistence and replay

Append a compaction event to the existing JSONL session. Do not start a new
session or rewrite earlier messages. Each event records a monotonically
advancing message boundary and the summary for the prefix through that
boundary. A later event supersedes the previous model-context snapshot; replay
uses the latest event and keeps messages after its boundary as the verbatim
tail.

The TUI and persisted session retain every original message. The TUI shows a
compaction marker at the boundary. The model receives a provider-safe marked
summary followed by the recent tail. Do not insert a mid-conversation
`system` message, since some provider chat templates reject it.

### Tool protocol constraints

- Never compact between an assistant tool call and its matching result. A
  boundary must include the complete call/result group.
- Compact only when the agent has reached a safe turn boundary and the serial
  tool queue is empty.
- A steer does not cancel or descope pending tool calls. Every issued
  `tool_call_id` must receive exactly one tool result before later user content
  enters the model context.
- Preserve the raw transcript even when an old result is dropped from the
  model-facing view.

## Invariants compaction must keep

- **Sole writer.** Only the outer agent loop mutates `Context` (blueprint §4C
  HARD rule). A background summarizer must never touch it; its result is
  applied by the loop, like a steer at a natural point.
- **Wire pairing.** Every `tool_call_id` needs exactly one result in the next
  request. A compaction cut must fall between call/result pairs, or
  synthesize errors the way `Context::hydrate` already does for orphans.
- **System tiers.** The two rebuilt system prompts are never persisted and
  never compacted; `InitialContext` content is per blueprint never dropped
  initially.
- **Append-only durability.** Session files are written once, never truncated
  or rewritten; a crash mid-compaction must leave a readable file.
- **Record vs model view.** The transcript (disk and TUI) is the human
  record; the model context is a view of it. Divergence is acceptable only
  when deliberate and recoverable on resume.
- **Recent tail verbatim.** The model needs the latest turns as they
  happened; only the old prefix is a summarization candidate.

## Persistence options considered

### Option 1 — session rollover with back reference

Summarize the old prefix, start a new session file whose first content is the
summary, and link the two files.

- Precedents: `/clear` already rolls over to a new file
  (`Store::start_new_session`); child transcripts already link via
  `meta.parent_session` from both ends (`docs/persistence.md`).
- Needs a new link field/event (for example `compacted_from` in the new
  `meta`, plus a forward-pointer event appended to the old file). Reusing
  `parent_session` would mislabel it as a child-agent run.
- The old file stays byte-for-byte intact; each file's disk usage stays
  bounded.
- Costs: `--conversation-id <old>` must follow the chain to the head, or
  hard-error with a hint (the resume contract prefers hard errors); the
  session list must mark superseded heads; the TUI transcript loses the full
  history unless it follows the link too. TUI input history already spans
  sessions, so it is unaffected.

### Option 2 — compaction event inside the session (selected)

Keep one session, one id. Append a new `StoreEvent::Compaction { boundary,
summary, … }` line; on hydrate, messages `[0..boundary)` are replaced by the
summary before the rest.

- The append-only contract is untouched: old messages stay on disk as the
  record, and the event only changes the view rebuilt at replay.
- Resume, `--conversation-id`, session listing, and locking are unchanged.
- The TUI can keep rendering the full transcript while the model sees the
  compacted view — record vs view, explicitly.
- Disk still grows; bounding it is the separate file-compression concern of
  §4H decision 11 — see *Terminology* below.
- Costs: `Store::read_messages` / `Context::hydrate` grow a replay rule, and
  repeated compactions need supersession semantics (boundaries must be
  monotonic) or a chain of events.

### Option 3 — wire-level view only

Compact purely at request-build time and persist nothing new. Rejected: the
summary would have to be recomputed on every resume (a model call at startup)
or lost. The summary is conversation state and must persist somewhere, which
collapses into option 2.

**Selected:** option 2. The session id remains stable, old messages remain in
the append-only record, and replay reconstructs the latest model-context
snapshot.

## Scheduling options considered

### Option A — hard boundary

At the trigger the conversation stops: no new requests (prompts queue, as
steers already do) until the summary is applied. Simplest consistency — one
context state at all times — but the user stares at a stall, and a failed
summarizer stalls worse.

### Option B — async with prefix swap

1. At the trigger, snapshot a **boundary index** and send the prefix
   `[0..boundary)` to the summarizer (`compact_engine`) while the
   conversation keeps running on the full context.
2. When the result returns, the loop applies it **at the next natural point**
   (the same injection discipline as steers): the model view becomes the
   leading system messages, a provider-safe marked summary, and
   `messages[boundary..]`. This replaces only the model-facing prefix; the
   transcript stays intact and messages committed after the snapshot survive.
3. In-flight streams are safe by construction: a stream holds a
   `Vec<Message>` clone taken at request start and only returns its own
   output, which appends fine after the swap.

Cost: between trigger and swap the context keeps growing. If it reaches the
hard limit while the summarizer is still out, something must give — gate then
(option A as last resort), or drop synchronously first
(`find_oldest_droppable`, the blueprint's original Phase 12 algorithm) and
let the summary replace those drops later.

### Option C — proactive summary with a hard backstop (selected)

Start compaction when a turn completes and usage crosses a *soft* threshold,
so it usually finishes in the background before the *hard* threshold. At the
hard threshold, drop old complete turns or oversized old results synchronously.
This combines the unblocked UX with a hard limit.

**Selected:** option C. Start the summary after a completed turn at the soft
threshold. Apply it at a safe point. The synchronous drop path is the hard
backstop at 90%.

## Sweep spot

Blueprint §4C already fixes the core (hysteresis, ratios instead of absolute
counts):

- **Hard trigger:** `total > context_length × 0.9`.
- **Target:** `≤ context_length × compact_target_ratio` (default 0.5),
  minimal intervention, preferring an oversized tool result over whole turns.
- Counts are cached per message at insertion (`tiktoken-rs`, byte heuristic
  for Ollama), so evaluating the trigger after each `Context::commit` is O(1).

The soft threshold is 70%. The hard threshold is 90%. The target ratio is
configurable per engine and defaults to 0.5. Keep the thresholds fixed for now.

Token estimates remain approximate because encodings vary by model family and
the Ollama fallback is a heuristic. If the summarizer's own context window
proves too small, add hierarchical summarization only after measuring that
failure. No `max_drop_ratio` guard or disable switch is planned now.

## What the summary must preserve

Decisions taken, open tasks, file paths and identifiers, user constraints,
and where the verbatim tail starts. It should carry a marker so the model can
tell it apart from user speech (the way `HARNESS_NOTICE_MARKER` does for
harness notices), and the cut point must land between tool call/result pairs.

## Failure handling

Summarizer errors, timeouts, or an unreachable `compact_engine` must never
leave the conversation wedged above the limit: fall back to synchronous
dropping (blueprint's original `find_oldest_droppable`), log the fallback,
and let the next sweep retry the summary.

## Terminology

§4H decision 11's "auto-compression" compacts old session **files** (gzip
plus `.meta.json` sidecar) — unrelated to this idea. Keep the terms apart:
*context compaction* (this idea, model-facing) vs *file compression*
(storage-facing).

## Related

- `docs/blueprint.md` §4C (tiered context, hysteresis, token counting) and
  Phase 12 (the checklist this idea refines)
- `docs/ideas/tool-result-size-guard.md` (per-result wire guard; defers the
  aggregate to this idea)
- `docs/configuration.md` (`compact_engine`, `context_length`)
- `docs/persistence.md` (append-only contract, resume, `parent_session`)

## Deferred

- A manual `/compact` command.
- Compaction for child-agent contexts.
- Hierarchical summarization if the compact engine cannot accept the selected
  prefix.
- Per-result wire-size protection and the default `glob` ceiling. See
  [`tool-result-size-guard.md`](./tool-result-size-guard.md).

No new architecture decision record is planned for the initial implementation.
