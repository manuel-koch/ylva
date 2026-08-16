# Ylva agent-harness features

## General

- Rust-based terminal AI agent application, runnable as a CLI
- Implements agent-harness functionality

## Build Info

Use `ylva --version` to print build provenance (`Version`, `Commit`,
`Built`, `Profile`).

## Logging

Logging is opt-in and controlled entirely by CLI flags (there is no logging
configuration file).
See [CLI options](./cli-options.md) for usage of `--log-file`, `--log-level`
and `--log-format` options.

## CLI Runtime

Non-interactive, one-shot session: the initial user turn is sent to the
agent, the assistant response is streamed to stdout as it arrives, and the
final committed conversation history is printed as JSONL on exit — one JSON
object per message, in the same wire shape the provider sees.

Initial user turn resolution: `--prompt <text>`, else piped stdin read to EOF
(only when stdin is not a terminal) — e.g. `echo '…' | ylva`.

Tool approval is host-driven: `read`/`glob` are auto-approved immediately;
**WIP:** `write` is always denied and the denial is fed back to the model as a
`ToolError` plus a follow-up prompt.

Tool status lines render as `[tool] <title> …` (e.g. `[tool] Read docs/features.md [limit=32]`)

**WIP:** Provider selection is from environment variables with Ollama-first precedence
(`OLLAMA_*`, then `OPENROUTER_*`), else `MockProvider` when no backend is configured.

## TUI Runtime

**WIP:** not yet implemented.

## Model Provider

Currently the following AI model providers are supported:

- Ollama ( via OpenAI compatible endpoints )
- OpenRouter ( via OpenAI compatible endpoints )

## Tools to be used by models

The following tools can be used by AI models during a conversation turn.

Tools report their results via **uniform tool-result envelope**:
Every `role:"tool"` message carries its `content` as JSON struct

- `{"ok":true,"result":...}` on success
- `{"ok":false,"error":...}` on failure

**WIP:** `read` and `write` currently only support UTF-8 code points via lossy decode.

**WIP:** `write` is currenty a stub toolthat is unconditionally denied!

- **read**: Read file content from given `path` using `mode`/`offset`/`limit`.
  Mode can be `bytes`/`chars`/`lines`.
  Offset and limit are 0-based!
- **write**: Write `content` to file at given `path`.  
- **glob**: Search for paths using a glob `pattern`, including wildcards like `*`, `?`, `**`, `[...]`.
