# Provider Model Favorites

Manual provider-connection favorites and allowed-model policies are implemented;
see [favorite and allowed models](../configuration.md#favorite-and-allowed-models)
and the [`/models` picker](../features.md#slash-commands).

Favorites are exact model IDs stored in the active profile's provider-connection
table. Ctrl+F marks or unmarks the highlighted row in either picker context.
Allowed-model patterns restrict which models can be selected and used. Discovery
may hide configured favorites that are not currently present in a provider catalog.

Session scanning, inferred favorites, and usage-based ranking are not part of this
feature and remain out of scope. The historical proposal below is retained only
as context, not as implementation guidance.

For example, provider connections can define favorites and allowed patterns:

```toml
[provider_connections.openrouter]
provider = "openrouter"
api_base_url = "https://openrouter.ai"
api_key = "${OPENROUTER_API_KEY}"
favorite_models = [
    "anthropic/claude-haiku-5.5",
    "openai/gpt-6-luna",
]
allowed_models = [
    "anthropic/*haiku*",
    "openai/gpt-6*",
]
```
