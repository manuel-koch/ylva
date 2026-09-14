# Backlog of issues discovered while testing

- [x] Tool "read" schema description seems to misguide LLM, it was not
      reading lines but was using the default (bytes) offset:
      ```text
      ---[ User ]---
      Read lines 33 to 34 of (big file!) tests/e2e/read-tool.zsh and do a spell check.
      Repoort only the findiings. Do not chaange anything!

      ---[ Assistant Thinking ]---
      The user wants to read lines 33 and 34 from the file `tests/e2e/read-tool.zsh` and then perform a spell check on those lines.
      Finally, the user explicitly requested to report *only* the findings and *not* change anything.
      
      First, I need to read the specified lines. The `read` tool is suitable for this.
      
      **Plan:**
      1. Call `read` on `tests/e2e/read-tool.zsh` with `offset: 32` (since it's 0-based) and `limit: 2` (to get 2 lines).
      2. Once the content is retrieved, I will perform a manual spell check check (or inform the user that an automated spell checker isn't available in the execution environment, but I will proceed with the output I get and look for obvious typos as the core task). Given the constraint of "Read lines 33 to 34," I am constrained by the output of the `read` tool.
      
      Let's start with the read action.
      [thought for 6.843s]
      
      [tool] Read tests/e2e/read-tool.zsh [offset=32, limit=2]
      [tool] Read tests/e2e/read-tool.zsh [offset=32, limit=2] done (0.000s) read_bytes=2 read_lines=1
        1: IR
      ```
      Resolved: omitted `mode` now defaults to `lines`; use `mode: "bytes"` for
      explicit byte ranges.

- [ ] Very long lines of LLM thinking/streaming are not wrapped at TUI right
      bubble edge.
      e.g. a long JSON line output by LLM that has no linebreak is just
      rendered as a very long line where the right portion of the line is
      beyond the right edge of the bubble and not visible.
