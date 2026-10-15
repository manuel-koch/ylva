# Agent skill discovery and on-demand loading

## Context

Ylva is a Rust CLI/TUI agent-harness with agent-type configuration, tool
allowlists, sandbox approvals, and persisted conversations. It currently has no
skill discovery or loading. This change adds reusable Agent Skills: directories
containing YAML-frontmatter `SKILL.md` instructions and optional sidecar files.
Advertising only names and descriptions keeps initial requests lean; the model
calls `load_skill` with a required `name` argument to obtain instructions.

The successful result excludes frontmatter, preserves the Markdown body, and
ends with a harness-owned footer containing the absolute canonical skill-file
path and instructions to resolve relative references against its parent
directory. Sidecars remain on-demand reads. An opt-in `skill_sidecar` named
sandbox rule lets users assign a verdict to sidecar reads without maintaining
individual paths. Loading itself neither executes scripts nor changes permissions.
Instructions are ordinary tool-result content, never additional system messages.

**Status: refined and confirmed after grilling.** All design decisions below
were confirmed by the user. This task authorizes this plan, not implementation,
staging, branching, or commits. A new implementing session can use this document
without reading the original idea or conversation.

### Goal and scope

The first version includes local discovery, system-prompt metadata advertisement,
one loading tool, per-agent eligibility, the opt-in read-only sidecar rule, and
provider-independent `ylva skill list`.
Installation, marketplaces, remote retrieval, configurable/profile-specific
search roots, recursive discovery, automatic scripts, hot reload, skill slash
commands, JSON listing, aliases, and implicit sidecar grants are excluded.

### Acceptance criteria

- Startup and listing share deterministic discovery, parsing, precedence,
  provenance, limits, and rejection diagnostics. No directory iteration order,
  provider request, or real user-home dependency leaks into unit tests.
- Exactly the eight roots and ordering in Implementation Hints are supported.
  External symlinks are eligible. Invalid or over-limit candidates are omitted
  with visible diagnostics; a valid lower-priority candidate may supply their
  name. At most 128 winning skills and 4 MiB of winning raw file bytes are retained;
  each individual candidate has a 64 KiB raw-byte limit.
- Required standard YAML `name` and `description` are validated. Optional and
  unknown fields confer no runtime behavior or permissions. Frontmatter never
  reaches a successful loaded body.
- `allowed_skills` works in inline agent-type tables and TOML/YAML agent-type
  files. Omission means all retained discovered names; `[]` means none;
  explicit values replace, not merge. Unknown explicit names fail startup,
  including in unused agent types.
- Every agent receives only its own callable catalog. Auto-denied skills are
  never advertised. Approval-required skills are advertised with an annotation
  in TUI mode and omitted headlessly. Explicitly selected denied skills, or
  approval-required skills in headless mode, fail startup.
- The loader respects `allowed_tools`; explicit nonempty `allowed_skills`
  without a permitted loader fails startup. Otherwise disabled loaders produce
  no advertisement. An empty effective catalog registers no loader.
- The loader enforces exact-name membership independently of prompt/schema
  hints. With no configured loader rules it auto-approves eligible named bodies;
  configured rules evaluate the snapshot's canonical source path. No policy
  denial can select a different same-name source.
- Successful loads deliver the complete body, footer, metadata, and serialized
  tool envelope within the current allocated result allowance. Otherwise emit
  a normal explicit tool error, not a successful partial skill. Keep ordinary
  tools' existing bounded projections and the safe request ceiling unchanged.
- The immutable per-process catalog serves repeated loads, subagents, factory
  copies, `/clear`, and TUI session switching. Restart rediscovers. Resume/clone
  replays earlier tool results without reopening their original files.
- Sidecar reads retain normal `read` rules unless the user configures
  `skill_sidecar`; the new rule accepts any existing verdict and uses ordinary
  last-match-wins semantics. It applies only to regular sidecar files beneath
  the canonical directory of a skill in that agent's callable catalog, excluding
  the instruction source itself and escaping symlinks. No successful prior load
  is required. Script execution and all non-read tool restrictions stay unchanged.
  A skill cannot grant any permission or inherited body to a subagent.
- Listing shows sorted winning names/descriptions/absolute paths plus shadowed
  provenance without loading a profile, initializing providers, consuming stdin,
  or writing sessions. Empty discovery is explicit. Reported omissions alone
  do not make a successful list fail.
- Documentation is part of implementation; final verification follows the
  repository's formatting, lint, and coverage policy.

## Implementation Plan

An implementing agent must flip each `[ ]` to `[x]` immediately after completing
that step and rewrite this file in place. Preserve checkbox states and consecutive
numbering when revising the plan. Each completed implementation step includes
its directly affected documentation, as required by the plan-execution workflow.

1. [ ] Read `src/agent/tools.rs` first to locate `Tool`, `ToolCall`,
   `ToolCallResult`, schemas, and registry construction. Run
   `cargo test --lib agent::tools::tests::registry` as the first targeted baseline.
   Check `docs/development-workflow.md`, `docs/known-test-issues.md`, and the ADR
   directory before implementation. Reconfirm the facts below against current
   code; the ADR directory was empty during planning. Ask before creating a new
   ADR or documentation file not already authorized.
2. [ ] Add the discovery/parser owner in `src/skills.rs`, with file-per-module
   children only when needed. Provide a pure explicit-root entry point taking
   workspace root, launch directory, OS home, and Ylva home; environment resolution
   stays at startup. Construct the approved ordered roots; enumerate immediate
   child directories deterministically and recognize exact-case `SKILL.md`.
   Resolve external symlinks, deduplicate canonical files, validate YAML, retain
   immutable winner bodies and provenance, and collect typed diagnostics instead
   of printing from the parser. Bound reads to the per-file limit plus one byte.
   Tests cover all required field/type/name limits, LF/CRLF, BOM, exact-case
   discovery, preserved body, empty body, duplicate YAML keys, malformed/absent
   frontmatter, invalid UTF-8, missing/unreadable/non-directory roots, symlinked
   roots/directories/files, external and broken symlinks, duplicate physical files,
   and deterministic precedence/fallback.
3. [ ] Implement winning-catalog limits and diagnostics in the discovery owner.
   Traverse roots by precedence and child names lexicographically. Reject
   oversized candidates before unbounded allocation; skip non-fitting new winners
   and continue, permitting smaller later candidates and same-name fallback.
   Shadowed files do not consume winning count/byte budgets. Keep enough
   rejected-name/path/reason provenance for unknown-allowlist errors and listing.
   Test exactly 64 KiB and one byte over; exactly 4 MiB and over; 128 and 129
   winning names; shadowed candidates; oversized preferred-source fallback;
   aggregate-budget rejection followed by a smaller candidate; and stable warnings.
   Emit visible startup/list stderr diagnostics and structured logs once per
   discovery, not repeatedly for each factory or spawned agent.
4. [ ] Add `allowed_skills: Option<Vec<String>>` to `src/config/agent.rs` and
   the separate `AgentConfigOverrides` in `src/config/agent_type_file.rs`.
   Wire built-in defaults, struct initializers, and field override application in
   `src/config/profile.rs`. Preserve whole-file layering and explicit-empty
   override semantics. Normalize duplicate allowlist entries without duplicate
   advertisements. Test omission, none/subsets, file-plus-inline replacement,
   TOML/YAML authoring, and environment interpolation. Keep startup errors
   consistent with selected-profile/source-path diagnostics.
5. [ ] Resolve skill availability per agent type using its effective
   `allowed_tools`, `allowed_skills`, loader sandbox rules, and actual CLI/TUI
   approval capability. Validate all configured types, including unused ones.
   Extend path-rule-kind and registered-tool-name validation for `load_skill`
   in `src/config/profile.rs` and `src/agent_factory.rs`. Without loader-specific
   rules use explicit empty path rules plus the tool's auto-approved abstention
   default, rather than the usual dot/outside floor. Configured rules fully
   replace that default and evaluate canonical snapshot paths with existing
   last-match-wins semantics. Test explicit unknown/denied/headless-unapprovable
   names, missing loader, empty lists/catalogs, TUI approval annotations,
   profile-versus-agent rule replacement, and no fallback after policy denial.
6. [ ] Implement `src/agent/tools/load_skill.rs` as an ordinary tool over the
   filtered immutable catalog. Derive typed required `name` arguments and
   result metadata schemas. Reject arbitrary paths, empty names, unknown names,
   and forbidden membership during execution; repeat canonical-path approval
   evaluation at invocation. Return unchanged body plus a clear harness-owned
   footer, canonical absolute source/name metadata, and Markdown content section.
   Escape paths/control characters in framing without corrupting body bytes.
   Tests assert exact result shape, no frontmatter, relative-reference guidance,
   repeat-load identity, file modification/deletion after snapshot, unknown and
   forbidden names, approval denial, and no sidecar/script side effects.
7. [ ] Add an opt-in complete-delivery policy at the existing tool-result
   boundary, leaving ordinary results' bounded projections unchanged.
   Reuse/extract the current tool-envelope serialization and token estimate;
   measure the entire actual envelope, including metadata/schema and escaped
   footer text. At result finalization, check the current live allowance before
   `Context::push_tool_result` persists success. If insufficient, route through
   the ordinary tool-error path and persist exactly one matching error, not a
   successful load followed by truncation. An early check may use the allowance
   already passed to `PendingTool::execute`, but cannot replace the final live
   check. Cover exact-fit/one-token-over, metadata-only overflow, queued-call
   sharing, changes between admission and completion, cancellation, and error
   framing. Verify ordinary tools still truncate normally and `agent_finish`
   remains unbudgeted; never raise or bypass the safe request ceiling.
8. [ ] Add opt-in `NamedSandboxRule::SkillSidecar` in `src/sandbox/rules.rs`
   and matching in `src/sandbox/path.rs`. Carry immutable canonical skill
   directory/source identities from each agent's callable catalog into its
   `read` sandbox context; avoid a mutable loaded-name set. Require the requested
   path to resolve to a regular file within a recorded canonical skill directory,
   excluding skill instruction sources and `SKILL.md` files. A missing path,
   unresolvable path, or escaping symlink does not match this rule and continues
   through ordinary rule evaluation. Accept any existing verdict with normal
   last-match-wins precedence; do not add the rule to the default floor.
   Add read-only rule-kind validation in `src/config/profile.rs` and defense in
   depth in rule resolution: reject it for all other tools, including
   `load_skill`. Tests cover nested sidecars, external skill directories,
   in-boundary/escaping symlinks, path traversal, non-files, instruction-file
   exclusions, per-agent catalogs, before-first-load behavior, unchanged behavior
   without the rule, all three verdicts, ordering, and profile/agent overrides.
9. [ ] Extend `src/agent/tools.rs` default CLI/TUI tool sets and
   `ToolRegistryConfig` with the resolved loader catalog. Compose one sorted
   per-agent name/description advertisement in
   `src/agent_factory.rs::compose_agent_system_prompt`; include TUI approval
   annotations, no bodies, and no unavailable skills. Keep the loader description
   focused on invocation rather than duplicating the catalog. Use generic
   rendering and registry-driven `/tools`/help paths, checking
   `src/cli_driver.rs` display-registry construction for consistency.
   Tests cover disabled loader, empty catalogs, filtered descriptions, root versus
   subagent catalogs, metadata framing, and direct adversarial calls.
10. [ ] Discover before session creation in `src/main.rs`, then attach the
   catalog and approval capability to `AgentFactory` for configured-engine and
   environment-engine paths. Preserve them through `without_store`, root/session
   construction, and subagent-manager factory copies. Each target agent type
   filters the global catalog independently, not by its parent's subset.
   Direct-library construction starts with an explicit empty catalog unless
   supplied; it performs no implicit real-home scan. Verify fresh roots,
   `/clear`, TUI session switching, resume/clone, and subagent independence with
   fake engines and temporary roots. Export required public APIs through
   `src/lib.rs`; keep main thin.
11. [ ] Add `SkillCommand::List` to `src/cli.rs` and its read-only owner in
    `src/skill_command.rs`; dispatch alongside existing provider-independent
    commands in `src/main.rs`. Share discovery and reports without loading a
    profile or creating a factory. Print terminal-safe sorted winner
    name/description/absolute-path records with valid shadowed provenance.
    Use the explicit empty message and exit rules below. Add binary/public tests
    for help/dispatch, exact output shape, collisions, warnings with exit 0,
    fatal/output errors, no credentials, ignored `--profile`, unread stdin,
    and no session/config writes. Re-export the owner through `src/lib.rs`
    when public tests need it.
12. [ ] Add assembled workflow regressions for successful/error envelopes,
    transcript persistence/replay, complete-delivery rejection, compaction,
    repeat loads, and approval routing. Verify earlier transcript results survive
    missing/changed source files and new loads use the new process snapshot.
    Test that loading and optional `allowed-tools` frontmatter cannot alter
    `read`, `bash`, `write`, `edit`, or subagent permissions. Only explicit user
    configuration may authorize `read` via `skill_sidecar`; that rule grants
    no execution or write access and never restores a filtered-out skill.
    Check tool-call/result
    pairing survives compaction and a reload can restore previously summarized
    instructions. Follow subscriber/cwd locks and UI-channel draining guidance
    in `docs/known-test-issues.md`; no live providers or real home fixtures.
13. [ ] Complete documentation in `README.md` (implemented status and loader),
    `CONTEXT.md` (Skill and skill catalog terms), `docs/features.md`
    (result/approval/lifecycle behavior), `docs/configuration.md`
    (roots/order/metadata/limits/allowlists/sandbox, including the read-only
    sidecar rule, preserved-floor examples, and canonical containment),
    `docs/cli-options.md`
    (listing and exit behavior), and `docs/implementation-notes.md`
    (snapshot/discovery/factory/budget ownership). Reconcile resolved questions
    in `docs/ideas/skills.md`, preserving unrelated user changes. Keep this plan
    current. `docs/keybindings.md` needs no change because no binding or slash
    command is introduced. Ask before creating any additional documentation.
14. [ ] Format/lint changed Markdown with repository-configured `rumdl`.
    For eventual code changes run `make format lint`, then the single mandatory
    gate `make test-coverage`, enforcing at least 80% aggregate line coverage.
    Do not run `make test` as a duplicate mandatory gate. Verify exact acceptance
    criteria, not merely compilation. Do not stage, branch, or commit without
    separate explicit consent.

## Implementation Hints

### Confirmed discovery and format contracts

The user confirmed these choices during plan refinement on 2026-10-15.

| Priority | Root |
| --- | --- |
| 1 | `<workspace-root>/.ylva/skills/` |
| 2 | `<workspace-root>/.agents/skills/` |
| 3 | `<workspace-root>/.github/skills/` |
| 4 | `<workspace-root>/.claude/skills/` |
| 5 | `<ylva-home>/skills/` |
| 6 | `<os-home>/.agents/skills/` |
| 7 | `<os-home>/.copilot/skills/` |
| 8 | `<os-home>/.claude/skills/` |

- Without a discovered workspace, the launch directory supplies the first four
  roots. An unavailable OS/Ylva home supplies no corresponding roots.
  An unavailable launch directory is a fatal initialization error.
- Discovery is one level: `<root>/<skill-name>/SKILL.md`. No ancestor chains,
  recursive descendant scan, or profile-specific roots are eligible.
  Root order plus lexicographic child directory names determines retention.
  Exact-case filenames are required even on case-insensitive filesystems.
- Roots, directories, and files may be external symlinks. Canonical source
  identity deduplicates physical files; earliest provenance is primary and
  aliases remain explainable. Directory-name validation uses the discovered
  logical skill directory, not the final target directory's basename.
  Broken/unreadable symlinks are rejected with diagnostics.
- The first valid retained candidate supplies a name. Invalid, oversized, or
  aggregate-limit-rejected candidates do not claim names. Valid lower-priority
  duplicates are shadowed; there is no body/metadata merge. Per-agent sandbox
  filtering occurs after global resolution and never selects a shadowed source.
- Required YAML fields are string `name` and string `description`. Names are
  1-64 ASCII lowercase letters/digits/hyphens, with no edge or consecutive
  hyphens, and match the logical directory name. Descriptions are nonblank and
  at most 1024 Unicode characters; multiline YAML strings are supported.
  A non-mapping frontmatter document, duplicate keys, absent closing delimiter,
  or empty/whitespace-only body is invalid. An initial UTF-8 BOM is stripped;
  body content is otherwise preserved. Frontmatter delimiters support LF/CRLF.
- `license`, `compatibility`, `metadata`, experimental `allowed-tools`, and
  unknown frontmatter fields have no runtime effect and are not injected.
  Required metadata remains available for advertising; "discard frontmatter"
  describes the loaded result, not loss of its discovery metadata.
- The fixed Ylva limits are 64 KiB raw bytes per candidate, 4 MiB cumulative
  raw file bytes for winners, and 128 winning names. Exact thresholds pass.
  Raw bytes count before BOM stripping. Shadowed files/physical aliases do
  not charge winner limits. A rejected candidate does not stop the remaining
  deterministic scan; smaller later candidates may still fit.
- Missing optional roots and directories without exact `SKILL.md` are normal
  absence. Malformed/non-UTF-8/empty/oversized/unreadable candidates, unreadable
  optional roots, broken symlinks, and limit omissions yield path/reason
  diagnostics and are skipped. Warnings appear on stderr and in enabled
  structured logs, with no skill-body logging.
- The catalog is immutable for the process/factory and includes winner bodies.
  Repeat loads return that snapshot even if the source changes/disappears.
  New processes rediscover. Old persisted tool results replay independently;
  no loaded-name set, unload operation, or implicit inherited body exists.

### Confirmed per-agent policy and model contracts

- `load_skill` is the only loading-tool name; it has no `skill` alias.
  Agent-type `allowed_skills` omission means all retained discovered names,
  explicit `[]` means none, and explicit lists replace inherited lists.
  Duplicate explicit entries have no additional effect.
- Every explicit name is checked against the retained global catalog before
  policy filtering. A name lost to invalidity/limits with no retained fallback
  is unknown and fails startup with useful rejection provenance.
- Loader permission remains subject to `allowed_tools`. Explicit nonempty
  skills with a disabled loader are startup errors; omission plus a disabled
  loader means no advertisement. Empty effective catalogs register no loader.
- `[agents.<type>.tools.load_skill]` replaces `[tools.load_skill]` when present;
  path-only rules use existing last-match-wins evaluation on canonical source
  paths. Without either table, allowed bodies auto-approve instead of inheriting
  the generic dot/outside floor. An explicit empty rule list also abstains,
  producing the loader's auto-approved default. No new auto-approval flag
  overrides configured restrictions.
- Discovery reads the snapshot before per-call approval. Loader sandbox rules
  restrict delivery to the model, not startup filesystem I/O. This explicit
  trust boundary allows externally linked installed skills; it is not a
  guarantee that discovery never reads denied files.
- Auto-denied winners are filtered from advertisement. An explicitly named
  auto-denied winner fails startup. Approval-required winners appear with
  annotations in the TUI, but are filtered headlessly; explicitly named
  approval-required winners fail headless startup. Validation covers unused
  agent types under the actual process mode, too.
- Prompt text presents only the callable name/description catalog and guidance
  to load relevant instructions before following them. There are no mandatory
  keyword triggers. The tool description explains invocation, not a second
  catalog. Tool execution independently enforces filtered membership.
- Sidecars use ordinary `read` approval with an optional user-configured
  `skill_sidecar` rule, valid only for `read`. The rule supports `auto_approved`,
  `auto_denied`, or `user_approval_required`, is absent from the default floor,
  and follows existing last-match-wins semantics. A configured rule list still
  replaces the built-in floor rather than extending it.
- Sidecar eligibility comes from the agent's callable catalog, not a loaded set:
  shadowed, disallowed, denied, and mode-unavailable skills contribute no roots.
  TUI approval-required skills are callable and do contribute roots. The rule
  is usable before any skill load and has no load/unload/resume grant lifecycle.
- A sidecar is a regular file whose canonical path is beneath a recorded
  canonical skill-file parent directory. Skill instruction sources themselves
  and files named `SKILL.md` are excluded. Nested resources are eligible.
  An external skill directory is supported, but a resource symlink escaping
  its canonical directory does not match; ordinary rules decide that read.
  Failed canonicalization/nonexistent paths likewise do not match. Matching
  uses the snapshot's recorded roots, not newly discovered or retargeted roots.
- The rule classifies paths only; it does not add the `read` tool when absent,
  freeze sidecar contents, grant write/execute access, or affect `bash`.
  Neither loaded state nor frontmatter grants filesystem access. Per-subagent
  catalogs and sidecar roots derive from that subagent's type, not the parent's subset.
- Complete-delivery applies to the serialized success envelope, not merely raw
  body length. A too-large result becomes a matching normal tool error before
  any successful result is persisted. Existing error framing and request
  ceilings still apply; ordinary tools retain their bounded-projection behavior.

Example of explicit sidecar approval while retaining the ordinary floor:

```toml
[tools.read]
rules = [
  { rule = "outside_workspace", verdict = "user_approval_required" },
  { rule = "dot_path", verdict = "user_approval_required" },
  { rule = "skill_sidecar", verdict = "auto_approved" },
]
```

The later sidecar rule overrides those two guards only for matching resources.
A subsequent deny rule can override the sidecar verdict. Existing explicit
`--auto-approve-read` precedence remains unchanged.

### Confirmed CLI contract

- `ylva skill list` prints the complete discovered catalog, not an agent's
  policy-filtered view; it intentionally does not load profiles or sandbox rules.
  Like existing maintenance commands it ignores preceding `--profile`.
- Winning names sort lexicographically. Each winner has its name, terminal-safe
  description, and absolute canonical path; valid shadowed paths follow that
  name in precedence order, with source provenance. Rejections are stderr
  diagnostics, never usable entries.
- Empty discovery prints `No skills discovered.`. A successful empty/partial
  listing exits 0, even with omission warnings. Fatal initialization/output
  failures exit 2 through the command's existing-style error reporting.
  It consumes no stdin and writes no profiles/sessions.

### Grounded repository facts and implementation seams

- `docs/ideas/skills.md` is the input, but its requirements are reproduced here.
  It was already modified by the user before planning; the updated version
  explicitly allowed `load_skill` or `skill`. No source changes were made.
  Before this plan, `docs/plans/` and the ADR directory contained only `.gitkeep`.
- Rust edition is 2021. `Cargo.toml` already includes `serde_yaml = "0.9"`,
  `serde`, `schemars`, `clap`, `tokio`, `dirs`, and test dependency `tempfile`.
  There is no dependency justification for adding a new YAML parser.
- `src/config/agent_type_file.rs` parses YAML/TOML persona frontmatter.
  Its delimiter handling is prior art; skills have a different schema and
  cannot be deserialized as `AgentConfig`. `AgentConfigOverrides` is a separate
  configurable-field representation used by `src/config/profile.rs`.
- Agent-type definitions resolve global, profile, then workspace files;
  inline field overrides are applied separately. Whole-file configuration
  replacement and field-override semantics are distinct.
- `src/config/layered.rs::discover_workspace_root` finds the nearest `.ylva/`
  or `.git` marker. OS home and Ylva home differ: `YLVA_HOME` relocates Ylva
  configuration, not other ecosystems' home directories.
- `src/config/instructions.rs` has an explicit-root pure discovery core, a
  64 KiB raw-byte limit, exact-case filenames, and BOM handling. Its trusted
  instruction files reject canonical escapes; skills intentionally allow
  external symlinks under the user-confirmed distinct trust contract.
- `AgentFactory::build` constructs agent-specific registries/sandbox rules and
  composes the global/persona prompt. Its `without_store` copy supports TUI
  session switching; new catalog/capability state has to survive that path.
  Subagents have independent contexts.
- `spawn_subagent` supplies a sorted named catalog in its tool description.
  `Tool`, `ToolCall`, and `ToolCallResult` in `src/agent/tools.rs`, with generic
  content sections and metadata, are the normal invocation/rendering contracts.
  `src/agent/tools/read.rs` demonstrates those contracts.
- `ToolCall::run` currently returns a boxed future of a boxed
  `ToolCallResult` or `ToolError`; no loader-specific event-loop execution path
  is required.
- `src/agent/tool_queue.rs::tool_output_token_limit` allocates result allowance
  before execution, reserving queued-call error framing and sharing remaining
  budget. `PendingTool::execute` in `src/agent/tool_execution.rs` currently
  receives but ignores that allowance.
- `src/agent/runtime/events.rs` handles `ToolDone`, persists the complete
  result through `Context::push_tool_result`, then limits its model projection.
  Complete-delivery validation must precede that success persistence; an early
  execution check alone does not ensure the final live allowance still fits.
  `is_result_budgeted` excludes only `agent_finish`.
- The ordinary sandbox floor requires dot/outside approval; `read` auto-approves
  only when policy abstains. Configured rule lists replace the floor.
  Headless mode denies interactive approvals. `Sandbox::new` uses cwd, which
  may differ from the discovered workspace root; discovery and approval
  boundaries are not interchangeable.
- `src/main.rs` dispatches `session`/`profile` before provider setup and stdin
  consumption. They do not load profiles. `--profile` is non-global and placed
  before a subcommand. `src/cli_driver.rs` builds a display registry separately,
  an additional surface for consistent tool availability.
- `src/agent/runtime/commands.rs` reconstructs context on `/clear`; initial
  system prompts are not persisted. Integration tests import the library via
  `src/lib.rs`, not `src/main.rs`. Owner-local tests belong under `src/`;
  public assembled workflows and executable behavior belong under root `tests/`.

### Verified ecosystem references

- [Agent Skills specification](https://agentskills.io/specification), checked
  2026-10-15, defines YAML `SKILL.md`, required `name`/`description`, and optional
  `license`, `compatibility`, `metadata`, and experimental `allowed-tools`.
  Discovery roots/order and resource limits are harness-specific choices.
  ASCII names are this plan's explicit resolution of inconsistent Unicode
  wording in the specification.
- [VS Code Agent Skills locations](https://code.visualstudio.com/docs/agent-customization/agent-skills),
  checked 2026-10-15, documents workspace `.github/skills/`, `.claude/skills/`,
  `.agents/skills/` and personal `~/.copilot/skills/`, `~/.claude/skills/`,
  `~/.agents/skills/`. It does not establish universal precedence.
- Progressive disclosure separates startup metadata, selected instructions,
  and later resource reads. The specification's suggested 5000-token body
  length is guidance, not the enforced Ylva raw-byte/result-budget contract.
