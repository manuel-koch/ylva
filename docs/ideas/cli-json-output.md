# JSON output for the CLI

The headless CLI currently streams
assistant text to stdout and has no `--format` flag.

## Goal

Add `--format json` for scripts that need a structured result instead of
parsing terminal-oriented text. Emit one final JSON
object after the run completes.

## Open questions

- Define the stable object schema, including the session ID, assistant output,
  tool outcomes, and errors.
- Decide what a failed or interrupted run emits. Avoid emitting malformed
  partial JSON. The exit code already reports the outcome (see
  [exit codes](../cli-options.md#exit-codes)); JSON output must not change it.
- Keep diagnostics and progress off stdout. Coordinate this with
  [`cli-output-streams.md`](./cli-output-streams.md) and
  [`cli-output-buffering.md`](./cli-output-buffering.md).
- Decide whether JSON includes the full transcript or only the final response.

## Related

- [`docs/cli-options.md#exit-codes`](../cli-options.md#exit-codes)
- [`cli-quiet-mode.md`](./cli-quiet-mode.md)
- [`docs/cli-options.md`](../cli-options.md)
