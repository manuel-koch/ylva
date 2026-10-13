# Structured CLI exit codes

**Status: planned.** Implementation is tracked in
[`docs/plans/cli-exit-codes-plan.md`](../plans/cli-exit-codes-plan.md). The
settled table is in [`docs/cli-options.md`](../cli-options.md#exit-codes).

The CLI used exit code `2` for usage and fatal startup errors. This idea
proposed codes `0` through `4`, but reused `2` for interruption, which collided
with the usage-error code.

## Settled mapping

| Code | Meaning |
| --- | --- |
| `0` | Successful run |
| `1` | General execution error |
| `2` | Usage or startup error (unchanged) |
| `3` | Approval required but denied (moved from `4`) |
| `130` | User interruption (SIGINT or SIGTERM) |

Tool results never set the exit code, so the proposed tool-failure code was
dropped. A tool's own non-zero exit status is not a run failure.

## Resolved questions

- The usage-error code `2` is kept. Interruption uses `130`, the shell
  convention for SIGINT.
- When one run has several outcomes, precedence is `130`, then `1`, then `3`,
  then `0`.
- A root-level provider or stream failure is `1`. Startup errors stay `2`.
- TUI and headless runs use the same mapping for the same outcome.
- The TUI outcome covers only the latest turn. Ctrl+C and Esc stop a turn but do
  not set `130`, because Ctrl+C is the normal way to quit the TUI. Only an
  external SIGINT or SIGTERM sets `130`, in any TUI state.

## Related

- [`docs/cli-options.md`](../cli-options.md)
- [`cli-json-output.md`](./cli-json-output.md)
