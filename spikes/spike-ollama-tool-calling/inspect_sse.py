#!/usr/bin/env python3
"""Spike 3: Ollama OpenAI-compatible tool calling - SSE inspector.

Reads a raw SSE exchange from a file (the /v1/chat/completions stream),
inspects every `data:` line for `delta.tool_calls`, assembles the incremental
`function.name` / `function.arguments` fragments per `tool_call_index`, checks
the delta key/nesting shape against the OpenRouter reference, and prints a
structured summary with PASS/FAIL verdicts for the blueprint's success criteria.

Usage: inspect_sse.py <sse_log_path> [http_code]
"""
import json
import sys


def parse_sse_events(raw_text):
    """Yield (kind, payload) tuples for each SSE event in raw_text.

    kind is "json" when the data parses as JSON, else "text". Comment lines
    (starting with ':') and empty lines are skipped.
    """
    for line in raw_text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith(":"):
            continue
        if not stripped.startswith("data:"):
            continue
        payload = stripped[len("data:"):].strip()
        if payload == "[DONE]":
            continue
        try:
            parsed = json.loads(payload)
        except ValueError:
            yield "text", payload
        else:
            yield "json", parsed


class ToolCallAccumulator:
    """Accumulate incremental tool-call fragments per `index`."""

    def __init__(self):
        self.fragments_by_index = {}

    def absorb(self, tool_call):
        index = tool_call.get("index")
        function = tool_call.get("function") or {}
        name = function.get("name")
        arguments = function.get("arguments")
        if index is None:
            return
        entry = self.fragments_by_index.setdefault(
            index, {"name": "", "name_chunks": 0, "arguments": "", "arguments_chunks": 0}
        )
        if isinstance(name, str) and name:
            entry["name"] += name
            entry["name_chunks"] += 1
        if isinstance(arguments, str) and arguments:
            entry["arguments"] += arguments
            entry["arguments_chunks"] += 1

    def summary(self):
        return [
            {"index": index, **fragments}
            for index, fragments in sorted(self.fragments_by_index.items())
        ]


ALLOWED_TOOL_CALL_KEYS = {"index", "id", "type", "function"}
ALLOWED_FUNCTION_KEYS = {"name", "arguments"}


def shape_keys(parsed):
    """Return the canonical set of delta.tool_calls key paths observed.

    Used to diff the byte-for-byte shape between two captures (e.g. Ollama vs
    OpenRouter). Each path is normalized so index values are ignored.
    """
    paths = set()
    for message in parsed.get("choices") or []:
        delta = message.get("delta") or {}
        tool_calls = delta.get("tool_calls")
        if not tool_calls:
            continue
        for tool_call in tool_calls:
            if not isinstance(tool_call, dict):
                continue
            for key in ALLOWED_TOOL_CALL_KEYS:
                if key in tool_call:
                    paths.add(f"tool_calls[].{key}")
            function = tool_call.get("function")
            if isinstance(function, dict):
                for key in ALLOWED_FUNCTION_KEYS:
                    if key in function:
                        paths.add(f"tool_calls[].function.{key}")
    return paths


def collect_shape_keys(raw_text):
    """Aggregate shape_keys across every data line of an SSE capture."""
    keys = set()
    for kind, parsed in parse_sse_events(raw_text):
        if kind == "json":
            keys.update(shape_keys(parsed))
    return keys


def extract_tool_calls(parsed):
    """Return (tool_calls, unexpected_keys) from a parsed delta chunk.

    Only considers chunks that actually carry `delta.tool_calls`. unexpected_keys
    collects any keys outside the OpenAI/OpenRouter reference shape.
    """
    all_messages = parsed.get("choices") or []
    tool_calls = []
    unexpected_keys = set()
    for message in all_messages:
        delta = message.get("delta") or {}
        chunk_tool_calls = delta.get("tool_calls")
        if not chunk_tool_calls:
            continue
        for tool_call in chunk_tool_calls:
            if not isinstance(tool_call, dict):
                continue
            unexpected_keys.update(set(tool_call) - ALLOWED_TOOL_CALL_KEYS)
            function = tool_call.get("function") or {}
            unexpected_keys.update(set(function) - ALLOWED_FUNCTION_KEYS)
            tool_calls.append(tool_call)
    return tool_calls, unexpected_keys


def main(argv):
    if len(argv) < 1:
        print("usage: inspect_sse.py <sse_log_path> [http_code] | --shape <sse_log_path>",
              file=sys.stderr)
        return 2

    if argv[0] == "--shape":
        if len(argv) < 2:
            print("usage: inspect_sse.py --shape <sse_log_path>", file=sys.stderr)
            return 2
        with open(argv[1], encoding="utf-8") as handle:
            raw_text = handle.read()
        for key in sorted(collect_shape_keys(raw_text)):
            print(key)
        return 0

    sse_log_path = argv[0]
    http_code = argv[1] if len(argv) > 1 else "unknown"

    with open(sse_log_path, encoding="utf-8") as handle:
        raw_text = handle.read()

    events = list(parse_sse_events(raw_text))

    print("== SSE inspection ==")
    print(f"sse_log:    {sse_log_path}")
    print(f"http_code:  {http_code}")
    print(f"data lines: {len(events)}")

    if not raw_text.strip():
        print("ERROR: empty SSE log (no data received).")
        return 1

    if not any(line.strip().startswith("data:") for line in raw_text.splitlines()):
        print("\nFAIL: response contains no `data:` SSE events "
              "(endpoint rejected the body or returned non-SSE content):")
        print(raw_text.strip()[:2000])
        print("\nVerdicts:")
        print("  A. accepts tools and streams delta.tool_calls  -> FAIL (no SSE stream)")
        print("  B. byte-for-byte OpenRouter-compatible shape   -> FAIL (not evaluated)")
        return 1

    accumulator = ToolCallAccumulator()
    tool_call_chunk_count = 0
    all_unexpected_keys = set()
    for kind, parsed in events:
        if kind != "json":
            continue
        tool_calls, unexpected_keys = extract_tool_calls(parsed)
        if tool_calls:
            tool_call_chunk_count += 1
            all_unexpected_keys.update(unexpected_keys)
            for tool_call in tool_calls:
                accumulator.absorb(tool_call)

    print(f"chunks with delta.tool_calls: {tool_call_chunk_count}")

    summary = accumulator.summary()
    if summary:
        print("\nIncremental tool-call fragments per tool_call_index:")
        for entry in summary:
            print(f"  index {entry['index']}:")
            print(f"    function.name      = {entry['name']!r} ({entry['name_chunks']} chunk(s))")
            print(f"    function.arguments = {entry['arguments']!r} ({entry['arguments_chunks']} chunk(s))")
    else:
        print("\nNo delta.tool_calls found in the stream.")

    print("\nShape check (against OpenRouter reference):")
    if all_unexpected_keys:
        print(f"  FAIL: unexpected delta keys present: {sorted(all_unexpected_keys)}")
        shape_ok = False
    else:
        print("  PASS: observed keys within tool_calls[i].{type,index,id,function} "
              "and .function.{name,arguments}")
        shape_ok = True

    http_ok = str(http_code) == "200"
    tool_calls_ok = tool_call_chunk_count > 0
    incremental_ok = bool(summary) and any(
        entry["name_chunks"] > 0 or entry["arguments_chunks"] > 0
        for entry in summary
    )

    criterion_a = http_ok and tool_calls_ok and incremental_ok
    criterion_b = shape_ok and tool_calls_ok

    print("\nVerdicts (Spike 3 success criteria):")
    details_a = []
    if not http_ok:
        details_a.append(f"http={http_code}")
    if not tool_calls_ok:
        details_a.append("no delta.tool_calls chunks")
    if not incremental_ok:
        details_a.append("no incremental name/arguments fragments")
    print(f"  A. accepts tools and streams delta.tool_calls  -> "
          f"{'PASS' if criterion_a else 'FAIL'}"
          + (f" ({', '.join(details_a)})" if details_a else ""))
    print(f"  B. byte-for-byte OpenRouter-compatible shape   -> "
          f"{'PASS' if criterion_b else 'FAIL'}")
    print()
    if not tool_calls_ok and http_ok:
        print("NOTE: HTTP 200 but no tool calls: the endpoint likely ignored tools "
              "and returned plain text. Per the plan this invalidates the unified "
              "provider -> an ollama_native provider is required.")

    return 0 if (criterion_a and criterion_b) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
