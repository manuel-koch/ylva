# Git hooks, conventional commits and GitHub CI/release automation

## Context

Ylva (Rust TUI agent harness, crate `ylva`, binary + library) has no git hooks, no
commit-message convention, no branch-naming convention and no CI. Commits land
directly on `main`; history is free-form (e.g. "Fix cargo audit issue").
Branches exist as `feature/<name>`.

Goal: establish enforced, local-first guardrails and a CI/release pipeline:

- prevent commits on `main`
- enforce [Conventional Commits](https://www.conventionalcommits.org/) messages
- enforce conventional branch names (`feature/`, `bugfix/`, `hotfix/`, `release/`, `chore/` …)
- decide the hook tool (candidates: `pre-commit`, `lefthook`, alternatives) based on
  reputation and fit for a Rust project
- decide whether format/lint/test run in hooks or only in CI
- set up GitHub Actions: builds on feature branches, automatic release on `main`
  commits, and semantic-version control of the `Cargo.toml` version

Acceptance criteria (definition of done):

1. A committed hook configuration, installable with one `make` target, rejects commits on
   `main`, non-conventional commit messages and non-conventional branch names, with a clear
   error message each. Verified by deliberately violating each rule.
2. Hooks are documented, including how to bypass in emergencies (`--no-verify`) and the
   fact that CI re-checks the same rules (hooks are not a security boundary).
3. CI runs on feature-branch pushes/PRs: `make format lint`, `make test-coverage`, and
   `make audit` (when `Cargo.toml`/`Cargo.lock` change), reusing Makefile targets.
4. A release workflow on `main` derives the next semantic version from conventional commits,
   updates `Cargo.toml`/`Cargo.lock`, tags `vX.Y.Z` and creates a GitHub release with notes.
5. The decision (tool choice, hooks vs CI scope, versioning tool) is recorded as an ADR.
6. All affected docs are updated in the same change set.

Constraints:

- Project rules in `AGENTS.md`: no staging/committing/branching without user consent; docs
  are part of the change set; new doc files require asking the user first; every
  project-authored source file needs the Apache-2.0 license header
  (`docs/development-workflow.md` → *License headers*; applies to source files and shell
  scripts, not to config files such as YAML/TOML); full descriptive names, no abbreviations.
- Initial public support is macOS only (see `docs/plans/public-release-plan.md`, task 9
  "Add macOS CI before announcement": workflows under `.github/workflows/` reusing Makefile
  targets, no secrets or paid-provider calls in untrusted PR jobs). This plan must stay
  consistent with that task, not duplicate it.
- Hooks must stay fast; the full test gate (`make test-coverage`) is slow.
- Merge strategy affects which commit messages reach `main` (squash merge → PR title must
  be conventional); this is unresolved, see Open Questions.

## Implementation Plan

An implementing agent must flip `[ ]` to `[x]` immediately after completing each step and
rewrite this file in place so progress is resumable across sessions. Obtain user consent
before editing files, creating new doc files, or staging/committing.

1. [x] **Resolve the open questions** with the maintainer (resolved; see Implementation Hints →
   *Decisions*).
2. [x] **Record two ADRs** in `docs/architecture-design-records/` (directory currently empty;
   use the `architecture-decision-record` skill and its template) one for hooks and conventions
   (hook tool, hooks-vs-CI scope, commit/branch rules), one for release automation (release-plz,
   versioning, merge-commit strategy, CI scope).
3. [x] **Add the hook configuration** `lefthook.yml` at the repo root with:
   - `pre-commit`: block when current branch is `main`; validate branch name against the
     regex `^(main|(feature|bugfix|hotfix|release|chore|docs|refactor|test)/[a-z0-9._-]+)$`
     (`main` stays a valid name so the no-commit-on-main check reports its own message);
     fast checks on staged files: `cargo fmt --check` for `*.rs`, `rumdl check` for `*.md`.
     No clippy/tests in hooks.
   - Skip main/branch checks on detached HEAD or while a rebase/merge/cherry-pick is in
     progress (detect via `.git/rebase-*`, `MERGE_HEAD`, `CHERRY_PICK_HEAD`).
   - `commit-msg`: validate Conventional Commits (`type(scope)!: description`; allowed types
     `feat fix docs style refactor perf test build ci chore revert`). Prefer a small
     check script or a standalone binary (e.g. `committed`, `cocogitto`) over
     requiring Node/commitlint, to keep the Rust-only toolchain.
   - Allow merge (`Merge pull request #N`, `Merge branch …`), revert and fixup/squash
     auto-generated messages and the `Co-authored-by` trailer used by Copilot commits. The
     check script/binary used by hooks and CI (steps 3 and 6) must implement this skip once.
4. [x] **Add Makefile targets** `install-git-hooks` (runs `lefthook install`;
   add `uninstall-git-hooks` if cheap) and a non-mutating `format-check` target
   (`cargo fmt --check` + `rumdl check`; `make format` rewrites files and cannot gate CI/hooks)
   to `Makefile` and its `.PHONY` list; mirror them in the
   target list in `docs/development-workflow.md`. Add a `check-branch-name` / `check-commit-message`
   script or target only if the hook tool cannot express the check directly (KISS/YAGNI).
5. [x] **Test the hooks**: add a script-level or shell test only if custom scripts are
   written; otherwise verify manually and record the commands in the ADR/dev-workflow doc.
   Manual verification: commit on `main` (rejected), commit with message `bad message`
   (rejected), `feat: add x` (accepted), branch `Foo_Bar` (rejected), `feature/foo` (accepted).
6. [x] **Add GitHub CI workflow** `.github/workflows/ci.yml`: triggers on `pull_request`
   and `push` to non-`main` branches; macOS runner; install Rust toolchain
   (rustfmt, clippy, llvm-tools-preview), `cargo-llvm-cov`, `rumdl`, `cargo-audit`; cache
   cargo; run `make format-check`, `make lint`,
   `make test-coverage`, `make audit`. Add a job that re-validates every commit message of
   the PR range (merge-commit strategy: all branch commits land on `main`) and the branch
   name by running the same script/binary as the hooks (e.g. over `origin/main..HEAD`),
   not a third-party Action. Skip merge commits. Run on macOS only, on PRs and feature-branch
   pushes. No secrets; live-provider tests
   (`test-e2e-persistence`) must not run. Use only the non-mutating
   `make format-check` from step 4.
7. [ ] **Tag the baseline** `v0.31.1` on current `main` (with maintainer consent; no tags exist).
8. [x] **Add release workflow** `.github/workflows/release.yml` on push to `main` using
   `release-plz` in release-PR mode: it opens/updates a release PR bumping `Cargo.toml` and
   `Cargo.lock` from conventional commits; merging it creates tag `vX.Y.Z` (same format as
   the former `make tag-version`) and a GitHub release with generated notes. GitHub releases only: no
   crates.io publishing (`publish = false` in release-plz config) and no binaries. Add
   `release-plz.toml` as needed: configure the changelog/commit parsers to skip merge commits
   and non-conventional commits (baseline tag from step 7 excludes pre-existing history).
   Authenticate with a fine-grained PAT repo secret (contents and pull-requests write) so
   release PRs trigger CI; never expose it to fork PR jobs.
9. [ ] **Configure branch protection** ruleset on `main` (GitHub setting, document steps):
   require PR and passing CI, merge-commit method only, bypass for the PAT owner used by release-plz.
10. [x] **Remove `make tag-version`** (Makefile target, `.PHONY` entry and the list in
    `docs/development-workflow.md`); release-plz is the single source of truth for tags.
    Update `docs/release-notes-workflow.md`: release-plz's generated `CHANGELOG.md` feeds the
    GitHub release; the curated notes workflow stays as an optional manual summary.
11. [x] **Add a PR template** `.github/pull_request_template.md`: short checklist (conventional
    commits, branch name, docs updated, `make format lint test-coverage` run). Local hooks can
    be bypassed; the template and the CI checks (steps 6 and 9) are the server-side backstop.
12. [x] **Add a commit message template** `.gitmessage` with the Conventional Commits format
    and allowed types as comments; have `make install-git-hooks` (step 4) set
    `git config --local commit.template .gitmessage`.
13. [x] **Add Dependabot** `.github/dependabot.yml` for `cargo` and `github-actions` ecosystems
    (weekly), with `commit-message.prefix: "chore(deps)"` so its commits pass the conventional
    commit CI check and release-plz parsing.
14. [x] **Reconcile `docs/plans/public-release-plan.md` task 9** (macOS CI): mark it as
    implemented by this plan's step 6 (cross-reference, no duplicate workflow), keeping its
    constraints (no secrets or paid-provider calls in untrusted PR jobs).
15. [x] **Update documentation** in the same change set:
    - `docs/development-workflow.md`: hooks section (install target, rules, bypass, commit
      template), CI contract, PR template, Dependabot, make-target list (incl.
      `format-check`), versioning/release flow.
    - `README.md`: contributor quickstart mention of `make install-git-hooks` and the
      commit/branch conventions (link to development-workflow; do not duplicate).
    - `AGENTS.md`: instruct agents to write conventional commit messages, use
      conventional branch names and never pass `--no-verify` (keeping the consent rule).
    - `docs/release-notes-workflow.md`: relation to conventional commits/automated notes.
    - `CONTEXT.md` only if new domain terms are introduced.
    - Run `rumdl fmt` on changed Markdown.
16. [ ] **Verify** (done locally: hooks, `make format-check`, `make test-git-conventions`;
    open: CI push, release-plz dry-run, both need GitHub): `make format lint`,
    `make test-coverage` (no Rust code changes expected, so this should be unaffected),
    run the hook violation checks from step 5, confirm the
    commit template is active, push a test branch to confirm the CI workflow, dry-run
    release-plz, and run `make audit` only if dependencies changed.
17. [ ] **Remove or mark the idea file** `docs/ideas/git-hooks-and-conventional-commits.md`
    as implemented (with user consent), following how earlier implemented plans/ideas were
    handled (see git log: "Remove implemented plan").

## Implementation Hints

Decisions (maintainer-confirmed 2026-10-22):

- License headers: only source files and shell scripts (shebang first, header after) carry
  the header; config files (`lefthook.yml`, workflows, `release-plz.toml`) do not.
- Hooks skip the branch-name and main checks on detached HEAD or during rebase, merge or
  cherry-pick; `commit-msg` validates only new messages.
- release-plz authenticates with a fine-grained PAT stored as a repo secret (so release PRs
  trigger CI); the ruleset bypass applies to it.
- Hook tool: `lefthook`. Local scope: branch/main guard, commit message, `cargo fmt --check`
  and `rumdl check` on staged files; lint/tests/audit only in CI.
- Branch names: `main` or `<prefix>/<lowercase-kebab>` with prefix one of
  `feature bugfix hotfix release chore docs refactor test`; no issue number required.
- CI commit check: same script/binary as the `commit-msg` hook; merge commits skipped; CI
  macOS only (PRs + feature-branch pushes). release-plz also ignores merge commits.
- Two ADRs (hooks/conventions; release automation).
- Merge strategy: merge commits (all branch commits reach `main`, so each must be conventional;
  GitHub's `Merge pull request #N` commits are non-conventional and must be ignored by the
  commit checks and by release-plz changelog/bump parsing).
- "No commits on main" is enforced locally without exceptions; server-side ruleset with release
  bot bypass; docs-only changes go through a short PR.
- Release: `release-plz` release-PR mode, GitHub releases only; baseline tag `v0.31.1`.
- Tools already installed locally: `/opt/homebrew/bin/pre-commit`, `/opt/homebrew/bin/lefthook`;
  not installed: `cargo-release`, `git-cliff`. Cargo bin has `cargo-llvm-cov`.
- Tool comparison (background for the ADR): `pre-commit` — Python-based, very widely used,
  huge hook ecosystem, manages hook envs; `lefthook` — single Go binary, no runtime, fast,
  parallel, simple YAML, popular in non-Python projects; alternatives: `cargo-husky` (Rust,
  build-script installs hooks, limited), `husky` (Node), plain `core.hooksPath` with a
  committed `.githooks/` dir (zero dependency, KISS). Rust-native conventional-commit
  linters: `committed`, `cocogitto` (`cog`, also bumps versions/changelog).
- Rust release tooling options: `release-plz` (release PR from conventional commits,
  crates.io-oriented, handles Cargo.lock), `cargo-release` (local-driven), `cocogitto`,
  `release-please` (Google, Rust strategy supported), `git-cliff` (changelog only).
- Makefile: `.PHONY` list at the top; `lint` = `cargo check` + `cargo clippy`;
  `format` = `format-rust` (`cargo fmt`) + `format-markdown` (`rumdl fmt`);
  `test-coverage` = `cargo llvm-cov` with `COVERAGE_THRESHOLD := 80`; `audit` = `cargo audit`;
  `tag-version` tags `v<version from Cargo.toml>` at HEAD.
- `Cargo.toml` package version is `0.31.1`, repo `https://github.com/manuel-koch/ylva`.
- No `.github/` directory exists; no tags exist; existing branches follow `feature/<name>`.
- Commits created by Copilot CLI include a `Co-authored-by: Copilot <…>` trailer; the
  message validator must allow trailers.
- `docs/architecture-design-records/` exists but is empty; `docs/development-workflow.md`
  is the source of truth for make targets (docs mirror, must be updated with Makefile).
- Docs are linted with `rumdl` using `.rumdl.toml`.
- Pre-commit hooks that run `cargo clippy`/tests are slow and cause `--no-verify` habits;
  common practice is fast checks (fmt, branch/message) locally and full verification in CI.
