# Provider Model Favorites

The `/models` picker is implemented and lists models from configured provider
connections. It does not yet group or rank favorites. This remains a separate,
deferred picker enhancement.

We could scan thru recent sessions on startup and find out
what provider/models where used in the past
and put them in a new "favorite" section in the `/models` slash-command
popup.
Sorting the "favorites" by the number of conversation-rounds it was used.
I.e. one time trying a model has less prio than using a model in multiple
session exclusively.

When reading past sessions we need to ensure that we don't reuse a provider/engine
that is currently not available/configured anymore !

For example define favorites on the provider connection
configuration:

```toml
[provider_connections.openrouter]
provider = "openrouter"
....
favorite_models = [
    "anthropic/claude-haiku-5.5",
    "openai/gpt-6-luna",
]
```
