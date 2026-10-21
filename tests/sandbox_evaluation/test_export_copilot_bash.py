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

import json
import tempfile
import unittest
from pathlib import Path

from export_copilot_bash import collect_corpus, parse_timestamp, write_corpus


class ExportCopilotBashTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.workspace = self.root / "workspace"
        self.workspace.mkdir()

    def write_session(self, name, events, workspace=None):
        path = self.root / name / "events.jsonl"
        path.parent.mkdir()
        start = {
            "type": "session.start",
            "data": {"context": {"cwd": str(workspace or self.workspace)}},
        }
        path.write_text("\n".join(json.dumps(event) for event in [start, *events]) + "\n")
        return path

    def invocation(self, call_id, command, timestamp="2026-10-21T12:00:00Z"):
        return {
            "type": "tool.execution_start",
            "timestamp": timestamp,
            "data": {
                "toolCallId": call_id,
                "toolName": "bash",
                "arguments": {"command": command},
            },
        }

    def collect(self, since=None, until=None, excluded_sessions=None):
        return collect_corpus(
            self.root, self.workspace, since, until, excluded_sessions or set()
        )

    def test_export_preserves_commands_frequency_provenance_and_completion(self):
        marker = self.root / "must-not-exist"
        command = f"printf data > '{marker}'\n# keep this verbatim"
        first = self.invocation("one", command)
        second = self.invocation("two", command)
        second["data"]["arguments"] = json.dumps(second["data"]["arguments"])
        second["data"]["toolName"] = "functions.bash"
        self.write_session("session", [
            first, first, second,
            {"type": "tool.execution_complete", "data": {"toolCallId": "one", "success": True}},
        ])

        corpus = self.collect()

        self.assertEqual([entry["command"] for entry in corpus["commands"]], [command, command])
        self.assertEqual([entry["id"] for entry in corpus["commands"]], ["session/one", "session/two"])
        self.assertEqual(corpus["commands"][0]["source"]["event_line"], 2)
        self.assertTrue(corpus["commands"][0]["source"]["tool_completion_success"])
        self.assertIsNone(corpus["commands"][1]["source"]["tool_completion_success"])
        self.assertFalse(marker.exists())

    def test_workspace_time_boundaries_and_exclusions_filter_invocations(self):
        self.write_session("included", [
            self.invocation("before", "cat before", "2026-10-21T11:59:59Z"),
            self.invocation("boundary", "cat included", "2026-10-21T14:00:00+02:00"),
            self.invocation("after", "cat after", "2026-10-21T12:00:01Z"),
        ])
        self.write_session("excluded", [self.invocation("excluded", "cat excluded")])
        self.write_session("other", [self.invocation("other", "cat other")], self.root / "other-workspace")
        boundary = parse_timestamp("2026-10-21T12:00:00Z")

        corpus = self.collect(boundary, boundary, {"excluded"})

        self.assertEqual([entry["id"] for entry in corpus["commands"]], ["included/boundary"])
        self.assertEqual(corpus["source"]["sessions"], 1)

    def test_invalid_records_and_conflicting_duplicates_fail_with_source_location(self):
        cases = [
            [self.invocation("one", "cat first"), self.invocation("one", "cat second")],
            [self.invocation("one", 42)],
            [self.invocation("one", "cat file", "not-a-timestamp")],
        ]
        for index, events in enumerate(cases):
            with self.subTest(index=index):
                path = self.write_session(str(index), events)
                with self.assertRaisesRegex(ValueError, "events.jsonl:[23]:"):
                    self.collect()
                path.unlink()
        path = self.write_session("malformed", [])
        with path.open("a") as stream:
            stream.write("{invalid\n")
        with self.assertRaisesRegex(ValueError, "events.jsonl:2:"):
            self.collect()

    def test_empty_results_invalid_ranges_and_naive_timestamps_are_errors(self):
        with self.assertRaisesRegex(ValueError, "no Bash invocations"):
            self.collect()
        with self.assertRaisesRegex(ValueError, "later than"):
            self.collect(parse_timestamp("2026-10-22T00:00:00Z"), parse_timestamp("2026-10-21T00:00:00Z"))
        with self.assertRaisesRegex(ValueError, "timezone"):
            parse_timestamp("2026-10-21T00:00:00")

    def test_output_is_formatted_private_and_never_overwrites_extended_corpus(self):
        self.write_session("session", [self.invocation("one", "cat file")])
        corpus = self.collect()
        output = self.root / "data" / "commands.json"
        write_corpus(output, corpus)

        self.assertEqual(json.loads(output.read_text()), corpus)
        self.assertIn('\n  "schema_version": 1,', output.read_text())
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)
        with self.assertRaises(FileExistsError):
            write_corpus(output, {"commands": []})
        self.assertEqual(json.loads(output.read_text()), corpus)


if __name__ == "__main__":
    unittest.main()
