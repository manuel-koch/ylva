# Glob tool filtering

Additional properties for the `glob` tool exposed to AI models:

- `git_ignore`: boolean (default false). When true, applies git-ignore rules
  when searching for matching files.
- `only_count`: boolean (default false). When true, returns only the match count,
  not the matched paths.
- `limit`: integer. Caps the number of results returned.
- `only_files`: boolean (default false). When true, returns only files.
- `only_dirs`: boolean (default false). When true, returns only directories.

## Structured result metadata

Add a `metadata` field (`serde_json::Value`) to `ToolCallResult` so tools
can return structured attributes alongside their text content. The metadata is
serialized into the tool-role message sent to the LLM.

Useful cases:

- **Truncation signalling** — when `limit` caps the result, metadata can carry
  `{"truncated": true, "total_matches": 500, "shown": 20}` so the LLM knows
  it hasn't seen the full set.
- **Aggregates replacing verbose output** — when `only_count` is set, metadata
  carries `{"matched_count": 42}` instead of a file list.

A bare count on an untruncated result adds no information the LLM cannot derive
itself; metadata is most valuable when the content is intentionally incomplete.
