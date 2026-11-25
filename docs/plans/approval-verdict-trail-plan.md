# Trace tool approval verdicts in Ylva logs

## Context

Ylva's Rust tool policy returns an `ApprovalCheck`, but the verdict alone cannot tell an operator
which component made the decision, which effective rule it used, or which earlier check led to it.
This makes implicit defaults, CLI overrides, and grant approvals hard to distinguish when
diagnosing a root or subagent tool call.

Add three pieces of provenance to every `ApprovalCheck`: `decision_maker` identifies the module or
component that issued the check; `rule_origin` identifies the rule, guard, or fallback that produced
it; and `parent` points to the earlier check it actually depended on, if any. Retain a single causal
chain, not a history of overwritten rules or all candidates. For a compound decision, keep the
decisive contribution when there is one; when the outcome depends on all contributions, identify the
aggregation rule without attributing the result to an arbitrary input.

The trail is diagnostic only. At `INFO`, Ylva's existing opt-in text and JSON logs must show the
effective check and, for an approval request, its final disposition if one is reached. Distinguish
approve once, human deny, headless CLI unable to ask, human grant-and-approve, and automatic
satisfaction by a grant. Live rechecks that consume an `ApprovalCheck` are in scope. Grant previews,
`clarify` (including its answer), cancellations, budget skips, and safeguards that do not use
`ApprovalCheck` are out of scope. An evaluation that leaves a request pending does not count as a
second prompt or a final disposition.

Rule locators must identify the exact CLI flag, the workspace or Ylva-home config layer plus TOML
key and zero-based rule index, or a stable built-in rule identifier. Use existing opaque grant
references where applicable. Trail records must not include tool arguments, raw rule or grant text,
absolute paths, URLs, credentials, or free-form denial reasons. Existing lower-level diagnostics
are not redesigned.

Approval policy and ordering, workspace trust, grant scope and eligibility, interaction and exit
behavior, transcript and replay, and the model-facing protocol must stay unchanged. ADRs
[0003](../architecture-design-records/0003-trust-workspace-configuration-by-content-hash.md) and
[0005](../architecture-design-records/0005-keep-tool-approval-grants-separate-from-sandbox-rules.md)
still apply: neither a grant nor a host response alters the sandbox check. A disposition explains
what happened after it. No new flags, configuration syntax, dependencies, or storage format are
required.

## Implementation Plan

Start with `src/sandbox/rules.rs` and run the existing precedence baseline:
`cargo test --lib auto_approve_catch_all_wins_over_configured_deny_rule`.
Write each behavior regression before making it pass. After verifying each step, immediately change
its `[ ]` to `[x]` in this file so work is resumable.

1. [ ] Separate the verdict from its provenance in `src/sandbox/rules.rs`. Give `ApprovalCheck` a
   typed verdict (including the existing denial reason), a stable component identifier, a typed
   origin, and an optional owned prior check. Require an explicit origin at construction; do not
   silently fill omissions with a default. Keep `Option<ApprovalCheck>` as the sandbox's abstention
   contract and make comparisons that control execution explicitly verdict-only. Migrate producers
   and consumers together, including the library re-exports in `src/sandbox.rs` and `src/lib.rs`,
   `Tool::needs_approval` and its implementations, `src/skills/policy.rs`, and test doubles. Cover
   schema rejection, validation inside tools, registry fallback, built-in tool verdicts, and tool
   defaults after sandbox abstention. Preserve all verdicts and error messages. Give `clarify`'s
   internal routing sentinel provenance for completeness, but do not emit it as a trail record.
   Verify with `cargo test --lib agent::tools`.
2. [ ] Carry effective rule identity from CLI/profile resolution to evaluation. In `src/cli.rs`,
   `src/startup.rs`, `src/config/profile.rs`, `src/agent_factory.rs`, `src/agent/tools.rs`, and
   `src/sandbox.rs`, attach the origin to the actual compiled or appended rule, not to the result
   after the fact. Retain the winning config layer and whether the rule list came from
   `tools.<tool>.rules` or `agents.<type>.tools.<tool>.rules`, and enumerate the original configured
   entries before compilation. A configured empty list replaces its fallback; it does not invent a
   configured rule. Cover per-tool flags (including spawn's direct flag path), built-in rules and
   tool-added rules; do not call a programmatically supplied rule a CLI flag or file-backed rule.
   Test workspace/home selection, agent versus profile lists, an empty override, and a CLI override
   over a configured match. Run `cargo test --lib config::profile`,
   `cargo test --lib agent_factory`, and `cargo test --lib sandbox`.
3. [ ] Propagate the winning check through the path, Bash, and network evaluators in
   `src/sandbox/{path,command,network}.rs` and their tool wrappers. Pass through unchanged checks
   rather than wrapping them redundantly; identify the combiner when it creates a new result.
   Preserve last-match precedence, Bash worst-verdict ranking and tie-breaking, abstention and
   default fallbacks. For Bash, retain the decisive statement for a single winning denial or
   approval requirement, and use stable aggregation/fallback origins for collective approval and
   unmatched or unanalysable statements. An independent override reports its own origin, not a
   displaced rule as its parent. Extend `cargo test --lib sandbox` with cases that assert the exact
   winner and parent while also checking the unchanged verdict and reason.
4. [ ] Carry the effective check through the actual approval lifecycle in
   `src/agent/tool_queue.rs` and `src/agent/runtime{.rs,/commands.rs}`. Keep the published pending
   check associated with the front call as long as it is pending and discard it when the call
   leaves the queue. Return grant coverage as a separate outcome with its references and policy
   parent, not a synthetic sandbox auto-approval. Carry the true disposition through the shared
   resolution path: user approve once, successful user grant-and-approve, automatic grant match
   (including grant-watch wake-ups), or denial. A failed grant save, stale command or ineffective
   resolution must not emit a successful disposition. Cover automatic and manual spawn admission
   as well as ordinary execution. In `src/cli_driver/run.rs`, preserve the existing `ToolPending`
   fallback/recheck and exit-code handling, but carry its actual source back to the runtime instead
   of inferring it from a reason string or attributing it to a human. Verify the grant, pending
   queue and command behavior with `cargo test --lib agent::tests::tool_queue` and
   `cargo test --test cli_exit_codes`.
5. [ ] Emit the trail using the existing `tracing` facade. Use a single safe projection for the
   structured `INFO` records: outcome, decision maker, rule locator, tool/call correlation, a
   record identifier, and an optional parent identifier that resolves in the same log. Scope
   records to the existing session/subagent and tool spans where available; distinguish multiple
   live checks for the same `tool_call_id` by phase and identifier. Reuse an already-emitted parent
   instead of logging it again. Consolidate the existing approval-path `INFO` events as needed so
   one disposition is not logged twice and the trail no longer writes raw denial reasons or tool
   arguments there. Log live `ApprovalCheck` consumers such as the `load_skill` runtime recheck and
   `web_fetch` hop denial callback; distinguish a hop's policy verdict from the enforcement
   callback's deny-only interpretation, with no request target in the event. Do not log checks from
   previews or make the low-level evaluator emit a trail on every invocation.
6. [ ] Verify the log contract end to end. In owner-local agent/runtime tests, capture text and
   JSON and assert that trail records are emitted at `INFO`. In the CLI fixture, verify with
   `--log-level info`. Assert that a configured winner and a CLI override can be identified,
   a pending check can be joined to its final disposition, and a subagent or live recheck is
   distinguishable from the root check. Cover grant match versus human grant creation and CLI
   no-interaction denial without a human attribution. Use sentinel secrets in rule text, reason,
   and tool inputs to check that trail records exclude them; assert that a grant preview and
   `clarify` produce no trail events. Verify unchanged tool execution, UI updates, and CLI outcomes
   through the existing fixtures. Use the tracing isolation workaround in
   [`docs/known-test-issues.md`](../known-test-issues.md#tracing-subscriber-isolation-in-tests).
7. [ ] Update [`docs/features.md`](../features.md#logging) with the opt-in `INFO` behavior,
   correlation and privacy limits;
   [`docs/configuration.md`](../configuration.md#sandbox-tool-approval-rules) with how a logged
   locator maps to the winning configured rule; and
   [`docs/implementation-notes.md`](../implementation-notes.md#tool-approval-grants) with the
   rationale for retaining a pending check across host responses and grant wake-ups. Keep field
   inventories out of the implementation notes. Do not change the ADRs, persistence docs,
   CLI options, or TUI styleguide: those contracts do not change. Run `rumdl check` on the updated
   documentation.
8. [ ] Run `make format lint` and `make test-coverage`, the single mandatory full test gate. Run
   `make audit` only if a dependency manifest or lockfile changed. Confirm that there are no
   unrelated diffs, and that logs can explain the same approvals and denials without changing
   session data, user interaction, or tool execution.

## Implementation Hints

- `ApprovalCheck` and `ApprovalVerdictConfig` are defined in `src/sandbox/rules.rs`; only the latter
  represents the serialized verdict. `SandboxContext` currently has separate configured and
  appended rules. Compiled rules and direct tool defaults do not currently retain source identity.
- `Cli::auto_approve_tools` reduces flags to `AutoApproveTool` values. At profile load the selected
  file path and the effective rules are available separately, via `profile_config_source_path` and
  `effective_tool_rules` in `src/config/profile.rs`. Agent-specific lists replace rather than merge
  with profile lists. `AgentFactory` currently forwards a map of bare lists to the registry.
- `SandboxContext::check_command` folds per-statement results, with the earlier result winning ties,
  before applying appended rules. It also exposes statement analysis to the grant matcher. The
  path and network evaluators have their own last-match logic. `check_private_address` in
  `src/sandbox/network.rs` returns a boolean enforcement result separate from the approval check.
- `evaluate_call_approval` in `src/agent/tool_queue.rs` owns schema → policy → grant ordering.
  `GrantDecision::Covered` already yields safe `GrantReference` values (`src/grants/state.rs`,
  `src/grants.rs`). Grant creation and the grant generation watch currently converge on
  `ApproveTool` in `src/agent/runtime/commands.rs`; headless CLI uses `DenyTool` when interaction
  is unavailable. These shared commands currently lose origin information.
- `src/agent/tools/load_skill.rs` rechecks for denial before returning the selected skill. In
  `src/agent/tools/web_fetch/call.rs` the `HopPolicy` callback consumes network checks for explicit
  denial without prompting for individual hops.
- `src/logging.rs` owns text and JSON subscriber setup. `src/test_support.rs` provides
  `capture_logs_with_format`; the workflow's session logging guidance describes span correlation
  and async instrumentation. Existing baselines include sandbox precedence cases and
  `granting_a_pending_bash_call_runs_it_and_covers_the_next_identical_call` in the agent
  tool-queue suite, plus `denied_approval_exits_with_approval_denied_and_run_continues` in
  `tests/cli_exit_codes.rs`.
