# EdenAI: thinking bubbles not rendered

Status: idea / investigation report. No code changed.

## Symptom

A session on EdenAI with multiple subagents showed no thinking bubbles in the TUI.

## Findings

- Model: `azure/gpt-6-luna@eu` via `https://api.edenai.run/v3/chat/completions`,
  `reasoning_effort=high`. Root and subagent requests are all identical in this respect.
- The model did reason. Responses report `reasoning_tokens` (e.g. 88, 919).
- Streamed deltas carry an opaque `reasoning_items` entry with `encrypted_content`
  and `"summary": []`.
- Across all 1,947 EdenAI response chunks in the session, none contain a
  non-empty summary, `reasoning`, `reasoning_content`, or `reasoning_details` text.
- `parse_sse_payload` (`src/llm/openai_compatible_provider.rs`) recognises only
  `thinking_delta`, `reasoning_content`, and bare `reasoning` string fragments.
  `reasoning_items` is ignored, so no `StreamEvent::Thinking` is emitted and no
  `AppendThinking` reaches the TUI.
- Control case: OpenRouter dumps for `openai/gpt-6-luna` in the same session stream
  readable `reasoning` text. The parser and rendering path work when the provider
  sends text.
- Subagent path (read-only check, not run as a test): `AppendThinking` →
  `SubagentToken { is_thinking: true }` → `TurnBuffer::push_thinking_chunk` →
  `ChatEntry::ThinkingMessage`. Wiring looks correct.

**Root cause:** EdenAI does not send readable reasoning text for this model in this
session. The TUI has nothing to render.

## Proposed solutions

1. **Parse `reasoning_items[].summary[].text` when non-empty.** Small and harmless.
   Does not fix this session, since summaries are empty.
2. **Request reasoning summaries from EdenAI.** The Responses path already sends
   `reasoning.summary = "auto"` (`src/llm/openai_responses.rs`); the Chat Completions
   request does not. Need to confirm whether EdenAI accepts an equivalent parameter.
   Preferred option.
3. **Placeholder bubble when reasoning is hidden.** When reasoning tokens are
   reported but no text arrives, show e.g. "reasoning (encrypted, N tokens)". Makes
   hidden reasoning visible without inventing content. Fallback if 2 is unsupported.

## Open questions

- Does EdenAI's chat-completions endpoint accept a reasoning-summary parameter, and
  under what name?
- Is the empty `summary` a property of the `azure/...@eu` deployment, or of all
  EdenAI routes for this model?

## Next steps

- Check EdenAI documentation for reasoning-summary support on chat completions.
- Decide between 2 and 3 (or both) before any implementation. Implementation would
  need a plan under `docs/plans/` and updates to `docs/provider-notes.md`.
