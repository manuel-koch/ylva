# Session

When Ylva CLI or TUI exits, it prints the session ID to resume a session.

We should print the session title (if available) too:

```text
Session: <session_id>
Title:   <session_title>
Resume with: ylva --conversation-id <session_id>
```

## Clone and continue a session

Implemented by [the blueprint finalization plan](../plans/blueprint-finalization-plan.md):
`--clone` with `--conversation-id <uuid>` in both TUI and headless modes
clones only a top-level session. The clone receives a new session ID, keeps
the source title, and records the source ID as `cloned_from`. The logical
session events are copied — messages, compaction snapshots, model/agent
changes, and child-run lifecycle records (the latter rebased onto the clone's
id) — while child transcript files remain shared read-only references. A
clone fails if the source is locked by another Ylva process or is a child
transcript; all new conversation writes go to the clone and the original
session remains byte-for-byte unchanged. See the
[clone contract](../persistence.md#clone-contract).

## List sessions

Add CLI command to allow listing recent session: `ylva session list`.

Would print timestamp sorted ( ascending ) session entries: `<timestamp> <session-id> <title>`

## Session maintenance

These features are not implemented and are deferred from the blueprint
finalization work.

### Delete sessions

Provide an explicit `ylva session delete` command for one or more session IDs, sessions
before a date, or all sessions. Require `--dry-run` support that lists the
exact `.jsonl`, `.jsonl.gz`, and `.meta.json` files that would be removed.
Never delete automatically. Refuse to remove a session held by an active
writer.

### Compress and decompress history

Keep new sessions as raw JSONL. A later storage-maintenance feature may
compress older unlocked sessions, selected by a configurable age or size
threshold, and provide manual `ylva session compress` and `ylva session decompress`
commands for a session ID, `--before <date>`, or `--all`. For archived sessions, write a
`.meta.json` sidecar before the `.jsonl.gz` archive so the session remains
discoverable. Remove the raw file only after the sidecar and archive are
complete. Compression never deletes history.
