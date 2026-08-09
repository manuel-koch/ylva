#!/usr/bin/env bash
#
# Spike 3: Ollama OpenAI-Compatible Tool Calling — OpenRouter capture + grading.
#
# POSTs a streaming chat-completions request to OpenRouter's OpenAI-compatible
# endpoint, captures the raw SSE stream, grades it against the spike's success
# criteria (inspect_sse.py), and prints the observed delta.tool_calls key shape
# (inspect_sse.py --shape). Compare that shape with the one from
# inspect_ollama_tool_calls.sh to confirm the byte-for-byte unified-provider
# claim across both backends.
#
# Requires OPENROUTER_API_KEY.
#
# See: docs/plans/spike-ollama-tool-calling.md
#
# Env overrides:
#   OPENROUTER_BASE_URL   OpenRouter base URL (default https://openrouter.ai/api/v1)
#   OPENROUTER_MODEL      OpenRouter model (default deepseek/deepseek-v4-flash-0731, moonshotai/kimi-k2.6)
#   OPENROUTER_API_KEY    required
#   TOOL_CHOICE           tool_choice value (default auto)
#   SPIKE_OUT_DIR         output dir for the raw SSE log (default <script>/out)

set -euo pipefail

OPENROUTER_BASE_URL="${OPENROUTER_BASE_URL:-https://openrouter.ai/api/v1}"
OPENROUTER_MODEL="${OPENROUTER_MODEL:-deepseek/deepseek-v4-flash-0731}"
TOOL_CHOICE="${TOOL_CHOICE:-auto}"

if [ -z "${OPENROUTER_API_KEY:-}" ]; then
    echo "FAIL: OPENROUTER_API_KEY is required." >&2
    exit 1
fi

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${SPIKE_OUT_DIR:-${SCRIPTS_DIR}/out}"
mkdir -p "${OUT_DIR}"

# Build a request body with a tools array + a user turn that invites a tool call.
request_body() {
    cat <<EOF
{
  "model": "${OPENROUTER_MODEL}",
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

sse_log="${OUT_DIR}/sse_openrouter_${OPENROUTER_MODEL//[:\/]/_}.log"

echo "== OpenRouter tool-calling capture =="
echo "base_url: ${OPENROUTER_BASE_URL}"
echo "model:    ${OPENROUTER_MODEL}"
echo "endpoint: ${OPENROUTER_BASE_URL}/chat/completions"
echo

set +e
http_code="$(request_body | curl -N -s -o "${sse_log}" -w '%{http_code}' \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer ${OPENROUTER_API_KEY}" \
    -d @- "${OPENROUTER_BASE_URL}/chat/completions")"
curl_rc=$?
set -e

echo "curl exit code:   ${curl_rc}"
echo "HTTP response:    HTTP/1.1 ${http_code}"
echo "Raw SSE captured to: ${sse_log}"
echo

if [ "${curl_rc}" -ne 0 ]; then
    echo "FAIL: curl to OpenRouter failed (exit ${curl_rc})."
    exit 1
fi

grade_rc=0
python3 "${SCRIPTS_DIR}/inspect_sse.py" "${sse_log}" "${http_code}" || grade_rc=$?
echo

echo "== delta.tool_calls shape (for comparison with Ollama) =="
python3 "${SCRIPTS_DIR}/inspect_sse.py" --shape "${sse_log}"
echo

exit "${grade_rc}"
