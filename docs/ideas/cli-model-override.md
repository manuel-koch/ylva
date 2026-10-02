# One-off CLI model override

Deferred from blueprint finalization. Headless runs currently select a model
through the active profile and its named engines. The CLI has no `--model`
flag.

## Goal

Allow a one-shot headless run to use a different model without editing the
profile, while leaving configured engines unchanged for later runs.

## Open questions

- Decide whether the override applies to the profile's default engine or
  requires a provider/connection to be selected as well.
- Define how the context length, reasoning effort, and model capabilities are
  resolved for a model that is not in the profile's configured engine.
- Decide how `--model` combines with `--profile`, `--provider`, and
  `--conversation-id`.
- Ensure a resumed run records the actual provider/model in its
  `model_changed` history.

## Related

- [`cli-provider-override.md`](./cli-provider-override.md)
- [`docs/configuration.md`](../configuration.md#engine-configuration)
- [`docs/cli-options.md`](../cli-options.md)
