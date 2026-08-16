# Ylva agent-harness CLI options

Reference for Ylva's command-line interface.

## Flags

| Flag | Type | Accepted values | Default | Description |
| --- | --- | --- | --- | --- |
| `--version` | none | — | — | Print build provenance (`Version`, `Commit`, `Built`, `Profile`) and exit. Takes precedence over all other arguments. |
| `--prompt <text>` | string | any text | *none* | Initial user turn sent to the agent. Resolved with the precedence described below. |
| `--log-file <path>` | path | any writable file path | *none* | Write logs to this file. The file is opened in truncate/overwrite mode on each start, so logs never accumulate across runs. |
| `--log-level <level>` | enum | `trace`, `debug`, `info`, `warn`, `error` | `info` | Minimum severity written. Selecting a level includes it and all higher-severity levels (`trace < debug < info < warn < error`). |
| `--log-format <format>` | enum | `text`, `json` | `text` | Output format. `text` is human-readable; `json` produces JSONL (one JSON object per line). |

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

**WIP while we still work on blueprint phases**

The provider is selected from environment variables with **Ollama-first**
precedence; when no backend is configured the `MockProvider` (deterministic
streaming tokens) is used, which is also what the hermetically tested paths run
against.

| Variable | Default | Description |
| --- | --- | --- |
| `OLLAMA_BASE_URL` | — | Base URL of the Ollama server. A non-empty value selects the Ollama backend. |
| `OLLAMA_MODEL` | `gemma4:e4b-mlx` | Ollama model. |
| `OLLAMA_API_KEY` | — | Optional Ollama API key. |
| `OPENROUTER_BASE_URL` | — | OpenRouter base URL. Used only when no `OLLAMA_BASE_URL` is set. |
| `OPENROUTER_MODEL` | `deepseek/deepseek-v4-flash-0731` | OpenRouter model. |
| `OPENROUTER_API_KEY` | — | OpenRouter API key. |
