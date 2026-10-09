# Session options and maintenance

## Context

Ylva is a Rust CLI/TUI agent-harness. This change makes persisted sessions easier
to identify, discover, maintain, and diagnose without losing transcripts or
changing the fresh-session default.

The requested behavior is:

- On CLI and TUI exit, print the persisted session ID, its title when available,
  and the existing resume command to stderr:

  ```text
  Session: <session_id>
  Title:   <session_title>
  Resume with: ylva --conversation-id <session_id>
  ```

- Add `ylva session list`, printing one `<timestamp> <session-id> <title>` entry
  per session in ascending timestamp order.
- Add explicit `ylva session delete` for one or more IDs, `--before <date>`,
  or `--all`, including `--dry-run` that names every actual `.jsonl`,
  `.jsonl.gz`, and `.meta.json` file selected. Never delete automatically or
  remove an active writer's session. A valid selector executes deletion unless
  `--dry-run` is present; there is no additional `--yes` flag.
- Keep new sessions as raw JSONL. Add manual `ylva session compress` and
  `ylva session decompress` with multiple-ID, date, and all selectors. Never
  transform active writers. Compression reduces disk usage, not transcript content.
- Keep archived sessions discoverable, resumable, cloneable, and readable for
  TUI input history and subagent drill-in. Write archive metadata before
  publishing the gzip archive; remove the source only after both are complete.
  Decompression removes redundant archive metadata only after raw restoration
  succeeds. Interrupted transitions must retain a recoverable complete copy.
- Correlate session work through root `tracing` spans in CLI and TUI. After
  `/clear`, new work uses the replacement session ID; old in-flight work keeps
  its original identity. Subagent diagnostics identify their run and parent
  session, including completion and transcript persistence. Do not add prompt,
  credential, or conversation-body logging.

Cloning already exists and is a compatibility constraint, not a new feature.
Automatic compression, sweep scheduling, and age/size configuration are explicitly
deferred by the user; no automatic maintenance is part of this implementation.
Whole-file migrations are conditional future work: schema version 1 and tolerant
per-event evolution remain unchanged unless an actual semantic break is identified.
There is no justification in these requirements for a schema-version bump or an
unused migration framework.

### Acceptance criteria

- Exit hints describe the session active at shutdown, including after `/clear`;
  untitled sessions omit the `Title:` line. Preserve explicit `--profile` hints,
  stderr routing, terminal restoration, and hints on failed runs with persisted state.
- Session commands run without starting an agent, contacting providers, consuming
  piped prompts, entering the TUI, or creating a conversation.
- Listing is read-only, deterministic, and ascending; timestamp ties use the UUID.
  Active sessions remain listable. Unreadable entries receive explicit diagnostics.
- Commands are scoped to the canonicalized launch directory, with no cross-project
  option. Listing includes roots only unless `--include-subagents` is supplied.
  It lists all matches, with no implicit recent-session limit.
- All maintenance commands require exactly one selector form: one or more UUIDs,
  `--before <date>`, or `--all`. Date-only input means UTC midnight; RFC 3339
  timestamps must include an offset. Selection uses a strictly earlier creation time.
- Bulk compression/decompression selects all transcripts; bulk deletion selects
  roots and their unshared descendants. Keep descendants referenced by surviving
  roots/clones. Explicit deletion of a referenced child is an error. Historical
  `cloned_from` provenance may name a deleted source; clones are never cascaded.
- Dry-run mutates nothing and reports exact selected paths, including multiple
  representations of the same session. Unknown explicit IDs are errors.
- All mutating operations use a shared identity-level exclusion protocol also
  honored by writers, resume, clone, title rewrite, and subagent transcript creation.
  A writer cannot start between a lock check and removal or representation change.
  Reference discovery and removal are coordinated with new clone/delegation links.
  Unix is the supported maintenance platform; other platforms explicitly refuse
  maintenance and archived resume requiring restoration.
- Compression/decompression preserve raw bytes, event order, metadata, compaction
  projections, title, agent-type selection, and root/subagent/clone links.
  Readers never perform repair, migration, or decompression as a side effect.
- Archive-transition failures, interrupted writes, and competing processes never
  remove the only valid copy. Corrupt archives or inconsistent metadata are
  reported, not accepted as empty sessions. Divergent raw/archive copies are
  diagnosed during discovery and block maintenance, resume, and clone; equivalent
  copies can be recovered under lock without mutation by read-only discovery.
- Validate syntax and unknown explicit IDs before any batch mutation. Thereafter,
  process independent targets/deletion groups and report partial failures with a
  nonzero exit status. A locked root blocks its descendant deletion group too.
  No all-or-nothing multi-session or multi-file deletion transaction is promised.
- Text and JSON logs retain correct root/session/subagent span chains across
  tasks, cancellation, `/clear`, title generation, and store writes.
- Implementation includes offline regression tests, affected documentation,
  `make format lint`, and `make test-coverage` with aggregate line coverage at least 80%.

## Implementation Plan

The implementing agent must change each `[ ]` to `[x]` immediately after completing
the step and rewrite this file in place. Completion includes that step's tests and
affected documentation; the final documentation step reconciles the full change set.
The user confirmed the refined behavioral contract after two grilling rounds.

1. [ ] Establish the persistence baseline. Start with `src/store/session_file.rs`,
   `src/store/writer.rs`, and
   `src/store/replay.rs`; run `cargo test --lib store::tests` as the persistence
   baseline. Recheck repository guidance and ADRs; the ADR directory is currently empty.
2. [ ] Correct exit-hint ownership in `src/main.rs`, `src/app.rs`,
   `src/cli_driver.rs`, and `src/tui_driver.rs`. Carry a snapshot of the actual final
   persisted session and available title across shutdown, rather than retaining
   only the startup UUID. Do not start or await a fresh utility request just to print
   a title. Extend `resume_hint_text` and existing hint tests for titled/untitled,
   explicit profile, failure, and `/clear` rollover cases.
3. [ ] Introduce nested clap session subcommands in `src/cli.rs` and early dispatch
   in `src/main.rs`, with orchestration outside the thin binary entry point.
   Implement `list` using `Store::list_sessions` without changing its existing
   newest-first contract: reverse/sort only the CLI projection. Apply the resolved
   current-directory scope and child filters, deterministic UUID tie-breaker, and safe single-line
   output for titles. Add parser/help and compiled-binary tests for exact output,
   empty stores, malformed metadata, and provider-independent execution. Preserve
   logging flags without loading provider configuration; list failures/warnings
   must be visible on stderr even when optional logfile logging is disabled.
4. [ ] Establish stable per-session locking in `src/store/session_file.rs` and
   integrate it with `src/store.rs` and `src/store/writer.rs`. Lock identity must
   survive title replacement, unlink, compression, and restoration; keep it held
   through the whole mutation, with nonblocking conflict errors. Retain stable
   lock files rather than unlinking/recreating their inode; exclude them from
   listing and deletion dry-run's transcript file set. Explicitly refuse
   maintenance on non-Unix. Test title rewrite against concurrent
   maintenance/resume/clone, competing writers, and lock release after writer drain.
5. [ ] Add shared raw/archive discovery and representation resolution beside the
   existing store owners. Define `.meta.json` sidecar validation and precedence for
   raw-plus-archive states, orphan sidecars, temporary files, and interrupted
   operations. Validate sidecar identity against its file name, project, and
   archive header; never treat a sidecar alone as a readable transcript.
   Equivalent raw/archive copies resolve to one session; divergent copies produce
   a visible diagnostic and block writes/resume/clone. Preserve newest-first
   library listing and read-only lookup.
   Route metadata, events, messages, latest agent type, input history, child replay,
   and clone reads through the shared resolution rather than raw-only paths.
   Test discovery and replay for raw, archived, duplicate, corrupt, and missing states.
6. [ ] Implement lossless compression/decompression under the identity lock.
   Promote the existing `flate2` dependency from dev-only to runtime use if chosen.
   Use same-directory temporary outputs, complete and verify them, synchronize
   necessary files/directories before source removal, and publish with atomic rename.
   Compression publishes complete metadata before archive publication; decompression
   publishes complete raw JSONL before archive/sidecar cleanup. Never overwrite a
   divergent destination. Add failure-injection and restart tests at every publication
   and removal boundary, plus byte-for-byte round-trip and metadata-link assertions.
7. [ ] Wire archived explicit resume in `src/store.rs` and `src/main.rs` to
   locked restoration followed by the existing append-open path. Keep read-only
   archive cloning source-preserving; it must not decompress or repair the source.
   Preserve tool bubbles, attachments, compaction/projections, agent selection,
   child drill-in, and input-history ordering. Extend `tests/store_persistence_e2e.rs`
   with offline archived-resume and archived-clone cases and owner-local replay tests.
8. [ ] Implement shared selector validation and manual maintenance dispatch.
   Reject missing or mutually exclusive selectors and invalid dates/IDs before
   mutation. Implement deletion dry-run from the exact resolved file set, revalidate
   under lock for real deletion, and honor the resolved child/clone linkage contract.
   Coordinate reference scans with concurrent clone publication and subagent link
   creation, using a narrowly scoped project coordination mechanism and deterministic
   lock acquisition. Determine references from surviving roots' lifecycle events,
   including clones, not only a child's original `parent_session`. Do not remove
   descendants if the selected root is locked. Dry-run lists only files that would
   actually be removed, and separately reports shared descendants retained.
   Implement manual compress/decompress through the same lifecycle operations.
   Real deletion requires a selector but no `--yes`; dry-run never changes files.
   Missing explicit IDs fail prevalidation; independent targets/groups proceed
   after runtime failures, with per-target diagnostics and nonzero status.
   An already compressed/decompressed target is an explicitly reported no-op.
   Test active writers, races after selection, unknown IDs, mixed representations,
   linkage, concurrent clone creation, partial batches, invalid inputs, and
   zero-side-effect dry-run, including unchanged directories and no new lock files.
9. [ ] Add session-scoped tracing ownership in `src/agent.rs`,
   `src/agent/runtime.rs`, `src/agent/runtime/commands.rs`, and the drivers.
   Instrument individual async scopes with immutable identity snapshots; replace
   the root span when `/clear` changes the session, rather than mutating a span
   already captured by old work. Propagate explicit parent spans into spawned
   request/tool/compaction/title/writer tasks and subagent work in
   `src/agent_factory.rs`. Give subagent outcomes and persistence the same run and
   parent-session identity. Extend text/JSON capture tests for interleaved tasks,
   rollover, cancellation, completion, and transcript persistence without new
   sensitive fields; use repository subscriber-isolation helpers.
10. [ ] Update `docs/cli-options.md`, `docs/persistence.md`, `docs/features.md`,
    `docs/implementation-notes.md`, and
    `docs/development-workflow.md` for implemented commands, selectors, locking,
    archive transitions/recovery, final hints, and logging ownership. Update
    `README.md`, `CONTEXT.md`, and `docs/ideas/session.md` to reflect implemented
    capabilities and domain terms without duplicating authoritative contracts.
    Update `docs/known-test-issues.md` only if this work changes its writer-lock
    workaround. Leave `docs/keybindings.md` unchanged unless key behavior changes.
    Leave `docs/configuration.md` unchanged unless an existing contract needs
    clarification; no compression configuration is added. Mark automatic compression
    and whole-file migrations deferred in `docs/ideas/session.md`.
11. [ ] Run targeted offline command/persistence/logging regressions as steps land,
    then `make format lint` and the single mandatory `make test-coverage` gate.
    Check Markdown with configured `rumdl`; do not run `make test` as a duplicate
    mandatory gate. Reconcile this checklist with actual implementation and docs.
    Do not stage, branch, or commit without separate user consent.

## Implementation Hints

- Confirmed scope is the current canonicalized launch directory only. Listing
  defaults to roots, `--include-subagents` includes transcripts, and no count
  limit or cross-project option is requested.
- Confirmed selectors are multiple UUIDs, exclusive `--before`, or `--all`.
  `--before` compares `created_at` strictly before a UTC date-midnight or an
  RFC 3339 instant. Compression/decompression bulk selection includes all transcripts;
  deletion bulk selection includes roots with unshared descendants.
- Confirmed deletion retains shared child transcripts referenced by surviving roots
  or clones, rejects explicit referenced-child deletion, and never cascades clones.
  `cloned_from` is historical provenance, so deleting a clone source is permitted.
  `--dry-run` previews exact removable transcript/metadata paths; without it, a
  valid selector executes. There is no `--yes` requirement.
- Confirmed batch policy validates syntax and unknown explicit IDs before mutation,
  then processes independent targets/groups with nonzero status on partial failure.
  Already-transformed compression/decompression targets are reported no-ops.
- Confirmed divergent representations are never guessed over: discovery diagnoses
  the conflict; mutation/resume/clone refuses it. Equal copies permit locked recovery.
- Confirmed maintenance platform is Unix. Non-Unix maintenance and archived resume
  that requires restoration are explicit errors; ordinary raw-session behavior is
  otherwise preserved.
- Automatic sweeps, threshold defaults/configuration, and unused schema migration
  infrastructure are deferred. New sessions remain raw JSONL indefinitely unless
  manually compressed.
- `src/cli.rs` owns clap parsing; `CliCommand` currently only contains `Init`.
  `src/main.rs` dispatches `init` before provider/session startup.
- `src/main.rs` captures `store.session_id()` before running the application.
  `App::run` currently returns `anyhow::Result<u8>`, not final session metadata.
  `resume_hint_text` prints no title and preserves an explicitly supplied profile.
- `Store::title()` exposes the shared persisted title snapshot; title generation
  is coordinated in `src/session_title.rs`. `/clear` replaces the context store
  and updates the shared current-store handle in `src/agent/runtime/commands.rs`.
- `src/store.rs` owns lifecycle; `store/event.rs` owns event types/schema constants;
  `store/session_file.rs` owns locks/header I/O/repair; `store/writer.rs` owns
  append/flush/title rewrite; `store/replay.rs` owns read-only reconstruction.
- Store paths are `<YLVA_HOME>/chats/<project_key>/<UUID>.jsonl`.
  `project_key` hashes the canonicalized launch directory, not the git root.
  Current reads and resume selection derive raw `.jsonl` paths.
- `Store::list_sessions` parses metadata headers, includes subagent transcripts,
  ignores non-JSONL files, and returns newest first. Input-history retrieval relies
  on that order and separately excludes children and the current session.
- Clone rejects children and locked sources, replays logical events, preserves
  the source, and shares completed child transcripts without copying their files.
- Unix locking uses `nix::fcntl::Flock`; non-Unix currently opens without a lock.
  Dropping a Store releases its writer lock asynchronously after channel drain.
  The current title rewrite replaces the raw inode before reacquiring its lock;
  inode-only checks cannot protect deletion/archive transitions.
- `Cargo.toml` includes `chrono`, `uuid`, `clap`, `tracing`, and `nix`; `flate2`
  exists only under dev-dependencies. Schema version is currently 1.
- Stream/tool tasks already use `tracing::Instrument`; not all spawned tasks
  inherit a session root. Child execution carries a run ID and the parent's
  session ID separately from the child's persisted transcript session.
- `src/logging.rs` owns subscribers; text renders the full span chain and JSON
  includes spans. `src/test_support.rs` owns unit-test capture/isolation helpers.
  `tests/common.rs` owns integration helpers and isolated subprocess environments.
- `docs/architecture-design-records/` and `docs/plans/` were empty at planning time.
  `docs/development-workflow.md` requires implementation, tests, and affected
  documentation together; integration-facing library APIs need `src/lib.rs` exports.
