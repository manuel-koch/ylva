# Ylva agent-harness configuration

Reference for Ylva's configuration files.

## Profile file

One TOML file describes a runtime profile. Ylva resolves that file through a
three-layer, per-file fallback chain:

```text
<workspace-root>/.ylva/profiles/<profile-name>/config.toml
    -> ~/.ylva/profiles/<profile-name>/config.toml
    -> code-owned defaults for the implicit `default` profile
```

The workspace root is discovered by walking up from the current working
directory to the nearest ancestor containing either a `.ylva/` directory or a
`.git` marker. A `.ylva/` directory defines a workspace-local configuration
layer; `.git` is the repository-root fallback. The global Ylva home is not
treated as a workspace marker when it is an ancestor of the current directory,
so projects under the user's home directory do not accidentally write to or
load from the global configuration layer. When no marker exists, there is no
workspace layer. The highest layer that defines the requested file replaces
that file entirely; lower layers are not merged into it.

An optional `~/.ylva/profiles/<profile-name>/.env` file supplies provider
environment variables and values for `${ENV_VAR}` interpolation in
`config.toml`. When a workspace root is discovered, the matching
`<workspace-root>/.ylva/profiles/<profile-name>/.env` is loaded as well.
Project values override home-profile values, and exported process values still
override both. Profiles without explicit `[engines.*]` or
`[provider_connections.*]` tables use it for provider discovery. Already
exported process variables take precedence over values from either profile
environment file.

- `--profile <name>` selects the profile directory explicitly; without it,
  the implicit `default` profile is used (see below for how a missing file
  is handled differently in each case).
- `~/.ylva` is used verbatim from the `YLVA_HOME` environment variable when
  it is set and non-empty, otherwise it is
  `${HOME}/.ylva` — see
  [persistence](./persistence.md#relocating-the-ylva-home-with-ylva_home). The
  same home holds the chat store.
- A **missing** file's behavior depends on how the profile was selected:
  - **Explicit** (`--profile <name>` given): a hard error (exit code `2`)
    naming the missing profile when no project or home file exists — the user
    asked for that specific profile, so it is never silently substituted.
  - **Implicit** (no `--profile` flag; the `default` profile): not an error —
    `ProfileConfig::default()` supplies one `default` agent type that may
    spawn children, the dark theme, and no configured engines. The backend
    then comes from the environment variables in `docs/cli-options.md`, so
    Ylva runs out of the box without writing a config file.
- An **unreadable or malformed** file is always a hard error (exit code `2`)
  naming the file and the parse problem, explicit or implicit; a config the
  user did write is never silently ignored.
- Profile validation errors (for example, an engine reference that has no
  corresponding `[engines.*]` table) name the selected profile and the full
  path of the resolved `config.toml` together with the validation detail.
- Unknown keys are ignored, so a file written for a newer Ylva still loads.

Example:

```toml
default_engine = "ollama/local"
theme = "dark"
input_history_limit = 50

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

# Implicit default agent config
# [agents.default]
# engine = "default"
# can_spawn = true

[agents.local-discovery]
description = "Read-only repository exploration: locate relevant files and symbols without modifying files."
engine = "ollama/local"
```

| Key | Type | Description |
|---|---|---|
| `default_engine` | `String` | Name of the `[engines.*]` entry used when an agent type names none. Defaults to `default`. |
| `compact_engine` | `Option<String>` | Engine used for summarisation tasks; resolves to `default_engine` when absent. Phase 10 wires this at runtime. |
| `utility_engine` | `Option<String>` | Isolated engine used to generate session titles from the first user message and completed assistant reply; resolves to `default_engine` when absent. |
| `default_model` | `Option<String>` | Model filled into every `[engines.*]` entry that names none of its own. |
| `theme` | `dark` \| `light` | Syntax-highlighting theme. Defaults to `dark`. See [features — rendering](./features.md#tui-architecture). |
| `input_history_limit` | `usize` | Maximum number of explicit messages from prior top-level sessions loaded into TUI input history. Defaults to `50`; `0` disables prior-session history. |
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
`compact_engine`, `utility_engine`, or an agent override) is a hard error rather
than a silent fallback.

When the root agent changes its active provider connection or model through
`/models`, a child spawned afterwards that resolves the same named engine uses
that active engine. A child whose agent configuration names a different engine
uses that named engine instead of inheriting the root's active selection.

An engine's `provider_connection` refers to the map key under
`[provider_connections.*]`; the connection's `provider` field is the
implementation kind. This allows multiple Ollama connections with different
URLs and credentials. Engine-level `api_base_url` and `api_key` values, when
present, override the connection values. A missing connection API key may be
filled from the matching provider environment variable; an inline value
remains authoritative.

`${ENV_VAR}` references are expanded in every TOML string value after the
profile `.env` and process environment are merged. Numeric and boolean fields
also accept expanded string values and parse them according to their field
type; for example, `temperature = "${TEMPERATURE}"`,
`timeout_secs = "${TIMEOUT_SECONDS}"`, and
`can_spawn = "${CAN_SPAWN}"`. Missing variables, malformed references, and
values that cannot be parsed as the target type are hard startup errors;
expanded secret values are never logged.

`ylva init` writes code-owned starters (`SOUL.md`,
`profiles/default/config.toml`, and `profiles/default/.env`) into `~/.ylva/`;
it does not seed agent-type files. Use `ylva init --workspace` to seed the
default profile config and `.env` starters plus `.gitignore` under the
discovered workspace root. Both commands write only missing files unless
`--force` is supplied. Global, profile-scoped, and project-scoped
`agents/*.md` and `agents/*.toml` files are discovered at runtime; richer
examples are documented below rather than shipped as repository templates.

### Global persona (`SOUL.md`)

`SOUL.md` defines the global persona for the top-level conversation. Its
initial system message is resolved independently from the profile, using the
same whole-file replacement order:

```text
<workspace-root>/.ylva/SOUL.md
    -> ~/.ylva/SOUL.md
    -> code-owned built-in SOUL
```

The resolved text is trimmed only at the end and becomes the global baseline
of every newly created root and child context, exactly once. Agent-type
persona and system-prompt content is additive: the effective prompt order is
global SOUL, agent-type `persona`, agent-type file body, the tool protocol,
then (for a child) its briefing and runtime constraints. The built-in value is
`You are Ylva, a helpful agent with tool capabilities.` and guarantees an
out-of-the-box global persona when neither disk layer exists.

### Agent-type files

Agent-type files are user-authored external configuration. Ylva loads them
from the project `.ylva/agents/`, the selected profile's
`~/.ylva/profiles/<profile-name>/agents/` directory, and the global
`~/.ylva/agents/` directory. For the same type, project scope shadows
profile scope, which shadows global scope; file sources replace the complete
definition rather than merging fields. A type may be written as pure TOML:

```toml
allowed_tools = ["read", "search", "glob"]
timeout_secs = 120
can_spawn = false
system_prompt = "Locate relevant files and explain the evidence."
```

or as Markdown with a required TOML `+++` or YAML `---` frontmatter block:

```markdown
+++
allowed_tools = ["read", "search", "glob"]
timeout_secs = 120
can_spawn = false
+++

Locate relevant files and explain the evidence.
```

The YAML variant uses the same Markdown body convention:

```markdown
---
allowed_tools:
  - read
  - search
  - glob
timeout_secs: 120
can_spawn: false
---

Locate relevant files and explain the evidence.
```

The frontmatter supplies agent settings and the Markdown body supplies the
system-prompt text. TOML frontmatter must begin with and close `+++`; YAML
frontmatter must begin with and close `---`. A plain Markdown file is a load
error. A profile's inline
`[agents.<type>]` table is applied afterward as a field-level override
(`engine`, `allowed_tools`, `timeout_secs`, `persona`, and related settings),
so `persona` is always prepended to the file body and also works when no file
body exists. Every configured agent `engine` must name an engine available to
the initialized profile; invalid references are hard startup errors rather
than deferred until that agent is spawned. If the active `config.toml` and its
same-scope `agents/<type>`
file both provide a base definition, loading fails with an error naming the
type, scope, and both paths; adapting a lower-scope file from the active
profile remains valid. `research` and `code_edit` are documentation examples
that users may copy and customize; `ylva init` does not create them. The
code-owned `default` agent remains available when no user-authored definition
replaces it.

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
| `context_length` | `Option<usize>` | Maximum token context window. `None` (the default when the field is left out of TOML) means the profile doesn't limit it explicitly. Accepts either a plain integer or a string with a `kb`/`mb` suffix (e.g. `"265kb"` = `265 * 1024`, `"2mb"` = `2 * 1024 * 1024`) — the same 1024-based units `/models` uses to display context lengths. `EngineConfig::effective_context_length` resolves the value actually used, in priority order: this configured value, then the provider's advertised context length for the active model (from its `/models` catalog), then the built-in default of `128 000`. Phase 10 reads the resolved value to drive compaction budgeting. |
| `api_base_url` | `Option<String>` | Optional engine-level override for the connection's base URL. |
| `api_key` | `Option<String>` | Optional engine-level override for the connection's API key. |
| `reasoning` | `Option<String>` | Initial explicit reasoning effort applied when the engine is constructed. `None` uses the provider's own default. Later changes go through `/models`, which validates the effort against the model's advertised options before switching. |

## Profile configuration (`ProfileConfig`)

A profile groups engine selection and per-agent settings for a runtime session.

| Field | Type | Description |
|---|---|---|
| `default_engine` | `String` | Name of the engine used when no agent-level override is present (required). |
| `compact_engine` | `Option<String>` | Engine used for summarisation tasks. Resolves to `default_engine` when absent. Phase 10 wires this at runtime. |
| `utility_engine` | `Option<String>` | Isolated engine used for utility tasks such as session-title generation. Resolves to `default_engine` when absent and does not follow `/models` runtime switching. |
| `provider_connections` | `HashMap<String, ProviderConnectionConfig>` | Named provider connections, keyed by connection name. |
| `engines` | `HashMap<String, EngineConfig>` | Named model/runtime configurations, keyed by engine name. |
| `agents` | `HashMap<String, AgentConfig>` | Per-agent-type settings, keyed by agent-type name. |

### Per-agent configuration (`AgentConfig`)

| Field | Type | Description |
|---|---|---|
| `engine` | `Option<String>` | Override the engine for this agent type. When absent, `ProfileConfig::default_engine` is used. |
| `description` | `Option<String>` | Model-facing summary of the tasks this agent type is suited for. The root agent receives it in the `spawn_subagent` tool description. |
| `allowed_tools` | `Option<Vec<String>>` | Restrict which tool names this agent may invoke. `None` falls back to the caller's flat default tool list (`AgentFactory::with_allowed_tools`, or `DEFAULT_CLI_TOOLS`/`DEFAULT_TUI_TOOLS` when unset). `Some(names)` is honored per `agent_type` by `AgentFactory::build`, so a distinct `agent_type` — including one used only for spawned children — can be restricted to a genuinely different tool set than the root or any other agent type. |
| `timeout_secs` | `Option<u64>` | Maximum wall-clock seconds before the agent is cancelled. `None` means no timeout. |
| `persona` | `Option<String>` | Optional agent-type persona instruction prepended to that type's system-prompt body. It is independent of the global `SOUL.md` persona and applies when the type runs as root or child. |
| `system_prompt` | `Option<String>` | File-authored agent-type system-prompt body. It is additive to the global SOUL and is not set by inline profile tables. |
| `can_spawn` | `bool` | When `true` (and this is a `Role::Root` agent), `AgentFactory::create` registers the `spawn_subagent` tool and wires a `ChildAgentManager` so this agent may delegate self-contained tasks to a concurrently-running `Role::Child` agent (one level deep; children cannot spawn further children). Use `--auto-approve-spawn` to skip the human-approval prompt for `spawn_subagent` calls. See "Child-agent delegation" in `docs/features.md`. |
| `tools` | `HashMap<String, ToolSandboxConfig>` | Per-tool sandbox rule lists scoped to this agent type only, keyed by tool name (`[agents.<type>.tools.<name>]` inline tables, or a file-authored agent-type's own top-level `[tools.<name>]`/`tools: { <name>: ... }` table). Same shape as the profile-level `[tool.<name>]` table (see "Sandbox" below), resolved with higher precedence than it for this agent type. |

Every loaded profile has a `default` agent type. If `[agents.default]` is
absent, Ylva supplies the built-in default configuration: it uses
`default_engine`, the normal tool set, and `can_spawn = true` for the root
agent. A present `[agents.default]` table replaces that built-in
configuration, so set `can_spawn = true` there when customizing the default
agent and retaining child-agent delegation. Other agent-type tables do not
remove the implicit `default` agent.

`AgentConfig.engine` selects a complete named engine; it is not a
field-by-field override layer. An agent cannot adjust that engine's model,
temperature, context length, reasoning settings, or provider connection
independently. If an agent needs specialized engine settings, define another
`[engines.<name>]` entry and select that engine for the agent type.

Agents that resolve the same named engine share its runtime selection. A
provider/model or reasoning-effort change made through `/models` therefore
applies to every agent using that engine, including already-running children.
The change does not alter an in-flight LLM request; it is observed by the
agent's next request, which may be another request in the same child run after
a tool call. The selection is dynamic rather than pinned at spawn time. Use
separate named engines when the root and a child must remain independent.

## Sandbox (`Sandbox` / `SandboxContext`)

`Sandbox` (`src/sandbox.rs`) holds the workspace root (canonicalized cwd at
startup) that path-based tool-approval rules are evaluated against.
`SandboxContext::check_path` normalizes a candidate path against that root
(collapsing `..` without touching the filesystem, resolving symlinked
prefixes such as macOS's `/var` → `/private/var`) and returns either an
explicit verdict or `None` (abstain, letting the calling tool apply its own
default).

**Default floor.** When a tool has no `[tool.<name>]` config table (see
below) at any resolved layer, two rules apply unconditionally, evaluated in
this order, before any custom rule appended in code (e.g.
`--auto-approve-write`/`--auto-approve-edit`, last match wins):

| Rule | Verdict | Notes |
|---|---|---|
| `outside_workspace` | `AutoDenied` | Any path that normalizes outside the workspace root. |
| `dot_path` | `AutoDenied` | Any content-tool path whose relative-to-root form has a component starting with `.` (dot-files/dot-directories). `glob` enumeration may inspect dot-prefixed paths inside the workspace. |

This floor is Rust struct-literal code, not parsed from any embedded TOML
file, so it can never be broken by a missing or corrupted config file.
`search`'s `path: None` (cwd, never leaves the workspace by construction)
skips the sandbox check entirely and is auto-approved; when a `path` is
given, it goes through the same `check_path`/`dot_path`/`outside_workspace`
evaluation as `read`, `write`, and `edit`. `glob` uses the same workspace
boundary and named-rule checks but allows dot-prefixed paths because it
only enumerates paths.

**Per-tool `[tool.<name>]` config.** A profile's `config.toml` may configure
a tool's rule list explicitly, e.g.:

```toml
[tool.read]
rules = [
  { rule = "dot_path", verdict = "user_approval_required" },
]

[tool.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { pattern = "**/build/**", verdict = "auto_approved" },
]
```

Each `rules` entry is one of:

- A **named rule**, `{ rule = "outside_workspace" | "dot_path", verdict = "...", reason = "..." }`,
  referencing one of the two built-in structural checks above with a
  configurable verdict — reorder them or change their verdict without
  reimplementing the path-comparison logic in glob syntax.
- A **custom glob rule**, `{ pattern = "...", verdict = "...", reason = "..." }`,
  matched against the canonicalized absolute path (the config-parseable form
  of the `SandboxRule` type tool constructors can also append in code).

`verdict` is one of `"auto_approved"`, `"auto_denied"`, or
`"user_approval_required"`; `reason` is optional and only used for
`"auto_denied"`, defaulting to the same message the built-in floor uses for
that rule (or a generic message for a custom glob rule) when omitted.

Rules are evaluated in list order; last match wins — the same
last-match-wins semantics as the default floor and as code-appended rules.

**A configured `[tool.<name>]` table fully replaces the default floor for
that tool — it does not merge with it.** A profile that configures
`[tool.read]` must include its own `{ rule = "outside_workspace", ... }`/
`{ rule = "dot_path", ... }` entries if it still wants those protections for
`read`; a tool with no `[tool.<name>]` table at all keeps the unconditional
default floor.

**Per-agent-type `[agents.<type>.tools.<name>]` override.** An agent type
may configure its own rule list for a tool, taking precedence over the
profile-level `[tool.<name>]` table for that tool, for that agent type only
— other agent types in the same profile are unaffected:

```toml
[tool.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
]

[agents.custom]
allowed_tools = ["read", "write"]

[agents.custom.tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { pattern = "**/build/**", verdict = "auto_approved" },
]
```

A file-authored agent-type definition (`agents/<type>.toml`, or a `.md`
file's `+++` TOML or `---` YAML frontmatter) configures the same override
with a top-level `[tools.<name>]` table (TOML) or `tools: { <name>: { rules:
[...] } }` (YAML) — no `agents.<type>.` prefix, since the file itself is
already scoped to that one agent type.

**Resolution order**, most specific wins, each tier fully replacing the
next (no merging across tiers):

1. `[agents.<type>.tools.<name>]` (or the agent-type file's `[tools.<name>]`/
   `tools: { <name>: ... }`), if present.
2. The profile-level `[tool.<name>]` table, if present.
3. The unconditional default floor described above.

Both `[tool.<name>]` and `[agents.<type>.tools.<name>]` (and their
file-authored equivalents) hard-validate their tool name against the
registered tool set (`read`, `write`, `glob`, `bash`, `search`, `edit`,
`clarify`) at startup; an unrecognized name is a hard error naming the
offending agent type (or "profile-level"), the unknown name, and its source.

`--auto-approve-write`/`--auto-approve-edit` continue to work unchanged:
each still appends its own catch-all `AutoApproved` rule (`pattern = "**"`)
after whichever rule list applies (agent-type-scoped, profile-level, or the
default floor), so the flag always wins regardless of `[tool.*]`/
`[agents.<type>.tools.*]` config.
