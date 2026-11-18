# Backlog of issues discovered while testing

- [ ] **Correct sandbox path glob separators and hidden-path matching.**
  [Sandbox path rules](../src/sandbox/path.rs) use `glob::Pattern::matches`,
  so `/workspace/*.md` also matches `/workspace/sub/notes.md` and
  `/workspace/.private.md`. For `read`, `write`, `edit`, `search`, and `glob`
  rules, `*`, `?`, and character classes must not cross path separators;
  `**` may match nested paths. Wildcards must not match a dot-prefixed
  component unless the pattern explicitly includes its leading dot. For
  example, `/workspace/.private/**/*.md` may match
  `/workspace/.private/notes.md`, but not
  `/workspace/.private/.nested/notes.md` or
  `/workspace/.private/.secret.md`.

  Keep the separate `dot_path` rule, rule ordering, `glob`'s `dot_ignore`, and
  the root-only checks for `search` and `glob` unchanged. Preserve existing
  CLI auto-approval flags' catch-all behavior, including for hidden paths.
  Add regression tests for direct, nested, explicitly hidden, and hidden-ancestor
  paths; `*` and `**`; and the interactions above.

  Migrate existing rules without silently weakening denials: narrowing a deny
  glob can stop it matching nested or hidden paths, including paths under hidden
  ancestors outside the workspace. Fail closed until review or migration when
  existing denial coverage cannot be preserved.
