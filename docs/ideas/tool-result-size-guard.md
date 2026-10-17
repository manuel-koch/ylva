# Wire-level tool-result size guard, and a bounded `glob`

Deferred for later implementation. This is a per-result wire guard plus a
bounded `glob`; aggregate context compaction is covered separately by
[`conversation-compaction.md`](./conversation-compaction.md).

## Problem

The harness already admits tools against the remaining context budget and
bounds their model-facing results and recovery projections; see
[context management](../features.md#context-management-and-compaction).
`glob` also applies a runtime output-token allowance. These protections do
not provide an independent fixed-byte serialization guard or a default
hard path-count/byte ceiling for `glob`.

Tool-specific source bounds are separate from the model-view budget:

| tool | output bound |
|---|---|
| `read` | 256 KiB raw selected bytes; 8 MiB whole-media ceiling (errors above) |
| `bash` | 1000 lines / 64 KiB |
| `search` | 1000 lines / 64 KiB |
| `glob` | no default hard path-count/byte ceiling; optional `limit` and runtime token allowance |
| `edit` diffs, child `agent_finish`, future tools | none |

Consequences:

- **`glob` without `limit`** can collect a large tree before the runtime
  allowance truncates its returned text. A default source ceiling would
  bound this independently of the active context window.
- Token-budget admission is advisory, not an exact provider-token or
  serialized-byte guarantee. A fixed wire backstop would turn an oversized
  envelope into a clear per-call error before dispatch.
- Canonical tool results remain intact in the transcript. Their model
  projections may be reduced or omitted from later context, but the serializer
  has no independent byte ceiling for whichever projection reaches it.
- Complete Base64 media can be much larger than ordinary text. Existing
  context projections keep binary payloads whole or omit them with an explicit
  notice; the proposed envelope guard must remain compatible with that policy
  and measure after modality-marker substitution.

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
- The threshold is a byte `const`, independent of the tokenizer and model
  context window. **512 KiB** of serialized
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

Aggregate request budgeting already belongs to
[conversation compaction](./conversation-compaction.md), which changes the
model view without rewriting the transcript. This guard deliberately covers
one serialized tool envelope at a time.

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
- Threshold: **512 KiB** of serialized envelope content, as a byte
  `const` measured after marker substitution. It bounds the wire, not the
  context: most LLMs would drown well below it, so a result reaching it means
  the producing tool is misconfigured and should be capped at the source.
- **`glob` clamp**: an explicit `limit` above the ceiling is clamped to the
  ceiling; `truncated: true` reports the cut.
- **Request total is out of scope** — existing conversation compaction and
  result allowances cover aggregate model-view budgeting.

## Preserve the canonical transcript

Truncating every tool result to roughly 8–16 KiB when inserting it into history
is not selected for this idea: it would change the persisted result as well
as the model-facing result. The
later implementation should keep the 512 KiB serialization-time failure guard
and bound `glob` output, while context compaction handles the aggregate model
view without rewriting the transcript. The exact `glob` ceiling remains to be
chosen when this deferred work is planned.

## Store oversized tool output to temp file for later retrieval

Copilot seems to handle oversized tool results in a different way.
It truncates the result and provides LLM with a hint that the full
tool result can be accesed from a temporary file:

Here a `bash` tool call created oversized result and Copilot instructed
LLM how to retrieve parts of the oversized result using other tools:

```text
Output too large to read at once (626.9 KB). Saved to: 
/var/folders/4l/pq2frs651d1ccv5rtxxmwjww0000gn/T/17920782216
86-copilot-tool-output-59363-59e04ef8-c03d-40a7-9846-495f08d
353e5.txt
Consider using tools like rg (for searching), head/tail (for
 viewing start/end), view with view_range (for specific 
sections), or jq (for JSON) to examine portions of the 
output.
Preview (first 500 chars):

<truncated tool result output>
```

Ylva could maintain a temporary directory bound to current session/runtime.
Cleanup of the temp dir could happen whenever handling of a session ends
( quitting ylva, `/clear` current session, `/session` resume or clone another
session ).
If LLM is actually interrested in reading the result, it could trigger a corresponding
`read`, `search` or `bash` tool call afterwards.

We could apply this oversized tool result handling for various tools:
`bash`, `glob`, `search`, `web_fetch`, `web_search`, result of a subagent.
