# Read Tool Auto-Approve

If a tool should be able to judge whether a given parameter set can be auto-approved,
the `needs_approval()` would need the current tool arguments.

The approval check will return an enum value:

- **AUTO_APPROVED**: The given arguments for the tool are ok to be run
- **AUTO_DENIED**: The given arguments for the tool are not ok to be run
- **USER_APPROVAL_REQUIRED**: Approval check was unable to decide whether it
  would be ok to run the tool with given arguments

## Evaluation model: last-match wins

All rules (named rules + glob list entries) form a single ordered sequence.
Every rule in the sequence is tested against the input path. The **last matching
rule's verdict** is the final answer. Earlier rules act as defaults; later rules
override them.

This is the same semantics as `.gitignore` and `rsync --filter`: a later
`allow` can reopen what an earlier `deny` closed, and a later `deny` can
narrow what an earlier `allow` opened.

**Zeroth rule (implicit, always first):** if no rule matches at all, the verdict
is `USER_APPROVAL_REQUIRED`. Unknown paths go to the human rather than silently
passing or silently failing.

**Path canonicalization:** before evaluating any rule, the input path is
canonicalized to an absolute path. This ensures relative patterns (e.g.
`**/*.md`) and absolute patterns (e.g. `/tmp/**`) are both matched against the
same absolute path string, eliminating ambiguity between the two forms.

## Rule ordering

```text
[zeroth rule: USER_APPROVAL_REQUIRED]   ← implicit, always first
[named rules, in TOML declaration order] ← act as early defaults
[glob list entries, top to bottom]       ← can override named rules
```

Named rules (`path_outside_workspace`, `dot_path`) are always evaluated before
the glob list, regardless of where they appear in the TOML file. They are
shorthand for common patterns that would otherwise require explicit glob entries.
A glob rule placed later in the list can override a named rule's verdict.

## Named rules

Read tool could be configured on how to handle the standard scenarios:

```toml
[tool.read]
path_outside_workspace = deny|allow|user  # default: deny
dot_path               = deny|allow|user  # default: deny
```

- `path_outside_workspace` fires when the canonicalized path is not under the
  detected workspace root.
- `dot_path` fires when any path component starts with `.` (e.g. `.env`,
  `.git/config`).

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

- zeroth rule → USER_APPROVAL_REQUIRED (no match yet)
- rule (1): outside workspace → AUTO_DENIED ← current last match
- rule (2): not a dot-path → no match
- rule (3): not a dot-file → no match
- rule (4): not `.env` → no match
- rule (5): matches `~/.cargo/registry/**` → AUTO_APPROVED ← current last match
- rule (6): no match
- **Final verdict: AUTO_APPROVED**

Evaluation trace for `<workspace>/.env`:

- zeroth rule → USER_APPROVAL_REQUIRED
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
