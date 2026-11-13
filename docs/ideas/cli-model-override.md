# One-off CLI model override

> **Superseded by the approved `--engine <name>` approach.** The old
> `--model <id>` design is not an implemented/supported feature contract;
> its prototype and corrective tasks are replaced, not completed. See the
> [implementation plan](../plans/cli-options-plan.md) and
> [CLI engine selection](../cli-options.md#engine-selection).
> The replacement selects complete configured engines, not raw model/provider overrides.

## Original motivation

Allow one-shot headless/TUI runs to choose a different model without editing
profile files. Profiles and named engines remain the repeatable source of truth.

## Approved replacement

Select an existing explicit `[engines.<name>]` entry with its complete resolved
connection, model, reasoning, context, output limit and other settings. After
resolving the root agent type, change only that type's in-memory `engine`
mapping; do not rewrite engine definitions, `default_engine`, personas,
permissions or profile files. Unknown, empty or whitespace-only names exit `2`
before provider requests or session creation/resume/clone writes.

The chat-only `--engine` flag is non-repeatable, works for fresh/resume/clone
launches and is rejected with every subcommand, including `models`, `connect`
and `trust`. Old `--model` and `--provider` flags are removed, not aliases.
Explicit engine selection is retained in shell-safe resume hints.

Normal named-engine validation, hydration and catalog advisories apply; the
old special blocking catalog check and reasoning-reset questions do not apply.
Explicit compaction and utility engines remain fixed, unset roles retain their
existing rules, and subagents retain existing sharing behavior. There are no
remaining open design questions; the plan records implementation, concurrent
all-connection listing fixes, documentation and verification tasks.

## Related

- [Provider override (also superseded)](./cli-provider-override.md)
- [Engine configuration](../configuration.md#engine-configuration)
- [Models listing contract](../cli-options.md#models-command)
