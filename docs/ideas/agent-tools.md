# Agent Tools

Allow an agent to configure its available tools and
ability to configure the tool rules too.

We could allow the same `[tool.<tool>]` config snippets within agent config
TOML/YAML:

`config.toml`:

```toml
[agents.custom]
description = "Custom agent definition."
engine = "ollama/local"
tools_available = ["read", "write"]

[agents.custom.tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { pattern = "**/build/**", verdict = "auto_approved" },
]
```

`agents/custom.yaml`:

```yaml
---
engine: "ollama/local"
tools_available: ["read","write"]
tools:
  write:
    - rule: "outside_workspace"
      verdict: "auto_denied"
    - pattern: "**/build/**"
      verdict: "auto_approved"
---
You are an utility agent with customized tool config.
```
