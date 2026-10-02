# One-off CLI provider override

Deferred from blueprint finalization. Provider selection currently comes from
the selected profile or environment-based discovery. The CLI has no
`--provider` flag.

## Goal

Allow a headless run to choose a provider connection without editing the
profile. The profile and named-engine path should remain the repeatable source
of truth.

## Open questions

- Decide whether the flag names a provider implementation (`ollama`,
  `openrouter`) or a named provider connection from the profile.
- Define how credentials and endpoint URLs are supplied. Do not log secrets.
- Decide how `--provider` combines with `--profile`, `--model`, and named
  engines.
- Validate the requested provider before opening a session.

## Related

- [`cli-model-override.md`](./cli-model-override.md)
- [`docs/configuration.md`](../configuration.md#engine-configuration)
- [`docs/cli-options.md`](../cli-options.md)
