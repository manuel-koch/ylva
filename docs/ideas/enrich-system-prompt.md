# Enrich system prompt

Extend the existing global `SOUL.md` loading without changing its layered
replacement behavior. Automatically discover a small, explicit set of trusted
instruction filenames, such as:

- `AGENTS.md`
- `CLAUDE.md`
- other compatible instruction files selected deliberately rather than
  loading arbitrary Markdown files

The missing design and implementation work is:

- define the exact trusted filename set and whether filename matching is
  case-sensitive;
- define discovery locations and workspace boundaries, including whether
  instruction files in parent directories, the workspace root, or the user's
  home directory are eligible;
- define precedence and merge order when multiple files are found, including
  files with different names at the same location;
- prevent instruction content from escaping the intended workspace trust
  boundary or overriding Ylva's runtime safety constraints; and
- specify behavior for unreadable, malformed, oversized, or duplicated files.

The resolved instruction content should be composed deterministically with the
existing `SOUL.md` prompt and documented as part of the initial system-prompt
contract.
