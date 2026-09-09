# Provider Resolution

Provider resolution is implemented by
[`ProviderRegistry`](../../src/llm/provider_registry.rs).

The registry has separate paths for explicit provider-connection
configuration and environment-based auto-detection:

- `ProviderRegistry::from_provider_config` validates the implementation kind
  stored in a named connection and requires `api_base_url` for
  OpenAI-compatible providers. Supported implementation names are `mock`,
  `ollama`, and `openrouter`.
- An engine's `provider_connection` names the connection map entry; it does
  not contain the implementation kind itself. Engine-level endpoint and
  credential values override the connection.
- `ProviderEnv::from_process_env_and_dotenv` loads the supported provider
  variables from the selected profile's `.env` and the process environment,
  with process values taking precedence.
- `ProviderRegistry::all_from_environment` discovers every configured
  provider. Ollama is returned first when `OLLAMA_BASE_URL` is non-empty;
  OpenRouter is also returned when any non-empty OpenRouter setting is
  present; otherwise the mock provider is selected.
- `ProviderRegistry::build` and
  `ProviderRegistry::provider_from_environment` construct the selected
  implementation. Ollama and OpenRouter use dedicated providers that share
  the OpenAI-compatible streaming transport; Ollama derives `/v1` chat and
  native `/api/tags`/`/api/show` catalog paths from its server-origin base
  URL, while OpenRouter queries its metadata-rich `/models` endpoint.

The environment path supplies the existing Ollama and OpenRouter defaults and
builds an `EngineRouter` when more than one provider connection is configured.
The `/models` picker queries each named connection once and selects a
provider-connection/model pair. Explicit profile values remain authoritative
and do not inherit environment defaults, except that a missing connection API
key is filled from the matching provider environment variable. Profile TOML
strings support `${ENV_VAR}` interpolation using the same merged environment.
