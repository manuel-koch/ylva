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
| `130` | User interruption (SIGINT or SIGTERM)   | Settled. Revised: an external signal in any TUI state (step 8.1) |

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

1. [x] Add a typed exit-code module.
   1. [x] Create `src/exit_code.rs` with a `#[repr(u8)]` enum `ExitCode`. Name
      each variant after its outcome (for example `Success`, `GeneralError`,
      `UsageError`, `ApprovalDenied` (value `3`), plus `Interrupted` (value `130`).
      Implement conversion to `u8`.
   2. [x] Re-export `ExitCode` from `src/lib.rs` so `tests/*.rs` can reach it
      (binary and library split in `AGENTS.md`).
   3. [x] Add unit tests that pin the numeric value of each variant.

2. [x] Route the existing usage and startup exits through `ExitCode::UsageError`.
   1. [x] Replace the literal `2` in `exit_with_error` (`src/main.rs`) and in the
      parse, `init`, `--color`, and `--log-file` exits (`src/main.rs`) with
      `ExitCode::UsageError`.
   2. [x] Confirm there is no behavior change. `tests/cli_help.rs` and
      `tests/session_commands.rs` must pass without edits to their expectations.
      Result: both pass unchanged; `tests/cli_help.rs` gained one new test (step 10).

3. [x] Add a shared run-outcome recorder.
   1. [x] Create `RunOutcome` in a module both drivers can use, and re-export it
      from `src/lib.rs`. Result: `src/run_outcome.rs`.
   2. [x] Feed it from `UiUpdate` through one method per event. Record root
      `UiUpdate::Error` (provider or stream error) and each denied approval
      (`UserApprovalRequired`). Do not record `ToolError` or `ToolKilled`, because
      tool results never set the exit code. Stream cancelled is not recorded:
      interruption comes from SIGINT only (steps 6, 8).
      Revised: SIGTERM also interrupts (step 6.2).
      Revised: the cancelled-stream mapping was removed after the TUI review in
      step 8.1. Result: `record_update` dispatches to `record_root_error` only.
      Denials and stop requests are recorded directly (steps 7, 8).
   3. [x] Change `run_cli_to_with_title` (`src/cli_driver.rs`) to return the
      `RunOutcome` instead of `Ok(())`. Keep the recording logic in small methods
      on `RunOutcome`, so the event loop only calls them. The public wrappers
      `run_cli_to` and `run_cli_to_with_options` keep their `Result<()>` signatures.

4. [x] Map the recorded outcome to a final code in `ConsoleApp::run`.
   1. [x] Replace the `0u8` mapping in `ConsoleApp::run` (`src/cli_driver.rs`)
      with a pure function `RunOutcome -> ExitCode`, using the precedence order
      in Implementation Hints. Result: `RunOutcome::exit_code`.
   2. [x] Unit-test the mapping function directly, without starting a process.
      Result: `src/run_outcome.rs` tests cover each outcome and the precedence.
   Deviation: an `Err` raised while a run is in progress (I/O failure, TUI driver
   failure) now maps to `GeneralError` (`1`) in `ConsoleApp::run` and
   `TuiApp::run`. Startup failures such as factory creation still propagate to
   `main`'s `exit_with_error` and exit `2`.

5. [x] Classify provider, stream, and agent-loop failures.
   1. [x] Verify before changing anything. Done: a headless run against an
      unreachable provider exits `0` and does not panic. Code reading: the `Err`
      that reaches `.expect("agent loop returned an error")` (`src/cli_driver.rs`,
      about line 1131) comes from `stream_error`, which is set only for child
      agents, and from `iteration_cap_exceeded`, which is set only when
      `own_run_id` is `Some`. Whether a root run can reach that case is unverified.
   2. [x] Route root `UiUpdate::Error` (provider and stream errors) through the
      `RunOutcome` path so they map to `GeneralError` (`1`), not `0`. This is the
      behavior change the idea needs. Verified after the change: the unreachable
      provider run exits `1`, and the mock HTTP 400 run exits `1`.
   3. [x] Confirm whether a root run can reach the iteration-cap `Err`. Result: it
      cannot. `iteration_cap_exceeded` is set only for child loops, and
      `stream_error` only for children. A root iteration cap sends a root
      `UiUpdate::Error`, so it maps to `1` through step 5.2. The `.expect` stays,
      because only child loops reach it.

6. [x] Map interruption, with a cooperative Ctrl+C handler.
   1. [x] Record the interruption at the SIGINT, not at the cancelled stream.
      Revised: the original mapping of the stream-cancelled outcome to
      `Interrupted` was removed in step 8.1.
   2. [x] Install a SIGINT handler on the headless path. Use the TUI bridge in
      `src/tui_driver/terminal.rs` as the reference, and send the existing stop
      command so the run ends through the normal outcome path. The run then prints
      the resume hint and exits `130`.
      Deviation: the relay uses tokio's unix signal streams instead of the `ctrlc`
      crate, so the `ctrlc` dependency is removed.
      Revised: the shared `StopSignals` (`src/stop_signal.rs`) relays SIGINT and
      SIGTERM for both headless and TUI. SIGHUP is not handled. The Windows Ctrl+C
      relay goes with `ctrlc`.
      A stop does not always emit `StreamCancelled`: a stop during a tool run emits
      `ToolKilled`, and a stop while idle emits `AgentIdle`. So once interrupted,
      the loop also ends on root `ToolKilled` and on `AgentIdle`, once no child is
      outstanding. Verified by `interrupted_stream_exits_with_interrupted_and_prints_resume_hint`
      and `interrupt_during_a_tool_run_exits_with_interrupted`.

7. [x] Record approval denial.
   1. [x] Record each denied `UserApprovalRequired` as an `ApprovalDenied`
      outcome in `RunOutcome`. Result: recorded in the headless `ToolPending`
      branch, which also covers child runs.
   2. [x] Keep the run going after a denial, as today. Denial sets the final code
      only through the precedence order in Implementation Hints.

8. [x] Apply the codes to the TUI.
   1. [x] Record the same outcomes in the TUI event path (`src/tui_driver/updates.rs`),
      using the shared recorder from step 3.
      Deviation: the recorder is `PendingRunIds::run_outcome`, not a new argument.
      `PendingRunIds` already reaches every update and effect path, and adding a
      parameter would have changed about 80 test call sites. A user denial is an
      `Effect::DenyTool`, not a `UiUpdate`, so it is recorded in `execute_effects`.
      Revised: `Effect::StopAgent` (Ctrl+C or Esc) records nothing, so a TUI stop
      never sets `130`, including a stop during a tool run. Revised: only an external
      SIGINT or SIGTERM records the interruption, in any TUI state, in the driver
      loop (`PendingRunIds::record_stop_signal`). Before the revision, only an
      active turn recorded it. A new prompt resets the outcome
      (`Effect::SubmitPrompt` with `steer: false`); a steer does not. So only the
      latest turn decides the code. Ctrl+C is the normal way to quit the TUI, so
      it must not return `130`.
      Updates drained during shutdown and session switch are not recorded.
   2. [x] Change `run` in `src/tui_driver.rs`, which previously returned
      `result.map(|_| 0u8)`, so it returns the same outcome code as headless.
   3. [x] Unit-test that the TUI and headless paths map the same outcome to the same
      code, so they cannot drift. Result:
      `tui_run_outcome_follows_driver_events_with_the_headless_precedence`. Both
      drivers share `RunOutcome::record_update` and `RunOutcome::exit_code`.

9. [x] Add integration tests, one per code.
   1. [x] Read `docs/known-test-issues.md` and the integration-test helpers
      section of `docs/development-workflow.md` before writing tests.
   2. [x] Create `tests/cli_exit_codes.rs`. Spawn the binary with an allowlisted
      environment and assert with `output.status.code()`. Result: a local
      `HeadlessRun` helper writes stdout to a file, so a test can wait for a marker
      before it sends SIGINT.
   3. [x] Cover success (`0`), interruption (`130`), provider error (`1`), and
      approval denial (`3`) as integration tests. Cover precedence (`1` over `3`,
      `3` over `0`) in the unit tests from step 4.2. Interruption also has a
      tool-run case (`130`).
   4. [x] Write each behavioral test so it fails before its step is implemented.
      Observed before the change: the provider error and denial exited `0`, and
      both interrupt tests were killed by the signal (`None`).

10. [x] Show the exit-code table in the `--help` epilog.
    1. [x] Add a test to `tests/cli_help.rs` that `--help` lists each code (`0`,
       `1`, `2`, `3`, `130`) with its meaning. Confirm it fails before item 2 is
       implemented. Result: `help_lists_every_exit_code_with_its_meaning`.
    2. [x] Describe each `ExitCode` variant in `src/exit_code.rs`, and build the
       epilog text from those descriptions, so the help text and the code cannot
       drift. Result: `help_epilog()`.
    3. [x] Attach the epilog to the `Cli` struct with `after_help` in the
       `#[command(...)]` attribute in `src/cli.rs` (line 43).
    4. [x] Check that the existing expectations in `tests/cli_help.rs` (lines 67
       and 84) still pass.

11. [x] Update documentation so it matches the code.
    1. [x] `docs/cli-options.md`: add the exit-code table as the single source of
       truth. It covers chat runs, the TUI, and `ylva session` subcommands.
       Update the `2`, `0`, and non-zero statements at lines 12, 17, 21–23, 44,
       and 111. State that tool results never change the exit code. Result: new
       `## Exit codes` section (anchor `#exit-codes`); the statements at the listed
       lines were checked, and the non-zero statement now links to the table.
    2. [x] `docs/features.md` (CLI mode, lines 61–62): link to the table in
       `docs/cli-options.md`, and state the codes are a contract.
    3. [x] `docs/configuration.md` (lines 45, 54): link to the table for profile
       errors. Do not repeat the code number.
    4. [x] `docs/persistence.md` (around line 299): link to the table's general-error
       row for a partial session-maintenance failure. Do not repeat the code number.
    5. [x] `docs/ideas/cli-exit-codes.md`: link this plan, mark the idea as
       planned, and record that tool results no longer set the exit code.
    6. [x] `docs/ideas/cli-json-output.md` and `docs/ideas/cli-quiet-mode.md`:
       update the open questions and cross-links that overlap with exit codes.
    7. [x] `CONTEXT.md`: check whether terms such as "run outcome" or "exit code"
       belong in the ubiquitous language. Add them only if they do. Result: added
       **Run outcome** and **Exit code** under a new `## Run outcome` section.
    8. [x] Keep one table. Search `docs/` for copied exit-code statements (for
       example "exits 2" or "exit code 1"). Replace each one outside
       `docs/cli-options.md` with a link to the table.
       Also updated, not in the step list: `docs/implementation-notes.md` (CLI driver
       section: `RunOutcome`, the interrupt end conditions, and the TUI recorder).

12. [x] Verify the change.
    1. [x] Run `make format lint`. Passes with no warnings.
    2. [x] Run `rumdl fmt` and `rumdl check` on every changed Markdown file, or
       `make format-markdown`. `rumdl check .` passes for all 54 files.
    3. [x] Run `make test-coverage` as the mandatory gate (at least 80% line
       coverage). Result: 1832 tests passed; total line coverage 88.52%.
    4. [x] Smoke-check the built binary. Confirm an unknown flag exits `2`, a
       successful run exits `0`, and the interruption and denial paths exit with
       their documented codes. Result: unknown flag `2`; unreachable provider `1`;
       `--help` epilog lists the table. Success, denial, stream interrupt, and tool
       interrupt are covered by `tests/cli_exit_codes.rs`, which passes five runs in
       a row.

Known limits, not covered by a test:

- Ctrl+C while the root is idle and waiting on a spawned child ends through
  `AgentIdle`, which is verified by code reading only. The mock server cannot hold a
  child stream open, so no integration test covers it.
- A second Ctrl+C during the stop does not force an exit. The first press is handled
  once per run.
- The TUI outcome covers only the latest turn. A fresh prompt resets it. An external
  SIGINT or SIGTERM sets `130` in any TUI state, including idle and while only a
  spawned child runs, so the TUI and headless modes agree.
  Revised: this replaces the earlier rule that recorded a SIGINT only during an
  active turn.

## Implementation Hints

Decisions (grilling):

- Exit codes: `0` success, `1` general error, `2` usage or startup error
  (unchanged), `3` approval denied (moved from the idea's `4`), `130` interruption
  (the shell convention for SIGINT; SIGTERM maps the same way after the revision).
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
- Headless Ctrl+C or SIGTERM: a cooperative stop-signal handler stops the agent, prints the resume
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
- Pre-revision fact (superseded): there was no SIGINT handler on the headless
  path, and the TUI bridged SIGINT to `AppAction::SigInt`. Verified before the
  change: `kill -INT` on a running headless run exited `130` by signal, with no
  resume hint and no `Interrupted` line.
  Revised: `StopSignals` (`src/stop_signal.rs`) serves both paths. The TUI relays
  it as `AppAction::StopSignal` (`src/tui_driver/terminal.rs`,
  `install_stop_signal_relay`), and the presenter maps `InputEvent::StopSignal`
  to `Effect::Quit` when idle and to `Effect::StopAgent` otherwise
  (`src/presenter/keyboard.rs`).
- Pre-change fact (superseded): `run` in `src/tui_driver.rs` returned
  `result.map(|_| 0u8)`. It now returns the `RunOutcome` exit code (see step 6).
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
