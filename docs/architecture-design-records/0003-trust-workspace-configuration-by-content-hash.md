---
status: accepted
date: 2026-10-23
decision-makers: Manuel Koch
consulted:
informed:
---

# Trust workspace configuration by content hash

## Context and Problem Statement

Ylva layers a workspace configuration (`.ylva/profiles/<name>/config.toml`, `.env` and
`.ylva/agents/**`) over the home profile. A cloned repository can therefore set provider
endpoints (sending the user's API keys to another host), interpolate arbitrary
environment variables and mark `[tools.bash]` rules `auto_approved`, which removes the
only safeguard for the deliberately unsandboxed bash tool. Path checks of the builtin
tools do not apply because Ylva itself reads these files at startup, before any tool runs.
How does Ylva ensure that repository-supplied configuration only takes effect with the
user's consent?

## Decision Drivers

* A cloned, untrusted repository must not redirect credentials, read unrelated
  environment variables or silently disable approvals
* Workspace override stays a supported feature
* Approval must not survive later edits (for example after `git pull`)
* Help, version, session and similar commands must work without any approval
* Simple, predictable behavior in non-interactive use

## Considered Options

* Content-hash trust per workspace and profile
* Restrict the workspace layer to harmless keys
* Warn only

## Decision Outcome

Chosen option: **Content-hash trust per workspace and profile**

* `ylva trust` records a SHA-256 over the selected profile's workspace `config.toml`,
  `.env` and all files below `.ylva/agents/` in `<ylva_home>/trusted-workspaces.toml`
  (mode `0600`), keyed by canonical workspace path and profile name. `config.toml` and
  `.env` are normalized to their effective content first, so comments, whitespace, quoting
  and key order do not change the hash; other files are hashed verbatim.
  `ylva trust --revoke` removes the entry.
* At startup the workspace layer is used only when the current hash equals the approved
  one. When the workspace ships covered configuration that is not approved, Ylva refuses
  to start with an error that names the reason (never approved, or changed since approval)
  and `ylva trust`. It does not fall back to the home profile, because running with
  different settings than the workspace intends would be surprising. A workspace without
  covered configuration needs no approval. This is identical for TUI and non-interactive
  runs; there is no prompt.
* The home profile is always trusted. A trusted workspace is treated like the home
  profile, so no additional endpoint, credential or interpolation guards are needed.
* Workspace instruction files (`AGENTS.md`, `SOUL.md`) and skills are not covered; they
  are prompt content, not configuration of credentials or approvals.

## Consequences

* Good, because an untrusted repository cannot change endpoints, secrets or approvals.
* Good, because the full workspace override feature remains available after approval.
* Good, because any edit to a covered file revokes approval automatically.
* Neutral, because the user has to review the files themselves before running `ylva trust`.
* Bad, because every legitimate edit of workspace configuration (including
  `ylva connect --workspace`) requires running `ylva trust` again.
* Bad, because a repository with unapproved configuration cannot be used with Ylva at all
  until the user reviews and approves it (`--version`, `trust`, `init`, `session`,
  `skill`, `profile` and `connect` commands still work).
* Bad, because a user who approves without reading remains exposed.

## Confirmation

* Unit tests of the trust store and hash cover approval, change detection, scoping and
  revocation.
* An integration test runs the binary against a workspace whose `.env` points to a mock
  server: untrusted, startup fails and the mock receives no request; after `ylva trust`, it does.

## Pros and Cons of the Options

### Content-hash trust per workspace and profile

Explicit approval stored as a hash, comparable to `direnv allow`.

* Good, because it keeps all workspace features and closes all three attack paths.
* Good, because the check lives in one place, at configuration loading.
* Bad, because it adds a persisted trust store.

### Restrict the workspace layer to harmless keys

The workspace layer may not set endpoints, interpolate variables or loosen approvals.

* Good, because it needs no store or approval step.
* Bad, because it removes existing, documented features and the rules grow with every new
  sensitive key.

### Warn only

Keep today's behavior and print a notice when sensitive keys appear.

* Good, because it is trivial.
* Bad, because the first request has already leaked credentials when the notice appears.
