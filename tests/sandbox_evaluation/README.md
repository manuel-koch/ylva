# Offline Bash sandbox evaluation

These tools collect Bash invocations and evaluate them through Ylva's production
extractor and approval rules. **Recorded commands are never executed.**

Run all commands below from the repository root. The exporter requires Python
3.10 or later, using only its standard library. The evaluator uses the project's
normal Rust toolchain.

## Evaluate a collection

The local historical corpus is at
`tests/sandbox_evaluation/data/copilot-bash-commands.json`.
Evaluate it with an explicit Bash policy:

To evaluate extracted corpus of
`data/sample.report.json`
using the bash policies, run
`make data/sample.report.json`.

Cargo registers [`bash_policy_replay.rs`](bash_policy_replay.rs) as an example
target even though its source lives here. Substitute any compatible JSON corpus.
`--rules` reads only the explicit `[tools.bash].rules` array from the given TOML
file, not layered profiles, agent overrides, `.env` files, provider credentials
or CLI auto-approval overrides. Omit it to evaluate the unconfigured default floor.
A supplied policy must contain a rules array; `rules = []` selects an empty
configured policy.

The formatted JSON report contains summary counts and ordered per-command results.
Each result preserves the command and provenance, reports `sandbox_verdict`
(`null` means abstention), and supplies the effective `verdict` after the Bash
tool's approval-required fallback. Denials include their reason. Auto-approval
is a policy verdict, not proof of read-only execution.

## Export agent session history

Extract bash invocations from agent sessions using `make gather_bash_calls`.

### Export Copilot session history

[`export_copilot_bash.py`](export_copilot_bash.py) creates a collection directly
from local `events.jsonl` files, without intermediate evaluation artifacts:

Point the evaluator at the new output file to evaluate the refreshed collection.
The exporter refuses to overwrite existing files, so manually extended corpora
are not lost.

- Omit `--since` and `--until` to collect all available matching history.
  Bounds are inclusive invocation timestamps and must include a timezone.
- `--sessions-root` defaults to `~/.copilot/session-state`.
- Repeat `--exclude-session ID` to omit sessions.
- Only Bash tool starts from sessions whose initial cwd matches `--workspace`
  are collected. Repeated events for one tool call are deduplicated; separate
  invocations of identical scripts remain separate entries.
- Completion success is recorded independently and does not prove shell execution.
  Malformed matching session data and empty results fail explicitly.

The exporter creates formatted JSON with owner-only permissions. Raw corpora and
reports belong in the Git-ignored `data/` directory: historical commands may
contain credentials, private paths and command payloads. The local corpus is not
distributed with a fresh checkout; export one before evaluating it there.
Only small, reviewed fixtures should be committed.

`grammar-expectations.json` is a committed fixture with verdict expectations for
the grammar rules in `sample.config.toml`; run it with `--rules sample.config.toml`.

## Extend a collection or add expectations

Append command entries with unique, nonblank IDs and nonblank command strings:

```json
{
  "schema_version": 1,
  "description": "Optional description",
  "source": {"origin": "Optional provenance"},
  "commands": [
    {
      "id": "redirection-example",
      "command": "printf example > must-not-be-created.txt",
      "expected_verdict": "user_approval_required",
      "source": {"note": "Optional per-command provenance"}
    }
  ]
}
```

`description`, `source`, and `expected_verdict` are optional. Expected verdicts
compare against the effective verdict and accept `auto_approved`, `auto_denied`,
or `user_approval_required`. An unmet expectation produces a nonzero exit status
**after** writing the report. Cases without expectations only contribute statistics.
Invalid inputs, unsupported schema versions, duplicate IDs, unknown corpus/entry
fields, empty collections, invalid policies and output failures also fail.

## Run the tool tests

The Rust example tests also run in the normal test and coverage gates. The Python
exporter tests are explicit and do not add a Makefile target. Changes still follow
the project's [verification policy](../../docs/development-workflow.md).

```shell
cd tests/sandbox_evaluation
make test
```
