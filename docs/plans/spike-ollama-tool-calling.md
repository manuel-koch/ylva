# Spike 3: Ollama OpenAI-Compatible Tool Calling — Validation Plan

## Context

**System being changed:** The Ylva repo currently contains documentation plus two
standalone proof-of-concept spike crates (`spikes/spike-tui-responsiveness` Spike 1,
`spikes/spike-http-stream-cancel` Spike 2). This plan covers the validation of the third
proof-of-concept from `docs/blueprint.md` §0 / Phase 0 — **Spike 3: Ollama OpenAI-Compatible
Tool Calling**. Per the delivery-form decision, this spike is **not** a new crate: it is a
raw `curl` POST plus a lightweight inspection script that validates the SSE stream shape.

**Goal & motivation:** `docs/blueprint.md` §0 Spike 3 must validate the assumption behind
Phase 2's **unified provider**: that Ollama's `/v1/chat/completions` endpoint (OpenAI
chat-completions spec) actually accepts the `tools` parameter and streams
`delta.tool_calls` chunks **byte-for-byte identically** to OpenRouter. This is what lets the
whole architecture use a single `reqwest` + `eventsource-stream` provider for both backends
(§2, §4A, §0 Spike 3). If Ollama rejects `tools` (HTTP 400/422) or ignores it and returns
plain text, the unified-provider claim is **invalidated** and Phase 2 must add a separate
`ollama_native` provider (POST to `/api/chat` with manual tool-injection prompting), plus
revise the "single provider serves both backends" claim in §4A and §5.

**Acceptance criteria (verbatim from `docs/blueprint.md` §0 Spike 3):**

- Ollama `/v1/chat/completions` accepts a `tools` array in the request body and returns SSE
  chunks containing `delta.tool_calls` with incremental `function.name` and
  `function.arguments` fields.
- The streaming shape is **byte-for-byte compatible** with OpenRouter's `delta.tool_calls`
  format (no custom Ollama field names or nesting).
- If **any** criterion fails: document the failure mode, add an `ollama_native` provider to
  the Phase 2 plan, and remove the "single provider serves both backends" claim from §4A
  and §5.

**Key constraints:**

- Tool-capable local model. **Decision:** use `gemma4:e2b-mlx` (already pulled) for the
  primary run; `gpt-oss:20b` as the fallback/confirmation model. Both are tool-capable.
- Connection targets local Ollama at `http://localhost:11434` (server confirmed running,
  v0.32.5).
- This is a validation spike, not production code. No `src/` tree, no wiring into the
  harness build. Deliverables live under `spikes/spike-ollama-tool-calling/` (script +
  README + recorded outcome), and `docs/plans/` holds this plan.
- OpenRouter byte-for-byte comparison is measured against the documented reference shape
  (`tool_calls[0].function.name` and `.arguments` in `delta`), not a live OpenRouter run by
  default, so the default pass is hermetic/local and needs no API key. An opt-in OpenRouter
  run (`OPENROUTER_API_KEY`) may be used to confirm the identical shape end-to-end.

## Implementation Plan

1. **Create the spike directory.** Create `spikes/spike-ollama-tool-calling/` containing
   `inspect_ollama_tool_calls.sh`, `inspect_openrouter_tool_calls.sh`, `inspect_sse.py`
   (the inspection scripts), `README.md`, and this plan's outcome note appended to
   `docs/blueprint.md` §0 Spike 3. Verify the directory exists before writing files.
   (No Cargo crate — the minimal form is a shell script.)

2. **Write the validation scripts** in `spikes/spike-ollama-tool-calling/`:
   `inspect_ollama_tool_calls.sh` and `inspect_openrouter_tool_calls.sh` (one capture +
   inspection script per backend, so Ollama and OpenRouter are distinct runs), plus
   `inspect_sse.py` (shared SSE parser/graders):
   - Build a request body JSON with a `tools` array (at least one tool, e.g. `get_weather`
     taking `location`/`unit` string args), a `messages` array containing a user turn whose
     phrasing invites a tool call, and `tool_choice: "auto"`.
   - Ollama script: default `OLLAMA_BASE_URL=http://localhost:11434`,
     `OLLAMA_MODEL=gemma4:e2b-mlx` (overridable via env/flags; allow
     `OLLAMA_MODEL=gpt-oss:20b`). `tool_choice` default `auto`. After the run it unloads
     the model (`ollama stop <model>`).
   - OpenRouter script: requires `OPENROUTER_API_KEY`; defaults
     `OPENROUTER_BASE_URL=https://openrouter.ai/api/v1`,
     `OPENROUTER_MODEL=deepseek/deepseek-v4-flash-0731`.
   - `curl -N -s <base_url>/v1/chat/completions` (or OpenRouter
     `<base_url>/chat/completions`) with the OpenAI-style JSON body (`stream: true`),
     piping the SSE stream to `jq`/`python3` (both confirmed present).
   - `inspect_sse.py` parses each `data:` line and checks for `choices[*].delta.tool_calls`
     entries; for each, extract and concatenate incremental `function.name` and
     `function.arguments` fragments, tracking per-call index.
   - Each script prints a structured summary: HTTP status line, count of SSE chunks
     containing `delta.tool_calls`, the incremental fragments seen for each
     `tool_call_index`, a `PASS`/`FAIL` verdict per acceptance criterion, and the observed
     `delta.tool_calls` key shape via `inspect_sse.py --shape`.
   - Detect and report the failure modes: HTTP 400/422 (rejected body) vs. 200-with-no-
     tool-calls (endpoint ignored `tools`, plain text only).

3. **Run the validation against the primary model.** Execute the script with
   `MODEL=gemma4:e2b-mlx`. Confirm: (a) HTTP 200, (b) at least one `delta.tool_calls`
   chunk with incremental `function.name`/`function.arguments`, (c) field names and nesting
   match `tool_calls[i].function.{name,arguments}` with no Ollama-specific renames. Capture
   raw SSE output to a log file for the byte-for-byte claim.

4. **Confirm with the fallback model.** Re-run with `MODEL=gpt-oss:20b` to confirm the
   shape is consistent across both tool-capable local models and is not a single-model
   artifact.

5. **Byte-for-byte shape check.** Compare the `delta` object keys/nesting observed against
   the OpenRouter reference shape. Run both scripts (`inspect_ollama_tool_calls.sh`, and
   `inspect_openrouter_tool_calls.sh` with `OPENROUTER_API_KEY`), then compare the two
   `inspect_sse.py --shape` outputs with a plain `diff` — no dedicated diff script; an
   empty diff asserts byte-for-byte compatibility with the OpenRouter capture, otherwise
   assert structural equality against the documented reference shape from the blueprint
   (§4A `DeltaChunk`: `tool_call_index`, `tool_call_id`, `function_name`,
   `arguments_fragment`).

6. **Record the outcome.** Under `docs/blueprint.md` §0 "**Spike 3 outcome:**" (currently a
   placeholder at line ~137), record `PASS`/`FAIL` per success criterion and the
   architectural consequence:
   - **Pass:** keep the unified provider; note the Ollama base_url/HTTP constraints.
   - **Fail:** document the exact failure mode (particular model or rejected body), add the
     `ollama_native` provider step to the Phase 2 plan, and remove the "single provider
     serves both backends" claim from §4A and §5.
   Per `AGENTS.md` (*Docs are part of the change set*), this blueprint edit ships with the
   spike. Do **not** create any other new doc files without asking.

7. **Write `spikes/spike-ollama-tool-calling/README.md`.** Mirror the Spike 1/2 README
   convention: what it proves, the acceptance criteria, how to run each script (default
   model, base URL, overrides), and how the verdict + `--shape` comparison are produced.
   Do not duplicate the full plan; link back to this plan file
   (`docs/plans/spike-ollama-tool-calling.md`).

## Consistency Issues

- **[low]** The blueprint §2 lists `ollama-rs` as "REMOVED — Ollama supports OpenAI-
  compatible API", and §0 Spike 3 line ~121 suggests *using* a tool-capable model such as
  `llama3.1:8b`/`qwen2.5`/`mistral-nemo` — none of which are pulled here. This plan resolves
  the contradiction by using the available `gemma4:e2b-mlx` / `gpt-oss:20b` models
  instead, and the outcome note must record which model was actually used so the doc does
  not misstate it.

## Implementation Hints

- Local Ollama confirmed running at `http://localhost:11434` (server `/api/version` →
  `0.32.5`).
- Models already pulled: `gemma4:e2b-mlx` (ID `e98b54fe1913`, 6.5 GB) and
  `gpt-oss:20b` (ID `117d0d84cf2a`, 7.7 GB). Neither needs pulling. Use
  `MODEL=gemma4:e2b-mlx` default, `gpt-oss:20b` to confirm.
- Tools present on PATH: `curl`, `jq`, `python3` (pyenv shim), `ruby`, `perl`,
  `ollama`. The inspection script may use `jq` and/or `python3` for SSE parsing; both are
  installed.
- The target endpoint is the raw OpenAI-compatible spec:
  `POST http://localhost:11434/v1/chat/completions` with JSON body fields `model`,
  `messages`, `tools`, `tool_choice`, `stream`.
- Reference `delta.tool_calls` shape (OpenRouter, per §4A `DeltaChunk`): SSE chunks carry
  `choices[*].delta.tool_calls[*].index`, `.id`, `.function.name` (may be `null`/absent on
  pure-arguments chunks), and `.function.arguments` (incremental JSON string fragments,
  accumulated across chunks).
- The prior two spikes are crates (`spikes/spike-http-stream-cancel/Cargo.toml` shows
  `edition = "2024"`, `reqwest 0.12` `json`+`stream`, `eventsource-stream 0.2`); this spike
  intentionally diverges (shell script only) per the delivery-form decision — do not add it
  to the root `Cargo.toml` or the workspace.
- `docs/blueprint.md` §0 Spike 3 spans lines ~108-137; the "**Spike 3 outcome:**" placeholder
  is at line 137. The unified-provider claims to revise on failure are at §4A (line ~321)
  and §5. Cite these when writing the outcome note.
- `AGENTS.md`: docs are part of the change set; only append the outcome note to
  `docs/blueprint.md` §0 Spike 3 and add the spike's own `README.md`; ask before creating any
  other new doc file.
