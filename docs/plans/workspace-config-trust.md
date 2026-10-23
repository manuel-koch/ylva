# Workspace configuration trust gate

## Context

Found by the security review in task 1 of the
[public release plan](./public-release-plan.md) (severity medium, confidence 7/10).

The workspace layer (`.ylva/profiles/<name>/config.toml` and `.env`) is loaded
without any trust decision
(`src/config.rs` `load_profile_environment_at_workspace_root`,
`src/config/profile.rs` workspace layer and `${ENV}` interpolation,
`src/llm/provider_registry.rs` `apply_missing_api_key*`). A cloned repository can
therefore:

- set a provider `api_base_url` (or `OLLAMA_BASE_URL` / `OPENROUTER_BASE_URL` via
  its `.env`) so the user's `COPILOT_GITHUB_TOKEN`, `OPENROUTER_API_KEY` or
  `OLLAMA_API_KEY` is sent to an attacker host;
- interpolate any process environment variable (`api_key = "${ANY}"`, URLs);
- set `[tools.bash]` rules to `auto_approved`, disabling approval prompts.

Workspace override is documented, intended behavior; the gap is the missing
consent step and warning.

## Decisions required before implementation

Ask the maintainer (do not assume):

- Trust model: per-workspace explicit trust (prompt + persisted allowlist keyed
  by canonical workspace path) **or** restricting the workspace layer to
  non-secret, non-endpoint, non-approval keys, **or** both.
- Behavior for untrusted workspaces in non-interactive CLI use (ignore layer
  with warning vs. configuration error, following the existing
  configuration-error exit convention).
- Whether credentials may ever be attached to a base URL that did not come from
  the home profile.

## Implementation Plan

1. [ ] **Record the decision** as an ADR in
   [architecture decisions](../architecture-design-records/) (use the
   architecture-decision-record skill) covering the chosen trust model.
2. [ ] **Failing tests first (TDD).** Configuration tests: an untrusted workspace
   profile/`.env` does not change provider endpoints, interpolated secrets or
   bash approval rules; a trusted one behaves as today (precedence preserved,
   missing-variable errors still explicit).
3. [ ] **Implement the trust gate** in config loading (start at
   `src/startup/profile.rs` and `src/config/profile.rs`), plus the persisted
   trust store (private file permissions via the existing `private_fs`
   helpers) if per-workspace trust is chosen.
4. [ ] **Credential/endpoint guard.** Refuse to attach provider credentials to an
   `api_base_url` not originating from a trusted layer; return an actionable
   error.
5. [ ] **Interpolation scope.** Limit which environment variables untrusted or
   workspace-layer values may interpolate, if the decision keeps any
   workspace-layer secrets.
6. [ ] **User interface.** Add the trust prompt/warning and any CLI flag or
   command; keep help, version and session commands usable without it.
7. [ ] **Documentation.** Update [configuration](../configuration.md),
   [CLI options](../cli-options.md) (if a flag/command is added),
   [features](../features.md), [implementation notes](../implementation-notes.md),
   [README](../../README.md) trust/safety wording, and
   [persistence](../persistence.md) if a trust store is added.
8. [ ] **Verify** per [development workflow](../development-workflow.md):
   `make format lint`, `make test-coverage`; `rumdl check` on changed docs.
   Then record the commit in [public release plan](./public-release-plan.md)
   task 1.

Exit criterion: cloning and running Ylva inside an untrusted repository cannot
redirect credentials, read unrelated environment variables, or silently disable
approvals.
