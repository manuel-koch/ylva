# Glob tool filtering

Include more properties to the `glob` tool exposed to AI models:

- `git_ignore`: a boolean flag (default false) to control whether
  git-ignore rules in the workspace structure should be used when searching
  for matching files.
- `only_count`: a boolean flag (default false) to control whether glob result
  only includes the number of matches but not the matched file path(s).
- `limit`: a boolean flag (default false) to control whether glob result
  should be clamped by given number of matches.
