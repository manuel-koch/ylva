# Structured CLI exit codes

Deferred from blueprint finalization. The current CLI uses exit code `2` for
usage and fatal startup errors; the blueprint proposes codes `0` through `4`
for success, general error, interruption, tool failure, and approval denial.

## Proposed mapping

| Code | Proposed meaning |
| --- | --- |
| `0` | Successful run |
| `1` | General runtime error |
| `2` | User interruption |
| `3` | Tool failure |
| `4` | Approval required but denied |

## Open questions

- Preserve a distinct usage-error code. The current parser already uses `2`,
  which conflicts with the proposed interruption code.
- Define how multiple tool failures affect the final code.
- Decide how provider failures, startup errors, and user interruption are
  distinguished when they happen in the same run.
- Keep the mapping consistent in both TUI and headless modes.

## Related

- [`docs/cli-options.md`](../cli-options.md)
- [`cli-json-output.md`](./cli-json-output.md)
