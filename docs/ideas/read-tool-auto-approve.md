# Read tool auto-approve

The read tool already auto-approves when the sandbox has no opinion, and its
path normalization, canonicalization, configured-rule evaluation, and CLI
catch-all precedence are implemented. The remaining work is a configurable
cross-filesystem policy.

Add `cross_filesystem` as a path-based named rule. In the current unified
configuration shape, it should be usable in the read tool's ordered rule list:

```toml
[tool.read]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { rule = "dot_path", verdict = "auto_denied" },
  { rule = "cross_filesystem", verdict = "user_approval_required" },
]
```

Define and implement the missing semantics:

- compare the candidate path's device/filesystem with the canonical workspace
  root, using the best available existing ancestor when the target itself does
  not exist;
- support the existing verdicts (`auto_approved`, `auto_denied`, and
  `user_approval_required`) and optional denial reasons;
- preserve ordered last-match-wins evaluation, including the fact that a
  configured `[tool.read]` rule list replaces the default floor and therefore
  must include `outside_workspace` and `dot_path` when those protections are
  desired;
- keep the same rule available through an agent-scoped
  `[agents.<type>.tools.read]` override; and
- preserve no-match abstention, which the read tool interprets as
  auto-approval, plus the CLI catch-all rule's final precedence.

Add configuration and sandbox tests covering same-device paths, mounted or
otherwise cross-device paths, missing targets, each verdict, rule ordering, and
agent-scoped configuration.
