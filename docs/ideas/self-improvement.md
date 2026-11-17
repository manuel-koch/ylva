# Self improvement

Guided self-improvement workflow enabling Ylva to optimize its own soft harness
artifacts and Rust implementation without autonomous drift, instruction bloat,
or reward hacking.

## Sources

- [Meta-Harness: End-to-End Optimization of Model Harnesses](https://arxiv.org/abs/2603.28052)
- [Harness Engineering for Self-Improvement](https://lilianweng.github.io/posts/2026-07-04-harness/)
- [Technology Radar: Feedback Flywheel](https://www.thoughtworks.com/radar/techniques/feedback-flywheel)
- [Martin Fowler: Feedback Flywheel](https://martinfowler.com/articles/reduce-friction-ai/feedback-flywheel.html)

## Design summary

Keep Ylva's Rust core lean and keep prompt-driven orchestration reliable by
splitting the workflow into **three small, composable skills** under
`.ylva/skills/` backed by one deterministic state-machine script
(`self_improve_ctl.py`, stdlib-only Python 3 persisting `runs/<run-id>/state.json`
on disk) and one small Rust prerequisite—live run telemetry via
`YLVA_RUN_REPORT_PATH` (modeled on `YLVA_REQUEST_DUMP_PATH` and
`YLVA_TOKEN_USAGE_DUMP_PATH`):

1. **`self-improve-reflect` (Feedback Flywheel)**: mine persisted session
   transcripts (`~/.ylva/chats/<project_key>/*.jsonl`) for recurring signals,
   curate an evidence-backed backlog, or draft a small fast-path fix directly.
2. **`self-improve-scenario` ("Red before Green" Calibration)**: interactively
   grill the user and dry-run a draft scenario against unmodified `HEAD` until
   it reliably triggers the target weakness or missing capability, locking a
   verified reproducing benchmark before any candidate worktrees run.
3. **`self-improve-tournament` (Worktree Tournament)**: pit $K$ distinct
   candidate strategies sequentially in isolated git worktrees against the
   locked target scenario and optional blind regression scenarios, compare
   contenders blindly against `HEAD`, and stop at a human promotion gate.

Why this split works within a skill architecture:

- **Deterministic code owns the state machine, not prompt prose**: ~75% of the
  workflow (worktrees, ephemeral `YLVA_HOME` seeding, sequential `ylva --no-tui`
  execution, live cost/timeout `SIGTERM`, blind-judge anonymization, cleanup,
  and step transitions in `runs/<run-id>/state.json`) lives in
  `self_improve_ctl.py`.
- **`SKILL.md` handles only interactive judgment**: mining triage, scenario
  grilling/calibration, `spec.json` + strategy briefing sign-off via `clarify`,
  driving `self_improve_ctl.py step` one atomic child/gate action per `bash`
  call (bounded by `[tools.bash].timeout_secs` and surviving context compaction
  via `state.json`), and presenting `verdict.md`.
- **Lean v1 slice first (KISS / YAGNI)**: v1 ships single-round (`R = 1`)
  tournaments with `1` calibrated target scenario (`trials = 1`) and an explicit
  optional regression list; multi-round archive refinement ($R > 1$), `N`-trial
  stochastic averaging, and automatic regression-bank matching are deferred to
  v2 until real v1 runs justify them.

See [`self-improvement.mmd`](../diagrams/self-improvement.mmd) for the end-to-end flow.

## Skill 1: `self-improve-reflect` (Feedback Flywheel)

- **Inputs**: root and subagent JSONL transcripts in
  `~/.ylva/chats/<project_key>/` (see [persistence](../persistence.md)), git
  history, and optional user notes.
- **Signal taxonomy**:
  - *Context*: missing project facts, stale conventions, undocumented invariants.
  - *Instruction*: prompt/persona phrasings that reliably helped or misled.
  - *Workflow*: decomposition, tool-use, or subagent patterns worth codifying.
  - *Failure*: verifier- or user-grounded root causes behind broken turns.
  - *Bloat / staleness*: unused, redundant, or contradictory rules to remove.
- **Backlog actions**: classify every entry in
  `<main-workspace>/self-improvement/backlog.jsonl` as **Add**, **Refine**, or
  **Prune**, citing `session_id` evidence and the target harness component.
- **Two-speed routing**:
  - **Fast-path**: small, unambiguous context fixes or prunings become a single
    proposed diff for direct human review without spinning up worktrees.
  - **Tournament-path**: behavioral, multi-file, Rust-code, or trade-off-heavy
    topics hand off to `self-improve-scenario` and `self-improve-tournament`.

## Skill 2: `self-improve-scenario` ("Red before Green" Calibration)

No tournament launches until a target scenario empirically reproduces the gap on
unmodified `HEAD`:

- **Prompt sourcing**:
  - *From a past session*: extract the real user prompt(s), multi-turn setup,
    and workspace assumptions from the cited `.jsonl` transcript and minimize
    them into a self-contained scenario.
  - *From a fresh problem or feature*: iteratively grill the user (`clarify` /
    multi-turn dialogue) on a concrete task where `HEAD` currently struggles,
    takes a clumsy/expensive path, or lacks the feature, grounding prompts in
    real repository paths or a minimal `fixture/`.
- **Interactive `HEAD` probe loop (`self_improve_ctl.py probe-scenario`)**:
  1. Draft `scenarios/<id>/scenario.toml` (plus optional `fixture/` and
     `verify.sh`) and dry-run it headlessly against current `HEAD`.
  2. Inspect the outcome with the user:
     - *Setup or sandbox error* (for example `exit 3` or missing fixture file)
       → fix the scenario configuration and re-probe.
     - *Gap not triggered* (`HEAD` already passes cleanly) → refine the prompt,
       multi-turn setup, or `verify.sh` with the user and re-probe (or stop if
       the issue no longer reproduces).
     - *Target gap confirmed ("Red")* → lock the scenario and persist that
       `HEAD` probe result so the tournament reuses it as the target baseline
       without paying twice.

### Scenario contract (`scenarios/<id>/`)

Scenarios are role-neutral, reusable directories under
`<main-workspace>/self-improvement/scenarios/<id>/`:

```text
scenarios/avoid-redundant-reads/
  scenario.toml
  fixture/          # optional: copied fresh as the rollout's launch directory
  verify.sh         # optional: deterministic exit-0 check after rollout
```

Example `scenario.toml`:

```toml
id = "avoid-redundant-reads"
tags = ["agents-md", "read", "tool-use"]
description = "Summarize module responsibilities without re-reading files."
prompts = [
  "Inspect src/store/ and summarize how project_key handles linked worktrees.",
  "Now check whether session deletion treats worktrees the same way.",
]
allowed_tools = ["read", "glob", "search"]
bash_commands = []
timeout_secs = 120
max_tool_iterations = 12
rubric = [
  "Answers both turns accurately from repository sources.",
  "Does not read the same file region repeatedly.",
]
```

- Whether a scenario serves as a **target scenario** (must improve over `HEAD`)
  or a **blind regression scenario** (already passes on `HEAD` and must keep
  passing) is assigned per tournament in `runs/<run-id>/spec.json`, not
  hardcoded inside `scenario.toml`.
- Prefer self-contained `fixture/` directories when a task does not require the
  full Ylva repository, reducing scenario rot as `src/` evolves.
- Multi-entry `prompts` execute sequentially against the same session via
  `--conversation-id <uuid>`, testing multi-turn retention and compaction as
  well as single-shot tasks.
- When `verify.sh` is present, the sidecar passes paths to the rollout fixture,
  JSONL transcript, and `YLVA_RUN_REPORT_PATH` JSON; a non-zero exit marks the
  scenario failed.

## Skill 3: `self-improve-tournament` (Compete & Judge)

### Pre-flight specification & sign-off

With a locked target scenario in hand, `self-improve-tournament` prepares
`runs/<run-id>/spec.json` and pauses once via `clarify` for launch sign-off:

- Optimization goal and locked `target_scenarios`.
- Optional `regression_scenarios` selected from `self-improvement/scenarios/`
  (`[]` allowed on cold start before saved scenarios exist; kept small—typically
  `1..3` checks touching overlapping areas—to bound rollout cost).
- Per-run **editable-path allowlist** covering the permitted subset of:
  - Soft harness artifacts (`SOUL.md`, `AGENTS.md`, `COMPACTION.md`,
    `SKILL.md`, `.ylva/agents/**`), and/or
  - Hard harness sources (`src/**`, `tests/**`, related `docs/**`).
- **Read-only boundary**: scenario definitions, `verify.sh` scripts, Judge
  instructions, and `.ylva/profiles/**` (connections, models, credentials) are
  never editable by candidates.
- $K$ distinct strategy briefings (typically $2..3$ in v1: for example
  prompt/skill refinement, workflow/subagent restructuring, Rust guardrail/tool
  change, or a minimal/subtractive variant)—never $K$ copies of the same prompt
  relying only on sampling temperature.
- Role engines (`target_engine`, `proposer_engine`, `judge_engine`) and hard
  run budgets ($K$ candidates, timeouts, token/cost caps).

### Sequential execution & blind scenario boundary

- **Regression staleness check first**: target baseline metrics come directly
  from the locked calibration probe. Before running candidates, the sidecar
  verifies any selected `regression_scenarios` still pass on current `HEAD`; if
  repo evolution made one fail on `HEAD`, execution pauses so the user can
  update, drop, or retire that stale scenario rather than falsely disqualifying
  candidates.
- **Sequential child execution**: child `ylva --no-tui` sessions (Proposers,
  Scenario Rollouts, Judge) run **one at a time** rather than in parallel.
  Worktrees isolate filesystem and branch state per candidate, while sequential
  execution avoids cross-process provider rate limits, removes CPU/disk
  contention during builds and timed rollouts, preserves native subagent
  concurrency inside scenarios, and halts spend immediately after at most one
  in-flight request crosses the budget cap.
- **Blind scenario storage**: scenarios and verifiers stay in
  `<main-workspace>/self-improvement/scenarios/` outside candidate worktrees.
  Proposers receive the goal and strategy brief, never regression prompts or
  `verify.sh` scripts.

### Skill package layout & state-driven step execution

All three skills live as immediate child directories under `.ylva/skills/` (per
[skill discovery](../configuration.md#agent-skills)) and share one stdlib-only
sidecar script:

```text
.ylva/skills/
  self-improve-reflect/
    SKILL.md
  self-improve-scenario/
    SKILL.md
  self-improve-tournament/
    SKILL.md
    prompts/
      proposer.md
      judge.md
    scripts/
      self_improve_ctl.py
```

- **Atomic `step` execution & `[tools.bash].timeout_secs` alignment**:
  - Instead of running an entire multi-candidate phase in one long shell call or
    relying on chat memory for loop counters, `self_improve_ctl.py` persists
    `runs/<run-id>/state.json` and advances **one child process or gate per
    invocation** (`check-regressions`, `propose c1`, `gates c1`,
    `rollout c1 <scenario>`, ..., `judge`, `cleanup`) via
    `python3 .ylva/skills/self-improve-tournament/scripts/self_improve_ctl.py step --run <id>`.
    Each step returns a compact JSON status summary, survives compaction, and
    lets a single `[tools.bash]` command-prefix rule optionally auto-approve
    `self_improve_ctl.py` calls.
  - When a Rust-modifying step (`cargo build` in a fresh worktree, `make
    test-coverage`, or a multi-turn Proposer run) needs more than `bash`'s
    `300s` default, the orchestrator profile or agent type raises
    `[tools.bash].timeout_secs` / `[agents.<type>.tools.bash].timeout_secs`
    (omitting `rules` to inherit existing `bash` sandbox rules), and child
    ephemeral profiles configure per-tool `timeout_secs` the same way.
  - `self_improve_ctl.py` sets its internal cooperative `SIGTERM` deadline
    slightly below the active `[tools.bash].timeout_secs` so slow steps shut
    down cleanly ahead of `run_bash`'s `SIGKILL`.

## On-disk layout & sandbox alignment

All local self-improvement state lives under a gitignored, **non-dotted**
directory in the main checkout:

```text
<main-workspace>/self-improvement/
  backlog.jsonl
  scenarios/<scenario-id>/
  runs/<run-id>/
    spec.json
    state.json
    baseline/
    candidates/<candidate-id>/
    blind-map.json
    judge-view/
      scorecard.json
      baseline/
      candidate-1/
    verdict.json
    verdict.md
  worktrees/<run-id>/<candidate-id>/
```

Why `<main-workspace>/self-improvement/` instead of `.ylva/self-improvement/`:

- **Avoids `dot_path` friction**: Ylva's `dot_path` rule checks every component
  of a resolved absolute path, including hidden launch ancestors (see
  [sandbox rules](../configuration.md#sandbox-tool-approval-rules)). A
  non-dotted base path lets the orchestrator read/write `backlog.jsonl`,
  `scenarios/`, and `runs/` under normal workspace rules, and avoids giving
  nested candidate worktrees a hidden path ancestor that would trip `dot_path`
  on every file access.
- **Gitignored isolation**: keeping `self-improvement/` gitignored prevents
  `git worktree add` from checking out `scenarios/` or `runs/` into candidate
  worktrees and keeps session trace data out of git history.
- **Workspace boundary for candidates**: each linked worktree under
  `worktrees/<run-id>/<candidate-id>/` has its own `.git` marker, making that
  candidate directory its own workspace root. Paths up in
  `<main-workspace>/self-improvement/scenarios/` or `runs/` therefore evaluate
  as `outside_workspace` (`auto_denied`) from inside any candidate run.

## Isolation, binaries & headless safety

### Ephemeral `YLVA_HOME` & workspace trust

Linked worktrees share `project_key` with the main checkout
([ADR-0004](../architecture-design-records/0004-group-sessions-by-main-worktree-path.md)),
and `.ylva/agents/**` shares content-hash workspace trust with
`.ylva/profiles/**`
([ADR-0003](../architecture-design-records/0003-trust-workspace-configuration-by-content-hash.md)).
To isolate candidate runs without touching `~/.ylva`:

1. Create a fresh ephemeral `YLVA_HOME` per headless run and seed it with a
   read-only copy of the active profile configuration and credentials.
2. Verify `.ylva/profiles/**` in the candidate worktree is byte-for-byte
   unchanged.
3. Run `ylva trust` against that ephemeral `YLVA_HOME` when `.ylva/agents/**`
   is in scope, keeping `~/.ylva/chats/` and `~/.ylva/trusted-workspaces.toml`
   untouched.

### Two-layer headless safety & Execution Envelope Briefing

Headless `ylva --no-tui` denies `UserApprovalRequired` calls and permanently
records exit code `3` (see [CLI options](../cli-options.md#exit-codes)), while
`[tools.bash]` treats heredocs (`<<EOF`), command substitutions (`$(...)`),
subshells, loops/conditionals, `VAR=val` prefixes, and non-`/dev/null` file
redirections as `UserApprovalRequired` (see
[`bash` command rules](../configuration.md#bash-command-rules)). `--auto-approve-*`
CLI catch-alls are avoided because they override `[tools.*]` `auto_denied`
boundaries.

1. **Ephemeral profile rules**: omit unused tools via `allowed_tools` (always
   omitting `clarify` in headless runs, and omitting `bash` for the Judge), set
   `outside_workspace = auto_denied` with descriptive `reason` hints on file
   tools, scope auto-approved `write`/`edit` to the candidate worktree, restrict
   `bash` to explicit flat command prefixes (`cargo`, `make`, `rumdl`, read-only
   `git`), and execute Scenario Rollouts inside fresh fixture copies rather than
   mutating the candidate's repository checkout.
2. **Auto-generated Execution Envelope Briefing**: because `self_improve_ctl.py`
   writes each child's ephemeral `config.toml`, it renders a concise capability
   briefing directly from those rules (Single Source of Truth):
   - Which tools are enabled vs. omitted (use `read`/`write`/`edit`/`search`/
     `glob` instead of shell text processing; never attempt interactive prompts).
   - Permitted path boundaries (and the editable-path allowlist for Proposers).
   - Permitted flat `bash` prefixes and forbidden shell syntax (no `VAR=val`,
     `$(...)`, heredocs, control flow, subshells, or file redirects beyond
     `2>&1` and `/dev/null`).
   - Identical injection across Baseline and Candidate Scenario Rollouts so
     every rollout competes under the same operational instructions without
     altering workspace `SOUL.md` or `AGENTS.md` under test.
3. **Post-proposer allowlist diff gate**: after each proposer exits, the sidecar
   checks `git status` and `git diff` against the pre-flight editable allowlist
   and immediately disqualifies any candidate that touched disallowed files.

### Pinned host binary vs. candidate binary

- **Orchestrator, Proposers, and Judge** always run using the **pinned host
  `ylva` binary** that launched the self-improvement session, so a broken
  candidate build or panic cannot take down orchestration or judging.
- **Soft-only candidates** skip compilation and run Scenario Rollouts with the
  pinned host `ylva` against the candidate worktree's updated instructions,
  skills, or agent definitions.
- **Rust-modifying candidates** compile into a worktree-local `target/`
  directory (avoiding Cargo lock contention and binary overwrites across
  concurrent worktrees) and run Scenario Rollouts with that candidate's built
  `./target/debug/ylva`.

## Engines, telemetry & circuit breakers

- **Engine separation**:
  - `target_engine`: the engine under test in Baseline and Candidate Scenario
    Rollouts (defaults to the active conversation engine, since harness gains
    are often model-specific).
  - `proposer_engine`: strong coding/reasoning engine used for Stage 1 mining
    and candidate generation.
  - `judge_engine`: strong reasoning engine evaluating anonymized candidates.
- **Live run-report environment variable (`YLVA_RUN_REPORT_PATH`)**: session
  `.jsonl` files do not persist billed costs or provider-reported token totals
  (see [model usage costs](../features.md#model-usage-costs)), and an exit-only
  JSON dump (`--format json` in [`cli-json-output.md`](./cli-json-output.md))
  cannot catch a runaway session mid-run. Following `YLVA_REQUEST_DUMP_PATH` and
  `YLVA_TOKEN_USAGE_DUMP_PATH` (see
  [debug environment variables](../cli-options.md#debug)), when
  `YLVA_RUN_REPORT_PATH` is set Ylva resolves it at startup, atomically rewrites
  that JSON file (temp file + `rename`, preventing partial reads) at startup
  (`status: "running"`), after every completed model request across root,
  subagent, compaction, and title calls, and on exit (`session_id`, status/run
  outcome, turn and tool counts, local token estimates, provider-reported
  tokens, and `CostLedger` totals), and **strips `YLVA_RUN_REPORT_PATH` from
  `run_bash` child environments** so nested `ylva` runs (for example inside
  `cargo test`) never overwrite the outer session's report.
- **Live supervisor & circuit breakers**:
  - The sidecar passes `YLVA_RUN_REPORT_PATH` in `subprocess` `env` (avoiding
    shell `VAR=val` prefixes), polls the active child process's report file,
    tracks cumulative spend and token totals across the tournament, and sends
    `SIGTERM` (`kill <pid>`) on budget breach—triggering headless Ylva's
    cooperative shutdown (`exit 130`) while flushing the transcript and final
    report, then judging completed work without starting further candidates or
    rounds.
  - Because providers report usage/cost only when a response stream completes
    and some providers (such as Ollama) report no monetary cost, pair the live
    cost cap with per-rollout wall-clock timeouts, `max_tool_iterations`,
    `max_output_tokens`, and token-budget ceilings from the same live report.

## Evaluation, judging & promotion

1. **Baseline anchored before candidates run**: reuse the locked `HEAD`
   calibration probe for target scenarios, and verify any selected regression
   scenarios still pass on `HEAD` before proposing candidates.
2. **Two-layer candidate evaluation**:
   - *Deterministic gates*: `make format-check lint`, targeted/full tests (for
     Rust changes), `rumdl check`, allowlist compliance, and prompt/skill
     estimated token delta.
   - *Scenario Rollouts*: sequential headless runs on target and blind
     regression scenarios, capturing JSON run reports, `verify.sh` outcomes, and
     full JSONL transcripts.
3. **Filtered blind Judge (`judge-view/`)**:
   - Hard-disqualify any candidate failing deterministic gates or regressing on
     regression scenarios relative to Baseline.
   - Materialize an anonymized read-only bundle under
     `runs/<run-id>/judge-view/` (`baseline/`, `candidate-1..N/`, keeping the
     real mapping in `runs/<run-id>/blind-map.json` outside `judge-view/`) with
     a compact `scorecard.json` (gate status, `verify.sh` results, and
     turn/tool/token/cost deltas vs. Baseline) alongside raw diffs and rollout
     transcripts—avoiding context overflow while keeping full trace access.
   - Run the Judge as a dedicated headless `ylva --no-tui --engine <judge_engine>`
     session with read-only inspection tools (`read`, `search`, `glob`) so it
     reads `scorecard.json` first, drills into raw traces selectively, and emits
     `verdict.json` + `verdict.md`.
   - Score task quality, efficiency, and an explicit **Occam's Razor bloat
     penalty** favoring smaller diffs and leaner prompts/skills at equal
     quality; allow **"Baseline wins — promote nothing"** whenever no candidate
     clears the bar.
4. **Human promotion gate, cleanup & scenario retention**:
   - Never auto-merge into the main checkout or mutate `~/.ylva`.
   - Automatically remove losing git worktrees and ephemeral `YLVA_HOME`
     directories once artifacts are recorded in `runs/<run-id>/`.
   - Keep a winning candidate on a clean `chore/self-improve-<slug>` branch and
     write `runs/<run-id>/verdict.md` summarizing evidence, trade-offs, and
     runner-up comparisons for human review and cherry-pick/merge.
   - When a tournament fix is promoted, offer to keep the calibrated target
     scenario in `self-improvement/scenarios/<id>/` so future tournaments can
     include it as a blind regression check (or delete it if one-off).

## Phased scope (KISS / YAGNI)

- **v1 slice (initial implementation)**:
  - `YLVA_RUN_REPORT_PATH` live atomic JSON telemetry in Rust (stripped from
    `run_bash` children).
  - Three small skills (`self-improve-reflect`, `self-improve-scenario`,
    `self-improve-tournament`) + stdlib `self_improve_ctl.py` advancing
    `runs/<run-id>/state.json` one atomic step per `bash` call.
  - `1` calibrated target scenario (`trials = 1`), single-round tournament
    (`R = 1`, $K = 2..3$ distinct strategies), explicit optional
    `regression_scenarios` (`[]` allowed on cold start), blind Judge, and Human
    Promotion Gate.
- **Deferred v2 extensions (add only when v1 evidence warrants them)**:
  - Multi-round refinement ($R > 1$) feeding Round-1 archive diagnostics and
    target-scenario failure summaries into later proposer rounds (Meta-Harness
    style).
  - Multi-trial (`trials = N`) averaging during calibration and rollouts for
    flaky/stochastic behavioral issues.
  - Automatic tag-matched regression selection as `self-improvement/scenarios/`
    grows large.

## Related

- [`../diagrams/self-improvement.mmd`](../diagrams/self-improvement.mmd)
- [`cli-json-output.md`](./cli-json-output.md)
- [`../persistence.md`](../persistence.md)
- [`../configuration.md`](../configuration.md)
- [`../cli-options.md`](../cli-options.md)
- [`../architecture-design-records/0003-trust-workspace-configuration-by-content-hash.md`](../architecture-design-records/0003-trust-workspace-configuration-by-content-hash.md)
- [`../architecture-design-records/0004-group-sessions-by-main-worktree-path.md`](../architecture-design-records/0004-group-sessions-by-main-worktree-path.md)
