# Backlog of issues discovered while testing

- [ ] `search` tool result is capped at byte level.
      Since matches are always relevant to "lines" this doesn't seem to make sense.
      We should limit on whole result-lines instead of byte count.
      We can could remove "truncated_at_bytes" and just state that result was
      capped.
