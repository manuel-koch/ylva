# Development workflow

After implementing new functionality or fixing bugs, run the test suite and the linter.

For new features come up with appropriate test case(s) to verify them.

Bugfixes MUST add or strengthen a test that fails before the fix and passes after.

Coverage threshold: **≥80% line coverage** is enforced on testable files
by `make test-coverage` (below the threshold the command fails).
`ui.rs` and `main.rs` are excluded because they require a live terminal.

The following `make` targets are available:

```shell
make format                          # cargo fmt — enforce consistent formatting
make lint                            # cargo check + cargo clippy — fix all warnings before committing
make test-coverage                   # run tests + print per-file coverage summary; fails if <80%
make test-coverage-missing           # same as test-coverage but also prints uncovered line numbers
make test                            # cargo test — all tests must pass
make build-debug                     # cargo build (debug)
make build-debug-and-run             # build debug binary and launch it
make build-release                   # cargo build (release)
make build-release-and-run           # build release binary and launch it
make install                         # cargo install --path . → ~/.cargo/bin/ylva
make tag-version                     # tag HEAD with version from Cargo.toml
make outdated-dependencies           # cargo update --dry-run — show available upgrades
make upgrade-dependencies            # cargo update — apply dependency upgrades to Cargo.lock
```

The `Makefile` at the project root is the source of truth for `make` targets.
This section is a documentation mirror — when a target is added, removed, renamed,
or its command changes, update this section in the same commit (see
[AGENTS](../AGENTS.md) → *Docs are part of the change set*).
Other documents that reference `make` targets must delegate here rather than
re-list targets, otherwise the lists drift the next time the `Makefile` changes.

## Planned implementations

Each implementation is driven by a dedicated plan document kept in
`docs/plans/<topic>.md` — one file per subject/feature. A plan states:

- the goal and scope (what "done" means),
- an ordered, independently-verifiable task checklist,
- for each task, the tests that must verify it,
- a **documentation step** that lists every `docs/*` file the change touches.

The plan is the source of truth for the work.

## Plan execution

When executing a plan:

- complete tasks in order; verify each via the test suite and linter before
  moving on (Testing Pyramid / TDD semantics).
- each committed change bundles implementation + tests + the **documentation
  update named in the plan's documentation step** — never commit code without
  its doc changes (see [AGENTS](../AGENTS.md) → *Docs are part of the change set*).
- before executing, check the change against the recorded decisions in
  [architecture-design-records](../docs/architecture-design-records/) and keep
  the implementation consistent with them; introduce a new record when the
  change deviates.
- if a plan doc is found to be out of date, update the plan to match the work;
  stale docs are bugs.

A plan step is "done" only when its docs are updated too. Partial completion is
not completion.
