# Ylva agent-harness CLI options

Reference for Ylva's command-line interface.

## Flags

| Flag | Type | Accepted values | Default | Description |
| --- | --- | --- | --- | --- |
| `--version` | none | — | — | Print build provenance (`Version`, `Commit`, `Built`, `Profile`) and exit. Takes precedence over all other arguments. |
| `--no-tui` | none | — | — | Force headless (CLI) mode even when stdout is a terminal. Useful for scripting or debugging without the TUI. |
| `--prompt <text>` | string | any text | *none* | Initial user turn sent to the agent. Resolved with the precedence described below. |
| `--profile <name>` | string | any profile directory name | `default` | Load `~/.ylva/profiles/<name>/config.toml`. When `--profile` is given explicitly, a missing file is a hard error (exit code `2`) naming the profile — the user asked for that specific profile, so it is never silently substituted. With no `--profile` flag at all (the implicit `default` profile), a missing file falls back to the in-code default profile instead, so Ylva still runs out of the box. A malformed file is always an error, named or not. See `docs/configuration.md`. |
| `--log-file <path>` | path | any writable file path | *none* | Write logs to this file. The file is opened in truncate/overwrite mode on each start, so logs never accumulate across runs. |
| `--log-level <level>` | enum | `trace`, `debug`, `info`, `warn`, `error` | `info` | Minimum severity written. Selecting a level includes it and all higher-severity levels (`trace < debug < info < warn < error`). |
| `--log-format <format>` | enum | `text`, `json` | `text` | Output format. `text` is human-readable; `json` produces JSONL (one JSON object per line). |
| `--conversation-id <uuid>` | uuid | any persisted session id | *none* | Resume the named session: its `.jsonl` file is re-attached for append instead of a fresh session being started. An unknown id is a hard error (exit code `2`) — never a silent fresh session. See `docs/persistence.md`. |
| `--auto-approve-edit` | none | — | — | Skip the human-approval prompt for `edit` tool calls. |
| `--auto-approve-write` | none | — | — | Skip the human-approval prompt for `write` tool calls. |
| `--auto-approve-spawn` | none | — | — | Skip the human-approval prompt for `spawn_subagent` tool calls. See `can_spawn` in `docs/configuration.md` and "Child-agent delegation" in `docs/features.md`. |

An invalid `--log-level`, `--log-format`, or `--conversation-id` value, a flag
missing its required value, or a `--log-file` that cannot be opened (for example, its containing
directory does not exist) prints a usage error to stderr and exits with code
`2` — the error names the attempted path.

## Resuming a session

On exit — success, error, or an unrecognized-tool/provider failure, as long as
a session was persisted — Ylva prints the session id and a ready-to-paste
command to stderr:

```text
Session: dbe9a693-d709-4670-89cd-6162e6ae3e11
Resume with: ylva --conversation-id dbe9a693-d709-4670-89cd-6162e6ae3e11
```

When the run used an explicit `--profile <name>`, the hint includes it too
(`--profile <name>` is otherwise omitted, since the implicit default profile
needs no flag to resume with the same configuration). See
`docs/persistence.md` for how `--conversation-id` re-attaches the session
file.

## Prompt resolution

The initial user turn (`--prompt`) is resolved by precedence:

1. `--prompt <text>`
2. piped stdin read to EOF — only when stdin is **not** a terminal (something
   is piped in)

## Environment variables

Provider environments are loaded from the selected profile's optional
`.env` file (`~/.ylva/profiles/<profile>/.env`) and from the process
environment. An already-exported process variable takes precedence. TOML
string values may reference the merged environment as `${ENV_VAR}`; a
missing reference is a startup error. Named
`[provider_connections.<name>]` tables are constructed independently, even
when they share the same implementation. `/models` queries every named
connection and displays connection-qualified model choices. When no explicit
engines or provider connections are configured, the supported environment
providers are discovered instead. When no backend is configured, the
`MockProvider` (deterministic streaming tokens) is used.

| Variable | Default | Description |
| --- | --- | --- |
| `OLLAMA_BASE_URL` | — | Origin of the Ollama server (for example, `http://localhost:11434`). The provider derives its `/v1` chat and native `/api` model endpoints. A non-empty value configures the Ollama backend. |
| `OLLAMA_MODEL` | `gemma4:e4b-mlx` | Ollama model. |
| `OLLAMA_API_KEY` | — | Optional Ollama API key. |
| `OPENROUTER_BASE_URL` | `https://openrouter.ai` | OpenRouter origin. The provider derives `/api/v1` for chat and model discovery. Unlike Ollama, OpenRouter is a single hosted service, so this has a working default and never has to be set manually. |
| `OPENROUTER_MODEL` | `deepseek/deepseek-v4-flash-0731` | OpenRouter model. |
| `OPENROUTER_API_KEY` | — | OpenRouter API key. Setting `OPENROUTER_API_KEY` (or `OPENROUTER_MODEL`) alone is enough to configure OpenRouter — `OPENROUTER_BASE_URL` does not need to be exported. |

For explicit provider connections, `api_key` values in `config.toml` remain
authoritative. When omitted, `OLLAMA_API_KEY` or `OPENROUTER_API_KEY` fills the
connection based on its `provider` implementation kind. API keys and expanded
environment values are never written to logs.

### Paths

| Variable | Default | Description |
| --- | --- | --- |
| `YLVA_HOME` | `${HOME}/.ylva` | Ylva's state root directly (chat store `chats/` and profiles `profiles/`). Set it to redirect Ylva's own state. See [persistence](./persistence.md#relocating-the-root-with-ylva_home). |

### Debug

| Variable | Default | Description |
| --- | --- | --- |
| `YLVA_REQUEST_DUMP_PATH` | — | When set, each outgoing HTTP request is written to a timestamped JSON Lines file, and the paired streamed response is written alongside it as a raw SSE transcript. The value is a path template: the directory and filename stem are extracted and the request is written as `<stem>_<timestamp>_<dump_id>_request.jsonl` — line 1 is the request's `scheme`/`host`/`path`/`headers` (any `Authorization`/`Cookie`/API-key-style header value is redacted as `<REDACTED>`), line 2 is the compact JSON body; the response is written as `<stem>_<timestamp>_<dump_id>_response.jsonl` (one line per SSE `data:` payload, or a `[transport] ...` marker for non-payload events such as cancellation or a transport error), sharing the exact same timestamp and per-request `<dump_id>` as its request so concurrent requests cannot overwrite each other's dumps. A relative directory is resolved against the process working directory. Used by `make build-debug-and-run-with-logs`. |
