---
status: accepted
date: 2026-10-22
decision-makers: Manuel Koch
consulted:
informed:
---

# Use release-plz for release automation

## Context and Problem Statement

Versions were bumped manually in `Cargo.toml` and tagged with `make tag-version`.
With [conventional commits](./0001-use-lefthook-and-conventional-commits-for-git-conventions.md)
the next semantic version and the release notes can be derived automatically.
Which tool does this, and how do changes reach `main`?

## Decision Drivers

* Rust-native handling of `Cargo.toml` and `Cargo.lock`
* GitHub releases only; no crates.io publishing and no binaries
* Release PRs must trigger CI
* Single source of truth for tags

## Considered Options

* release-plz in release-PR mode
* cargo-release
* cocogitto
* release-please

## Decision Outcome

Chosen option: **release-plz in release-PR mode**

* `.github/workflows/release.yml` runs on every push to `main`: it opens or updates a
  release PR (version bump in `Cargo.toml`/`Cargo.lock` and `CHANGELOG.md`) and, once
  that PR is merged, tags `vX.Y.Z` and creates the GitHub release.
* `release-plz.toml` sets `publish = false`, ignores merge and non-conventional commits.
* release-plz authenticates with a fine-grained PAT stored as the repository secret
  `RELEASE_PLZ_TOKEN` (contents and pull-requests write), so release PRs trigger CI.
  The secret is never exposed to fork PR jobs.
* Pull requests are merged with merge commits, so every branch commit reaches `main`
  and must be conventional. GitHub's `Merge pull request #N` commits are ignored.
* A ruleset on `main` requires a PR and passing CI, allows only the merge-commit
  method and lets the PAT owner bypass it.
* CI (`.github/workflows/ci.yml`) runs on macOS for PRs and non-`main` pushes and
  reuses Makefile targets: `format-check`, `lint`, `test-coverage`, and `audit` when
  `Cargo.toml` or `Cargo.lock` change. No secrets and no live-provider tests.
* `make tag-version` is removed. The baseline tag is `v0.31.1`.

## Consequences

* Good, because versions, tags and notes follow from commit messages.
* Good, because tags have a single source of truth.
* Neutral, because docs-only changes also go through a short PR.
* Bad, because a PAT must be created and rotated.
* Bad, because merge-commit strategy requires every branch commit to be conventional.

## Confirmation

* After merging a `feat:` or `fix:` PR, a release PR appears with the bumped version.
* Merging the release PR creates tag `vX.Y.Z` and a GitHub release.

## Pros and Cons of the Options

### release-plz in release-PR mode

* Good, because it handles `Cargo.lock` and the release PR flow natively for Rust.
* Bad, because it is crates.io oriented; publishing must be disabled explicitly.

### cargo-release

* Good, because simple and well known.
* Bad, because it is driven locally and does not derive versions from commits.

### cocogitto

* Good, because it combines commit linting with version bumps.
* Bad, because it has no release PR flow.

### release-please

* Good, because it supports a release PR flow with a Rust strategy.
* Bad, because Rust and `Cargo.lock` handling is less native.

## Links

* [release-plz](https://release-plz.dev/)
* [Hooks and conventions ADR](./0001-use-lefthook-and-conventional-commits-for-git-conventions.md)
