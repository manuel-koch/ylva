# Ylva agent-harness configuration

Reference for Ylva's configuration files.

## Engine configuration (`EngineConfig`)

`EngineConfig` is the single source of truth for sampling parameters tied to a
named backend. In Phase 5 the fields are advisory: `EngineImpl` delegates
directly to the wrapped `Provider`, which already bakes these settings in at
construction time. Phase 7 will wire them into provider construction from a
TOML config file.

| Field | Type | Description |
|---|---|---|
| `provider` | `String` | Provider name (`"ollama"`, `"openrouter"`, `"mock"`). |
| `model` | `String` | Model identifier passed to the provider. |
| `temperature` | `Option<f32>` | Sampling temperature. `None` uses the model's own default. |
| `context_length` | `usize` | Maximum token context window (default `128 000`). |
| `api_base_url` | `Option<String>` | Override for the provider's base URL. |
| `api_key` | `Option<String>` | API key for authenticated providers. |

## Profile configuration (`ProfileConfig`)

A profile groups engine selection and per-agent settings for a runtime session.

| Field | Type | Description |
|---|---|---|
| `default_engine` | `String` | Name of the engine used when no agent-level override is present (required). |
| `compact_engine` | `Option<String>` | Engine used for summarisation tasks. Resolves to `default_engine` when absent. Phase 7 wires this at runtime. |
| `agents` | `HashMap<String, AgentConfig>` | Per-agent-type settings, keyed by agent-type name. |

### Per-agent configuration (`AgentConfig`)

| Field | Type | Description |
|---|---|---|
| `engine` | `Option<String>` | Override the engine for this agent type. When absent, `ProfileConfig::default_engine` is used. |
| `allowed_tools` | `Option<Vec<String>>` | Restrict which tool names this agent may invoke. `None` falls back to the caller's flat default tool list (`AgentFactory::with_allowed_tools`, or `DEFAULT_CLI_TOOLS`/`DEFAULT_TUI_TOOLS` when unset). `Some(names)` is honored per `agent_type` by `AgentFactory::build`, so a distinct `agent_type` — including one used only for spawned children — can be restricted to a genuinely different tool set than the root or any other agent type. |
| `timeout_secs` | `Option<u64>` | Maximum wall-clock seconds before the agent is cancelled. `None` means no timeout. |
| `persona` | `Option<String>` | Optional persona instruction prepended to the system prompt. |
| `can_spawn` | `bool` | When `true` (and this is a `Role::Root` agent), `AgentFactory::create` registers the `spawn_subagent` tool and wires a `ChildAgentManager` so this agent may delegate self-contained tasks to a concurrently-running `Role::Child` agent (one level deep; children cannot spawn further children). Use `--auto-approve-spawn` to skip the human-approval prompt for `spawn_subagent` calls. See "Child-agent delegation" in `docs/features.md`. |
