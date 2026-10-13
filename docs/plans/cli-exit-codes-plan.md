# Structured CLI exit codes

## Context

**System changed:** the headless CLI driver (`ConsoleApp` in `src/cli_driver.rs`),
the TUI driver (`src/tui_driver.rs`), and the process exit path in `src/main.rs`.
The `ylva session` subcommands use the same table.

**Goal:** give headless runs a documented, distinct exit code per terminal
outcome, so scripts and CI can branch on the failure type without parsing output.

**Motivation:** source idea [`docs/ideas/cli-exit-codes.md`](../ideas/cli-exit-codes.md).
It proposes codes `0`–`4`, but it reuses `2` for interruption, which collides
with the usage/startup error code that is already documented and tested. It also
leaves precedence, denial semantics, Ctrl+C handling, and TUI scope open. Grilling
settled all of them; the decisions are in Implementation Hints.

**Current behavior (verified by running the debug binary where marked):**

- Exit `2` covers every fatal usage and startup error: argument parsing, `init`,
  `--color`, `--log-file`, profile, provider env, session open, `--agent`, and
  engine validation.
- Exit `0` covers `--help` and `--version`, and every headless run that reaches
  the end of the loop, including runs with a cancelled stream, a failing tool, or
  a denied approval.
- Verified: an unknown flag exits `2`.
- Verified: a headless run against an unreachable provider exits `0`. It prints
  the error and resume hint, and does not panic. This is the main gap against
  the idea.
- Verified: SIGINT on a headless run exits `130` by signal. It prints no resume
  hint and no `Interrupted` line.
- A partial session-maintenance failure exits `1`.

**Exit-code table (the idea, updated by grilling):**

| Code | Meaning                                  | Status                       |
| ---- | ---------------------------------------- | ---------------------------- |
| `0`  | Successful run                           | Settled                      |
| `1`  | General execution error                  | Settled (also partial session maintenance today) |
| `2`  | Usage or startup error                   | Settled, unchanged           |
| `3`  | Approval required but denied             | Settled (moved from `4`)     |
| `130` | User interruption (SIGINT)              | Settled                      |

**Acceptance criteria (definition of done):**

- Every headless run exits with exactly one code from the documented table. When
  several outcomes occur in one run, the precedence order in Implementation Hints decides.
  The TUI exits with the same code for the same outcome.
- Usage and startup errors still exit `2`. The existing expectations in
  `tests/cli_help.rs`, `tests/session_commands.rs`, and `tests/cli_profile.rs`
  pass without changing their expected values.
- `--help` and `--version` still exit `0`.
- Each code has at least one integration test that asserts it through
  `output.status.code()`.
- The documentation listed in step 11 describes the same table as the code. The
  `--help` epilog (step 10) lists the same codes.
- `make format lint` and `make test-coverage` pass, with at least 80% line
  coverage.

**Constraints:**

- Exit `2` for usage and startup errors is a public contract, documented in
  `docs/cli-options.md`. It must not change.
- Tool denial is returned to the model and the run continues (grilling round 1).
  Denial sets the final code only through the precedence order.
- Tool results never set the exit code (grilling round 2). A tool's own exit
  status, such as a non-zero `bash` exit, is not a run failure.
- Every exit path must print the session resume hint and drop the log guard
  before `std::process::exit`, otherwise buffered logs are lost.
- Use full, descriptive names; no abbreviations (`AGENTS.md`). Keep `src/main.rs`
  thin (event loop and key handling only).
- Non-goals: structured JSON output (`docs/ideas/cli-json-output.md`), quiet
  mode, and separating stdout from stderr. Those are separate ideas.

## Implementation Plan

Work through the steps in order. After completing each step, flip its `[ ]` to
`[x]` and rewrite this file in place, so progress survives session restarts.

1. [ ] Add a typed exit-code module.
   1. [ ] Create `src/exit_code.rs` with a `#[repr(u8)]` enum `ExitCode`. Name
      each variant after its outcome (for example `Success`, `GeneralError`,
      `UsageError`, `ApprovalDenied` (value `3`), plus `Interrupted` (value `130`).
      Implement conversion to `u8`.
   2. [ ] Re-export `ExitCode` from `src/lib.rs` so `tests/*.rs` can reach it
      (binary and library split in `AGENTS.md`).
   3. [ ] Add unit tests that pin the numeric value of each variant.

2. [ ] Route the existing usage and startup exits through `ExitCode::UsageError`.
   1. [ ] Replace the literal `2` in `exit_with_error` (`src/main.rs`) and in the
      parse, `init`, `--color`, and `--log-file` exits (`src/main.rs`) with
      `ExitCode::UsageError`.
   2. [ ] Confirm there is no behavior change. `tests/cli_help.rs` and
      `tests/session_commands.rs` must pass without edits to their expectations.

3. [ ] Add a shared run-outcome recorder.
   1. [ ] Create `RunOutcome` in a module both drivers can use, and re-export it
      from `src/lib.rs`.
   2. [ ] Feed it from `UiUpdate` through one method per event. Record stream
      cancelled (interruption), root `UiUpdate::Error` (provider or stream error),
      and each denied approval (`UserApprovalRequired`). Do not record `ToolError`
      or `ToolKilled`, because tool results never set the exit code.
   3. [ ] Change `run_cli_to_with_title` (`src/cli_driver.rs`) to return the
      `RunOutcome` instead of `Ok(())`. Keep the recording logic in small methods
      on `RunOutcome`, so the event loop only calls them.

4. [ ] Map the recorded outcome to a final code in `ConsoleApp::run`.
   1. [ ] Replace the `0u8` mapping in `ConsoleApp::run` (`src/cli_driver.rs`)
      with a pure function `RunOutcome -> ExitCode`, using the precedence order
      in Implementation Hints.
   2. [ ] Unit-test the mapping function directly, without starting a process.

5. [ ] Classify provider, stream, and agent-loop failures.
   1. [x] Verify before changing anything. Done: a headless run against an
      unreachable provider exits `0` and does not panic. Code reading: the `Err`
      that reaches `.expect("agent loop returned an error")` (`src/cli_driver.rs`,
      about line 1131) comes from `stream_error`, which is set only for child
      agents, and from `iteration_cap_exceeded`, which is set only when
      `own_run_id` is `Some`. Whether a root run can reach that case is unverified.
   2. [ ] Route root `UiUpdate::Error` (provider and stream errors) through the
      `RunOutcome` path so they map to `GeneralError` (`1`), not `0`. This is the
      behavior change the idea needs.
   3. [ ] Confirm whether a root run can reach the iteration-cap `Err`. If it can,
      route it to `GeneralError` so it cannot exit `101`.

6. [ ] Map interruption, with a cooperative Ctrl+C handler.
   1. [ ] Map the stream-cancelled outcome to `Interrupted` (`130`).
   2. [ ] Install a SIGINT handler on the headless path. Use the TUI bridge in
      `src/tui_driver/terminal.rs` as the reference, and send the existing stop
      command so the run ends through the normal outcome path. The run then prints
      the resume hint and exits `130`.

7. [ ] Record approval denial.
   1. [ ] Record each denied `UserApprovalRequired` as an `ApprovalDenied`
      outcome in `RunOutcome`.
   2. [ ] Keep the run going after a denial, as today. Denial sets the final code
      only through the precedence order in Implementation Hints.

8. [ ] Apply the codes to the TUI.
   1. [ ] Record the same outcomes in the TUI event path (`src/tui_driver/updates.rs`),
      using the shared recorder from step 3.
   2. [ ] Change `run` in `src/tui_driver.rs`, which currently returns
      `result.map(|_| 0u8)`, so it returns the same outcome code as headless.
   3. [ ] Unit-test that the TUI and headless paths map the same outcome to the same
      code, so they cannot drift.

9. [ ] Add integration tests, one per code.
   1. [ ] Read `docs/known-test-issues.md` and the integration-test helpers
      section of `docs/development-workflow.md` before writing tests.
   2. [ ] Create `tests/cli_exit_codes.rs`. Use the helpers in `tests/common` and
      assert with `output.status.code()`.
   3. [ ] Cover success (`0`), interruption (`130`), provider error (`1`), and
      approval denial (`3`) as integration tests. Cover precedence (`1` over `3`,
      `3` over `0`) in the unit tests from step 4.2.
   4. [ ] Write each behavioral test so it fails before its step is implemented.

10. [ ] Show the exit-code table in the `--help` epilog.
    1. [ ] Add a test to `tests/cli_help.rs` that `--help` lists each code (`0`,
       `1`, `2`, `3`, `130`) with its meaning. Confirm it fails before item 2 is
       implemented.
    2. [ ] Describe each `ExitCode` variant in `src/exit_code.rs`, and build the
       epilog text from those descriptions, so the help text and the code cannot
       drift.
    3. [ ] Attach the epilog to the `Cli` struct with `after_help` in the
       `#[command(...)]` attribute in `src/cli.rs` (line 43).
    4. [ ] Check that the existing expectations in `tests/cli_help.rs` (lines 67
       and 84) still pass.

11. [ ] Update documentation so it matches the code.
    1. [ ] `docs/cli-options.md`: add the exit-code table as the single source of
       truth. It covers chat runs, the TUI, and `ylva session` subcommands.
       Update the `2`, `0`, and non-zero statements at lines 12, 17, 21–23, 44,
       and 111. State that tool results never change the exit code.
    2. [ ] `docs/features.md` (CLI mode, lines 61–62): link to the table in
       `docs/cli-options.md`, and state the codes are a contract.
    3. [ ] `docs/configuration.md` (lines 45, 54): link to the table for profile
       errors. Do not repeat the code number.
    4. [ ] `docs/persistence.md` (around line 299): link to the table's general-error
       row for a partial session-maintenance failure. Do not repeat the code number.
    5. [ ] `docs/ideas/cli-exit-codes.md`: link this plan, mark the idea as
       planned, and record that tool results no longer set the exit code.
    6. [ ] `docs/ideas/cli-json-output.md` and `docs/ideas/cli-quiet-mode.md`:
       update the open questions and cross-links that overlap with exit codes.
    7. [ ] `CONTEXT.md`: check whether terms such as "run outcome" or "exit code"
       belong in the ubiquitous language. Add them only if they do.
    8. [ ] Keep one table. Search `docs/` for copied exit-code statements (for
       example "exits 2" or "exit code 1"). Replace each one outside
       `docs/cli-options.md` with a link to the table.

12. [ ] Verify the change.
    1. [ ] Run `make format lint`.
    2. [ ] Run `rumdl fmt` and `rumdl check` on every changed Markdown file, or
       `make format-markdown`.
    3. [ ] Run `make test-coverage` as the mandatory gate (at least 80% line
       coverage).
    4. [ ] Smoke-check the built binary. Confirm an unknown flag exits `2`, a
       successful run exits `0`, and the interruption and denial paths exit with
       their documented codes.

## Implementation Hints

Decisions (grilling):

- Exit codes: `0` success, `1` general error, `2` usage or startup error
  (unchanged), `3` approval denied (moved from the idea's `4`), `130` interruption
  (the shell convention for SIGINT).
- Tool results never set the exit code (round 2). A tool's own exit status, such
  as a non-zero `bash` exit, is not a run failure. The idea's tool-failure code is
  dropped, so `3` is approval denial.
- Denied approval: the run continues (round 1). Denial sets the final code only
  through the precedence order.
- Precedence, highest first (round 2): interruption (`130`), general error (`1`),
  approval denied (`3`), success (`0`). Usage errors (`2`) exit before a run
  starts, so they do not compete.
- Scope: chat runs, the TUI, and `ylva session` subcommands (round 1).
- The TUI exits with the same code as headless for the same outcome (round 1).
- Outcome recording: one shared recorder, fed from `UiUpdate` and used by both
  drivers (round 2).
- Headless Ctrl+C: a cooperative SIGINT handler stops the agent, prints the resume
  hint, and exits `130` through the normal path (round 2).

Facts from code reading. Items marked Verified were checked by running the debug
binary. Other items are unverified by execution.

- Exit points in `src/main.rs`:
  - about lines 86–131: parse, `init`, color, log-file, and session-command exits;
  - about lines 396–403: the final `exit_code as i32`, applied only when non-zero;
  - about lines 419–425: `exit_with_error`, which exits `2`.
- `--help` exits `0` (about line 90). `--version` returns early with `0`.
- The `--help` text comes from the `Cli` struct's `#[command(...)]` attribute in
  `src/cli.rs` (line 43). It has no `after_help` today.
- `App::run` returns `anyhow::Result<u8>` (`src/app.rs`, lines 17–20). The
  comment there says `main.rs` converts the result to `std::process::exit`.
- `ConsoleApp::run` (`src/cli_driver.rs`, about lines 333–365) maps
  `run_cli_to_with_title(...)` to `0u8` on `Ok`.
- `run_cli_to_with_title` (`src/cli_driver.rs`, line 594) returns `Ok(())` after:
  - stream cancelled (about line 1037, prints `---[ Interrupted ]---`);
  - a root `UiUpdate::Error` (about line 1064);
  - a `ToolError` (about line 914, prints and continues);
  - a `ToolKilled` (about line 949);
  - a denied approval (`ApprovalCheck::UserApprovalRequired`, about line 780,
    prints "tool requires approval, not supported in CLI mode").
- Agent loop: `src/agent/runtime.rs` (lines 533–537) returns `Err` for
  `state.stream_error` and `state.iteration_cap_exceeded`. `stream_error` is set
  only for child agents (`src/agent/runtime/events.rs`, line 138). The driver
  joins the loop with `.expect("agent loop returned an error")` (`src/cli_driver.rs`,
  about line 1131). Root provider errors arrive as `UiUpdate::Error` and do not
  panic (verified).
- There is no SIGINT handler on the headless path. Handlers exist only for the
  TUI: `src/tui_driver/terminal.rs` (lines 147–157) bridges OS SIGINT to
  `AppAction::SigInt`, and `src/events.rs` (lines 41–44) defines `SigInt`. The
  presenter maps `SigInt` to `Effect::StopAgent` (`src/presenter/keyboard.rs`,
  line 52 and others). Verified: `kill -INT` on a running headless run exits
  `130` by signal, with no resume hint and no `Interrupted` line.
- `run` in `src/tui_driver.rs` (about lines 860–911) returns
  `result.map(|_| 0u8)`.
- Both drivers consume `UiUpdate`. Headless matches `UiUpdate::ToolError`,
  `ToolKilled`, `StreamCancelled`, and `Error` in `src/cli_driver.rs` (about lines
  914–1064). The TUI handles `UiUpdate` in `src/tui_driver/updates.rs`.
- `session_command::run` (`src/session_command.rs`, line 30) returns `u8`:
  - `Ok(0)` on success;
  - `u8::from(!report.failures.is_empty())` on partial maintenance failure
    (line 176), which gives `1`.
- Logging: `exit_with_error` drops the log guard before `std::process::exit`.
  Any new exit path must do the same (`src/logging.rs`, lines 98–99;
  `docs/features.md`, around line 49).
- `print_resume_hint` runs on every exit path (`docs/features.md`, line 59).
- Documentation that pins exit codes: `docs/cli-options.md` (lines 12, 17,
  21–23, 44, 111), `docs/features.md` (lines 61–62), `docs/configuration.md`
  (lines 45, 54), and `docs/persistence.md` (around line 299).
- Integration tests run the binary with `std::process::Command` and assert
  `output.status.code()` (`tests/cli_help.rs`, line 67). The binary is `ylva`
  (`Cargo.toml`).
- Related ideas: `docs/ideas/cli-quiet-mode.md` and
  `docs/ideas/cli-output-streams.md` link to `docs/ideas/cli-exit-codes.md`.
  `docs/ideas/cli-json-output.md` overlaps with the structured-result question.
