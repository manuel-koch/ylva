# Documentation review report and remediation plan

## Review baseline and authority

- Review date: 2026-10-16.
- Repository baseline: `c680adb`; the worktree was clean before this report.
- Scope: all 51 existing authored Markdown files and all nine Mermaid sources.
  This report is an additional file, not part of that original count.
- Non-idea documentation takes precedence over `docs/ideas/*.md`. Idea documents
  are proposals or historical material, not requirements for current behavior.
  Every idea finding is low priority, including contradictions with implementation.
- This is a report-only review. No reviewed document, diagram, implementation,
  configuration, or generated image is being repaired by this change.
- Priorities: **Medium** means a misleading behavioral/configuration contract or
  conflicting contributor rule; **Low** means maintenance, navigation, clarity,
  or historical-idea debt. Findings do not imply a software vulnerability.
- References below use the original files' line numbers at the review baseline.
  Implementation references corroborate selected claims; this is not an exhaustive
  source-code audit.

## Summary

| Area | Medium | Low | Total |
| --- | --- | --- | --- |
| Current Markdown documents | 8 | 12 | 20 |
| Mermaid sources | 1 | 1 | 2 |
| Idea documents, including grouped orphan finding | 0 | 10 | 10 |
| Total | 9 | 23 | 32 |

The principal problems are conflicting compaction/configuration contracts, an
ambiguous execution-root definition, misleading default-agent and approval
descriptions, and a diagram that misses the skill-loader policy exception.
No high-priority finding or broken local Markdown link was identified.
Historical proposals account for low-priority debt, not current requirements.

**Pass 1 - 0 fixed, 32 findings reported for action across 38 affected files.**
This count groups repeated manifestations of one issue and does not count
unchanged documents cited only as corroborating evidence as affected files.
The full corpus was reviewed once; repair/re-review passes are not authorized
or represented as completed.

## Review coverage and checks

The Markdown inventory comprises three root documents, 12 non-idea documents under
`docs/` including the diagram README, 34 idea documents, and two Markdown agent-type
configuration files under `.ylva/`. All are included, with agent-type files assessed
as configuration rather than ordinary documentation pages.

Generated/build/dependency/tooling content, Git metadata, ignored Graphify output,
and logs are excluded. Existing generated SVGs are supplementary local artifacts;
the `.mmd` files, not those exports, are authoritative. There are no authored ADRs
or pre-existing plans: both directories contain only `.gitkeep`.

Checks completed for the original Markdown corpus:

- [x] `rumdl check . --output-format concise`: one existing issue,
  `docs/ideas/sample-prompts.md:228`, rule `MD012`.
- Local relative-link and heading-anchor scan: 180 links across 51 files,
  no missing target or unresolved heading found. This is a local structural
  check, not live external-URL validation.
- Fenced-block closure scan: no unclosed Markdown fence found.
- Literal source-path checks found no missing implementation file after excluding
  documented patterns, deliberately prohibited paths, and example output names.
- The existing Graphify graph was queried for orientation. Findings rely on direct
  document reads and selected source checks, not on treating the cached graph as
  current evidence.

Mermaid checks and limits:

- All nine sources were read in full and checked against their documented
  selective scope, current references, and targeted implementation evidence.
  No fenced Mermaid block was found in the Markdown corpus.
- The existing local Docker image contained Mermaid 11.17.2. Parsing used
  `--pull=never --network none --read-only` and read-only source mounts, without
  installing dependencies or regenerating repository files.
- Unmodified Node `mermaid.parse()` accepted the two sequence diagrams. The
  other seven encountered `DOMPurify.sanitize is not a function`, an environment
  problem rather than a demonstrated source grammar error.
- With an in-memory identity sanitizer shim, Mermaid's grammar parser accepted
  all nine sources, including frontmatter/configuration. This is grammar-only
  validation, not proof that browser sanitization, ELK layout, or fresh SVG
  rendering succeeds. Those remain unverified.
- All nine existing ignored SVGs parse as XML. Representative labels agree with
  their sources and no syntax-error banner was found. Local exports corroborate
  existing artifacts only; they do not replace a fresh renderer run.

No live external links, provider APIs, registry availability, or trademark
status were checked. Example endpoints and explicit future proposals are not
reported as broken merely because they are illustrative or unimplemented.

## Current-document findings

### D01 - Manual compaction is both absent and implemented

- [x] Reconcile manual-compaction availability.

**Priority: Medium. Category: contradiction.**

- Evidence: [features](../features.md), line 313, says there is no manual
  `/compact` command. The same file, lines 1255-1258 and 1292-1305, documents the
  command and its behavior. [README](../../README.md), line 57, also marks it
  implemented; [keybindings](../keybindings.md), lines 105-106, documents its controls.
- Corroboration: `src/slash_command.rs:34-36,87-98` includes `Compact` and `Session`.
- Impact: the overview tells users that an available feature does not exist.
- Resolution: replace the obsolete sentence with a link to the authoritative
  slash-command contract; retain the distinction between root manual compaction
  and independent automatic subagent compaction.
- Applied: the scope overview now describes TUI `/compact` as root-only and links
  to the slash-command contract. Verified against that contract and Markdown lint.

### D02 - Discovered workspace root and launch-directory sandbox are conflated

- [x] Distinguish discovered workspace and execution/sandbox roots.

**Priority: Medium. Category: terminology and behavioral contradiction.**

- Evidence: [CONTEXT](../../CONTEXT.md), lines 8-10, scopes configuration and
  filesystem access to one workspace root. [configuration](../configuration.md),
  lines 148-159, discovers that root by walking to a `.ylva`/`.git` marker, but
  lines 815-820 define the sandbox root as canonicalized startup cwd.
  Lines 428-429 call the workspace root the tool-path anchor.
  [features](../features.md), lines 617-620, likewise equates normal tool cwd
  with the workspace root.
- Corroboration: `src/sandbox.rs:50-66` roots `Sandbox::new` at `current_dir()`;
  `src/main.rs:204-207` discovers the configuration workspace separately.
- Impact: launching from a workspace subdirectory changes relative tool paths,
  approval boundaries, and session grouping without changing configuration discovery.
  The present wording hides that distinction.
- Resolution: document discovered configuration workspace versus launch-directory
  execution/sandbox anchor explicitly. Include a nested-directory example and link
  to [project-key derivation](../persistence.md#project_key-derivation).
  Do not change runtime anchoring as an incidental documentation fix.
- Applied: defined the sandbox root separately in `CONTEXT.md`; corrected
  configuration, tool, trusted-source-path, and persistence wording; added a
  nested-launch example linked to project-key derivation. Runtime anchoring is
  unchanged. Verified against startup/sandbox code, Markdown lint, and local links.

**Follow-up: workspace-based `outside_workspace` evaluation (requested).**

- [x] Add regression coverage for nested launches, discovered and missing workspace
  markers, configured/default rules, snapshot paths, and symlink escapes.
- [x] Change only the `outside_workspace` comparison to use the nearest discovered
  workspace, falling back to the launch directory when no marker is found.
  Preserve relative-path resolution, `dot_path`, and `cross_filesystem` anchoring.
- [x] Update `CONTEXT.md`, `docs/configuration.md`, `docs/features.md`, and
  `docs/implementation-notes.md` to describe the changed rule boundary.
- [x] Run `make format lint` and `make test-coverage`.

Applied: only `matches_outside_workspace` changes runtime behavior. The new
regression cases failed before the fix and pass afterward; the full coverage
gate passes with 88.80% aggregate line coverage. At that stage, `dot_path` did not
match paths outside the launch-directory sandbox root, even if they contained a
dot-prefixed component. The separately requested follow-up below supersedes that
behavior.

**Follow-up: location-independent `dot_path` (requested).**

- [x] Cover dot-prefixed components inside and outside the workspace, hidden
  launch ancestors, snapshot paths, and the existing enumeration exception.
- [x] Check all components of the resolved absolute path without stripping the
  sandbox root. Preserve rule ordering and `outside_workspace` behavior.
- [x] Update affected test fixtures to use non-hidden temporary directories when
  the test requires ordinary visible paths.
- [x] Update `CONTEXT.md`, `docs/configuration.md`, `docs/features.md`,
  `docs/implementation-notes.md`, and `docs/known-test-issues.md`.
- [x] Run `make format lint` and `make test-coverage`.

Applied: `dot_path` now checks every component of the resolved absolute path,
including hidden ancestors and paths outside the workspace. The enumeration
exception and configured-rule precedence are unchanged. Regression tests failed
before the change and pass afterward. Visible-path fixtures use non-hidden
temporary directories so the default `.tmp` prefix does not change their intent.

**Follow-up: consistent sandbox-root naming (requested).**

- [x] Rename the stored launch-directory field and its dependent parameters,
  diagnostics, comments, and callers to `sandbox_root`. Reserve `workspace_root`
  for the discovered-or-defaulted workspace boundary.
- [x] Preserve the workspace rule, universal dot-path rule, relative-path
  resolution, and sandbox-relative filesystem-device checks.
- [x] Update `docs/implementation-notes.md` to describe the names directly,
  without a historical-name qualification.
- [x] Verify constructor, shared-root update, rule, and integration coverage
  with `make format lint` and `make test-coverage`.

Applied: `Sandbox::sandbox_root` and dependent path-evaluator names now identify
the launch-directory anchor; `workspace_root` names the effective workspace
boundary. Without a marker, both roots have the same path. Updated callers,
filesystem-device diagnostics, and implementation notes. Both follow-ups pass
`make format lint` and the full `make test-coverage` gate with 88.80% aggregate
line coverage.

### D03 - Inline default-agent overrides are described as whole replacements

- [x] Correct default-agent override semantics.

**Priority: Medium. Category: implementation mismatch.**

- Evidence: [configuration](../configuration.md), lines 675-681, says a present
  `[agents.default]` replaces the built-in configuration and requires restating
  `can_spawn = true`. [features](../features.md), lines 1110-1114, repeats the
  replacement claim. Configuration lines 520-535 otherwise describe inline
  fields as individual overrides.
- Corroboration: `src/config/profile.rs:464-476,882-899,934-938` inserts the
  built-in default before applying inline overrides and changes `can_spawn`
  only when explicitly supplied. Without a replacing file definition,
  `[agents.default]` containing just a persona retains spawning permission.
- Impact: users can wrongly assume that customizing the default agent disables
  delegation unless explicitly enabled.
- Resolution: distinguish whole-file source precedence from field-level inline
  overrides, including the special built-in default and explicit
  `can_spawn = false` needed to disable its delegation.
- Applied: updated `docs/configuration.md` and `docs/features.md` to distinguish
  whole-file replacement from field-level inline overrides. Added a persona-only
  example that retains the built-in spawning permission and documented explicit
  `can_spawn = false` to disable delegation. Verified against the configuration
  loader and Markdown lint; runtime behavior is unchanged.

### D04 - Feature overview presents the sandbox floor as unconditional

- [x] Qualify the default sandbox floor.

**Priority: Medium. Category: contradiction.**

- Evidence: [features](../features.md), lines 112-119, says every path tool
  honors the built-in outside-workspace/dot-path floor.
  [configuration](../configuration.md), lines 914-922, explicitly says a configured
  tool table replaces that floor. Configuration lines 95-98 also describe the
  separate unconfigured `load_skill` policy.
- Impact: a reader can infer that custom auto-approval rules cannot remove the
  default checks, although the configuration reference says they can.
- Resolution: qualify the overview with "when no tool-specific rules are configured"
  and link to replacement semantics and the loader exception. Preserve CLI
  auto-approval flags' documented final precedence.
- Applied: updated `docs/features.md` to qualify the ordinary path-tool default
  floor, explain profile-level and per-agent rule replacement, and link to the
  configuration reference and `load_skill` exception. Explicitly retained final
  precedence for `--auto-approve-write` and `--auto-approve-edit`. Verified against
  sandbox and skill-loader policy code and Markdown lint; runtime behavior is
  unchanged.

### D05 - Duplicate profile tables disagree about whether `default_engine` is required

- [x] Consolidate profile tables and correct `default_engine` requirements.

**Priority: Medium. Category: contradiction and duplication.**

- Evidence: [configuration](../configuration.md), line 239, gives a default of
  `default`; line 593 calls the same field required.
- Corroboration: `src/config/profile.rs:43-44` supplies a serde default.
- Impact: users are given incompatible instructions for authoring a minimal profile.
- Resolution: keep one authoritative top-level profile-field table and link to it
  from the other section. Explain separately that a resolved engine reference must
  exist when explicit engines are configured.
- Applied: consolidated the profile-field reference in `docs/configuration.md`
  and linked to it from the profile configuration section. Documented that
  `default_engine` is optional and defaults to `default`, while explicit engine
  tables must define the resolved name. Preserved utility-engine runtime isolation.
  Verified against deserialization and engine-reference validation and Markdown
  lint; runtime behavior is unchanged.

### D06 - Workspace-shim instructions show the home-shim PATH

- [x] Correct the workspace-shim PATH example.

**Priority: Medium. Category: incorrect command context.**

- Evidence: [configuration](../configuration.md), lines 334-341, says each workspace's
  `.ylva/shims` must be added separately and introduces the output of
  `ylva profile shims --workspace`, but its example exports `~/.ylva/shims`.
- Impact: following the example does not put that workspace's generated shims on PATH.
- Resolution: show a clearly labeled workspace path or instruct users to use the
  exact path emitted by their command. Keep the home-scope example separately labeled.
- Applied: updated `docs/configuration.md` with a labeled workspace-path example
  and instructions to use the exact line emitted by `ylva profile shims --workspace`.
  Kept the default home-scope example separate, paired with `ylva profile shims`.
  Verified against shim PATH output and Markdown lint; runtime behavior is unchanged.

### D07 - Profile-edit refusal guarantees overstate preflight and rollback

- [x] Clarify profile-edit refusal and rollback guarantees.

**Priority: Medium. Category: implementation mismatch.**

- Evidence: [CLI options](../cli-options.md), lines 335-340, says invalid in-scope
  configuration causes exit 2 with no changes and blocks `edit`, alongside
  `create`, `delete`, and `shims`.
- Corroboration: `src/profile_command.rs:468-498` launches the editor before
  calling `plan_shims`. The editor can change the target configuration before
  an invalid sibling or edited configuration causes reconciliation to fail.
  The same document, lines 296-303, correctly places reconciliation after editing.
- Impact: readers can mistake shim-planning guarantees for transactionality of
  the user's editor changes.
- Resolution: limit "no changes" to reconciliation's own planned writes.
  Explicitly state that editor changes persist even if subsequent validation or
  shim reconciliation fails. Changing edit transactionality would be separate work.
- Applied: updated `docs/cli-options.md` to distinguish shim-planning refusals
  from editor changes and application-time I/O failures. Clarified that `edit`
  runs the editor before scope-wide reconciliation validation and never rolls
  back its changes, while `create` and `delete` check before changing profile
  files. Verified against command ordering, shim planning/application, and
  Markdown lint; runtime behavior is unchanged.

### D08 - Context-estimate refresh descriptions disagree

- [x] Reconcile context-estimate refresh descriptions.

**Priority: Low. Category: contradiction.**

- Evidence: [features](../features.md), lines 243-244, says the last request estimate
  remains until another request. Lines 902-909 describe a startup baseline and
  immediate model-view refresh on `/models`. [implementation notes](../implementation-notes.md),
  lines 866-878, records those additional update paths.
- Corroboration: `src/agent/runtime.rs:268-278` emits startup usage;
  `src/agent/runtime/commands.rs:634-656` emits model-switch usage.
- Resolution: describe the estimate as last-request data unless replaced by startup,
  model-selection, or compaction updates; link rather than duplicate its lifecycle.
  Do not incorrectly promise an immediate baseline after `/clear`, whose handler
  has a different lifecycle.
- Applied: centralized the readout lifecycle in `docs/features.md`'s TUI
  architecture section and linked to it from the token-estimate explanation.
  Clarified that startup, model-selection, and compaction updates can replace
  the last-request estimate; `/clear` removes the readout without emitting an
  immediate startup baseline. Verified against runtime update paths and Markdown
  lint; runtime behavior is unchanged.

### D09 - General shim descriptions omit the default-profile exception

- [x] Document the default-profile shim exception consistently.

**Priority: Low. Category: contradiction.**

- Evidence: [features](../features.md), lines 1135-1138, says each profile gets a
  launcher; [CLI options](../cli-options.md), lines 313-322, first says every
  profile with a config gets one, then correctly says `default` gets none.
  [configuration](../configuration.md), lines 312-318, describes `shim_name`
  without that exception.
- Corroboration: `src/profile_shim.rs:113-122` skips `default` and rejects its
  explicit shim name.
- Resolution: qualify the general statements as non-default profiles and centralize
  shim naming and default-profile restrictions in the CLI reference.
- Applied: qualified launcher descriptions in `docs/features.md` and
  `docs/cli-options.md` as non-default profiles with a `config.toml`.
  Centralized shim naming, key placement, and default-profile restrictions in
  the CLI shim reference, linking to it from `docs/configuration.md` and the
  feature overview. Verified against shim planning and Markdown lint; runtime
  behavior is unchanged.

### D10 - Request-dump format follows the token-dump paragraph without a named subject

- [x] Separate request-dump and token-dump explanations.

**Priority: Low. Category: ambiguity and misplaced explanation.**

- Evidence: [CLI options](../cli-options.md), lines 442-450, describes
  `YLVA_TOKEN_USAGE_DUMP_PATH` as one append-only metadata file. Lines 452-474
  immediately switch to "The value is a path template" and request/response payload
  files, without naming `YLVA_REQUEST_DUMP_PATH` again.
- Impact: readers can attribute the content-bearing request-dump format to the
  metadata-only token diagnostic.
- Resolution: separate the two existing variables into clearly named subsections
  or explicitly name `YLVA_REQUEST_DUMP_PATH` at the transition.
- Applied: split the Debug explanations in `docs/cli-options.md` into named
  subsections for `YLVA_TOKEN_USAGE_DUMP_PATH` and `YLVA_REQUEST_DUMP_PATH`,
  explicitly identifying the single metadata-only file versus the HTTP dump
  path template. Runtime behavior is unchanged.

### D11 - Blanket lint-suppression prohibition conflicts with a required exception

- [x] Document the shared-helper lint-suppression exception.

**Priority: Medium. Category: contributor-rule contradiction.**

- Evidence: [development workflow](../development-workflow.md), lines 83-85,
  forbids blanket `#![allow(...)]`; lines 321-326 require
  `#![allow(dead_code)]` in integration-test shared helpers.
- Resolution: document the already-established shared-helper exception in the
  general rule and cross-link it. Preserve the prohibition for ordinary
  production code; do not remove the helper requirement as an incidental cleanup.
- Applied: updated `docs/development-workflow.md` to name the required
  shared-helper `#![allow(dead_code)]` as the narrow exception to both the blanket
  suppression prohibition and the `#[expect(...)]` preference. Cross-linked the
  general rule and shared-helper convention, preserving ordinary production-code
  restrictions and the existing helper requirement. Markdown lint passes.

### D12 - Queue indicator still describes a single subagent slot

- [x] Correct the single-slot subagent queue description.

**Priority: Low. Category: stale behavior description.**

- Evidence: [features](../features.md), lines 984-985, says queued bubbles wait
  for "the one running-child slot"; lines 791-797 and
  [configuration](../configuration.md), line 246, document a default cap of four
  and configurable unlimited execution.
- Resolution: say "an available scheduler slot" and link to the concurrency setting.
- Applied: updated `docs/features.md` to describe queued subagent bubbles as
  waiting for an available scheduler slot and linked to the authoritative
  `max_concurrent_subagents` configuration table. Markdown lint passes;
  runtime behavior is unchanged.

### D13 - Implementation notes contain an interrupted session-command explanation

- [x] Repair the interrupted session-command explanation.

**Priority: Low. Category: incomplete prose.**

- Evidence: [implementation notes](../implementation-notes.md), lines 46-47,
  repeats "Shim scripts"; line 53 begins the fragment
  "profile/provider initialization or stdin consumption. Its CLI listing projection".
  The missing subject obscures the transition from profile handling to session listing.
- Resolution: remove the duplicate fragment and restore an explicit
  `session_command.rs` subject using the surrounding lifecycle explanation.
- Applied: removed the repeated "Shim scripts" fragment from
  `docs/implementation-notes.md` and restored the explicit `session_command.rs`
  dispatch explanation before the session-listing details. Verified against
  startup dispatch ordering and Markdown lint; runtime behavior is unchanged.

### D14 - The entry point has no setup path or documentation navigation

- [x] Add or clarify entry-point setup and navigation.

**Priority: Low. Category: entry-point gap.**

- Evidence: [README](../../README.md), lines 1-59, contains an introduction and
  feature checklist only, with no links to the CLI/configuration/development
  references and no installation/run guidance. [AGENTS](../../AGENTS.md), line 9,
  describes it as the intro and quickstart.
- Resolution: add a small getting-started/navigation section linking existing
  authoritative references. Avoid copying their large option/target tables.
  Correct the "quickstart" description if setup guidance remains elsewhere.
- Applied: added a short source-install and TUI-launch quickstart to `README.md`,
  including prerequisites and links to provider/profile setup. Added navigation
  to the existing CLI, configuration, keybindings, features, domain language,
  development workflow, and implementation notes without duplicating their
  detailed tables. The `AGENTS.md` quickstart description now matches the README.

### D15 - Two non-idea pages have no inbound Markdown link

WON'T FIX: These are no relevant documents for current review.

- [ ] Resolve non-idea orphan-page disposition.

**Priority: Low. Category: discoverability; author decision required.**

- Evidence: [backlog issues](../backlog-issues.md) and
  [naming research](../naming-research.md) have no inbound Markdown link in the
  reviewed corpus.
- Resolution: decide whether they are active references or retained historical
  records, then link, label, merge, or retire them. No deletion is proposed
  automatically. `.ylva` agent-type files are not documentation orphans:
  their entry point is configuration discovery.

### D16 - Detailed configuration contracts are maintained in multiple references

- [x] Consolidate duplicated configuration contracts.

**Priority: Low. Category: single-source-of-truth risk.**

- Evidence: [features](../features.md), lines 1142-1183, repeats the detailed
  trusted-instruction discovery, framing, limits, failure, and precedence contract
  owned by [configuration](../configuration.md), lines 343-448.
  Features lines 1085-1140 likewise restate configuration/profile mechanics.
  The duplicated profile tables and shim/default-agent drift above demonstrate
  the practical maintenance cost.
- Resolution: retain user-visible capability summaries in features, but link to
  configuration for exact discovery/precedence/limit contracts. Consolidate the
  duplicate table as part of D05, without counting that repair twice.
- Applied: reduced the engine/profile, agent-type, shim, persona, and initialization
  sections in `docs/features.md` to capability summaries with targeted links to
  the owning configuration and CLI references. Detailed trusted-instruction
  discovery, precedence, framing, limits, and failures remain in
  `docs/configuration.md`. Preserved feature headings and their anchors; the
  duplicate profile table was already consolidated under D05.

### D17 - User-facing terminology does not follow the glossary

- [ ] Align explanatory terminology with the glossary.

**Priority: Low. Category: terminology drift.**

- Evidence: [CONTEXT](../../CONTEXT.md), lines 77-80, defines "Subagent" and
  explicitly discourages "child agent"/"child". [features](../features.md),
  lines 783 and 825 onward, and [implementation notes](../implementation-notes.md),
  lines 668 onward, use child-agent headings and prose throughout.
- Resolution: use "subagent" in authored explanatory prose and headings, while
  retaining actual identifiers such as `ChildAgentManager`, event names, and
  persisted `child_start` fields. Preserve existing inbound anchors or update
  all their references together if headings change.

### D18 - Naming research infers namespace ownership from absent resources

- [ ] Correct unsupported namespace-availability conclusions.

**Priority: Low. Category: unsupported conclusion.**

- Evidence: [naming research](../naming-research.md), lines 109-115, records an
  existing personal `ylva` account but concludes that the `ylva` organization
  can be created because `/orgs/ylva` returns 404. Lines 106-107 and 120-124
  propagate this into claimable Go/GHCR/repository coordinates.
- Impact: an absent organization or repository does not establish the right to
  create it under an occupied GitHub account namespace.
- Resolution: distinguish resource absence from namespace control. Keep this
  historical evidence, retract unsupported availability conclusions, and use
  a namespace actually controlled by the project for actionable publishing advice.
  No new account/domain/registry availability check was performed in this review.

### D19 - Naming-research reproducibility command uses an unsupported flag

- [ ] Correct the unsupported naming-research command.

**Priority: Low. Category: invalid documented command.**

- Evidence: [naming research](../naming-research.md), line 696, suggests
  `gh search repos ylva in:name --exact`.
- Corroboration: the installed CLI rejects this with `unknown flag: --exact`;
  `gh search repos --help` lists `--match`, not `--exact`.
- Resolution: use a supported query and explicitly filter returned repository
  names for equality. Do not present `in:name` alone as exact-name matching.

### D20 - Naming research mixes historical exploration with current decisions

- [ ] Reconcile historical naming status and conclusions.

**Priority: Low. Category: stale metadata/status and overstatement.**

- Evidence: [naming research](../naming-research.md), lines 59-67, still asks
  readers to choose a project name and perform checks later marked done at
  lines 200-203. Line 221 labels Nerida "best pick" despite Ylva being the
  stated top pick and current project name. Lines 194-197 assert "no IP cost"
  while lines 205-217 retain uncompleted trademark checks.
- Resolution: label this as dated historical research, reconcile completed
  versus outstanding work, qualify unverified conclusions, and add observation
  timestamps where evidence exists. Do not fabricate fresh availability/legal
  verification or treat historical notes as legal clearance.

## Diagram findings

### M01 - Configuration diagram applies the ordinary floor to skill loading

- [ ] Show the skill-loader policy exception.

**Priority: Medium. Category: policy contradiction.**

- Evidence: [configuration/policy diagram](../diagrams/configuration-policy-flow.mmd),
  line 30, describes tool-rule resolution as
  `Agent entry > profile entry > built-in floor`; line 39 feeds that resolution
  into callable-skill filtering. It omits the loader's explicit default exception.
  [Configuration](../configuration.md), lines 93-99, and
  [implementation notes](../implementation-notes.md), lines 102-105, say the
  unconfigured loader uses empty rules rather than ordinary outside/dot-path floors.
- Corroboration: `src/skills/policy.rs:146-153` and `src/agent/tools.rs:881-890`
  default the loader to an empty rule list.
- Impact: the diagram implies that unconfigured skill loading requires ordinary
  path approval, unlike the current callable-skill contract.
- Resolution: qualify the ordinary-tool fallback and show the `load_skill`
  empty-rules/eligible-instruction auto-approval exception explicitly. Keep skill
  sidecar reads governed by independent `read` policy.

### M02 - Subagent sequence reverses event enqueue and task launch

- [ ] Correct subagent event-enqueue/task-launch ordering.

**Priority: Low. Category: sequence ordering mismatch.**

- Evidence: [subagent lifecycle sequence](../diagrams/subagent-lifecycle-sequence.mmd),
  lines 19-20 and 57-58, launches the child before recording `child_start`,
  for both immediate and dequeued starts.
- Corroboration: `src/agent/child_agent_manager.rs:488-498` enqueues
  `store.record_child_start(...)` before invoking `spawn_fn`. Both startup
  paths use that method.
- Resolution: move each parent event arrow before its child-launch arrow.
  Label this enqueueing/publication order, not guaranteed disk synchronization.

No finding is raised for deliberately selective member inventories, omitted
independent state dimensions, or non-exhaustive failure paths already described
by the diagram scope. In particular, the diagrams' manual-compaction references
are correct; D01 must not be resolved by removing `/compact` from the diagrams.

## Idea-document findings

All findings here are **Low** priority. Resolve ambiguity in favor of non-idea
references, and treat stale statements as historical proposals rather than
evidence that current behavior should change.

### I01 - Compaction research mixes updated facts and superseded recommendations

- [ ] Label superseded compaction research.
- Evidence: [conversation compaction](../ideas/conversation-compaction.md),
  lines 319-321, says current totals omit much of the wire payload; lines 453-457
  describe a single integer conflating estimates and observed usage. Lines 10-23
  and 75-81 in the same note already distinguish those representations, as do
  [features](../features.md), lines 236-258. Lines 601-607 still defer manual
  `/compact`, which the current slash-command reference documents as implemented.
- Resolution: label superseded research, link current accounting contracts, and
  remove implemented manual compaction from the deferred list. Preserve genuinely
  deferred tokenizer/calibration work.

### I02 - Web-search idea incorrectly marks StartPage as shipped

- [ ] Correct web-search backend implementation status.
- Evidence: [web search](../ideas/web-search.md), lines 3-9, checks off StartPage
  and omits Brave. [Configuration](../configuration.md), lines 703-713, names the
  current backends as DuckDuckGo, SearXNG, and Brave.
- Resolution: distinguish implemented selection from future backend proposals;
  mark StartPage proposed and link to the current backend reference.

### I03 - Actionable proposal text uses obsolete public identifiers

- [ ] Correct obsolete public identifiers in actionable proposals.
- Evidence: [system prompt versus tools](../ideas/system-prompt-vs-tools.md),
  lines 8-23, 34-40, 50-57, and 73-76, uses `read_file` as a callable tool and
  suggests a per-profile persona field. Current public names/ownership are `read`,
  independently resolved global `SOUL.md`, and per-agent-type `persona`.
  [Copilot provider](../ideas/copilot-provider.md), lines 9-13, suggests
  `provider_configs.*` instead of `[provider_connections.<name>]`.
  [Tool-result guard](../ideas/tool-result-size-guard.md), line 24, calls the
  terminal tool `finish` instead of `agent_finish`.
- Resolution: correct actionable identifiers and link to the tool/configuration
  references without turning the rest of each proposal into an implemented contract.
  Do not rename real internal functions merely because public tool names differ.

### I04 - Tirith proposal still depends on an implemented prerequisite

- [ ] Update the Tirith prerequisite status.
- Evidence: [shell hardening](../ideas/shell-hardening-with-tirith.md), lines 69-74,
  waits for `CommandGlob`; [configuration](../configuration.md), lines 970-1042,
  already describes command-shape rules, structural parsing, and aggregation.
- Resolution: link the existing command-rule capability and identify Tirith
  integration itself as deferred. Do not imply that Tirith has shipped.

### I05 - Copilot verification examples were damaged by redaction and editing

- [ ] Repair redacted Copilot examples without exposing secrets.
- Evidence: [Copilot provider](../ideas/copilot-provider.md), lines 497 and 545,
  has `format!("******", copilot_token)` with an unused formatting argument.
  Line 465 has an unmatched inline-code delimiter. Lines 859 and 866 show
  redaction-only authorization values although line 864 tells readers to replace
  `SESSION_TOKEN`. Line 843 uses `GHO_TOKEN` despite the surrounding `ghu_` flow.
  Lines 858 and 865 hardcode the generic origin despite earlier
  `endpoints.api` guidance at lines 118-126 and 352-359.
  Lines 400-403 also contain a damaged sentence.
- Resolution: preserve all redaction but use consistent non-secret placeholders;
  feed the discovered endpoint into illustrative follow-up commands and repair
  the table/prose. No live authentication claim was verified.

### I06 - Sample prompt assumes unimplemented structured CLI conversation output

- [ ] Clarify the sample prompt's expected JSONL input.
- Evidence: [sample prompts](../ideas/sample-prompts.md), lines 176-178,
  asks to parse conversation JSONL output from a CLI run. Current headless output
  is streamed assistant text, diagnostic JSONL is separate, and conversation
  JSONL belongs to persisted sessions. Structured final output remains proposed
  in [CLI JSON output](../ideas/cli-json-output.md), lines 3-10.
- Resolution: identify the intended input as session JSONL or diagnostic logs.
  If this is intentionally a negative capability test, state that expectation.

### I07 - Timing/cost proposals do not identify their already-implemented portions

- [ ] Distinguish shipped timing/cost features from proposals.
- Evidence: [timing](../ideas/timing.md), lines 3-15, proposes running-thinking
  timing without acknowledging existing thinking timers and completed tool
  durations. [Costs](../ideas/costs.md), lines 3-7, proposes the already-present
  approximate context/window readout.
- Resolution: label partial implementation and keep extra timing breakdowns,
  post-mortem analysis, and billed-cost totals explicitly deferred. Approximate
  context estimates must not be described as billed usage.

### I08 - Redaction proposal contradicts its own absolute guarantee

- [ ] Qualify the redaction proposal's guarantee.
- Evidence: [redaction](../ideas/redact-sensitive-info.md), lines 3-5, says
  sensitive values can never reach an LLM, but lines 7-10 acknowledge that
  alternative encodings can evade the proposed exact-value filter.
- Resolution: describe best-effort defense in depth and keep variable-selection
  policy as an unresolved design question, not a guarantee of prevention.

### I09 - Sample-prompt Markdown fails the configured linter

- [ ] Fix the sample-prompt Markdown lint issue.
- Evidence: [sample prompts](../ideas/sample-prompts.md), line 228:
  `MD012`, multiple consecutive blank lines.
- Resolution: remove the extra blank line in an authorized cleanup.

### I10 - Twenty-four idea documents have no inbound Markdown link

- [ ] Resolve idea-document discoverability disposition.
- Evidence: local link-graph inspection found no inbound Markdown link for:

  ```text
  anthropic-provider.md
  approval-verdict-trail.md
  bash-approval.md
  bash-progress.md
  copilot-provider.md
  costs.md
  defer-markdown-parsing-fallback.md
  enhanced-markdown-rendering.md
  monokai-theme.md
  openai-provider.md
  openrouter-app.md
  provider-model-favorites.md
  redact-sensitive-info.md
  sample-prompts.md
  session.md
  shell-hardening-with-tirith.md
  slash-commands.md
  system-prompt-vs-tools.md
  timing.md
  tui-animated-logo.md
  tui-enhancements.md
  user-approval-for-repeated-tool-calls.md
  web-fetch.md
  web-search.md
  ```

- All paths above are beneath `docs/ideas/`. This is discoverability debt, not
  proof these historical documents should be deleted.
- Resolution: decide whether to expose an ideas directory link, add links from
  related references, or explicitly retain them as an unindexed scratch collection.
  No new index file is required by this plan.

**Not a confirmed finding:** `sample-prompts.md:204-210` contains
`for n in seq 1 5`, which iterates over literal words rather than a generated
numeric sequence. The note does not state its expected count and may test exact
command fidelity. Clarify its intent before changing the sample.

## Remediation plan

The review itself does not authorize these repairs.

Check an item only after its fix is applied and verified. For findings requiring
an author decision, record the agreed disposition before checking the item.
Keep the original review summary as the audit baseline.

### Execution and final verification

- [ ] Reconcile current behavioral contracts first: D01-D07 and D11.
  Update `docs/features.md`, `docs/configuration.md`, `docs/cli-options.md`,
  `docs/development-workflow.md`, `docs/persistence.md`, and `CONTEXT.md` in the same
  documentation change.
  Verify default-agent inheritance and nested-launch-directory examples against
  the existing implementation before finalizing wording.
- [ ] Resolve M01-M02 against authoritative current references.
  Update affected `docs/diagrams/*.mmd` sources and their scope descriptions in
  `docs/implementation-notes.md` and `docs/diagrams/README.md` where necessary.
  Keep generated SVGs out of the authored change set.
- [ ] Remove redundant/stale explanations and clarify navigation: D08-D17.
  Update `README.md`, `AGENTS.md`, `CONTEXT.md`, `docs/features.md`,
  `docs/configuration.md`, `docs/cli-options.md`, `docs/implementation-notes.md`,
  and `docs/keybindings.md` only as required by changed links/headings.
  Decide the ownership/status of `docs/backlog-issues.md` and
  `docs/naming-research.md` before moving or removing content.
- [ ] Reconcile historical naming claims: D18-D20, in `docs/naming-research.md`.
  Separate old observations from current verified facts.
- [ ] Triage I01-I10 last. Mark superseded proposals or link their
  implemented portions to the current references; preserve genuinely unimplemented
  alternatives rather than rewriting all proposals as product documentation.
  Affected idea files are `conversation-compaction.md`, `web-search.md`,
  `system-prompt-vs-tools.md`, `copilot-provider.md`, `tool-result-size-guard.md`,
  `shell-hardening-with-tirith.md`, `sample-prompts.md`, `timing.md`, `costs.md`,
  and `redact-sensitive-info.md`, all under `docs/ideas/`. Decide I10 navigation
  separately; do not rewrite all orphaned ideas solely to add status text.
- [ ] Re-run `rumdl check . --output-format concise`, local-link/anchor checks,
  and Mermaid parsing/rendering for changed sources. Repeat the documentation
  consistency pass after repairs; do not call this report-only pass a clean audit.
  No Rust implementation change is planned; if separately authorized, it must
  follow the development workflow's implementation/test gates.

## Complete review inventory

### Root and current reference documents

```text
AGENTS.md
CONTEXT.md
README.md
docs/backlog-issues.md
docs/cli-options.md
docs/configuration.md
docs/development-workflow.md
docs/diagrams/README.md
docs/features.md
docs/implementation-notes.md
docs/keybindings.md
docs/known-test-issues.md
docs/naming-research.md
docs/persistence.md
docs/release-notes-workflow.md
.ylva/profiles/default/agents/broken.md
.ylva/profiles/default/agents/research-and-discover.md
```

The two agent-type files have valid frontmatter/body structure. Their naming and
runtime engine availability are not documentation-orphan issues; this review
does not execute a developer's local profile or treat an intentionally named
`broken` configuration as a confirmed product-documentation defect.

### Idea documents

```text
docs/ideas/agent-steering.md
docs/ideas/anthropic-provider.md
docs/ideas/approval-verdict-trail.md
docs/ideas/bash-approval.md
docs/ideas/bash-progress.md
docs/ideas/cli-json-output.md
docs/ideas/cli-model-override.md
docs/ideas/cli-output-buffering.md
docs/ideas/cli-output-streams.md
docs/ideas/cli-provider-override.md
docs/ideas/cli-quiet-mode.md
docs/ideas/conversation-compaction.md
docs/ideas/copilot-provider.md
docs/ideas/costs.md
docs/ideas/defer-markdown-parsing-fallback.md
docs/ideas/enhanced-markdown-rendering.md
docs/ideas/inter-agent-communication.md
docs/ideas/monokai-theme.md
docs/ideas/openai-provider.md
docs/ideas/openrouter-app.md
docs/ideas/provider-model-favorites.md
docs/ideas/redact-sensitive-info.md
docs/ideas/sample-prompts.md
docs/ideas/session.md
docs/ideas/shell-hardening-with-tirith.md
docs/ideas/slash-commands.md
docs/ideas/system-prompt-vs-tools.md
docs/ideas/timing.md
docs/ideas/tool-result-size-guard.md
docs/ideas/tui-animated-logo.md
docs/ideas/tui-enhancements.md
docs/ideas/user-approval-for-repeated-tool-calls.md
docs/ideas/web-fetch.md
docs/ideas/web-search.md
```

### Mermaid sources

```text
docs/diagrams/class-diagram.mmd
docs/diagrams/compaction-data-flow.mmd
docs/diagrams/configuration-policy-flow.mmd
docs/diagrams/session-lifecycle-state-diagram.mmd
docs/diagrams/session-storage-state-diagram.mmd
docs/diagrams/subagent-lifecycle-sequence.mmd
docs/diagrams/tui-event-flow.mmd
docs/diagrams/tui-reducer-state-diagram.mmd
docs/diagrams/turn-execution-sequence.mmd
```
