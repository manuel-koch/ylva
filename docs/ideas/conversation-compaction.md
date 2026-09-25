# Conversation compaction

Not implemented. Blueprint §4C and Phase 12 already fix the in-memory shape
(tiered messages, cached token counts, hysteresis trigger); this idea covers
what the blueprint leaves open: where the compacted conversation lives, how it
relates to the ongoing session, whether compaction blocks the conversation,
and where the sweep point sits. It is also the "total request size" half that
`tool-result-size-guard.md` deliberately defers (its *Options considered*,
option 4).

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

## Where the compacted conversation lives

The "spawn a new session with a back reference?" question.

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

### Option 2 — compaction event inside the session (tentative lean)

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

**Open:** option 1 vs option 2. Option 2 leans ahead because it satisfies
every invariant without touching the resume contract; option 1 wins only if
per-file disk bounding and "one session = one compaction era" semantics are
judged more important than id stability.

## When compaction runs

The hard-boundary and async-swap questions.

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
   (the same injection discipline as steers): `messages = [system…, summary,
   messages[boundary..]]` — a prefix replacement, never a `ClearContext`-style
   whole-context swap, so messages committed after the snapshot survive
   untouched.
3. In-flight streams are safe by construction: a stream holds a
   `Vec<Message>` clone taken at request start and only returns its own
   output, which appends fine after the swap.

Cost: between trigger and swap the context keeps growing. If it reaches the
hard limit while the summarizer is still out, something must give — gate then
(option A as last resort), or drop synchronously first
(`find_oldest_droppable`, the blueprint's original Phase 12 algorithm) and
let the summary replace those drops later.

### Option C — proactive idle sweep with a hard backstop

Start compaction when a turn completes and usage crosses a *soft* threshold,
so it usually finishes in the background before the *hard* threshold; at the
hard threshold fall back to option A or to synchronous dropping. Combines B's
unblocked UX with A's guarantee, at the cost of two thresholds and a little
more state.

**Open:** A vs B vs C. C leans ahead: B without a backstop can overrun the
window, A alone stalls on every compaction.

## Sweep spot

Blueprint §4C already fixes the core (hysteresis, ratios instead of absolute
counts):

- **Hard trigger:** `total > context_length × 0.9`.
- **Target:** `≤ context_length × compact_target_ratio` (default 0.5),
  minimal intervention, preferring an oversized tool result over whole turns.
- Counts are cached per message at insertion (`tiktoken-rs`, byte heuristic
  for Ollama), so evaluating the trigger after each `Context::commit` is O(1).

Remaining choices:

- **Soft trigger** (for option C): an earlier ratio — around 0.7 — at which a
  background sweep starts after a turn completes. The exact number is a
  calibration detail for the plan, not a decision now.
- **Config surface:** `context_length` is already the override lever (profile,
  then catalog, then 128 000). Whether trigger/target ratios also need profile
  config (blueprint says per-`EngineConfig`), plus a disable switch and a
  manual `/compact` slash command, are open — decide in the plan.
- **Estimation error:** tiktoken is model-family dependent and the Ollama byte
  heuristic is rough; the 0.9 margin absorbs it. If compaction misbehaves,
  lower the trigger or override `context_length` instead of adding
  guardrails (the blueprint's YAGNI note on `max_drop_ratio` still applies).
- **Model switch:** `/models` to a smaller window changes the effective
  `context_length` immediately — re-evaluate the trigger on the switch, not
  only on the next commit; the new window may already be over the hard
  threshold.
- **The summarizer's own window:** the prefix sent to `compact_engine` can
  itself exceed that engine's window on very long sessions. Hierarchical
  (bucket-then-merge) summarization is the escape hatch; defer until measured.
- **Subagents:** child contexts are short-lived and one-shot; compaction is a
  root-session concern only (open to confirmation).

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

## Open questions

1. Where does the compacted conversation live: session rollover with a back
   reference, or a compaction event inside the current session?
2. Blocking model: hard boundary, async prefix swap, or proactive sweep with
   a hard backstop?
3. Trigger ratios: hard 0.9 / target 0.5 per blueprint — is a soft sweep
   threshold, and profile-level config knobs for it, warranted?
4. Does the TUI keep rendering the full transcript while the model sees the
   compacted view, and how is that divergence shown to the user?
5. If option 1: does `--conversation-id` follow a rollover chain, or is a
   superseded session a hard error with a hint?
6. Is a manual `/compact` slash command in scope?

## Next step

Resolve the open questions, then turn the outcome into a plan
(`plan-software-change` skill, `docs/plans/`) that updates
`docs/blueprint.md` Phase 12, `docs/features.md`, `docs/configuration.md`,
`docs/persistence.md`, `docs/implementation-notes.md` and `CONTEXT.md` in the
same commit. The persistence-shape choice (open question 1) likely warrants
an architecture decision record.
