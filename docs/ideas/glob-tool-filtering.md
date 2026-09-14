# Glob tool filtering

Patterns are resolved relative to cwd unless absolute. To search beneath a
sub-directory, include that directory in the pattern (for example,
`docs/**/*.md`); there is no separate `path` argument.
Bare `**` matches directories; use `**/*` to match files and directories
recursively.

Additional properties for the `glob` tool exposed to AI models:

- `git_ignore`: boolean (default true). When true, applies git-ignore rules
  when searching for matching files. It does not exclude the `.git` directory
  or other dot-prefixed paths.
- `dot_ignore`: boolean (default false). When true, excludes paths with a
  dot-prefixed component.
- `only_count`: boolean (default false). When true, returns only the match count,
  not the matched paths.
- `limit`: integer. Caps the number of results returned. Omit it for no
  result-count limit rather than using an arbitrary large value.
- `only_files`: boolean (default false). When true, returns only files.
- `only_dirs`: boolean (default false). When true, returns only directories.

The supplied search root is never a match and is always excluded from returned
paths and all counts, regardless of the pattern or option flags. Only
descendants of the search root are eligible.

## Structured result metadata

The result metadata contains flat `files`, `directories`, and `total` fields,
where `total` is the sum of the other two fields, plus a required boolean
`truncated`. `truncated` is `false` when all matching paths are returned and
`true` when `limit` omits additional paths, without reporting an omission
count:

```json
{
  "files": 42,
  "directories": 3,
  "total": 45,
  "truncated": false
}
```

The counts describe matching descendants returned after type filters and
`limit`. With `only_count`, no paths are listed, but the same counts are
returned and the result text retains the matching total.

CLI/TUI summaries omit default-valued `files`, `directories`, and `truncated`
properties. They also omit `total` when either type count is zero because it is
inferable, except that an empty result displays `total=0`; the complete metadata
remains available to the LLM/API.
