# Improved rendering (large expanded bubbles)

Expanding a tool bubble with very large content (e.g. a `read` of a
>3000-line file) causes visible lag in scrolling and key/mouse
responsiveness, with the process pinned at ~100% CPU while the bubble is
expanded.

## Root cause

The chat view is fully re-flowed on every frame, not just on content
changes:

- The event loop (`tui_driver.rs`) calls `draw_frame` once per loop
  iteration — on every key, mouse move, spinner tick, or agent update.
- `draw_frame` → `render::render` → `tui_widgets::draw` rebuilds the
  *entire* conversation from scratch each time: every `ChatEntry` is
  re-wrapped via `render_markdown`/`append_*_lines`, including every
  currently-expanded tool bubble.
- `render_markdown` (`src/markdown.rs`) runs a full `pulldown-cmark` parse +
  `textwrap` wrap + `Line`/`Span` build on the whole string on every call.
  The only cache in the pipeline, `highlighted_lines_cached`
  (`src/highlight.rs`), memoizes syntax highlighting of fenced code blocks
  by content hash — it does not cover the markdown-to-`Line`
  conversion/wrapping that dominates cost for a large plain/markdown blob.
- There is no viewport virtualization: off-screen lines of a huge expanded
  bubble are fully materialized (parsed, wrapped, styled) even though only
  a fraction is visible.

So a mouse-move or key event — which fires continuously while scrolling —
triggers a full re-parse+re-wrap of a multi-thousand-line bubble every
frame, and the event loop can't process the next input until that
synchronous work finishes.

## Suggested changes

1. **Memoize per-entry rendered lines.** Cache the `Vec<Line>` output of
   `render_markdown`/`append_tool_bubble_lines` keyed by
   `(entry_id/version, content_hash, wrap_width, collapsed)`. Recompute an
   entry only when its content, width, or collapsed flag actually changes —
   not on every draw. Highest-impact fix.
2. **Decouple "frame redraw" from "layout rebuild."** Track a dirty flag on
   `ConversationListState`; skip the full rebuild loop when nothing
   content-relevant changed (e.g. a pure mouse-move for hover highlighting),
   recomputing only the small bit of styling that depends on hover/selection.
3. **Virtualize large bubbles.** Compute a cheap line-count/height estimate
   for scrollbar math without full styling, and only run `render_markdown`
   on lines within the visible viewport (+ small overscan), similar to list
   virtualization. Bounds per-frame cost regardless of content size.
4. **Extend caching to the markdown pass itself**, not just syntax
   highlighting — content-address `render_markdown(content, width)` output
   directly.
5. **Optional UX safeguard:** cap what's rendered by default for very large
   tool results (e.g. "showing first/last N lines — press to view full"),
   independent of the collapse feature, so a single large file read can't
   degrade the whole session.

Start with #1 (memoization cache) — it directly fixes the reported
CPU/lag with the least architectural change.
