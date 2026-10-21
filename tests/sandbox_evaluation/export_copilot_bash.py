#!/usr/bin/env python3
# Copyright © 2026 Manuel Koch
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Export recorded Bash tool starts as replay JSON without executing commands."""

import argparse
import json
import os
from datetime import UTC, datetime
from pathlib import Path


def parse_timestamp(value):
    if not isinstance(value, str):
        raise TypeError("timestamp must be a string")
    timestamp = datetime.fromisoformat(value)
    if timestamp.tzinfo is None:
        raise ValueError(f"timestamp must include a timezone: {value}")
    return timestamp.astimezone(UTC)


def read_event(line, path, line_number):
    try:
        event = json.loads(line)
    except json.JSONDecodeError as error:
        raise ValueError(f"{path}:{line_number}: {error}") from error
    if not isinstance(event, dict) or not isinstance(event.get("data"), dict):
        raise TypeError(f"{path}:{line_number}: event must contain a data object")
    return event


def session_commands(path, workspace, since, until):
    with path.open(encoding="utf-8") as stream:
        first = read_event(stream.readline(), path, 1)
        if first.get("type") != "session.start":
            raise ValueError(f"{path}:1: expected session.start")
        if first["data"].get("context", {}).get("cwd") != str(workspace):
            return []
        starts = {}
        completions = {}
        for line_number, line in enumerate(stream, 2):
            event = read_event(line, path, line_number)
            data = event["data"]
            call_id = data.get("toolCallId")
            if event.get("type") == "tool.execution_complete":
                completions[call_id] = data.get("success")
            if (
                event.get("type") != "tool.execution_start"
                or data.get("toolName") not in ("bash", "functions.bash")
            ):
                continue
            try:
                timestamp = parse_timestamp(event["timestamp"])
                arguments = data["arguments"]
                if isinstance(arguments, str):
                    arguments = json.loads(arguments)
                command = arguments["command"]
            except (KeyError, TypeError, ValueError) as error:
                raise ValueError(f"{path}:{line_number}: invalid Bash invocation: {error}") from error
            if (since is not None and timestamp < since) or (
                until is not None and timestamp > until
            ):
                continue
            if not isinstance(call_id, str) or not call_id.strip():
                raise ValueError(f"{path}:{line_number}: missing toolCallId")
            if not isinstance(command, str) or not command.strip():
                raise ValueError(f"{path}:{line_number}: command must be a nonblank string")
            if call_id in starts:
                if starts[call_id]["command"] != command:
                    raise ValueError(f"{path}:{line_number}: conflicting duplicate {call_id}")
                continue
            starts[call_id] = {
                "id": f"{path.parent.name}/{call_id}",
                "command": command,
                "source": {
                    "session_id": path.parent.name,
                    "tool_call_id": call_id,
                    "event_line": line_number,
                    "timestamp": event["timestamp"],
                },
            }
        for call_id, record in starts.items():
            record["source"]["tool_completion_success"] = completions.get(call_id)
        return list(starts.values())


def collect_corpus(sessions_root, workspace, since, until, excluded_sessions):
    if not sessions_root.is_dir():
        raise ValueError(f"session root is not a directory: {sessions_root}")
    if since is not None and until is not None and since > until:
        raise ValueError("--since must not be later than --until")
    commands = []
    for path in sorted(sessions_root.glob("*/events.jsonl")):
        if path.parent.name not in excluded_sessions:
            commands.extend(session_commands(path, workspace, since, until))
    if not commands:
        raise ValueError("no Bash invocations matched the workspace and filters")
    return {
        "schema_version": 1,
        "description": "Recorded Bash invocations for static policy replay. Never execute these commands.",
        "source": {
            "workspace": str(workspace),
            "start_timestamp_inclusive": since.isoformat() if since is not None else None,
            "end_timestamp_inclusive": until.isoformat() if until is not None else None,
            "excluded_sessions": sorted(excluded_sessions),
            "sessions": len({record["source"]["session_id"] for record in commands}),
        },
        "commands": commands,
    }


def write_corpus(output, corpus):
    content = json.dumps(corpus, indent=2, ensure_ascii=False) + "\n"
    output.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        stream.write(content)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--sessions-root", type=Path, default=Path.home() / ".copilot/session-state"
    )
    parser.add_argument("--workspace", type=Path, default=Path.cwd())
    parser.add_argument("--since", help="Inclusive invocation timestamp, including timezone")
    parser.add_argument("--until", help="Inclusive invocation timestamp, including timezone")
    parser.add_argument("--exclude-session", action="append", default=[])
    parser.add_argument("--output", type=Path, required=True, help="New JSON file; never overwritten")
    arguments = parser.parse_args()
    try:
        corpus = collect_corpus(
            arguments.sessions_root.expanduser(),
            arguments.workspace.expanduser().resolve(),
            parse_timestamp(arguments.since) if arguments.since is not None else None,
            parse_timestamp(arguments.until) if arguments.until is not None else None,
            set(arguments.exclude_session),
        )
        write_corpus(arguments.output, corpus)
    except (OSError, ValueError) as error:
        parser.exit(1, f"export failed: {error}\n")
    print(f"Saved {len(corpus['commands'])} Bash invocations to {arguments.output}")


if __name__ == "__main__":
    main()
