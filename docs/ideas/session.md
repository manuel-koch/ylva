# Session

CLI and TUI exit hints identify the final active session, including after
`/clear`, and print its persisted title when available:

```text
Session: <session_id>
Title:   <session_title>
Resume with: ylva --conversation-id <session_id>
```

## Clone and continue a session

Implemented with `--conversation-id <uuid> --clone` in TUI and headless modes.
See the
[clone contract](../persistence.md#clone-contract).

## List sessions

Implemented with `ylva session list`, least recently used first, with no implicit recent
limit. See [Session commands](../cli-options.md#session-commands).

## Session maintenance

### Delete sessions

Implemented as explicit maintenance with dry-run previews. Root deletion retains
subagent transcripts shared with surviving roots/clones and refuses active
writers. See [Session commands](../cli-options.md#session-commands).

### Compress and decompress history

Manual compression and decompression are implemented. New sessions remain raw
JSONL. Archives stay discoverable, readable, resumable and cloneable, with
recoverable interrupted transitions. See
[archive recovery](../persistence.md#manual-maintenance-and-archive-recovery).

Automatic compression, sweep scheduling, and age/size thresholds are **deferred**.
No compression configuration or automatic maintenance is implemented.

## Session-wide logging correlation

Implemented with immutable session and subagent spans. New work after `/clear`
uses the replacement identity; old work keeps its original identity.
See [logging conventions](../development-workflow.md#logging).

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
