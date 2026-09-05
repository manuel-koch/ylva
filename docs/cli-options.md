# Ylva agent-harness CLI options

Reference for Ylva's command-line interface.

## Flags

| Flag | Type | Accepted values | Default | Description |
| --- | --- | --- | --- | --- |
| `--version` | none | — | — | Print build provenance (`Version`, `Commit`, `Built`, `Profile`) and exit. Takes precedence over all other arguments. |
| `--no-tui` | none | — | — | Force headless (CLI) mode even when stdout is a terminal. Useful for scripting or debugging without the TUI. |
| `--prompt <text>` | string | any text | *none* | Initial user turn sent to the agent. Resolved with the precedence described below. |
| `--log-file <path>` | path | any writable file path | *none* | Write logs to this file. The file is opened in truncate/overwrite mode on each start, so logs never accumulate across runs. |
| `--log-level <level>` | enum | `trace`, `debug`, `info`, `warn`, `error` | `info` | Minimum severity written. Selecting a level includes it and all higher-severity levels (`trace < debug < info < warn < error`). |
| `--log-format <format>` | enum | `text`, `json` | `text` | Output format. `text` is human-readable; `json` produces JSONL (one JSON object per line). |
| `--auto-approve-edit` | none | — | — | Skip the human-approval prompt for `edit` tool calls. |
| `--auto-approve-write` | none | — | — | Skip the human-approval prompt for `write` tool calls. |
| `--auto-approve-spawn` | none | — | — | Skip the human-approval prompt for `spawn_subagent` tool calls. See `can_spawn` in `docs/configuration.md` and "Child-agent delegation" in `docs/features.md`. |

An invalid `--log-level` or `--log-format` value, a flag missing its required
value, or a `--log-file` that cannot be opened (for example, its containing
directory does not exist) prints a usage error to stderr and exits with code
`2` — the error names the attempted path.

## Prompt resolution

The initial user turn (`--prompt`) is resolved by precedence:

1. `--prompt <text>`
2. piped stdin read to EOF — only when stdin is **not** a terminal (something
   is piped in)

## Environment variables

### WIP: still working on blueprint phases

The provider is selected from environment variables with **Ollama-first**
precedence; when no backend is configured the `MockProvider` (deterministic
streaming tokens) is used, which is also what the hermetically tested paths run
against.

| Variable | Default | Description |
| --- | --- | --- |
| `OLLAMA_BASE_URL` | — | Base URL of the Ollama server. A non-empty value selects the Ollama backend. |
| `OLLAMA_MODEL` | `gemma4:e4b-mlx` | Ollama model. |
| `OLLAMA_API_KEY` | — | Optional Ollama API key. |
| `OPENROUTER_BASE_URL` | `https://openrouter.ai/api/v1` | OpenRouter base URL. Used only when no `OLLAMA_BASE_URL` is set. Unlike Ollama, OpenRouter is a single hosted service, so this has a working default and never has to be set manually. |
| `OPENROUTER_MODEL` | `deepseek/deepseek-v4-flash-0731` | OpenRouter model. |
| `OPENROUTER_API_KEY` | — | OpenRouter API key. Setting `OPENROUTER_API_KEY` (or `OPENROUTER_MODEL`) alone, with no `OLLAMA_BASE_URL` set, is enough to select the OpenRouter backend — `OPENROUTER_BASE_URL` does not need to be exported. |

### Debug

| Variable | Default | Description |
| --- | --- | --- |
| `YLVA_REQUEST_DUMP_PATH` | — | When set, each outgoing HTTP request body is written to a timestamped JSON file. The value is a path template: the directory and filename stem are extracted and the actual file is written as `<stem>_<unix_seconds>_<millis>.json`. A relative directory is resolved against the process working directory. Used by `make build-debug-and-run-with-logs`. |
