# Session

## Export session

Add a new command `ylva session export` to save a given session in another human readable
format like HTML (html,htm), Markdown (md) or PDF (pdf).

- Export a given session in markdown format and save it at selected path:
  `ylva session export -f md -o <PATH> <UUID>`
- Export a given session in markdown format and output it to stdout:
  `ylva session export -f md <UUID>`

## Whole-file schema migrations

**Deferred**, conditional future infrastructure, not a migration currently required:
the store uses schema version `1` and tolerant event reading. Keep additive
changes on the existing [per-event evolution path](../persistence.md#schema-tolerance).

If a genuine whole-file semantic break requires a new schema version, add
a chain of small, idempotent upgraders. Acquire the writer lock before
migrating; write a complete temporary file and atomically replace the original
only after conversion succeeds. A failed or interrupted migration must leave
the original readable, and read-only discovery must never rewrite sessions.
Test repeated upgrades, interrupted writes, unsupported versions, and
preservation of messages and root/subagent linkage.
