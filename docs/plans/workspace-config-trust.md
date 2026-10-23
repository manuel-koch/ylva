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

## Decisions

Recorded in [ADR-0003](../architecture-design-records/0003-trust-workspace-configuration-by-content-hash.md):

- Trust model: explicit, direnv-style approval of a content hash (normalized
  `config.toml` and `.env`, plus `.ylva/agents/**`) per canonical workspace
  path and profile, stored in `~/.ylva/trusted-workspaces.toml`.
- Untrusted or changed workspace configuration makes startup fail with an
  actionable error (no fallback to the home profile), in TUI and
  non-interactive runs alike.
- Approval is CLI-only (`ylva trust [--revoke]`); there is no prompt. A trusted
  workspace is treated like the home profile, so separate endpoint, credential
  and interpolation guards are unnecessary.

## Implementation Plan

1. [x] **Record the decision** as ADR-0003.
2. [x] **Tests first.** Unit tests for hash, normalization, scoping, revocation
   and store permissions; integration tests in `tests/cli_workspace_trust.rs`.
3. [x] **Implement the trust gate** in `src/config/workspace_trust.rs` and
   `src/startup/profile.rs`, with the private trust store.
4. [x] **User interface.** `ylva trust [--revoke]`; help, version and session
   commands do not load a profile and stay usable.
5. [x] **Documentation.** [configuration](../configuration.md),
   [CLI options](../cli-options.md), [features](../features.md),
   [implementation notes](../implementation-notes.md),
   [README](../../README.md) and [persistence](../persistence.md).
6. [x] **Verify** per [development workflow](../development-workflow.md):
   `make format lint`, `make test-coverage`; `rumdl check` on changed docs.
   Then record the commit in [public release plan](./public-release-plan.md)
   task 1.

Exit criterion: cloning and running Ylva inside an untrusted repository cannot
redirect credentials, read unrelated environment variables, or silently disable
approvals.
