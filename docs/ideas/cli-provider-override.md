# One-off CLI provider override

> **Superseded by the approved `--engine <name>` approach.** The old
> `--provider <name>` design and `models` provider filter are not an
> implemented/supported feature contract; their prototype and corrective tasks
> are replaced, not completed. See the
> [implementation plan](../plans/cli-options-plan.md) and
> [CLI engine selection](../cli-options.md#engine-selection).
> The replacement selects complete configured engines, not raw model/provider overrides.

## Original motivation

Choose a provider connection for a one-shot chat run without editing the
selected profile. Keep credentials and endpoints in the profile's existing
connection configuration rather than supplying them through new CLI flags.

## Approved replacement

`--engine <name>` selects an existing explicit `[engines.<name>]` entry's
complete resolved configuration, including its provider connection and model,
for the resolved root agent type through its in-memory `engine` mapping.
It does not rewrite engine definitions, `default_engine`, other type mappings,
personas, permissions or profile files. Unknown, empty or whitespace-only
names exit `2` before provider requests or fresh/resumed/cloned session writes.

The chat-only flag is non-repeatable, works for headless/TUI fresh/resume/clone
launches and is rejected with every subcommand, including `models`, `connect`
and `trust`. Removed `--provider` and `--model` flags are not aliases. Explicit
engine selection remains in shell-safe resume hints. Normal named-engine
catalog behavior applies without a special CLI fetch/check. Fixed compaction
and utility engines, unset-role rules and existing subagent sharing are unchanged.

The independent `ylva models` command lists **all** connections, including those
unused by explicit engines, with no provider filter. Concurrent fixes use normal
trust/environment loading, allowlists, scores, favorites-first deterministic
ordering and safe grouped/aligned output without selection or preference writes.
There are no remaining open design questions; implementation and verification
are tracked in the plan.

## Related

- [Model override (also superseded)](./cli-model-override.md)
- [Engine configuration](../configuration.md#engine-configuration)
- [Models listing contract](../cli-options.md#models-command)
