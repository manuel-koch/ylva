# CLI subagent output stream handling

Deferred findings from analyzing the headless CLI (`--no-tui`) rendering of
`spawn_subagent` runs, motivated by the release-notes workflow
(`.github/workflows/release-notes.sh` + `release-notes.config.toml`). Two
symptoms were reported after enabling `--auto-approve-spawn`: tool output
overlapping mid-line with the root agent's streamed text, and the
`spawn_subagent` invocation line showing no task/briefing. A third question
arose while inspecting the output: which agent is actually "thinking" behind
`---[ Assistant Thinking ]---`.

All findings are analysis only — no behavior change is implied by this
document.

## Root cause 1: subagent tool lines land mid-stream

The CLI prints assistant thinking/response chunks with `write!(out, …)` and no
trailing newline; the open section is closed only by `flush_stream_section`
(`src/cli_driver/output.rs`). Some update arms in `src/cli_driver/run.rs`
flush before printing their own line, others do not:

| Update arm                                                        | Flushes first |
| ----------------------------------------------------------------- | ------------- |
| `ToolPending`, `ToolStart`, `ToolAutoDenied`, `ClarifyPending`, `Notice`, "running…" slow-tool timer | yes           |
| `ToolDone`, `ToolError`, `ToolKilled`, `Error { run_id: Some }`, `OutputTruncated`                  | no            |

The root loop forwards subagent UI updates concurrently through the `select!`
in `src/agent/runtime.rs` (`recv_subagent_ui` →
`RunState::forward_subagent_update`), so a subagent's `ToolDone`/`ToolError`
can arrive at any moment while the root's Response/Thinking section is open.
Its `[subagent <run_id>] [tool] … done` line is then appended at the current
cursor position — mid-line. The TUI never had this problem because bubbles are
separate widgets; the CLI's inline-stream model simply misses the "close the
section before printing a line" discipline in the five arms listed above.

Fix shape: add the same `flush_stream_section` call the flushing arms already
make. Mechanical, confined to `src/cli_driver/run.rs`.

## Root cause 2: no task/briefing shown when the subagent spawns

With `--auto-approve-spawn` there is no `ToolPending` step; the call goes
straight to `ToolStart`. The CLI's `ToolStart` arm only records the call id for
end-of-run bookkeeping and ignores `content_sections` entirely.

The sections exist: `resolve_spawn_subagent`
(`src/agent/tool_queue/spawn.rs`) computes `pending_content_sections()` (the
Task/Briefing markdown), stashes them in the loop's `spawn_content_sections`
map, and the eventual real `ToolDone` (sent only when the subagent actually
finishes, from `handle_subagent_finish`) attaches them again. So the task and
briefing only ever print at completion time, never at spawn time.

Until then the user sees just the label in the slow-tool line, e.g.
`[tool] Spawn Subagent "Clarify release fix details" running…`.

Fix shape: render `content_sections` in the `ToolStart` arm. For
`spawn_subagent` that prints Task/Briefing at spawn; for other tools
`pending_content_sections` is what the approval dialog would have shown, so
this also aligns auto-approved tools with the TUI, which shows them from the
bubble's creation.

## Root cause 3: thinking attribution is implicit

In CLI mode, subagent text/thinking is deliberately dropped: the subagent's
tokens arrive as `UiUpdate::SubagentToken { run_id, is_thinking }` (attributed
in `agent_factory.rs::attribute_subagent_update`), but the CLI's arm for
`SubagentStarted`/`SubagentEngineConfigured`/`SubagentToken`/`SubagentDone` is
a no-op ("headless mode has no separate subagent transcript to render into").

Consequence: every `---[ Assistant Thinking ]---` header in headless output is
the root's. The ambiguity is a labeling gap, not misattribution — untagged
output is the root, tagged lines (`[subagent <run_id>] …`) are subagent
events, and nothing states that convention.

Two directions:

- Minimal: leave behavior as-is and document the convention (e.g. in
  `docs/features.md`'s CLI section).
- Fuller: render subagent thinking/text headless as tagged sections (e.g.
  `---[ subagent <run_id> Thinking ]---`). Needs per-run stream-section state
  in the CLI loop (today there is a single `current_section` for the root), so
  it is a larger change than the other two fixes.

## Related, resolved earlier: spawn denied in CLI mode

The original blocker, root-caused separately: `spawn_subagent` was registered
but not auto-approved. `allowed_tools` only decides registration (and
`spawn_subagent` is not even gated by it — it is structural via `can_spawn`,
which defaults to `true`). Approval for `spawn_subagent` cannot be configured
via `[tools.*]` rules at all; `SpawnSubagentTool::needs_approval` holds only
the `--auto-approve-spawn` override. The headless CLI denies anything
requiring approval (`CLI_APPROVAL_UNAVAILABLE_REASON`, "tool requires approval,
not supported in CLI mode"), so the workflow must pass `--auto-approve-spawn`,
which the script now does.
