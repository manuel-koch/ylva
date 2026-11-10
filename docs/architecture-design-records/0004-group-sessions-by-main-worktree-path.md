---
status: accepted
date: 2026-11-10
decision-makers: Manuel Koch
consulted:
informed:
---

# Group sessions by main worktree path

## Context and Problem Statement

Ylva stores sessions under `chats/<project_key>/`, where the key hashes the canonicalized
launch directory. A linked git worktree (`git worktree add`) is another checkout of the same
repository in a different directory, so its sessions were split from those of the main
checkout. Switching branches in one checkout already changes the files a resumed session
refers to, so a worktree adds no new class of staleness. How should worktrees of one
repository share sessions without weakening safety boundaries?

## Decision Drivers

* Worktrees of one repository should see the same sessions
* Existing session keys must stay valid (no migration)
* Non-git directories remain first-class projects
* Subdirectory scoping of sessions is kept
* No `git` process is spawned at startup; unreadable metadata must not cause errors

## Considered Options

* Canonical launch directory only
* Git root
* Equivalent path in the main worktree

## Decision Outcome

Chosen option: **Equivalent path in the main worktree**

* Not inside git or unresolvable metadata: canonical launch directory.
* Inside a main checkout (`.git` is a directory): canonical launch directory.
* Inside a linked worktree (`.git` file with `gitdir:` whose directory has `commondir`):
  the anchor is the parent of the common git directory when that is named `.git`, otherwise
  the common directory (bare repository). The key hashes the anchor joined with the launch
  directory relative to the linked worktree root. The anchor need not exist.
* Submodules (no `commondir`) and malformed metadata fall back to the launch directory.
* Git metadata files are read directly; no `git` process is spawned.
* Sandbox root, workspace root discovery, workspace trust and instruction loading are
  unchanged. Workspace trust ([ADR-0003](0003-trust-workspace-configuration-by-content-hash.md))
  stays per worktree path while sessions are shared: trust is a safety boundary, sessions
  are not. The asymmetry is deliberate.

## Consequences

* Good, because worktrees share and resume the same sessions.
* Good, because main checkout and non-git keys are unchanged; no migration.
* Good, because a subdirectory of a worktree maps to the same subdirectory of the main
  checkout.
* Neutral, because a resumed session may reference files of another branch, as after a
  branch switch.
* Bad, because `ylva session delete --all` and `--before` affect sessions of every
  worktree of the repository.

## Confirmation

Unit tests in `src/store/tests/project_identity.rs` build git metadata by hand (main,
linked, submodule, malformed, bare, missing main, symlink) and run one real
`git worktree add`.

## Pros and Cons of the Options

### Canonical launch directory only

* Good, because it is the simplest rule.
* Bad, because sessions are split across worktrees of one repository.

### Git root

* Good, because all subdirectories share sessions.
* Bad, because it changes existing keys for subdirectory launches and treats non-git
  directories differently.

### Equivalent path in the main worktree

* Good, because it keeps existing keys and subdirectory scoping while merging worktrees.
* Bad, because it requires reading git metadata files.

## Links

* Related: [ADR-0003](./0003-trust-workspace-configuration-by-content-hash.md) (workspace
  trust stays per worktree path)
