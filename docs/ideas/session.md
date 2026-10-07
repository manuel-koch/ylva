# Session

When Ylva CLI or TUI exits, it prints the session ID to resume a session.

We should print the session title (if available) too:

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

Add CLI command to allow listing recent session: `ylva session list`.

Would print timestamp sorted ( ascending ) session entries: `<timestamp> <session-id> <title>`

## Session maintenance

These features are not implemented.

### Delete sessions

Provide an explicit `ylva session delete` command for one or more session IDs, sessions
before a date, or all sessions. Require `--dry-run` support that lists the
exact `.jsonl`, `.jsonl.gz`, and `.meta.json` files that would be removed.
Never delete automatically. Refuse to remove a session held by an active
writer.

### Compress and decompress history

Keep new sessions as raw JSONL. Add automatic compression of older unlocked
sessions after configurable age or size thresholds, alongside manual
`ylva session compress` and `ylva session decompress`
commands for a session ID, `--before <date>`, or `--all`. For archived sessions, write a
`.meta.json` sidecar before the `.jsonl.gz` archive so the session remains
discoverable. Remove the raw file only after the sidecar and archive are
complete. Decompression restores raw JSONL and removes the redundant sidecar
only after restoration succeeds. Never compress a session held by an active
writer. Compression never deletes history.

Decide when the automatic sweep runs and how thresholds are configured.
Keep archive discovery and explicit resume consistent with raw sessions,
and make interrupted compression/decompression recoverable without data loss.

## Session-wide logging correlation

Some diagnostics already include session IDs, but logging does not provide
session-wide correlation through a root `tracing` span. Add structured session
identity to the full log span chain, following
[the logging conventions](../development-workflow.md#logging).

After `/clear` starts a new session, new work must use the new session ID;
in-flight work from the old session must retain its original identity.
Distinguish subagent runs with their run ID and parent session ID, including
their outcome and transcript-persistence diagnostics. Cover TUI and headless
runs without logging prompts, credentials, or conversation bodies.

## Whole-file schema migrations

Conditional future infrastructure, not a migration currently required:
the store uses schema version `1` and tolerant event reading. Keep additive
changes on the existing [per-event evolution path](../persistence.md#schema-tolerance).

If a genuine whole-file semantic break requires a new schema version, add
a chain of small, idempotent upgraders. Acquire the writer lock before
migrating; write a complete temporary file and atomically replace the original
only after conversion succeeds. A failed or interrupted migration must leave
the original readable, and read-only discovery must never rewrite sessions.
Test repeated upgrades, interrupted writes, unsupported versions, and
preservation of messages and root/subagent linkage.
