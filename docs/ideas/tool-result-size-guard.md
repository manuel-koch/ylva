# Wire-level tool-result size guard, and a bounded `glob`

Deferred for later implementation. This is a per-result wire guard plus a
bounded `glob`; aggregate context compaction is covered separately by
[`conversation-compaction.md`](./conversation-compaction.md).

## Problem

Every tool result is serialized into each request verbatim
(`serialize_tool_result`, `src/llm/openai_compatible_messages.rs`), and only
some tools cap what they return:

| tool | output bound |
|---|---|
| `read` | 256 KiB raw selected bytes; 8 MiB whole-media ceiling (errors above) |
| `bash` | 1000 lines / 64 KiB |
| `search` | 1000 lines / 64 KiB |
| `glob` | **none** — `limit` is optional, omitted means unlimited |
| `edit` diffs, child `finish`, future tools | none |

Consequences:

- **`glob` without `limit`** can return a megabyte of paths (`**` over a large
  tree), and its `truncated` metadata only appears when `limit` was set — an
  oversized result is silently complete, with no signal to the model.
- A too-large request is rejected by the provider (400/413): the whole turn
  dies on an opaque error the model cannot act on, instead of a tool failure
  it could recover from by narrowing the call.
- Tool results are re-sent on **every** later request, so one oversized
  result inflates the rest of the session.
- `read` on a model without the media modality keeps the complete Base64 in
  the tool message (see the `read` bullet in `docs/features.md`): the 8 MiB
  ceiling still lands inline as ~10.7 MB ≈ 700k tokens. On a capable model
  the same read projects to an `[image attached]` marker and stays small.

## Options considered

### 1. Guard at message build (favored — shape decided)

Substitute the payload inside `serialize_tool_result` — the single choke
point shared by the OpenAI-compatible and Ollama providers, and the only place
that sees the final size after modality projection and marker substitution.

- **Substitute, never omit**: both wire protocols require exactly one tool
  result per `tool_call_id`.
- **Report the substitution as a failure**: reuse the existing
  `ToolEnvelope { ok: false, error }` shape (what real tool errors emit),
  naming the tool, the size, the limit, and a narrowing hint
  (`offset`/`limit`, `pattern`, `limit`) — not an `ok: true` marker swap.
- Keep the persisted `Message::ToolResult` and the TUI/CLI output intact —
  only the request changes, deterministically on every build.
- Measure the serialized envelope after marker substitution; exclude
  attachment parts, which are separate messages bounded by provider limits
  and `READ_MEDIA_MAX_BYTES`.
- The threshold is a byte/char `const`: there is no tokenizer, and
  `ModelInfo` carries no context-window size. **512 KiB** of serialized
  envelope content — a backstop below which results are assumed fine, not a
  budget: 512 KiB is still far more than most LLMs can usefully absorb, so a
  tool that routinely reaches it is itself a bug.

### 2. Substitute at tool completion

Turn an oversized result into `Message::ToolError` before persisting: smaller
stored transcript and model/UI agreement — but the human loses the real
output, and a tool that succeeded is reported as failed. Rejected in favor
of option 1.

### 3. Per-tool caps only

Close the gaps tool by tool (see below) and rely on the domain-specific
remedies each cap carries. Cheap and precise, but no protection for
unbounded or future tools, and it leaves the inline-Base64 case unfixed.

### 4. Total request size guard — out of scope here

Sums the whole history. Dropping or rewriting old messages breaks the
transcript, so this belongs to **conversation compaction**, to be written up
as a separate idea; this guard deliberately covers one tool result at a
time.

## Related: bound `glob` output

Give `glob` the treatment `search` and `bash` already have:

- A hard ceiling applied when `limit` is omitted or set above it (precedent:
  `SEARCH_OUTPUT_LIMIT_LINES` / `SEARCH_OUTPUT_LIMIT_BYTES`), reported as
  `truncated: true` in result metadata. An explicit `limit` above the ceiling
  is **clamped** to it, not rejected — the ceiling is a bound, not a policy
  the model can violate into an error.
- Choose the unit: a path-count cap is the natural primary (paths are long
  enough that count and bytes track each other), with a byte cap as backstop.
- Disclose the ceiling in the tool description and in the `glob` bullet of
  `docs/features.md`.
- Tests: omitted `limit` over a fixture tree larger than the ceiling →
  capped with `truncated: true`; an explicit `limit` above the ceiling →
  clamped to the ceiling; an explicit `limit` below it stays unchanged.

## Decisions

- Report the substitution as an **`ok: false` failure** (option 1) — never an
  `ok: true` marker swap, never an omission.
- Threshold: **512 KiB** of serialized envelope content, as a byte/char
  `const` measured after marker substitution. It bounds the wire, not the
  context: most LLMs would drown well below it, so a result reaching it means
  the producing tool is misconfigured and should be capped at the source.
- **`glob` clamp**: an explicit `limit` above the ceiling is clamped to the
  ceiling; `truncated: true` reports the cut.
- **Request total is out of scope** — that is conversation compaction, to be
  written up as a separate idea.

## Relationship to the blueprint's insertion cap

The blueprint also proposed truncating every tool result to roughly 8–16 KiB
when inserting it into history. That behavior is not selected for this idea:
it would change the persisted result as well as the model-facing result. The
later implementation should keep the 512 KiB serialization-time failure guard
and bound `glob` output, while context compaction handles the aggregate model
view without rewriting the transcript. The exact `glob` ceiling remains to be
chosen when this deferred work is planned.
