# Quiet mode for the CLI

The headless CLI currently emits progress
and tool status while a run is active and has no `--quiet` flag.

## Proposed behavior

Add `--quiet` to suppress progress indicators and tool status. Keep assistant
content on stdout so quiet mode remains useful in a pipeline. Quiet mode does
not change the exit code (see [exit codes](../cli-options.md#exit-codes)).

## Open questions

- Decide whether `--quiet` also suppresses the final session ID and resume
  command printed to stderr.
- Define whether quiet mode affects warnings and errors or only routine
  progress.
- Keep this independent of `--format json`; the two flags should compose
  without hiding failures.

## Related

- [`cli-output-streams.md`](./cli-output-streams.md)
- [`docs/cli-options.md#exit-codes`](../cli-options.md#exit-codes)
- [`docs/cli-options.md`](../cli-options.md)
