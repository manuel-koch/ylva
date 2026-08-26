# Read Tool Auto-Approve

If a tool should be able to judge whether a given parameter set can be auto-approved,
the `needs_approval()` would need the current tool arguments !

The approval check will return an enum value:

- **AUTO_APPROVED**: The given arguments for the tool are ok to be run
- **AUTO_DENIED**: The given arguments for the tool are not ok to be run
- **USER_APPROVAL_REQUIRED**: Approval check was unable to decide whether it
  would be ok to run the tool with given arguments

Auto handle the following scenarios:

1. deny: files outside current workspace / cwd
2. deny: any file that starts with "." ( like ".env" )
3. allow/deny: whether or not matched by the `deny_globs`
4. allow/deny: whether or not matched by the `allow_globs`

`deny` verdict has precedence over `allow` verdict.

Read tool could be configured on how to handle rule 1. and 2.:

- `deny` -> `AUTO_DENIED`
- `allow` -> `AUTO_APPROVED`
- `user` -> `USER_APPROVAL`

```toml
[tool.read]
path_outside_workspace = deny|allow|user (default=deny)
dot_path = deny|allow|user (default=deny)
globs = [
    "deny:*.md" # deny any markdown files in workspace root
    "deny:**/*.pdf" # deny any pdf files in workspace
    "allow:**/.*" # allow any dot file in workspace
    "user:/**/.*" # delegate any dot file to user approval
    "allow:~/.cargo/registry/**"
    "allow:/tmp/**"
]
```

## Open Questions

- How to integrate tool config into agent config ?
  Currently agent config only seems to include a flat `allowed_tools` list.
  There is no way to re-configure or override specific config per tool for
  just this agent.
  - we could introduce custom tools with a new name ( keeping the title ),
    that derive from a known tool and configure it.
    E.g. `read-for-research-agent` ( only allows reading markdown files within project ),
    could be used by the `research-agent` config to include a specialized `read` tool.
