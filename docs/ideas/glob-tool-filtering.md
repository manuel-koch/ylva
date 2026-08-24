# Glob tool filtering

Include more properties to the `glob` tool exposed to AI models:

- `git_ignore`: a boolean flag (default false) to control whether
  git-ignore rules in the workspace structure should be used when searching
  for matching files.
- `only_count`: a boolean flag (default false) to control whether glob result
  only includes the number of matches but not the matched file path(s).
- `limit`: a boolean flag (default false) to control whether glob result
  should be clamped by given number of matches.
- `only_files`: a boolean flag (default false) to control whether glob result
  should only include files
- `only_dirs`: a boolean flag (default false) to control whether glob result
  should only include directories

## Structured result metadata

Enhance `ToolCallResult` with a `metadata` field (`serde_json::Value`) so tools
can return structured attributes alongside their text content. The metadata is
serialized into the tool-role message sent to the LLM.

Useful triggers:

- **Truncation signalling** — when `limit` caps the result, metadata can carry
  `{"truncated": true, "total_matches": 500, "shown": 20}` so the LLM knows
  it hasn't seen the full set.
- **Aggregates replacing verbose output** — when `only_count` is set, metadata
  carries `{"matched_count": 42}` instead of a file list.

A bare count on an untruncated result adds no information the LLM cannot derive
itself; metadata is most valuable when the content is intentionally incomplete.
