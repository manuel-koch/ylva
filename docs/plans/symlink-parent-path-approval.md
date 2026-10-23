# Symlink-parent path approval bypass

## Context

Found by the security review in task 1 of the
[public release plan](./public-release-plan.md) (severity medium, confidence 8/10).

The sandbox verdict and the actual file access resolve `..` differently:

- `SandboxContext::normalize` / `evaluate_path` in `src/sandbox/path.rs`
  collapses `..` lexically, then canonicalizes best-effort.
- Execution (`ApprovedTarget::capture` in `src/agent/tools/approved_target.rs`)
  lets the kernel resolve `link/..` physically, i.e. relative to the symlink's
  target.

Example: `ws/link -> ~/.ssh/keys`; the path `link/../id_rsa` is judged as
`ws/id_rsa` (inside the workspace, not a dot path, no rule fires) but opens
`~/.ssh/id_rsa`. `read`, `search` and `glob` auto-approve
when no rule fires; `write` and `edit` show the user a misleading path. A
repository can ship the symlink and a prompt injection can supply the path.

Existing symlink tests (`src/sandbox/path/tests.rs`) only cover symlinks as the
final path component.

## Implementation Plan

1. [x] **Reproduce with failing tests (TDD).** In `src/sandbox/path/tests.rs`
   add cases for `symlink/../file` where the symlink points outside the
   workspace and into a dot directory; assert the verdict matches the
   physically resolved target (`outside_workspace` / `dot_path`).
2. [x] **Evaluate the physical path.** Run the verdict on the same
   `canonicalize_best_effort(std::path::absolute(path))` value that execution
   opens, without lexically collapsing `..` beforehand. Keep the
   non-existent-path (write/create) handling of best-effort canonicalization.
3. [x] **Re-check at execution.** Verify the verdict against
   `ApprovedTarget.canonical_path` so a path changed between approval and access
   (TOCTOU) cannot widen access. Decide the failure mode (error vs. re-prompt)
   consistently with existing approved-target mismatch handling.
4. [x] **Tool-level regression tests.** Add one focused case per affected tool
   family (read/search/glob auto-approval, write/edit prompt path) in the
   existing tool test modules; avoid duplicating step 1 cases.
5. [x] **Check ADRs** in [architecture decisions](../architecture-design-records/)
   covering sandbox/approval; add a record if behavior deviates.
6. [x] **Documentation.** Update [features](../features.md) (path scoping rules)
   and [implementation notes](../implementation-notes.md) (verdict/execution
   path resolution) in the same commit.
7. [x] **Verify** per [development workflow](../development-workflow.md):
   `make format lint` and `make test-coverage`; `rumdl check` on changed docs.
   Then record the commit in [public release plan](./public-release-plan.md)
   task 1.

Exit criterion: no path form (including `symlink/..`) yields an approval verdict
that differs from the location actually accessed.
