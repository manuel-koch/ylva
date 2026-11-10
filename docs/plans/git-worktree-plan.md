# Share session storage across git worktrees of one repository

## Context

Ylva stores sessions under `${YLVA_HOME:-~/.ylva}/chats/<project_key>/`. Today
`project_key(cwd)` (`src/store.rs`) is the SHA-256 hex digest of the canonicalized
launch directory. Its doc comment and `docs/persistence.md` ("`project_key`
derivation") state it is deliberately not the git root, because a plain non-git
directory is a first-class project. No ADR records this; it is a simplicity default.

A git worktree (`git worktree add`) is a second checkout of the same repository in
another directory. Ylva sees it as an unrelated directory, so sessions of the same
repository are split across worktrees. Switching branches inside one checkout already
changes the files a resumed session refers to, so a worktree adds no new class of
staleness; sharing sessions is acceptable.

Goal: sessions launched from a worktree share storage with the same place in the
main checkout, while everything that affects safety stays anchored to the launch
directory.

Decided design: the key is the hash of the **equivalent path in the main
worktree**.

1. Not inside git, or identity cannot be resolved: canonical launch directory
   (unchanged).
2. Inside the main checkout: canonical launch directory (unchanged, so existing
   sessions keep their key; no migration).
3. Inside a linked worktree: `anchor / (launch directory relative to the linked worktree root)`,
   where the anchor is the parent of the common git directory when that is named `.git` (the main
   checkout root), otherwise the common directory itself (bare repository). The anchor need not
   exist on disk. A linked worktree root therefore maps to the main checkout root; a subdirectory
   maps to the same subdirectory in the main checkout.

Acceptance criteria:

- Launching from a linked worktree root and from the main checkout root yields the
  same `project_key`; `ylva session list` and resume see the same sessions.
- Launching from `<linked>/a/b` yields the same key as `<main>/a/b`, and a different
  key than `<main>`.
- Main checkout keys are bit-identical to today's keys.
- Bare-repository worktrees map to the bare directory as anchor.
- A deleted or moved main checkout does not change the key (the anchor is only an
  identifier).
- Non-git directories, submodules and unreadable or malformed git metadata fall
  back to the canonical launch directory without error.
- Sandbox root, workspace root discovery, workspace trust and instruction loading
  are unchanged (still the launch directory / nearest marker).
- No `git` process is spawned; `.git` metadata is read directly.
- Docs and a new ADR record the decision; `make format lint` and
  `make test-coverage` pass.

Out of scope (YAGNI): recording branch or worktree path in session metadata,
filtering `session list` by worktree, migrating existing sessions.

## Implementation Plan

Implementing agent: flip `[ ]` to `[x]` immediately after finishing each step and
rewrite this file in place so progress is resumable across sessions.

1. [x] Read `docs/known-test-issues.md` (cwd and `tracing` entries),
   `docs/development-workflow.md` (Testing strategy, License headers, Logging) and
   `docs/architecture-design-records/0003-*.md` for the ADR format.
2. [x] Write failing unit tests first (TDD) in the existing store test layout
   (`src/store/tests/`), using temporary directories with hand-built git metadata
   (no real `git` binary): main checkout (`.git` directory) keeps the plain key;
   linked worktree root equals main root key; linked worktree subdirectory equals the
   same main subdirectory key and differs from the root key; non-git directory
   unchanged; submodule-style `.git` file (no `commondir`) falls back to cwd;
   `.git` file with unreadable or malformed content falls back; `commondir` pointing
   to a missing main checkout still yields the main-equivalent key; bare-repository
   worktree maps to the bare directory anchor; symlinked paths still canonicalize to
   one key. Add one test that runs real `git worktree add` in a temporary
   repository and asserts the key equals the main checkout's; skip it when `git` is
   unavailable (first check `docs/known-test-issues.md` for subprocess/cwd rules and
   isolate git config via `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` if needed).
3. [x] Add a git identity resolver (new file-per-module, e.g.
   `src/store/project_identity.rs`, declared from `src/store.rs`; include the license
   header) with a function like `equivalent_main_worktree_path(canonical_cwd: &Path)
   -> Option<PathBuf>`. Algorithm: walk `canonical_cwd.ancestors()` to the nearest
   entry named `crate::GIT_ENTRY_NAME`; if it is a directory, return `None` (main
   checkout, plain key); if it is a file, parse the `gitdir: <path>` line (relative
   paths resolve against the file's parent), read `<gitdir>/commondir` (relative to
   `<gitdir>`), canonicalize the result best-effort (do not require existence of the anchor),
   derive the anchor (parent if the common dir is named `.git`, else the common dir
   itself), and return `anchor.join(canonical_cwd.strip_prefix(linked_root))`.
   Return `None` only when metadata is absent or malformed (no `commondir`, missing
   `gitdir:` line, unreadable files). Log fallbacks at `debug` via `tracing`.
4. [x] Change `project_key` in `src/store.rs` to hash the resolver's path when
   `Some`, else the canonical cwd. Keep the signature so all callers
   (`src/store.rs` `create`/`resume`, `src/session_command.rs`,
   `src/startup/session.rs`, `src/tui_driver/sessions.rs`) need no change. Update the
   doc comment; remove "Deliberately not the git root" in favor of the new rule.
5. [x] Run the tests from step 2 until green; add one integration-level check only
   if a `tests/` boundary exercises `ylva session list` from a worktree directory
   cheaply (otherwise skip; the unit tests cover the contract).
6. [x] Add ADR `docs/architecture-design-records/0004-group-sessions-by-main-worktree-path.md`
   following the format of the existing ADRs (context, options considered:
   cwd-only, git root, main-worktree-equivalent path; decision; consequences:
   sessions shared across worktrees and branches, subdirectory scoping kept, safety
   boundaries still launch-directory based, no migration). State explicitly that
   workspace trust (ADR-0003) stays per worktree path while sessions are shared:
   trust is a safety boundary, sessions are not; the asymmetry is deliberate.
7. [x] Update docs in the same change: `docs/persistence.md` ("`project_key`
   derivation", remove "does not use the git root", describe worktree mapping and
   fallbacks); `docs/features.md` (Sessions bullet around line 186 mentions the
   canonicalized launch directory); `docs/implementation-notes.md` only if a
   cross-module constraint is worth a few sentences (re-read its top guideline
   first); `CONTEXT.md` only if a new term such as "Project key" is needed (ask the
   user before creating any new doc file; editing existing ones is fine). Run
   `rumdl fmt` then `rumdl check` on the changed Markdown.
8. [x] Run `make format lint` then `make test-coverage` (coverage threshold in
   `docs/development-workflow.md`). `make audit` is not needed (no dependency
   change).
9. [x] Report done only when all steps above are checked and docs are updated.
   Do not commit or stage without explicit user consent.

## Consistency Issues

- [low] `ylva session delete --all` and `--before` operate on one project key, so
  after this change they affect sessions from every worktree of the repository
  (accepted, YAGNI; read-only previews exist). Update the wording in
  `docs/cli-options.md` (Session commands) if it says "this directory".
- [low] A resumed session started in a different worktree may carry stale file
  references. This matches today's branch-switch behavior and needs no code
  change; mention it in `docs/persistence.md`.

## Implementation Hints

- `project_key` lives in `src/store.rs` (around line 96) and uses
  `SandboxContext::canonicalize_best_effort` (`src/sandbox.rs`) and `sha2::Sha256`;
  it is re-exported from `src/lib.rs`. Test callers pass temp directories directly
  (`src/session_command/tests.rs`, `src/tui_driver/tests/sessions.rs`,
  `src/store/tests/maintenance.rs`).
- Workspace root discovery (`discover_workspace_root` in `src/config/layered.rs`)
  already treats any existing `.git` entry (file or directory) as a marker via
  `crate::GIT_ENTRY_NAME`, so a linked worktree is already its own workspace root.
  Workspace trust is keyed by canonical workspace root
  (`src/config/workspace_trust.rs`, `workspace_key`); trust therefore stays
  per-worktree and must not be touched.
- Git layout facts: main checkout has `.git/` directory. A linked worktree has a
  `.git` file containing `gitdir: <main>/.git/worktrees/<name>`; that directory
  holds a `commondir` file (usually `../..`) pointing to `<main>/.git`. Submodules
  have a `.git` file pointing into `<super>/.git/modules/<name>` with no
  `commondir`. Bare-repository worktrees have a common dir not named `.git`.
- Workspace trust (ADR-0003) stays per worktree workspace root by design; sessions
  are shared. Record this asymmetry in the new ADR.
- `CONTEXT.md` defines Workspace root as nearest marker or launch directory; the
  sandbox root is the canonicalized launch directory. Neither changes.
- Project conventions (AGENTS.md): full descriptive names, license headers on every
  new source file, `foo.rs` file-per-module (no `mod.rs`), docs updated in the same
  commit, tests per `docs/development-workflow.md`, coverage threshold 80%.
