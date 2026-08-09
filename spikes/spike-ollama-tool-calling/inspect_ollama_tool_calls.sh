#!/usr/bin/env bash
#
# Spike 3: Ollama OpenAI-Compatible Tool Calling — Ollama capture + grading.
#
# POSTs a streaming chat-completions request to local Ollama's
# /v1/chat/completions endpoint, captures the raw SSE stream, grades it against
# the spike's success criteria (inspect_sse.py), and prints the observed
# delta.tool_calls key shape (inspect_sse.py --shape) so it can be compared with
# the OpenRouter capture.
#
# See: docs/plans/spike-ollama-tool-calling.md
#
# Env overrides:
#   OLLAMA_BASE_URL   base URL (default http://localhost:11434)
#   OLLAMA_MODEL      Ollama model name (default gemma4:e2b-mlx; gpt-oss:20b confirms)
#   TOOL_CHOICE       tool_choice value (default auto)
#   SPIKE_OUT_DIR     output dir for the raw SSE log (default <script>/out)
#
# After the run the model is unloaded (`ollama stop`) so the next Ollama model
# under test is loaded fresh rather than reusing the previous model's context.

set -euo pipefail

OLLAMA_BASE_URL="${OLLAMA_BASE_URL:-http://localhost:11434}"
OLLAMA_MODEL="${OLLAMA_MODEL:-gemma4:e2b-mlx}"
TOOL_CHOICE="${TOOL_CHOICE:-auto}"

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${SPIKE_OUT_DIR:-${SCRIPTS_DIR}/out}"
mkdir -p "${OUT_DIR}"

# Build a request body with a tools array + a user turn that invites a tool call.
request_body() {
    cat <<EOF
{
  "model": "${OLLAMA_MODEL}",
  "messages": [
    {"role": "user", "content": "What is the weather currently like in Berlin in Fahrenheit?"}
  ],
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "get_weather",
        "description": "Get the current weather for a location",
        "parameters": {
          "type": "object",
          "properties": {
            "location": {"type": "string", "description": "City and country, e.g. Berlin, Germany"},
            "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}
          },
          "required": ["location", "unit"]
        }
      }
    }
  ],
  "tool_choice": "${TOOL_CHOICE}",
  "stream": true
}
EOF
}

sse_log="${OUT_DIR}/sse_ollama_${OLLAMA_MODEL//[:\/]/_}.log"

echo "== Ollama tool-calling capture =="
echo "base_url: ${OLLAMA_BASE_URL}"
echo "model:    ${OLLAMA_MODEL}"
echo "endpoint: ${OLLAMA_BASE_URL}/v1/chat/completions"
echo

set +e
http_code="$(request_body | curl -N -s -o "${sse_log}" -w '%{http_code}' \
    -H "Content-Type: application/json" \
    -d @- "${OLLAMA_BASE_URL}/v1/chat/completions")"
curl_rc=$?
set -e

echo "curl exit code:   ${curl_rc}"
echo "HTTP response:    HTTP/1.1 ${http_code}"
echo "Raw SSE captured to: ${sse_log}"
echo

if [ "${curl_rc}" -ne 0 ]; then
    echo "FAIL: curl to Ollama failed (exit ${curl_rc})."
    ollama stop "${OLLAMA_MODEL}" || true
    exit 1
fi

grade_rc=0
python3 "${SCRIPTS_DIR}/inspect_sse.py" "${sse_log}" "${http_code}" || grade_rc=$?
echo

echo "== delta.tool_calls shape (for comparison with OpenRouter) =="
python3 "${SCRIPTS_DIR}/inspect_sse.py" --shape "${sse_log}"
echo

# Unload the model so the next Ollama model under test is loaded fresh.
echo "Unloading model (ollama stop):"
ollama stop "${OLLAMA_MODEL}" || true

exit "${grade_rc}"
