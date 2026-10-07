# Enhanced Markdown rendering

Ylva renders common Markdown structure in chat bubbles, but model responses can
also use extensions such as inline math. For example, `$\approx 40$ tokens` is
intended to show "approximately 40 tokens" with the expression marked as
mathematics. Today the dollar signs and backslash command are displayed as
literal text.

## Use case

Make common model-authored Markdown easier to read in the TUI without hiding or
changing the response text. Inline math is one concrete gap: `$...$` is a common
Markdown convention for LaTeX-style math, though it is not part of CommonMark.
Other math-enabled Markdown tools may also use `\(...\)` for inline math and
`$$...$$` or `\[...\]` for display math.

The terminal cannot assume it can render a typeset equation. Even without
typesetting, recognizing a math span and styling it consistently could help
separate expressions from surrounding prose. For simple expressions, a future
renderer might also display a few well-understood commands as Unicode, such as
`\approx` as `≈`, provided it preserves the original meaning and avoids
misleading partial conversions.

## Current behavior

`src/markdown.rs` parses responses with `pulldown-cmark` and renders a supported
subset: paragraphs, emphasis, inline code, fenced code blocks, lists, headings,
blockquotes, and tables. It does not enable or handle math events. The Markdown
parser's ordinary text events therefore send math notation through the normal
word-wrapping path, with no math-specific style. Fenced code and inline code are
already handled separately and should remain literal.

## Possible solution

1. Check whether the project's `pulldown-cmark` version can parse the desired
   math delimiters as dedicated events. If it can, enable only the needed
   extension and carry math spans through the existing word-wrapping and table
   paths with a dedicated style. Keep the original expression text intact.
2. If parser support is unavailable or too broad, add a small inline recognizer
   for `$...$` in ordinary prose. It must leave escaped dollar signs, currency
   amounts, code spans, and fenced code untouched. Do not treat every pair of
   dollar signs as math without checking delimiter boundaries.
3. Keep the initial feature to styling and literal display. Treat actual
   equation layout or broad LaTeX-to-Unicode conversion as separate work, since
   terminal width, fonts, and incomplete command support can make those
   renderings misleading.

Display math can be considered separately. A multiline expression may need its
own block layout and wrapping rules, so it should not be implied by adding
inline math support.

## Questions for implementation

- Which delimiters should Ylva recognize: `$...$` only, or also `\(...\)`?
- Should recognized expressions keep the delimiters visible, or omit them in
  the rendered view while retaining the original response for copying?
- Which style best distinguishes math without making it look like code?
- Is there a terminal-friendly rendering library worth using, or should Ylva
  keep expressions as source text?
