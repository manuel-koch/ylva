# Separate CLI content and status streams

Assistant text, user echoes, thinking, tool/status output, and agent errors
currently share the CLI driver's output writer. Fatal startup errors and the
session resume hint go to stderr.

## Proposed contract

- stdout contains assistant content, or the structured result selected by
  `--format json`.
- stderr contains progress, tool status, warnings, and errors.
- Piping stdout must not capture terminal-oriented status text.

## Open questions

- Decide whether tool results belong in stdout, stderr, or only the final JSON
  object.
- Define how color settings interact with redirected streams.
- Keep the session ID and resume command on stderr unless quiet-mode policy
  explicitly suppresses them.

## Related

- [`cli-json-output.md`](./cli-json-output.md)
- [`cli-quiet-mode.md`](./cli-quiet-mode.md)
- [`cli-output-buffering.md`](./cli-output-buffering.md)
