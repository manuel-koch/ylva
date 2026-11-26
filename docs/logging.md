# Reading Ylva logs

Logging is opt-in and controlled by CLI flags, not a configuration file.
Use `--log-file` to capture a TUI or CLI run.

## Text and JSON logs

- **Text** is for reading directly. Each event shows its timestamp, level,
  message and fields, active spans, and abbreviated source location when
  available.
- **JSON** is for filtering or analysis. Each event is a separate JSON object
  on one line (JSONL). The message and event fields are in `fields`; for scoped
  events, `spans` lists the active spans from outermost to innermost.

The first startup event, `build info` at `INFO`, carries `version`, `commit`,
`built`, and `profile` (the same values as `ylva --version`). It appears for
subcommands too. In a chat run, the `starting` event identifies the app as
`cli` or `tui` in its `app` field.

## Correlating events

Use the `session` span's `session_id` to group work. After `/clear`, new work
uses the replacement ID; work already started keeps its original ID, including
requests, tools, compaction, title generation, and writes. In a `subagent` span,
`run_id` identifies the subagent run and `parent_session_id` links it to the
parent session. Use `tool_call_id` to group events for one tool invocation.

## Tool approval trail

With `--log-level info` or more verbose logging, look for `tool approval trail`
events. A policy check reports the effective rule or fallback, its decision
maker, and the outcome. A later disposition can record a user's approval or
denial, a saved or matching grant, or how the headless CLI resolved a pending
request. A recheck gets its own event and phase.

- `record_id` identifies one trail event. IDs are assigned by a counter shared
  across the Ylva process, not per session or per tool call.
- `parent_id`, when present, references another event's `record_id` in that
  same process. Follow the link to the earlier check or decision that this event
  follows from. It need not be the previous line in the file.

For example, these abbreviated/wrapped text records concern the same Bash call:

```text
INFO tool approval trail \
     tool=bash \
     tool_call_id=<call-id> \
     phase="admission" \
     outcome="user_approval_required" \
     decision_maker="tool" \
     rule_locator="builtin:bash.default" \
     record_id=2
....
INFO tool approval trail \
     tool=bash \
     tool_call_id=<call-id> \
     phase="disposition" \
     outcome="grant_and_approve" \
     decision_maker="human" \
     rule_locator="host.grant_and_approve" \
     record_id=3 parent_id=2
```

- Record `2` says the tool's fallback required user approval.
- Record `3` says the user saved a grant and approved that call.
  Its `parent_id=2` connects the resolution to the check that prompted it.
  In JSON, the same IDs appear as numbers under `fields`.

### Rule locators

The locator identifies the effective rule, not every rule that matched. For
example, `ylva_home:tools.bash.rules[1]` is the second entry in the Ylva-home
profile's Bash rules, and `workspace:agents.reviewer.tools.bash.rules[0]`
is the first entry in the workspace profile's inline `reviewer` agent rules.
Standalone agent-type files use `agent_file:<scope>:<name>.<format>` followed
by the rule key and index in the TOML or Markdown front matter. Indices are
zero-based in the original list. Non-simple identifiers are shown as
`sha256-64:` followed by the first 16 hexadecimal digits of their SHA-256
digest. A CLI override names its exact flag (for example,
`cli:--auto-approve-bash`), and a built-in rule uses `builtin:<identifier>`.
See [sandbox configuration](./configuration.md#sandbox-tool-approval-rules)
for how rules are selected.

## Other diagnostics

- At `DEBUG`, `ylva session` logs `resolved session store` with `cwd`,
  `chats_root`, and `project_key`. Use it to diagnose an empty `session list`
  when the run started in an unexpected directory.
- At `DEBUG`, `agent command received` records the `AgentCommand` variant
  in `command`. Payloads are represented by lengths or correlation IDs
  rather than their contents. Approval outcomes are separate events.
- Session-title lifecycle events report `stage` values such as `scheduled`,
  `request_started`, `response_received`, `normalized`, `persist_started`,
  and `persisted`.
  Failures and skips include a reason or an error stage. These events include
  the session ID, selected utility engine, provider and model, and character
  counts, not prompt, reply, or title text.
- Provider and stream failures, tool failures, and fatal startup errors are
  logged when a logfile is enabled. Fatal startup errors flush the logfile
  before exit. A provider stream failure produces one user-facing error even
  if lower layers log separate diagnostics.

## Before sharing logs

Approval trail events omit tool arguments, raw rule and grant text, absolute
paths, URLs, credentials, and free-form denial reasons. They use opaque grant
references and omit grant previews and `clarify` exchanges.

That guarantee applies to the trail, not the whole logfile. For example, the
Bash sandbox's `DEBUG` check logs the command itself. In text logs, that check
quotes the command and escapes embedded quotes and newlines; escaping does not
redact content. Tool argument spans can also include command text. Review logs
for sensitive content before sharing them.
