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
