# Spike 3: Ollama OpenAI-Compatible Tool Calling

A lightweight validation spike that checks Ollama's `/v1/chat/completions`
endpoint (the OpenAI chat-completions spec) accepts a `tools` array and streams
`delta.tool_calls` chunks whose key/nesting shape is compatible with the
OpenRouter reference. This is what lets the whole Ylva architecture use a single
`reqwest` + `eventsource-stream` provider for both Ollama and OpenRouter backends
(see `docs/blueprint.md` §0 Spike 3 / §2 / §4A / §5).

Unlike Spikes 1 and 2, which are standalone Rust crates, this spike is a `curl`
POST plus a lightweight Python inspection script. It is not production code,
is not wired into any future `src/` tree, and is not added to the workspace.

The default run requires a local Ollama server; it needs no API key and
no network beyond `localhost`. A live OpenRouter comparison is opted in via
`inspect_openrouter_tool_calls.sh` (requires `OPENROUTER_API_KEY`) and is not
part of the default pass. Without it, Criterion B's "byte-for-byte compatible with
OpenRouter" claim is only checked structurally against the documented reference
shape, not against a live OpenRouter stream.

## What it proves

- Ollama `/v1/chat/completions` accepts a `tools` array in the request body and
  streams SSE chunks containing `delta.tool_calls`.
- The `tool_calls[i]` object carries `function.name` and `function.arguments`
  under the standard nesting — no custom Ollama field names or nesting.
- When run alongside `inspect_openrouter_tool_calls.sh`, the two `delta.tool_calls`
  shapes are compared to check they match byte-for-byte.

## How to run

```shell
./inspect_ollama_tool_calls.sh
```

`inspect_ollama_tool_calls.sh` POSTs a streaming chat-completions request with a
`get_weather` tool and a user turn that invites a tool call, captures the raw SSE
stream to `out/sse_ollama_<model>.log`, hands it to `inspect_sse.py` for parsing
and grading, then prints the observed `delta.tool_calls` shape via
`inspect_sse.py --shape`.

`inspect_openrouter_tool_calls.sh` does the same against OpenRouter (requires
`OPENROUTER_API_KEY`; captures to `out/sse_openrouter_<model>.log`).

### Options

| Env var        | Default              | Meaning                        |
| ---            | ---                  | ---                            |
| `OLLAMA_BASE_URL` | `http://localhost:11434` | Base URL of the Ollama server. |
| `OLLAMA_MODEL` | `gemma4:e4b-mlx`     | Ollama model to test (tool-capable). |
| `OPENROUTER_BASE_URL` | `https://openrouter.ai/api/v1` | OpenRouter base URL. |
| `OPENROUTER_MODEL` | `deepseek/deepseek-v4-flash-0731` | OpenRouter model. |
| `OPENROUTER_API_KEY` | — | Required by `inspect_openrouter_tool_calls.sh`. |
| `TOOL_CHOICE`  | `auto`               | `tool_choice` value in the body. |
| `SPIKE_OUT_DIR`| `<script>/out`       | Where the raw SSE logs are written. |

Examples:

```shell
# Ollama: gemma4:e4b-mlx against local Ollama.
./inspect_ollama_tool_calls.sh

# Confirm with the fallback Ollama model.
OLLAMA_MODEL=gpt-oss:20b ./inspect_ollama_tool_calls.sh

# OpenRouter capture (requires the API key).
OPENROUTER_API_KEY=... ./inspect_openrouter_tool_calls.sh
```

After each Ollama run the script unloads the model with `ollama stop <model>` so
the next model under test is loaded fresh rather than reusing the prior context.

### Comparing the shapes (byte-for-byte check)

Each script prints its own `inspect_sse.py --shape` output. Compare the two—no
dedicated diff script—by diffing the shape lines:

```shell
./inspect_ollama_tool_calls.sh
OPENROUTER_API_KEY=... ./inspect_openrouter_tool_calls.sh

diff <(python3 inspect_sse.py --shape out/sse_ollama_gemma4_e2b-mlx.log) \
     <(python3 inspect_sse.py --shape out/sse_openrouter_deepseek_deepseek-v4-flash-0731.log)
```

An empty diff means the `delta.tool_calls` key structures are identical
(byte-for-byte compatible), confirming the unified-provider claim.

## Success criteria

On exit the script prints a per-criterion `PASS`/`FAIL` verdict and a structured
summary:

- **Criterion A — accepts `tools` and streams `delta.tool_calls`:** HTTP 200 and at
  least one SSE chunk carrying `delta.tool_calls` with incremental
  `function.name` / `function.arguments` fragments.
- **Criterion B — OpenRouter-compatible shape:** observed `tool_calls[i]` keys are
  within `{id, index, type, function}` and the nested `function` keys within
  `{name, arguments}` (no Ollama-specific renames); the live byte-for-byte check is
  the empty-diff comparison of the two `--shape` outputs above.

The process exits non-zero if either criterion fails. When the endpoint silently
ignores `tools` (HTTP 200 with no tool calls), the script prints a note pointing
toward the `ollama_native` provider.

## Outcome

**Criterion A: PASS** on live Ollama for `gemma4:e4b-mlx` and `gpt-oss:20b`.
**Criterion B: live PASS** — the Ollama shapes were compared with live
OpenRouter captures for `deepseek/deepseek-v4-flash-0731` and
`moonshotai/kimi-k2.6`; both diffs were empty. The recorded model outputs and
comparison result are in `docs/blueprint.md` §0 Spike 3. Raw capture files are
not retained in this repository. No rerun is required for blueprint
finalization.
