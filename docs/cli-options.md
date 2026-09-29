# Ylva agent-harness CLI options

Reference for Ylva's command-line interface. Parsing is backed by `clap`
(derive model in `src/cli.rs`); the tables below describe the resulting
surface.

## Flags

| Flag | Type | Accepted values | Default | Description |
| --- | --- | --- | --- | --- |
| `--version` | none | — | — | Print build provenance (`Version`, `Commit`, `Built`, `Profile`) and exit. Takes precedence over all other arguments. |
| `--help` | none | — | — | Print usage (all flags and subcommands) to stdout and exit with code `0`. |
| `init [--workspace] [--force]` | subcommand | `--workspace`, `--force` | — | Write configuration starters and exit before provider/session startup. Without `--workspace`, writes `SOUL.md`, `profiles/default/config.toml`, and an annotated `profiles/default/.env` under `YLVA_HOME` (or `${HOME}/.ylva`); `--workspace` writes the default profile config and `.env` starters plus `.gitignore` under the discovered workspace root's `.ylva/`; both modes write-if-missing unless `--force` is supplied. Agent-type files are not seeded. |
| `--no-tui` | none | — | — | Force headless (CLI) mode even when stdout is a terminal. Useful for scripting or debugging without the TUI. |
| `--color=<mode>` | enum | `auto`, `no`, `yes` | `auto` | Select CLI color behavior: detect terminal support, force no colors, or force colors. Has no effect on TUI rendering. |
| `--prompt <text>` | string | any text | *none* | Initial user turn sent to the agent. Resolved with the precedence described below. |
| `--profile <name>` | string | any profile directory name | `default` | Resolve `<workspace-root>/.ylva/profiles/<name>/config.toml`, then `~/.ylva/profiles/<name>/config.toml`. When `--profile` is given explicitly, a missing file at both disk layers is a hard error (exit code `2`) naming the profile. With no `--profile` flag, the implicit `default` profile uses `ProfileConfig::default()` when no disk file exists. A malformed or unreadable file at the winning layer is always an error. See `docs/configuration.md`. |
| `--log-file <path>` | path | any writable file path | *none* | Write logs to this file. The file is opened in truncate/overwrite mode on each start, so logs never accumulate across runs. |
| `--log-level <level>` | enum | `trace`, `debug`, `info`, `warn`, `error` | `info` | Minimum severity written. Selecting a level includes it and all higher-severity levels (`trace < debug < info < warn < error`). |
| `--log-format <format>` | enum | `text`, `json` | `text` | Output format. `text` is human-readable; `json` produces JSONL (one JSON object per line). |
| `--conversation-id <uuid>` | uuid | any persisted session id | *none* | Resume the named session: its `.jsonl` file is re-attached for append instead of a fresh session being started. An unknown id is a hard error (exit code `2`) — never a silent fresh session. See `docs/persistence.md`. |
| `--auto-approve-read` | none | — | — | Skip the human-approval prompt for `read` tool calls. |
| `--auto-approve-write` | none | — | — | Skip the human-approval prompt for `write` tool calls. |
| `--auto-approve-glob` | none | — | — | Skip the human-approval prompt for `glob` tool calls. |
| `--auto-approve-bash` | none | — | — | Skip the human-approval prompt for `bash` tool calls. |
| `--auto-approve-search` | none | — | — | Skip the human-approval prompt for `search` tool calls. |
| `--auto-approve-edit` | none | — | — | Skip the human-approval prompt for `edit` tool calls. |
| `--auto-approve-spawn` | none | — | — | Skip the human-approval prompt for `spawn_subagent` tool calls. See `can_spawn` in `docs/configuration.md` and "Child-agent delegation" in `docs/features.md`. |

An invalid `--color`, `--log-level`, `--log-format`, or `--conversation-id`
value, an unknown flag, a flag missing its required value (for any
value-taking flag, including `--profile`), or a `--log-file` that cannot be
opened (for example, its containing directory does not exist) prints a usage
error to stderr and exits with code `2` — the error names the attempted path
or flag. Repeating `--color` or `--prompt` uses the last occurrence; other
value-taking flags may not be repeated. The `init` subcommand may follow
global flags (`ylva --no-tui init --workspace`). The `init` subcommand also
exits with code `2` when its target root cannot be resolved or a seeded file
cannot be written.

When the CLI writes to a terminal, its section headers and tool status lines
use the same semantic colors as the TUI. Colors are disabled automatically for
non-terminal output, `TERM=dumb`, or when the standard `NO_COLOR` environment
variable is set when `--color=auto` is selected. Use `--color=no` to disable
them explicitly, or `--color=yes` to force them even for non-terminal output,
`TERM=dumb`, or `NO_COLOR`. The flag only affects CLI output, not the TUI.

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
| `YLVA_HOME` | `${HOME}/.ylva` | Ylva's home directly (chat store `chats/` and profiles `profiles/`). Set it to redirect Ylva's own state. See [persistence](./persistence.md#relocating-the-ylva-home-with-ylva_home). |

### Debug

| Variable | Default | Description |
| --- | --- | --- |
| `YLVA_REQUEST_DUMP_PATH` | — | When set, every outbound HTTP request is dumped, including model catalogs, Ollama metadata, chat/SSE, and web search. Request and response files share a timestamp and per-request id. A relative directory is resolved against the process working directory. Used by `make build-debug-and-run-with-logs`. |

The value is a path template: the directory and filename stem are extracted and
the request is written as
`<stem>_<timestamp>_<dump_id>_request.jsonl`. Its first line contains the
request's `scheme`/`host`/`path`/`query`/`headers`; `Authorization`, `Cookie`,
and API-key-style header values are redacted as `<REDACTED>`. Its second line is
the compact JSON request body, or `null` for a request without a body. Base64
tool-result payloads and inline image/file data are replaced with stable
redaction markers while roles, IDs, media types, filenames, and other safe
metadata remain. Client-level default headers, such as Ylva's `User-Agent`, are
not included in the header projection. The live request is not changed.

The paired response is written as
`<stem>_<timestamp>_<dump_id>_response.<extension>`. The extension and file
contents follow the response `Content-Type`: `text/event-stream` uses `.jsonl`
with one line per SSE `data:` payload, `application/json` uses `.json` with the
original buffered bytes, `text/html` uses `.html`, `text/plain` uses `.txt`,
and common XML, JavaScript, image, and PDF types use their corresponding
extensions. Unknown buffered types use `.bin`. Buffered responses are written
as their original bytes rather than JSON-encoded strings. Transport failures
without response headers use `.jsonl` and are recorded as `[transport] ...`
markers.
