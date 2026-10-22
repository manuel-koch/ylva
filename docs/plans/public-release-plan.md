# Public release readiness

## Context

Assessment date: 2026-10-21. Assessed checkout: `9af29ec`, package version `0.31.0`.

Decisions refined with the maintainer on 2026-10-21.

**Two separate milestones: publish experimental source, then earn confidence
before promotion.** Ylva is a personally maintained, best-effort project, not a
supported-product launch. Source publication need not wait until Ylva carries
the maintainer's daily workload. Announcement requires additional evidence and
an explicit maintainer decision.

The existing CLI/TUI, multiple providers, persistent sessions, compaction, tool
approvals, skills, and concurrent subagents form a useful initial offering.
More providers, UI features, or architectural refinement are not prerequisites.
The proposed audience is developers wanting configurable CLI/TUI agent workflows
with provider choice and local models. This is an audience hypothesis, not an
established advantage over other tools. The representative workflow is exploring
a repository, making a small approval-controlled change with verification, and
resuming unfinished work another day.

Initial support is **macOS only**. Linux is unverified and Windows unsupported.
Installation is from source. Maintenance has no response-time guarantee.
Provider integrations are entirely best-effort: there is no designated primary
provider, model-quality guarantee, claim of compatibility with "most" models, or
maintained model-results catalogue.

This is a repository-based assessment, not release certification. The assessment
did not run the full test suite, measure current coverage, perform a security or
Git-history audit, or exercise live providers. GitHub repository lookup failed,
so hosted CI, releases, and current visibility could not be confirmed.

The maintainer authorized updating this plan only, not implementing it. Obtain
separate approval for implementation, new documentation files beyond this file,
staging, commits, history rewrites, publication, and repository visibility changes.
Follow [the development workflow](../development-workflow.md) for implementation
and verification, and check applicable
[architecture decisions](../architecture-design-records/) before behavior changes.

Each milestone has its own gates below. Failed or incomplete checks block that
milestone, not automatically both. There is no arbitrary release deadline or
implementation-effort cap. Narrow scope before adding work; reassess usability
after five real dogfooding sessions rather than hardening indefinitely.
Never waive credential/private-data blockers or misrepresent safety.

## Implementation Plan

All implementation tasks remain unchecked. On authorized execution, mark each
`[ ]` as `[x]` immediately after completion and update this file in place.
Record the tested commit, environment, result, and remaining limitations without
including credentials or private conversation data. Documentation changes belong
in the same change set as the behavior they describe.

Tasks 1-7 prepare source publication; tasks 8-11 prepare promotion. Dogfooding can
start before source publication, but does not block that milestone. No model-results
matrix is required: workflow observations and candidate verification are evidence
for this project, not model certification.

1. [ ] **Clear the public tree/history, licensing, and high-risk paths.**
   Review the intended public tree and full Git history, including configuration,
   fixtures, scripts, generated artifacts, and any proposed release attachments,
   for credentials and private data. Review project and dependency licensing and
   required notices. Perform a focused security review of credential handling,
   tool approvals, filesystem access, shell execution, and outbound requests.
   Record findings privately when disclosure would expose sensitive information.
   If credentials are found, revoke/rotate them; deleting the current file is
   insufficient. Preserve existing history if cleared; otherwise stop for an
   explicit remediation decision and approval before any history rewrite or
   replacement with a fresh public snapshot.
   Exit criterion: a cleared intended public tree/history, appropriate attribution,
   and explicit disposition of serious security findings. This is not proof of
   vulnerability absence and does not imply that the assessment found a leak.

2. [ ] **Resolve Copilot distribution through a bounded evidence review.**
   Start with [provider notes](../provider-notes.md) and the Copilot authentication
   implementation. Review integration provenance, relevant published terms, and
   disclosures in comparable agent projects, recording authoritative sources and
   uncertainties. Stop with a concise evidence summary and request the maintainer's
   explicit retain/change/exclude decision; do not expand into an indefinite
   provider-certification exercise or silently remove the integration.
   Other projects' similar implementations are precedent, not proof of permission
   or prohibition. A GitHub endorsement is not a release requirement; best-effort
   maintenance also does not resolve distribution uncertainty.
   Update [provider notes](../provider-notes.md) and affected README claims with
   the decision and any required implementation changes.
   Exit criterion: an explicit, documented distribution decision and completion of
   its required changes before source publication.

3. [x] **Remove development-only configuration from the active default path.**
   Resolved by removing the tracked workspace `.ylva/` folder (default and
   `other` profiles, the `broken` and `research-and-discover` agents, and the
   `ylva-other` shim). A fresh checkout now has no workspace profile, so startup
   uses the user's own profile. Original scope: inspect the workspace default
   profile and agent definitions, then follow profile loading in
   `src/startup/profile.rs` and interpolation in `src/config/profile.rs`.
   Move development-only configuration out of the automatically selected default
   path, preserving workspace-profile precedence and explicit missing-variable
   errors. Do not hide the problem with silent fallbacks.
   Add focused regression coverage in the existing CLI/configuration tests.
   Update [README](../../README.md) and [configuration](../configuration.md).
   Exit criterion: a fresh checkout does not require the maintainer's environment,
   local model, or deliberately invalid agent configuration.

4. [ ] **Require real-provider setup before ordinary conversations.**
   Replace the implicit mock fallback for ordinary CLI/TUI conversation startup
   with an actionable configuration error. Point to existing
   `ylva connect ollama`, `ylva connect openrouter`, and configuration instructions.
   Use the existing configuration-error exit convention. Keep help, version,
   provider/configuration commands, and session-management commands usable without
   a real provider. Do not add a setup wizard, setup screen, or connectivity
   preflight; configured-provider failures follow the normal request error path.
   Preserve mocks for tests/internal development through explicit test setup, not
   a new public demo flag or selectable mock-provider feature.
   Start from `src/startup/engines.rs`, `src/llm/provider_registry.rs`, and their
   callers to locate the actual fallback before editing. Add focused regression
   cases for no-provider CLI/TUI startup and unaffected non-conversation commands,
   adapting existing mocked tests as necessary.
   Update [README](../../README.md), [CLI options](../cli-options.md),
   [configuration](../configuration.md), and [features](../features.md) in the same
   change set; update [implementation notes](../implementation-notes.md) if startup
   mechanics documented there change.
   Exit criterion: no ordinary conversation silently uses a mock; a clean macOS
   checkout can install from source and receive correct setup guidance without
   credentials or an available local server.

5. [ ] **Publish honest expectations and reporting routes.**
   State the personal/experimental status, macOS-only support, source installation,
   and best-effort maintenance in [README](../../README.md). Include concise Ollama
   and OpenRouter setup links without introducing a preferred model or compatibility
   catalogue. Update [provider notes](../provider-notes.md) so integration behavior,
   observed limitations, and model-quality expectations are clearly distinguished.
   Explain near the README's sandbox claim that approvals are policy checks, not
   OS/process isolation. Describe external-provider data sharing, local transcript
   storage, sensitive diagnostic dumps, and the implications of auto-approval.
   Keep details in [features](../features.md), [configuration](../configuration.md),
   [CLI options](../cli-options.md), and [persistence](../persistence.md), updating
   them with any related behavior changes.
   Use GitHub Issues for ordinary bugs and private vulnerability reporting for
   sensitive reports, with no response-time guarantee. Confirm availability and,
   with approval, configure these routes. If private reporting is unavailable,
   obtain a dedicated fallback contact before publication; do not infer permission
   to publish a personal email. Obtain approval before creating a separate
   security-policy document.
   Exit criterion: users can understand scope, trust boundaries, setup, and reporting
   without interpreting "best-effort" as either a safety guarantee or no guidance.

6. [ ] **Verify the source-publication candidate locally.**
   Run `make format lint`, `make test-coverage`, `make audit`, and `rumdl check`
   on changed Markdown. Fix formatting/lint findings and retain the existing 80%
   aggregate line-coverage threshold. Do not also run `make test` as a redundant
   mandatory gate. Recheck clean installation and missing-provider guidance after
   fixes. Record the exact candidate commit, environment, and results, with no
   unverified changes after the passes.
   Exit criterion: tasks 1-5 are cleared and local verification passes for the
   publication candidate. Hosted CI and five-session dogfooding are not required
   for this milestone.

7. [ ] **Publish experimental source with explicit approval.**
   Obtain approval for repository visibility changes and any commits, tags, or
   release publication involved. Verify external repository access, documentation
   links, installation instructions, and private reporting availability.
   Exit criterion: cleared source is publicly accessible with honest experimental
   disclosures. Do not treat this as an alpha announcement or an invitation to
   depend on Ylva for daily work.

8. [ ] **Dogfood five real sessions and reassess.**
   Exercise understanding unfamiliar code, making a small approval-controlled
   change with verification, and resuming unfinished work on another day.
   Record interventions, approval friction, confusing UI, lost context, and fallback
   to another harness. Separate model mistakes from harness failures; useful task
   results and successful tool execution are different observations.
   Use a concise workflow log in this plan, without private work content or a
   maintained model-results catalogue. Reassess after five real sessions rather
   than waiting for an undefined feeling of readiness.
   Data loss, unintended execution, or unusable cancellation blocks announcement
   until resolved. Other findings inform a maintainer decision to narrow scope,
   continue dogfooding, or proceed; five sessions do not automatically earn a pass.
   Exit criterion: observations and an explicit reassessment are recorded, with
   announcement-blocking defects fixed and verified.

9. [ ] **Confirm macOS CI before announcement.**
   The macOS workflow already exists:
   [`ci.yml`](../../.github/workflows/ci.yml) runs format check, lint, the 80%
   aggregate coverage gate, a conditional dependency audit, the git convention
   checks, and Markdown checks (`rumdl`, via `make format-check` and `make lint`),
   and is documented in
   [development workflow](../development-workflow.md#continuous-integration-and-releases).
   Do not add a duplicate workflow. It already runs without secrets or
   live-provider tests, so only the hosted result remains to be confirmed.
   Exit criterion: the announcement candidate has passing hosted macOS checks.
   This task does not retroactively block experimental source publication.

10. [ ] **Verify the announcement candidate and prepare accurate notes.**
    Apply the development workflow's code-change gates to fixes arising from
    dogfooding or CI. Verify the final candidate's installation/setup path and
    affected workflow behavior; record local and hosted results for that commit.
    Reconfirm the publication gates if the public tree or distribution changes.
    Update [README](../../README.md), [provider notes](../provider-notes.md), and
    other affected behavior docs with remaining limitations. Prepare any release
    notes using [release-notes workflow](../release-notes-workflow.md).
    Exit criterion: no unresolved announcement blocker, no unverified changes after
    the recorded results, and an explicit maintainer go/no-go decision.

11. [ ] **Promote cautiously and collect feedback.**
    Obtain explicit approval before announcement, tagging, or publishing a release.
    Invite a small technical audience to try the representative workflow, retaining
    the personal-project and best-effort expectations. Collect first-run failures,
    approval friction, provider issues, and session-reliability feedback through
    the agreed reporting routes.
    Exit criterion: actual user feedback informs the next iteration; broader
    promotion is a separate decision rather than an automatic next step.

## Proposed Improvements

- Prebuilt binaries and screenshots/demo media are optional polish, not source
  publication or announcement gates. Obtain approval before creating new assets.
- Additional providers, setup wizards, UI enhancements, and architectural cleanup
  remain deferred unless observed workflow failures make a focused change necessary.
- Narrow the advertised surface before adding assurance work. Do not introduce
  model certification or a compatibility matrix as a hidden release requirement.

## Implementation Hints

These are findings from the original assessed checkout, not claims about future
revisions. In particular, the mock fallback below is existing behavior that task 4
intentionally replaces, not the agreed first-run contract.

| Area | Finding | Evidence |
| --- | --- | --- |
| Product scope | Core functionality is substantial enough for an early-adopter release. | [Features](../features.md), [README](../../README.md) |
| Onboarding blocker | The previously tracked default profile selected `local-dense`, using `gemma4:e4b-mlx`, and referenced multiple environment variables. A clean user's checkout does not have the advertised unconfigured default. | Removed tracked workspace profile (resolved by task 3) |
| Startup contract | Implicit startup loads the workspace default profile when present. Missing referenced environment variables are errors, not mock-provider fallbacks. | `src/startup/profile.rs`, `src/config/profile.rs`, `src/config.rs` |
| Development artifacts | The previously tracked workspace included a `broken` agent backed by a model named `invalid`. These were active configuration, not isolated test fixtures. | Removed tracked workspace agents and profile (resolved by task 3) |
| Provider uncertainty | Copilot uses an undocumented API and hardcoded VS Code client identity. The notes flag possible terms conflicts and unverified live behavior. | [Provider notes](../provider-notes.md) |
| Safety messaging | Detailed docs describe Bash approval rules as heuristics without enforced isolation, while the README uses the shorter sandbox claim. | [Features](../features.md), [README](../../README.md) |
| Quality evidence | A broad test suite and an 80% aggregate line-coverage gate exist. Current passing results were not established during the assessment. | `tests/`, `src/**/tests*`, [Makefile](../../Makefile) |
| Automation | No tracked `.github/workflows/` files were present. Hosted automation could not be verified because GitHub lookup failed. | Tracked-file inventory and repository lookup |
| Licensing | The package declares Apache-2.0 and the repository has a license file. Dependency/history clearance was not assessed. | [Cargo.toml](../../Cargo.toml), [LICENSE](../../LICENSE) |
| Presentation | The README has source-install instructions and extensive feature links, but no explicit alpha status, supported-platform matrix, or screenshot/demo. No tracked security-policy file was found. | [README](../../README.md), tracked-file inventory |

The existing [known test issues](../known-test-issues.md) document tracing,
current-directory, and integration-test side effects that matter for isolation.
The architecture-decision and implementation-plan directories contained only
placeholders at assessment time; no release-specific recorded decision was found.

The maintainer reports having tried frontier models through OpenRouter and small
local Ollama models. The local models worked but produced lower-quality results.
This is maintainer-reported experience, not a measured compatibility guarantee.

## Evidence-dependent decisions

The design decisions above are settled. Execution must still obtain evidence and
explicit decisions at these gates; none is silently assumed to pass:

- Copilot retain/change/exclude disposition after the bounded review in task 2.
- Remediation and any history changes if task 1 finds publication blockers.
- A dedicated private reporting contact if GitHub private reporting is unavailable.
- Findings-driven scope changes after five sessions, and the final announcement
  go/no-go decision.
