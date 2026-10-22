---
status: accepted
date: 2026-10-22
decision-makers: Manuel Koch
consulted:
informed:
---

# Use lefthook and conventional commits for git conventions

## Context and Problem Statement

Ylva has no commit-message convention, no branch-naming convention and no guard
against committing on `main`. History is free-form, which prevents deriving
versions and release notes from commits. Which tool enforces the conventions
locally, and which checks belong into hooks versus CI?

## Decision Drivers

* Rust-only toolchain; avoid requiring Node or Python for contributors
* Hooks stay fast; slow checks cause `--no-verify` habits
* Hooks are bypassable, so the same rules must be re-checked server-side
* One implementation of each rule (SSOT)

## Considered Options

* lefthook with a project shell script
* pre-commit
* core.hooksPath with committed git hooks
* husky with commitlint

## Decision Outcome

Chosen option: **lefthook with a project shell script**

* Hook configuration is `lefthook.yml`; `make install-git-hooks` installs it and
  sets `commit.template` to `.gitmessage`.
* All rules live in `scripts/check-git-conventions.sh`, used by the hooks and by CI.
* Commit messages follow Conventional Commits with types
  `feat fix docs style refactor perf test build ci chore revert`. Merge, revert,
  `fixup!`/`squash!` messages and trailers such as `Co-authored-by` are accepted.
* Branch names are `main` or `<prefix>/<lowercase-kebab>` with prefix one of
  `feature bugfix hotfix release chore docs refactor test`. Automation branches
  (`dependabot/…`, `release-plz-…`) are accepted by the script for CI.
* Committing on `main` is rejected locally without exceptions. Branch and main
  checks are skipped on detached HEAD and during rebase, merge or cherry-pick.
* Local hooks run only fast checks: branch guard, branch name, commit message,
  `cargo fmt --check` for staged Rust files and `rumdl check` for staged Markdown
  files. Clippy, tests and audit run only in CI.

## Consequences

* Good, because lefthook is a single binary without a language runtime.
* Good, because the check script has one implementation shared with CI.
* Neutral, because contributors install `lefthook` themselves (e.g. `brew install lefthook`).
* Bad, because hooks can be bypassed with `--no-verify`; CI re-checks branch name and
  commit messages, and the `main` ruleset requires a PR with passing CI.

## Confirmation

* `make test-git-conventions` passes.
* In a hook-enabled checkout: a commit on `main`, the message `bad message` and the
  branch `Foo_Bar` are rejected; `feat: add x` on `feature/foo` is accepted.
* The CI job `conventions` fails for non-conforming branches and commits.

## Pros and Cons of the Options

### lefthook with a project shell script

* Good, because single Go binary, fast, simple YAML.
* Good, because rules are testable without the hook tool.
* Bad, because a custom shell script must be maintained.

### pre-commit

* Good, because widely used with a large hook ecosystem.
* Bad, because it requires Python and manages its own hook environments.

### core.hooksPath with committed git hooks

* Good, because zero dependencies.
* Bad, because no staged-file filtering or parallelism; more custom shell code.

### husky with commitlint

* Good, because commitlint is the de-facto conventional commit linter.
* Bad, because it requires Node, which the project otherwise does not need.

## Links

* [Conventional Commits](https://www.conventionalcommits.org/)
* [lefthook](https://github.com/evilmartians/lefthook)
* [Release automation ADR](./0002-use-release-plz-for-release-automation.md)
