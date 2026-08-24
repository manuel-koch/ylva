# Resolving Provider

Each provider has a method to get all envvars that it can use.

Provider registry has a method to get all envvars that any provider may use.

Provider uses a map of envvar name/value to find a provider that matches best.

```text
# PSEUDO CODE

Provider::get_usable_envvar_names() -> Vec[String]

ProviderRegistry::get_envvar_names_for_auto_detection() -> Vec[String]

ProviderRegistry::resolve_provider(name:Option<String>,envvars:Dict[str,str]) -> Provider
```

```toml
[agents.default]
engine = "default"
```

```toml
[engines."ollama/foo"]
provider = "ollama"
model = "foo"
```

```toml
[providers.ollama]
base_url = ${OLLAMA_BASE_URL:-http://localhost:11434}
api_key = ${OLLAMA_API_KEY}

[providers.openrouter]
base_url = ${OPENROUTER_BASE_URL:-https://openrouter.ai/api/v1}
api_key = ${OPENROUTER_API_KEY}
```
