# Argument-aware Bash approval

## Context

Ylva is a Rust CLI/TUI agent-harness. Its `bash` tool executes a script through
`bash -c`; an approval policy decides whether to execute automatically, deny,
or ask the user. Existing command rules match literal argv prefixes with an
optional trailing wildcard. They do not understand command options or subcommands.
Consequently, approving a familiar command name with arbitrary arguments can
also approve writes or arbitrary execution through that command.

The goal is to reduce repetitive prompts for deliberately selected command
forms without claiming to prove that a command is read-only. Command names,
arguments, environment, executable resolution, configuration files, plugins,
and filesystem state all affect actual execution. An allow-list is an approval
convenience, not an execution sandbox or a security boundary. Reading secrets
and network exfiltration are not prevented by argument classification.

This plan translates the Bash approval research into the agreed first delivery:
argument-conditioned rules backed by small built-in command grammars, with
conservative treatment of unsupported command forms. The design decisions below
were refined through grilling and confirmed by the user. Source implementation
was separately authorized by the user's request to execute this plan; the
checklist records execution progress.

The agreed first delivery excludes executable pinning/sanitized `PATH`,
Bash operand path scoping, learned persistent rules, an approval-verdict trail,
OS-enforced isolation, heuristic subprocess scanners, LLM judging, and a new
approval-explanation UI. Those are independent projects, not implicit completion
requirements. Existing dedicated `read`, `glob`, and `search` tools remain the
preferred way to perform scoped file access.

### Acceptance criteria

- Command rules accept optional `with_arguments` and `only_arguments` conditions
  only for commands with a code-owned grammar. Invalid fields, invalid argument
  patterns, and unsupported grammar selections fail configuration loading
  explicitly rather than silently weakening a restriction.
- Conditions are additional filters on the existing argv-shape matcher.
  `command = ["git", "**"]` still requires one or more arguments; the grammar
  does not turn `command = ["git"]` into a prefix selector. Command-name globs
  are not part of this delivery.
- Conditions inspect parsed option/subcommand identities, not raw argv
  occurrences. Option values and operands cannot impersonate a dangerous option
  or a safe subcommand.
- `with_arguments` matches when any configured pattern matches a confidently
  placed option/subcommand. An unrelated unplaceable token does not by itself
  erase earlier evidence. Unknown arity stops collection of later evidence
  when placement becomes ambiguous; ambiguous placements provide no evidence.
- `only_arguments` matches only when parsing is complete and every parsed
  option/subcommand matches an allowed pattern. Unknown or ambiguous syntax
  prevents that rule from matching; it does not itself produce a verdict.
- When both fields are present, both must match. Empty `with_arguments`
  is invalid. Empty `only_arguments` permits only complete parses with no
  options/subcommands; recognized operands do not count as options.
- Equivalent documented option aliases share condition matches. Conditions
  do not constrain values; a permitted value-taking option permits any value
  that satisfies its grammar's structural requirements.
- Rules with neither condition preserve their existing literal/wildcard
  semantics. Grammars do not independently approve or deny commands.
  Existing valid configurations remain valid and retain their behavior;
  only the specifically named sample configuration is migrated.
- Existing per-command last-match-wins evaluation, configured-rule replacement
  of the default floor, strictest-verdict script aggregation, unmatched-command
  handling, and the final `--auto-approve-bash` override remain intact.
- Unsupported shell constructs still require approval. Assignment prefixes,
  path-qualified executable names, and wrappers are ineligible for conditional
  rules, but existing explicit unconditional rules retain their behavior.
  Shell-state commands are absent from the conservative configuration example.
- Tests demonstrate both useful approved forms and non-approvals for write/exec
  forms, option-value confusion, unknown options, and mixed scripts. No test
  executes a destructive example.
- A conservative documented configuration example uses argument allow-lists,
  retains structural protections explicitly, and never describes Bash rules
  as proof of read-only execution.
- Source implementation satisfies the repository's mandatory formatting,
  lint, and coverage gates, with affected documentation updated alongside it.

## Implementation Plan

An implementing agent must change `[ ]` to `[x]` immediately after completing
each step and rewrite this file in place. Preserve completed checkbox states
when renumbering. Resolve the open decisions before implementing dependent
steps if new evidence invalidates this agreed contract. Each completed
implementation unit includes its focused tests and the corresponding
documentation from step 9; do not defer documentation until a
later commit.

1. [x] Establish the agreed delivery contract and baseline. Open
   `src/sandbox/command.rs` first, then read `src/sandbox/rules.rs` and this
   plan's Implementation Hints. Run `cargo test --lib sandbox::command::tests`
   as the first focused baseline. Check `docs/architecture-design-records/`
   again before coding and raise any newly discovered conflict.
   Verify the existing unmatched-script and CLI-override tests remain the
   baseline rather than reimplementing their already-correct behavior.
2. [x] Add the shared argument-parsing model and one minimal grammar in a
   private file-per-module under `src/sandbox/`, registered in
   `src/sandbox.rs`. Represent confidently identified options/subcommands,
   operands, option values, and incomplete/ambiguous analysis separately.
   Use static option tables with arity, aliases, supported attached-value
   forms, terminators, and operand ordering. Represent each option's known
   aliases together so patterns match equivalent identities; never include
   values in those matches. Stop later evidence collection when unknown arity
   makes placement ambiguous, retaining only earlier confident evidence.
   Introduce command-specific hooks only when necessary. Start with `cat`
   as a small vertical slice. Test known options, missing values, `--`,
   unknown tokens, quoted literals, and bundled flags without shell execution.
3. [x] Extend `SandboxRuleConfig`, `RawSandboxRuleConfig`, and `ResolvedRule`
   in `src/sandbox/rules.rs` with the agreed condition contract. Compile
   argument patterns once, using existing glob infrastructure where its
   semantics fit. Share semantic validation with
   `src/config/profile.rs::validate_tool_sandbox_rules`; extend the existing
   `ConfigError` definition as needed. Reject conditions on non-command rules,
   unsupported commands, invalid patterns, and any disallowed combination
   or selector shape. Conditional selectors require a literal bare executable
   name in the first token with a built-in grammar; trailing argv wildcards
   retain their existing meaning. Combine both conditions with AND, reject empty
   `with_arguments`, and retain empty `only_arguments` as a meaningful value.
   Cover TOML and agent-type configuration loading, unknown-key errors, and
   malformed condition patterns in owner-local rule/configuration tests.
4. [x] Change `SandboxContext::with_configured_rules` and `with_tool_rules` to
   return `Result` and propagate construction errors rather than skipping
   invalid entries. Carry that result through
   `src/agent/tools.rs::default_registry`, `src/agent_factory.rs`, skill-policy
   construction in `src/skills/policy.rs`, and directly dependent callers.
   Cover malformed direct-construction vetoes following an unconditional allow.
   Valid existing rules must keep their behavior; test callers unwrap known
   valid fixtures, while production callers propagate errors explicitly.
5. [x] Integrate conditional matching into
   `src/sandbox/command.rs::evaluate_command`. Reuse one parsed result per
   command across conditional rules; do not parse commands for unconditional
   rules. Implement presence matching and complete allow-list matching as
   distinct operations, independent of verdict. Require the existing
   `CommandGlobPattern` to match before applying either condition.
   Validate grammar selection from the selector's literal first token;
   retain the existing final `*`/`**` behavior. Test last-match precedence,
   an unparsed wildcard followed by a conditional veto, fully unmatched
   scripts, mixed approved/unmatched scripts, unanalysable statements,
   configured default-floor replacement, and final CLI-override precedence.
6. [x] Apply the executable/assignment/wrapper boundary at conditional-rule
   eligibility without changing extraction of existing supported forms. Do not unwrap
   wrappers or expand shell variables during analysis. Add regressions for
   `FOO=bar cat file`, `./cat file`, `/tmp/cat file`, `env cat file`,
   `command cat file`, `sudo cat file`, `time cat file`, shell-state commands,
   and scripts combining those with an approved command. Verify explicit
   unconditional rules and `--auto-approve-bash` behave exactly as agreed.
7. [x] Extend the agreed built-in grammar roster one command at a time, with tests
   and documentation per command: `cat`, `ls`, `head`, `tail`, `wc`, `grep`,
   `rg`, `sort`, `find`, and `git status|log|diff|show`. Keep `sed`/`awk`
   unsupported by conditional rules. Record tool/OS
   documentation provenance and review date beside each grammar. Recognize
   dangerous options sufficiently to support veto rules without conflating
   parseability with an approving policy. Cover macOS BSD/GNU differences,
   option values, long `=` forms, short bundles, operands preceding options,
   and subcommand-specific options only where actually supported.
8. [x] Strengthen owner-local tests in `src/sandbox/command/tests.rs`,
   `src/sandbox/rules/tests.rs`, the extractor tests, and grammar-local tests.
   Use a small independently authored bypass matrix covering `find -exec`,
   `-execdir`, `-delete`, `-fprint*`, `sort -o`, `rg --pre`, Git execution/output
   controls, and unsupported `sed -i`/`awk` programs. Include `find -name -exec`
   as option-value confusion, operands named `status` or `./-exec`, unknown
   options before known dangerous options, missing values, and ambiguous
   bundles. Verify `find . -weird -exec` does not create confident `-exec`
   evidence after an unknown-arity region, while confidently recognized
   earlier options remain available for presence matches. Add bounded
   generated-input cases proving no panics and no
   complete allow-list match for unplaceable input. Reuse existing tools;
   introduce a property-testing dependency only if justified. Extend
   `src/agent/tools/tests/registry.rs` and profile/agent-type tests for actual
   conditional-rule wiring rather than duplicating parser cases. Verify
   representative existing unconditional configurations preserve their
   results, including grammarless broad approvals.
9. [x] Update `docs/configuration.md` with field syntax, selector semantics,
   empty/composed-condition behavior, glob matching, supported grammars,
   partial-versus-complete parsing, precedence, and migration examples.
   Update `docs/features.md` to distinguish heuristic approval from enforced
   isolation; update `docs/implementation-notes.md` with parser ownership,
   validation, and matching mechanics. Update `docs/ideas/bash-approval.md`
   to distinguish delivered behavior from deferred research when implementing
   the feature. Keep README/domain terminology unchanged unless the agreed
   behavior makes their current descriptions inaccurate. Do not create new
   documentation or ADR files without user approval. Migrate the sample
   `.ylva/profiles/other/config.toml` to the documented conservative rules,
   retain exact `true`/`false` approvals and all structural protections, and
   leave unrelated profile settings unchanged. Retain its grammarless broad
   `nl`, `cut`, `tr`, `echo`, `printf`, and `test` approvals as explicitly
   documented user-owned opt-outs, not as argument-checked rules. Do not
   migrate any other existing user configuration automatically.
10. [x] Verify the completed source change using `make format lint` followed
   by the single mandatory full test/coverage gate, `make test-coverage`,
   maintaining at least 80% aggregate line coverage. Run `make audit` only
   if dependencies changed. Verify changed Markdown with `rumdl check`.
   Do not run `make test` as a duplicate mandatory gate. Review the diff for
   unintended execution, configuration, or path-policy changes, finish the
   checklist, and leave staging/committing to a separately authorized action.

Execution verification: `make format lint`, `make test-coverage` and changed-file
`rumdl check` passed. Aggregate line coverage was 88.35%. Dependencies were
unchanged, so no dependency audit was required. The utility option definitions
use the user-selected module name `utility_options.rs`. No files were staged
or committed.

## Proposed Improvements

- Use a deliberately small initial grammar roster and add further commands
  only when repeated approval friction demonstrates value. Treat incomplete
  support as reduced convenience, not a reason to guess command semantics.
- Prefer examples with explicit option/subcommand allow-lists rather than
  shipping broad approving wildcards plus a growing dangerous-option deny-list.
  Known dangerous options belong in grammar metadata for recognition, not in
  a hidden grammar-owned verdict engine.

## Implementation Hints

- Grilling decision: the delivery is argument-conditioned rules and built-in
  grammars only, not Bash operand path scoping or the full multi-layer roadmap.
- Grilling decision: unconditional rules remain deliberate user-owned opt-outs.
  Assignment-prefix, executable-path, and wrapper restrictions apply only to
  conditional-rule eligibility; structural rule ordering and the final
  `--auto-approve-bash` override remain unchanged.
- Grilling decision: initial grammars cover `cat`, `ls`, `head`, `tail`, `wc`,
  `grep`, `rg`, `sort`, `find`, and `git status|log|diff|show`.
  `sed` and `awk` do not receive conditional-rule grammars.
- Grilling decision: grammar checks engage only when `with_arguments` or
  `only_arguments` is present. An unconditional rule is an intentional
  user-owned choice and retains former matching behavior.
- Grilling decision: implementation includes documented example rules and
  migration of `.ylva/profiles/other/config.toml`, not documentation alone.
- Grilling decision: existing argv-shape matching remains authoritative.
  Conditions add filters; they do not change a literal selector into a prefix.
  The idea's single-name conditional examples need explicit argv tails when
  matching commands with arguments. Command-name globs are excluded.
- Grilling decision: both conditions combine with AND. Empty `with_arguments`
  is a configuration error; empty `only_arguments` allows recognized operands
  but no options/subcommands.
- Grilling decision: invalid direct-construction rules return an error through
  a fallible context-construction API. They are not skipped and do not become
  an approval-required fallback. The CLI override applies only after a valid
  policy has been constructed, not as recovery from invalid configuration.
- Grilling decision: grammars cover a documented conservative BSD/macOS and
  GNU/Linux subset, plus documented Git/ripgrep versions. Cross-variant
  conflicting forms are incomplete rather than guessed. Grammar selection
  does not execute the command or inspect `--help` through `PATH`.
- Grilling decision: equivalent documented short/long aliases match the same
  conditions, including wildcard patterns. Option values are structurally
  consumed/validated but excluded from condition matching.
- Grilling decision: partial scans preserve confident evidence before
  ambiguity but do not collect later evidence after unknown arity makes its
  placement uncertain. This supersedes the research idea's expectation that
  a veto necessarily recognizes `-exec` after an unknown `-weird`.
- Grilling decision: existing valid configurations remain valid and work the
  same. Only `.ylva/profiles/other/config.toml` is migrated. That sample retains
  broad grammarless `nl`, `cut`, `tr`, `echo`, `printf`, and `test` approvals,
  documented as explicit unconditional opt-outs; exact `true`/`false` approvals
  also remain.
- Research source: `docs/ideas/bash-approval.md`. Its ten layers are a menu
  of options, not an existing feature contract. External product behavior and
  OS-sandbox availability in that document are explicitly unverified; this
  plan does not depend on either being current.
- `src/sandbox/bash_command_extractor.rs` owns `ArgvToken`,
  `ExtractedCommand`, `ExtractedStatement`, and `ExtractionResult`. It uses
  existing `tree-sitter`/`tree-sitter-bash` 0.25 dependencies. Only `list`,
  `pipeline`, and `command` nodes are traversed; unsupported constructs become
  `Unanalysable`, while a script parse failure returns `ParseFailed`.
- The extractor currently records assignment prefixes and backgrounding as
  flags, not as universally unanalysable statements. It retains unquoted
  tilde text literally; path scoping cannot safely use that text as an
  already-resolved pathname. Tilde expansion is not required for the agreed
  first delivery because it does not add Bash operand path checks.
- `src/sandbox/rules.rs::CommandGlobPattern` permits only literal prefixes
  and a final bare `*` or `**`. `*` matches exactly one extra token and
  abstains on an unquoted glob/brace token; `**` matches one or more extra
  tokens without that restriction. Neither means zero or more tokens.
- `RawSandboxRuleConfig` denies unknown fields and its conversion selects
  exactly one of `rule`, `pattern`, or `command`. The public rule enum is
  constructed directly in multiple test suites; new fields affect those
  construction sites, not only deserialization.
- `ResolvedRule::from_config` returns typed errors. Before this implementation,
  `SandboxContext::with_configured_rules` logged and skipped invalid compiled
  rules. A skipped conditional veto after an unconditional allow could weaken
  policy; construction now returns an error through the registry, factory and
  skill policy instead.
- `src/config/profile.rs::validate_tool_sandbox_rules` rejects misplaced
  rule kinds and command wildcards at load time. Profile tests live in
  `src/config/profile/tests/sandbox.rs` and unknown-key tests in
  `src/config/profile/tests/unknown_keys.rs`. Agent-type files share the
  tool-rule configuration shape.
- `src/sandbox/command.rs` already requires every command in a script to
  receive auto-approval for structural auto-approval. Its aggregation orders
  denial above approval-required above auto-approval. A wholly unmatched
  script abstains; `BashTool::needs_approval` maps abstention to user approval.
- Without configured Bash rules, backgrounding is denied and assignment
  prefixes/unquoted glob or brace expansion require approval. A configured
  rule list replaces that floor completely. Catch-all CLI rules are applied
  after the structural result, even after denial.
- Ylva currently ships no built-in command-name auto-approval list. The
  broad `cat`/`grep`/`ls` and similar rules are in the workspace-local
  `.ylva/profiles/other/config.toml`; that example re-adds structural rules
  after its wildcards. Removing a supposed shipped list is not an applicable
  implementation task.
- `src/agent/tools/bash.rs` delegates approval to the sandbox and owns the
  tool fallback. Process execution lives under `src/agent/tools/bash/`;
  execution changes are not needed for argument-conditioned rules.
- `docs/configuration.md` is the approval-rule reference;
  `docs/features.md` describes user-visible behavior;
  `docs/implementation-notes.md` describes contributor-facing internals.
  `docs/architecture-design-records/` contained no records when researched.
- Owner-local parser/policy tests belong under `src/`; public integration
  tests use exports from `src/lib.rs`, not `src/main.rs`. New Rust files
  require the existing copyright/Apache-2.0 header, descriptive identifiers,
  and file-per-module layout. Fallible parsing/configuration uses explicit
  results, not panics or success-shaped defaults.
- Tests involving tracing or process-global cwd follow
  `docs/known-test-issues.md` subscriber-lock rules. Pure command parsing
  requires neither shell subprocesses nor global environment mutation.
- Planning itself changes Markdown only; source build/coverage gates above
  apply to eventual implementation, not creation of this document.
