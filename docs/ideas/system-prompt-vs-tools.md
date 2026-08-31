# Guiding LLM on tool usage and cross-tool best practices

## Problem

Models vary widely in how efficiently they use tools. The most common
failure modes observed:

- **Full-file reads instead of search.** The model uses `read_file` to look
  for a symbol or pattern instead of `search`, pulling large files into the
  context window unnecessarily. Each file read is one tool-call round, so a
  task that touches many files inflates context and burns through the
  iteration limit quickly.

- **Sequential reads that could be parallel.** The model reads files one at a
  time even when the reads are independent (e.g. tracing a module tree where
  sibling modules could be fetched in one batch).

- **Ignoring `offset`/`limit`.** The model reads an entire file when only a
  small region is relevant (e.g. just the first few lines of a config file or
  a specific function).

- **Re-reading files already in context.** Particularly with weaker models,
  the model calls `read_file` on content it received in a prior round.

Whether this is a model capability issue or a prompt/description gap is hard
to disentangle. Stronger models (GPT-4-class and above) tend to self-select
the right tool without guidance; smaller or quantised models often default to
the most literal interpretation of the task ("read the file").

## Options for improvement

### 1. System prompt guidance

Add an efficiency section to `DEFAULT_SYSTEM_PROMPT` (or to the per-profile
`persona` field) that explicitly names preferred patterns:

```text
Prefer search over read_file when locating symbols, declarations, or
patterns — only read a file when you need its full content. Use
offset/limit on read_file when only a region is relevant.
```

**Pro:** highest-weight instruction channel; models follow system-prompt
directives reliably.  
**Con:** increases token cost on every request; doesn't travel with the tool
if the prompt is swapped out.

### 2. Tool description nudges

Add self-contained usage tips inside each tool's description:

- `read_file`: "Use `offset`/`limit` when only a region is needed. Prefer
  the `search` tool when you only need to locate a pattern rather than read
  content."
- `search`: "The preferred first step for locating symbols, declarations, or
  patterns across files — avoid reaching for `read_file` just to check
  whether something exists."

**Pro:** guidance travels with the schema; works even when the system prompt
is replaced.  
**Con:** cross-tool preference ("use X not Y") is awkward to express inside a
single tool's description; models weight schema text less than system-prompt
text.

### 3. Both

Use tool descriptions for self-contained, tool-specific tips (offset/limit,
output caps, glob filtering) and the system prompt for cross-tool behavioral
rules (search-first, batching independent reads).

## Recommendation

Implement option 3 as a low-risk incremental improvement:

1. Update `read_file` and `search` descriptions with the nudges above.
2. Append a short "Tool use" paragraph to `DEFAULT_SYSTEM_PROMPT`.
3. Observe whether the iteration-heavy prompt in `docs/ideas/sample-prompts.md`
   (module-tree tracing) completes within the limit with a capable model.

The gain will be most visible with mid-tier models. For genuinely weak models
with limited tool-call reasoning, prompt guidance alone is unlikely to fully
compensate — model selection remains the primary lever.
