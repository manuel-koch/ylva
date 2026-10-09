# Session

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
