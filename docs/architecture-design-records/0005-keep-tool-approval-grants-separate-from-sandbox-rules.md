---
status: accepted
date: 2026-11-24
decision-makers: Manuel Koch
consulted:
informed:
---

# Keep tool-approval grants separate from sandbox rules

## Context and Problem Statement

Every tool call whose sandbox verdict is `UserApprovalRequired` prompts the user, even
when the user approved a similar call a moment ago. Users want reusable approvals
(*grants*) without weakening the sandbox policy, without turning Ylva into an OS
isolation boundary and without letting a cloned repository pre-approve tool calls.
Where do grants live, how do they relate to the configured rule lists, and how are
persistent grants trusted?

## Decision Drivers

* An effective `AutoDenied`, a schema error, a disabled tool or a mandatory interaction
  (`clarify`) must never be overridden by a grant
* Grants are user-owned and per workspace and profile; rule lists stay configuration
* A cloned repository must not ship grants that silently approve calls (ADR 0003)
* One evaluation path for the TUI and headless runs
* Matching must judge the same resolved value as the sandbox policy

## Considered Options

* Separate grant store evaluated after the policy verdict
* Generate `auto_approved` entries in the `[tools.*]` rule lists
* Remember approvals only for the running session

## Decision Outcome

Chosen option: **Separate grant store evaluated after the policy verdict**

* Evaluation order: validate arguments, then the existing tool policy. Only an ordinary
  `UserApprovalRequired` verdict is looked up in the grants; every other verdict, and
  every tool that declares mandatory human interaction, keeps its behavior. Grants never
  change `ApprovalCheck`.
* Temporary grants live in one in-memory state shared by the root conversation and all
  its subagents. Persistent grants live in
  `<workspace-root>/.ylva/profiles/<profile>/grants.toml` and apply to every agent type.
  Rule lists are not rewritten and not replaced by agent-specific lists.
* A profile directory that contains only `grants.toml` is valid. It does not shadow the
  home profile, does not create a `config.toml` and needs no shim.
* `grants.toml` is covered by the workspace trust of ADR 0003: its content is part of the
  selected profile's fingerprint (malformed content is hashed verbatim). A grant
  confirmed in the running session takes effect immediately but does not record trust;
  the next launch requires `ylva trust`.
* `--no-workspace-grants` skips loading persistent grants for the whole invocation after
  the normal trust check.

## Consequences

* Good, because sandbox rules and grants cannot contaminate each other and denials stay
  final.
* Good, because a repository cannot approve calls without the user's `ylva trust`.
* Neutral, because the trust fingerprint changes whenever a grant is saved, so the next
  launch needs `ylva trust` again.
* Bad, because the user now has a second place (`grants.toml`) that can approve calls.

## Confirmation

* Unit tests of the matchers, the store and the evaluator cover precedence over denials
  and mandatory interaction.
* Trust tests cover a grants-only workspace profile and malformed `grants.toml`.

## Pros and Cons of the Options

### Separate grant store evaluated after the policy verdict

* Good, because precedence is explicit and testable in one place.
* Bad, because a second matcher implementation exists next to the sandbox rules.

### Generate `auto_approved` entries in the `[tools.*]` rule lists

* Good, because no new evaluator is needed.
* Bad, because last-match-wins ordering lets a generated entry override later denials and
  rewrites user configuration.

### Remember approvals only for the running session

* Good, because it is simple and leaves no files behind.
* Bad, because repeated prompts return in every new session.
