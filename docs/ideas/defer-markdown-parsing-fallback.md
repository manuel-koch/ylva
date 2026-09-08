# Fallback: defer Markdown parsing until streaming is Done

`docs/blueprint.md` §4F originally prescribed rendering the partial
assistant/thinking message as plain text while streaming, deferring
`pulldown-cmark`/`syntect` parsing until the message is `Done`, purely as a
precaution against per-frame parsing cost. This was never implemented and
was explicitly dropped during Phase 7 planning: the shipped behavior (live
`render_markdown` on the growing partial buffer every frame, with `syntect`
code-fence highlighting served from `src/highlight.rs`'s content-addressed
`highlighted_lines_cached`) already works without reported jank, and Spike 1
(§0) never actually measured `pulldown-cmark`/`syntect` cost during
streaming — only raw token throughput.

**Only revisit this** if a real, measured frame-time regression appears
during streaming (e.g. via `tracing`-logged frame times, or user-reported
jank on very long/code-heavy messages). If revisited:

- Prefer a narrower fix first — e.g. cap re-parsing frequency (re-parse at
  most every N ms instead of every frame) rather than dropping to plain
  text, which is a worse user experience.
- Only fall back to "plain text while streaming, full parse on `Done`" as a
  last resort, and scope it to the specific slow case identified (e.g. only
  while an open, still-growing code fence is present) rather than the whole
  message.
