# Ylva agent-harness configuration

Reference for Ylva's configuration files.

## Profile file

One flat TOML file describes a runtime profile:

```text
~/.ylva/profiles/<profile-name>/config.toml
```

An optional `~/.ylva/profiles/<profile-name>/.env` file supplies provider
environment variables and values for `${ENV_VAR}` interpolation in
`config.toml`. Profiles without explicit `[engines.*]` or
`[provider_connections.*]` tables use it for provider discovery. Already
exported process variables take precedence over values from the profile file.

- `--profile <name>` selects the profile directory explicitly; without it,
  the implicit `default` profile is used (see below for how a missing file
  is handled differently in each case).
- `~/.ylva` is used verbatim from the `YLVA_HOME` environment variable when
  it is set and non-empty, otherwise it is
  `${HOME}/.ylva` — see
  [persistence](./persistence.md#relocating-the-root-with-ylva_home). The same
  root holds the chat store.
- A **missing** file's behavior depends on how the profile was selected:
  - **Explicit** (`--profile <name>` given): a hard error (exit code `2`)
    naming the missing profile — the user asked for that specific profile,
    so it is never silently substituted.
  - **Implicit** (no `--profile` flag; the `default` profile): not an error —
    Ylva falls back to an in-code default profile (one `default` agent type
    that may spawn children, the dark theme, and no configured engines — the
    backend then comes from the environment variables in
    `docs/cli-options.md`), so it still runs out of the box without ever
    writing a config file.
- An **unreadable or malformed** file is always a hard error (exit code `2`)
  naming the file and the parse problem, explicit or implicit; a config the
  user did write is never silently ignored.
- Profile validation errors (for example, an engine reference that has no
  corresponding `[engines.*]` table) name the selected profile and the full
  `config.toml` path together with the validation detail.
- Unknown keys are ignored, so a file written for a newer Ylva still loads.

Example:

```toml
default_engine = "ollama/local"
theme = "dark"

[provider_connections.ollama-local]
provider = "ollama"
api_base_url = "http://localhost:11434"

[provider_connections.ollama-cloud]
provider = "ollama"
api_base_url = "https://ollama.com"
api_key = "${OLLAMA_CLOUD_API_KEY}"

[engines."ollama/local"]
provider_connection = "ollama-local"
model = "gemma4:e4b-mlx"

[engines."ollama/cloud"]
provider_connection = "ollama-cloud"
model = "gemma4:31b-cloud"
temperature = 0.2

[agents.default]
engine = "ollama/local"
can_spawn = true
```

| Key | Type | Description |
|---|---|---|
| `default_engine` | `String` | Name of the `[engines.*]` entry used when an agent type names none. Defaults to `default`. |
| `compact_engine` | `Option<String>` | Engine used for summarisation tasks; resolves to `default_engine` when absent. Phase 9 wires this at runtime. |
| `default_model` | `Option<String>` | Model filled into every `[engines.*]` entry that names none of its own. |
| `theme` | `dark` \| `light` | Syntax-highlighting theme. Defaults to `dark`. See [features — rendering](./features.md#tui-architecture). |
| `keybindings` | table of strings | **Reserved.** Accepted and validated so a profile carrying one loads, but Phase 7 ships the fixed key map in `docs/keybindings.md` and does not apply these entries yet. |
| `[provider_connections.<name>]` | table | A named provider connection. Its `provider` field selects the implementation kind; its endpoint and credentials belong to this connection. |
| `[engines.<name>]` | table | A named engine — see `EngineConfig` below. |
| `[agents.<type>]` | table | Per-agent-type settings — see `AgentConfig` below. |

When at least one `[engines.*]` table is present, those engines are what
`AgentFactory` builds. Every named provider connection is also made available
to `/models`, including connections that are not referenced by an engine. A
profile with provider connections but no engines gets one implicit engine per
connection. Only a profile with neither explicit engines nor provider
connections uses environment-based provider discovery. The first configured
engine is initially active, and `/models` queries each named provider
connection once before displaying `provider_connection/model` choices. The
profile's `default_engine` is initially active.
Referencing an engine name that no table defines (from `default_engine`,
`compact_engine`, or an agent override) is a hard error rather than a silent
fallback.

An engine's `provider_connection` refers to the map key under
`[provider_connections.*]`; the connection's `provider` field is the
implementation kind. This allows multiple Ollama connections with different
URLs and credentials. Engine-level `api_base_url` and `api_key` values, when
present, override the connection values. A missing connection API key may be
filled from the matching provider environment variable; an inline value
remains authoritative.

`${ENV_VAR}` references are expanded in every TOML string value after the
profile `.env` and process environment are merged. Missing variables and
malformed references are hard startup errors; expanded secret values are never
logged.

**WIP:** The three-layer project/home/embedded fallback chain, `SOUL.md`, and
`agents/*.md` persona sourcing are **not** implemented — they remain Phase 8
work.

## Engine configuration (`EngineConfig`)

`EngineConfig` is the single source of truth for the sampling and model
parameters tied to a named provider connection. Each `[engines.<name>]` table
in the profile file deserializes into one `EngineConfig`, and
`AgentFactory` resolves its `provider_connection` against the profile's
connection map before building the provider.

At runtime, `EngineImpl` and providers share an `EngineRuntimeConfig` snapshot
for mutable request settings (`model`, `temperature`, and `context_length`).
Connection settings remain fixed for the lifetime of the engine; a runtime
model switch affects subsequent requests without rebuilding the provider.

| Field | Type | Description |
|---|---|---|
| `provider_connection` | `String` | Name of the connection under `[provider_connections.*]`. The connection's `provider` field must be `"mock"`, `"ollama"`, or `"openrouter"`. Ollama uses the server origin as `api_base_url` and derives both `/v1` chat and `/api` catalog endpoints; OpenRouter uses its origin as `api_base_url` and derives `/api/v1` for chat and model discovery. |
| `model` | `String` | Model identifier passed to the provider. May be omitted, in which case the profile's `default_model` fills it in. |
| `temperature` | `Option<f32>` | Sampling temperature. `None` uses the model's own default. |
| `context_length` | `usize` | Maximum token context window (default `128 000`). Phase 9 reads this to drive compaction budgeting. |
| `api_base_url` | `Option<String>` | Optional engine-level override for the connection's base URL. |
| `api_key` | `Option<String>` | Optional engine-level override for the connection's API key. |

## Profile configuration (`ProfileConfig`)

A profile groups engine selection and per-agent settings for a runtime session.

| Field | Type | Description |
|---|---|---|
| `default_engine` | `String` | Name of the engine used when no agent-level override is present (required). |
| `compact_engine` | `Option<String>` | Engine used for summarisation tasks. Resolves to `default_engine` when absent. Phase 9 wires this at runtime. |
| `provider_connections` | `HashMap<String, ProviderConnectionConfig>` | Named provider connections, keyed by connection name. |
| `engines` | `HashMap<String, EngineConfig>` | Named model/runtime configurations, keyed by engine name. |
| `agents` | `HashMap<String, AgentConfig>` | Per-agent-type settings, keyed by agent-type name. |

### Per-agent configuration (`AgentConfig`)

| Field | Type | Description |
|---|---|---|
| `engine` | `Option<String>` | Override the engine for this agent type. When absent, `ProfileConfig::default_engine` is used. |
| `allowed_tools` | `Option<Vec<String>>` | Restrict which tool names this agent may invoke. `None` falls back to the caller's flat default tool list (`AgentFactory::with_allowed_tools`, or `DEFAULT_CLI_TOOLS`/`DEFAULT_TUI_TOOLS` when unset). `Some(names)` is honored per `agent_type` by `AgentFactory::build`, so a distinct `agent_type` — including one used only for spawned children — can be restricted to a genuinely different tool set than the root or any other agent type. |
| `timeout_secs` | `Option<u64>` | Maximum wall-clock seconds before the agent is cancelled. `None` means no timeout. |
| `persona` | `Option<String>` | Optional persona instruction prepended to the system prompt. |
| `can_spawn` | `bool` | When `true` (and this is a `Role::Root` agent), `AgentFactory::create` registers the `spawn_subagent` tool and wires a `ChildAgentManager` so this agent may delegate self-contained tasks to a concurrently-running `Role::Child` agent (one level deep; children cannot spawn further children). Use `--auto-approve-spawn` to skip the human-approval prompt for `spawn_subagent` calls. See "Child-agent delegation" in `docs/features.md`. |

## Sandbox (`Sandbox` / `SandboxContext`)

`Sandbox` (`src/sandbox.rs`) holds the workspace root (canonicalized cwd at
startup) that path-based tool-approval rules are evaluated against.
`SandboxContext::check_path` normalizes a candidate path against that root
(collapsing `..` without touching the filesystem, resolving symlinked
prefixes such as macOS's `/var` → `/private/var`) and returns either an
explicit verdict or `None` (abstain, letting the calling tool apply its own
default).

Phase 6 hardcodes two rules, evaluated in this order, before any per-tool
named glob rule (last match wins):

| Rule | Verdict | Notes |
|---|---|---|
| `outside_workspace` | `AutoDenied` | Any path that normalizes outside the workspace root. |
| `dot_path` | `AutoDenied` | Any path whose relative-to-root form has a component starting with `.` (dot-files/dot-directories). |

**WIP:** These two rules are not yet configurable — Phase 8 is expected to load
per-tool `[tool.*]` rule lists (including `outside_workspace`/`dot_path`
overrides) from the layered TOML config; until then the defaults above apply to
every tool uniformly. `search`'s `path: None` (cwd, never leaves the
workspace by construction) skips the sandbox check entirely and is
auto-approved; when a `path` is given, it goes through the same
`check_path`/`dot_path`/`outside_workspace` evaluation as `read`, `write`,
`edit`, and `glob`.
