# Read Tool Auto-Approve

If a tool should be able to judge whether a given parameter set can be auto-approved,
the `needs_approval()` would need the current tool arguments.

The approval check will return an enum value:

- **AUTO_APPROVED**: The given arguments for the tool are ok to be run
- **AUTO_DENIED**: The given arguments for the tool are not ok to be run
- **USER_APPROVAL_REQUIRED**: A rule explicitly requires the human to decide

## Evaluation model: last-match wins

All rules (named rules + glob list entries) form a single ordered sequence.
Every rule in the sequence is tested against the input path. The **last matching
rule's verdict** is the final answer. Earlier rules act as defaults; later rules
override them.

This is the same semantics as `.gitignore` and `rsync --filter`: a later
`allow` can reopen what an earlier `deny` closed, and a later `deny` can
narrow what an earlier `allow` opened.

**Absent verdict (`None`):** when no rule matches at all, `check_path` returns
`None` — the sandbox has no opinion. Each tool then applies its own default:

- Read → `AutoApproved` (reading is safe when nothing forbids it)
- Write → `UserApprovalRequired` (writing always needs explicit sign-off unless a rule permits it)

This cleanly separates "no rule fired" from `UserApprovalRequired` as an
explicit rule verdict, so a `user:*.secret` glob correctly overrides a read
tool's default auto-approve, and an `allow:dist/**` glob correctly bypasses a
write tool's default user-approval requirement.

**Path resolution:** before evaluating any rule, the input path is first
normalized lexically (collapsing `..` and `.` components without filesystem
access), then resolved via `canonicalize` if the path exists on disk.
All rule evaluation runs against the **canonical path** when available, so
symlinks cannot hide their real location, name, or extension from rules
(e.g. a workspace symlink named `config.txt` pointing at `.env` is caught
by the dot-path check; a symlink pointing outside the workspace is caught
by the workspace-membership check). For paths that do not yet exist
(e.g. a write target), lexical normalization is used as a fallback.

## Rule ordering

```text
[named rules, in TOML declaration order] ← act as early defaults
[glob list entries, top to bottom]       ← can override named rules
```

Named rules (`path_outside_workspace`, `dot_path`) are always evaluated before
the glob list, regardless of where they appear in the TOML file. They are
shorthand for common patterns that would otherwise require explicit glob entries.
A glob rule placed later in the list can override a named rule's verdict.

## Named rules

Each tool can be configured on how to handle the standard scenarios:

```toml
[tool.read]
path_outside_workspace = deny|allow|user  # default: deny
dot_path               = deny|allow|user  # default: deny
cross_filesystem       = deny|allow|user  # default: allow
```

- `path_outside_workspace` fires when the canonicalized path is not under the
  detected workspace root.
- `dot_path` fires when any path component of the path **relative to the
  workspace root** starts with `.` (e.g. `.env`, `.git/config`).
- `cross_filesystem` fires when the canonicalized path resides on a different
  filesystem (device) than the workspace root. Useful when a remote share or
  removable volume is mounted inside the workspace. **Not configured by default**
  (sandbox abstains — the tool's own default applies). Set to `deny` to restrict
  the tool to the workspace's own filesystem, or `user` to escalate cross-device
  paths to human approval. Note: only reliable for paths that exist at check time;
  for non-existent write targets the parent directory's device is compared instead.

## Glob rules

The `globs` list appends further rules after the named rules. Each entry is a
`"verdict:pattern"` string where `verdict` is `allow`, `deny`, or `user`, and
`pattern` is a standard glob (`*`, `?`, `**`, `[...]`). Patterns are matched
against the canonicalized absolute path.

```toml
[tool.read]
path_outside_workspace = deny   # (1) deny outside workspace  [named rule]
dot_path               = deny   # (2) deny dot-paths          [named rule]
globs = [
    "allow:**/.*",              # (3) re-allow dot-files inside workspace
    "user:/**/.env",            # (4) but escalate .env files to user approval
    "allow:~/.cargo/registry/**", # (5) allow cargo registry regardless of workspace
    "allow:/tmp/**",            # (6) allow /tmp regardless of workspace
]
```

Evaluation trace for `~/.cargo/registry/src/lib.rs` (absolute):

- rule (1): outside workspace → AUTO_DENIED ← current last match
- rule (2): not a dot-path → no match
- rule (3): not a dot-file → no match
- rule (4): not `.env` → no match
- rule (5): matches `~/.cargo/registry/**` → AUTO_APPROVED ← current last match
- rule (6): no match
- **Final verdict: AUTO_APPROVED**

Evaluation trace for `<workspace>/.env`:

- rule (1): inside workspace → no match
- rule (2): dot-path → AUTO_DENIED ← current last match
- rule (3): matches `**/.env` → AUTO_APPROVED ← current last match
- rule (4): matches `/**/.env` → USER_APPROVAL_REQUIRED ← current last match
- rules (5)(6): no match
- **Final verdict: USER_APPROVAL_REQUIRED**

## Open Questions

- How to integrate tool config into agent config?
  Currently agent config only includes a flat `allowed_tools` list with no way
  to reconfigure individual tool behaviour per agent type.
  - One approach: named tool variants — e.g. `read-for-research-agent` derives
    from `read` with a narrowed config (only allows reading markdown files within
    the project). The agent type config references the variant by name.
