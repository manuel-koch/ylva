# Refactor Handwritten CLI Option Parsing to clap

## Context

Ylva's CLI arguments are parsed by handwritten code scattered across `src/main.rs`: seven
independent scans of `Vec<String>` (`run_init_command`, `color_mode_from_args`,
`explicit_profile_name_from_args`, `conversation_id_from_args`, `prompt_from_arg`,
`logging_from_args`, plus inline `--version` / `--no-tui` / `--auto-approve-*` checks in
`main()`), each with subtly different semantics (some missing values are silently ignored,
others are hard errors; unknown flags are silently ignored; there is no `--help`). The
seed idea (`docs/ideas/cli-options-parsing.md`) calls this brittle and asks for a refactor
to an established crate, included as a **new phase in `docs/blueprint.md`**, shifting the
not-yet-started phases back.

The blueprint already anticipates this: Phase 12 ("CLI Surface") contains a bullet adding
`clap` for argument parsing, and blueprint §2/§9 list `clap` in the starter crate set.
No ADRs exist (`docs/architecture-design-records/` contains only `.gitkeep`), so no
recorded decision constrains the change.

**Goal:** Replace all handwritten argument scanning with a `clap` 4 derive-based parser
that preserves the currently implemented flag surface and observable behaviors (except
the deliberate, documented changes listed under Consistency Issues), move parsing into a
testable lib module, and record the work as a new blueprint phase with renumbering.

**Acceptance criteria:**

- All currently supported flags parse through one `clap` `Parser` struct/subcommands:
  `--version`, `init [--workspace] [--force]`, `--no-tui`, `--color=<auto|no|yes>`,
  `--prompt <text>`, `--profile <name>`, `--log-file`, `--log-level`, `--log-format`,
  `--conversation-id <uuid>`, and the seven `--auto-approve-*` boolean flags.
- `--help` exists (new, courtesy of clap) and `docs/cli-options.md` documents it. This is
  the **only** new flag; everything else still unimplemented in blueprint §6 (`--format
  json`, `--quiet`, `--provider`, `--model`, `--agent`, richer exit codes, store-management
  subcommands) stays in the renumbered Phase 13 "CLI Surface".
- Deliberate behavior changes land as decided: unknown flags rejected (exit 2), missing
  values hard-error for every flag (including `--profile`), duplicate `--prompt` is
  last-wins (was first-wins), `init` accepted after global flags (was first-position-only).
- Every existing unit and integration test passes; assertions on exact error strings are
  updated wherever clap's formatting replaces the handwritten message (decision: accept
  clap's messages rather than wrapping them), except distinctive semantic phrases
  (`invalid --conversation-id …`, color-mode guidance) preserved via `value_parser`.
- Usage errors still exit with code `2`, `--version` still prints the rich `BUILD_INFO`
  rendering with empty stderr, and log-file content written during startup is still
  flushed before any error exit (clap's own error path must not bypass `LogGuards`).
- `docs/blueprint.md` contains a new `Phase 11` for this refactor; old Phase 11
  "Context Compaction" becomes Phase 12, old Phase 12 "CLI Surface" becomes Phase 13,
  all internal "Phase 11"/"Phase 12" references are updated, and old Phase 12's dedicated
  `clap` bullet is removed (consumed by the new phase).
- Required docs are updated in the same change set (see Implementation Plan step 9).
- Verification gates pass: `make format lint` and `make test-coverage` (≥80% line
  coverage on testable files).

**Key constraints:**

- Binary + library split: anything `tests/*.rs` needs must be re-exported from
  `src/lib.rs`; keep `main.rs` thin. Parsing lives in a new `src/cli.rs` (settled).
- `clap 4.6.6` is already in `Cargo.lock` (transitive via `jsonschema 0.17`); adding it
  as a direct dependency pulls no new crate into the tree. Edition 2021, no workspace.
- `main.rs` is excluded from the coverage gate; the new `src/cli.rs` in the lib is **not**
  excluded and must meet the ≥80% threshold (existing parsing tests largely cover this).
- Piped stdin remains an implicit prompt source (`--prompt` → stdin → demo prompt);
  `TERM`/`NO_COLOR` env handling for `--color=auto` stays outside clap.
- `std::process::exit` skips destructors: current `exit_with_error` flushes `LogGuards`
  manually; clap's default error handling exits internally, so parsing must go through
  `try_parse_from` and route errors through the existing exit path.
- Scope is fixed: no new flags beyond clap's automatic `--help` (settled).
- The seed idea file `docs/ideas/cli-options-parsing.md` **stays** after execution
  (settled — do not delete it).
- No ADR will be created (settled — none exist; AGENTS.md only requires ADRs when
  deviating from an existing one).
- Follow repository conventions: full descriptive names, no abbreviations, TDD, docs
  updated in the same commit as code.

**Starting point:** open `src/main.rs` (parsing lives at lines 37–744; unit tests at
790–1093) and `docs/cli-options.md` (the flag reference table). Run the existing CLI
tests first to establish a green baseline:
`cargo test --test cli_version --test cli_profile --test cli_init`.

**Note for the implementing agent:** flip each `[ ]` to `[x]` immediately after
completing the step and rewrite this file in place, so progress is resumable across
sessions. Keep step numbers consecutive; when inserting/removing steps, carry over
checkbox states.

## Implementation Plan

1. [x] **Add clap dependency.** Add `clap` with the `derive` feature to `[dependencies]`
   in `Cargo.toml` (blueprint §2/§9 already list it; lockfile already contains 4.6.6
   transitively). Verify `cargo check` stays clean and no unexpected new crates appear
   in `Cargo.lock`.

2. [x] **Create `src/cli.rs` with the derive model.** Define `struct Cli` (`#[derive(Parser)]`)
   plus an `Init` subcommand mirroring the current surface: `--version` handled outside
   clap (disable clap's version flag via `disable_version_flag` so the rich
   `BUILD_INFO.render()` output in `main.rs:111-114` and `tests/cli_version.rs`
   assertions stay intact), `init` subcommand with `--workspace` and `--force` (accepted
   after global flags too — clap's ordering flexibility is deliberately embraced), `--no-tui`,
   `--color <mode>` (accept `auto|no|yes`, case-insensitive as today), `--prompt <text>`,
   `--profile <name>`, `--log-file <path>`, `--log-level <level>`,
   `--log-format <format>`, `--conversation-id <uuid>`, and seven `--auto-approve-*`
   booleans. Use `try_parse_from`, never clap's built-in `parse()`/auto-exit. Map any
   parse error to the existing usage-error path (eprintln `error: …`, flush log guards,
   exit `2`) — see Implementation Hints. Export via `src/lib.rs`.

3. [x] **Port flag-family semantics deliberately.** Replace each handwritten scan with
   fields on `Cli`/`Init` under the settled strictness policy:
   - Missing values hard-error for every flag, including `--profile` (currently silently
     ignored via `explicit_profile_name_from_args`); update the unit test at
     `main.rs:806-814`.
   - Unknown flags are rejected by clap (currently silently ignored). Confirm every
     existing test/e2e invocation uses only valid flags (research says they do).
   - Duplicate `--prompt`: last occurrence wins (replaces current
     first-wins). Duplicate `--color`: last wins (unchanged). **Fact:** clap 4
     rejects repeated `Set` args with `ArgumentConflict`, so last-wins is
     implemented via self-`overrides_with` on both fields. Pin both with tests.
   - `--auto-approve-*` remain value-less booleans; enum validation stays case-insensitive
     (`value_parser` or post-processing) to keep tests at `main.rs:906-947` passing.
   - `--conversation-id <uuid>`: validate via clap `value_parser`; preserve distinctive
     wording (`invalid --conversation-id …`) where tests assert it, otherwise accept
     clap's formatted message and update the assertion.

4. [x] **Rewire `main.rs`.** Replace the seven parse functions and the inline `args`
   scanning in `main()` with a single `Cli::try_parse_from(std::env::args())` call at the
   top of `main()`. Keep mode dispatch semantics: `--version` pre-empts everything;
   `init` runs before TUI/headless detection; TUI iff stdout is a terminal and `--no-tui`
   absent; prompt precedence `--prompt` → stdin → demo prompt unchanged. Delete the now-dead
   helper functions; keep `main.rs` to event-loop/dispatch only (AGENTS.md: keep `main.rs`
   thin).

5. [x] **Migrate and extend unit tests.** Move the parsing unit tests from
   `main.rs:790-1052` into `src/cli.rs` `#[cfg(test)]` (they test the lib now — note
   `known-test-issues.md`: `cfg(test)` guards behave differently for lib code reached from
   integration tests). Keep `should_use_cli_color` env-matrix tests where they fit
   (color decision logic is not clap's job). Add new tests: `--help` exits 0 and lists all
   flags; unknown flag exits 2 with a clap error; missing-value errors for `--profile`,
   `--prompt`, `--log-file`, `--conversation-id`; duplicate `--prompt` and `--color`
   last-wins; `ylva init --workspace` parses as the subcommand (including after a global
   flag). Because `src/cli.rs` counts toward the ≥80% coverage gate, ensure every
   field/branch has a test before proceeding.

6. [x] **Run and fix integration/e2e tests.** Run
   `cargo test --test cli_version --test cli_profile --test cli_init --test store_persistence_e2e`
   and the full suite. Expect fallout in tests asserting exact stderr error strings;
   per the settled wording policy, update those assertions to clap's messages unless the
   distinctive phrase was deliberately preserved via `value_parser` (log each changed
   assertion here). **Outcome:** no integration-test assertion needed changing —
   all CLI suites passed unchanged. Confirm the
   resume-hint text (`"ylva --conversation-id <id>"`, tested
   in `main.rs:884-904` and `store_persistence_e2e.rs`) is untouched — it is generated
   from parsed data, not parsing.

7. [x] **Verify log-guard flush behavior.** Reproduce the startup-error path that writes
   `--log-file` output (covered by `tests/cli_profile.rs`) and assert the log file is
   complete after a parse-error exit as well as after a post-parse startup error. If clap
   error text must be intercepted to flush guards, do it in the single `try_parse_from`
   match in `main()` — do not let clap exit on its own.
   **Outcome:** parse errors occur *before* `init_logging`, so no log guard exists on
   that path (nothing to flush — documented in `main()`); post-parse startup errors go
   through `exit_with_error`, which flushes; `cli_profile`'s `--log-file` assertion
   passes.

8. [x] **Update `docs/blueprint.md` (phase insertion + renumbering).**
   - Insert a new `### Phase 11: CLI Option Parsing (clap)` after Phase 10 (Full
     Configuration & Persona System, ends ~line 3041), containing: goal (replace
     handwritten brittle parsing with `clap`, move into lib), the tasks from steps 1–7 in
     blueprint bullet style (`- [x]` as completed), `**Key crates:** clap`.
   - Renumber old `Phase 11: Context Compaction & Token Budgeting` → Phase 12 and old
     `Phase 12: CLI Surface` → Phase 13 (only these two phases are not started).
   - Update every "Phase 11"/"Phase 12" textual reference: blueprint lines 1059, 1079,
     1097, 1103, 1107, 1128, 1136, 1186, 1826, 3066, 3269 and the doc comment at
     `src/llm/engine.rs:47`.
   - In (now) Phase 13, remove the `clap` bullet (consumed by the new phase) and drop the
     stale `Key crates: clap` line; while there, fix the two stale claims in that phase:
     `--auto-approve-read` and `--conversation-id` are already implemented and documented
     in `docs/cli-options.md`.
   - Do **not** fix unrelated stale items (Phase 10's unchecked Goal, flags documented
     but unimplemented such as `--format`/`--quiet`/`--provider`/`--model`/`--agent`);
     report them instead (stale-doc rule: report, don't fix without consent).

9. [x] **Update documentation.** In the same change set, update:
   - `docs/cli-options.md` — add `--help` row, note unknown-flag rejection and that
     parsing is clap-backed, document the deliberate behavior changes (missing-value
     errors, last-wins duplicates), refresh the exit-code paragraph if error wording
     changed.
   - `docs/features.md` (`## CLI Runtime`) — mention `--help` and clap-backed parsing.
   - `docs/implementation-notes.md` (`## CLI driver` and any main.rs description) —
     document `src/cli.rs`, the derive model, and how dispatch in `main()` consumes it.
   - `docs/development-workflow.md` — only if it references the removed `main.rs` parsing
     helpers (verify; likely no change).
   - `docs/blueprint.md` — already handled in step 8.
   - Check `CONTEXT.md` for CLI-related ubiquitous-language terms; update only if the
     refactor introduces/changes a term (e.g. "CLI surface" vs "CLI option parsing").
   - Do not create any new doc file without asking the user.

10. [x] **Run verification gates.** Per `docs/development-workflow.md`: run
    `make format lint`, then the single mandatory gate `make test-coverage` (do not also
    run `make test`). Respect `docs/known-test-issues.md`: tracing-subscriber lock rules
    for anything touching `init_logging`, cwd-lock rules for tests reading `current_dir()`.
    Fix failures before marking this step done; confirm the coverage threshold holds with
    `src/cli.rs` newly counted.

11. [x] **Final consistency pass.** Re-read this plan and `docs/blueprint.md`: all
    checkboxes in steps 1–10 flipped, new phase checkboxes flipped in blueprint, no
    dangling "Phase 11"/"Phase 12" references (`rg -n "Phase 1[123]" docs/ src/`), the
    idea file `docs/ideas/cli-options-parsing.md` still present, and this file's noted
    deliberate behavior changes match the code and docs.

## Proposed Improvements

- Add `ylva completions`/shell-completion generation later (clap_complete) — defer; not
  requested by the idea and YAGNI until users ask.

## Consistency Issues

- [high] The idea demands a *new* blueprint phase shifting not-started phases back, but
  old Phase 12 already contains the `clap` bullet — inserting without consuming that
  bullet would list the same work twice. Resolved by plan step 8 (remove the bullet when
  renumbering).
- [low] clap's uniform missing-value/unknown-flag errors contradict current behavior
  (`--profile` missing value silently ignored; unknown flags silently ignored). Settled:
  the stricter clap behavior is adopted deliberately; steps 3, 5, 6 and the
  `docs/cli-options.md` update implement and document it.
- [low] `main.rs` parsing unit tests do not count toward coverage today; moving them to
  `src/cli.rs` newly subjects them to the ≥80% gate. Settled: `src/cli.rs` location was
  chosen knowingly; existing tests largely cover this, but any partially-tested helper
  (e.g. prompt/stdin precedence helpers) must gain tests.

## Implementation Hints

- Parsing entry points in `src/main.rs`: `main()` at 109 (version 111, init 116-125,
  color 134, logging 136, `--no-tui` 153, auto-approve table 220-228 applied 264-268);
  helpers `run_init_command` 37-59, `color_mode_from_args` 83-107,
  `explicit_profile_name_from_args` 448-458, `conversation_id_from_args` 623-636,
  `prompt_from_arg` 663-671, `logging_from_args` 681-744. Unit tests 790-1093.
- `src/lib.rs` (lines 43-101) is the re-export surface; add `pub mod cli;` and re-export
  `Cli` (and any types tests need). `tests/*.rs` cannot reach `main.rs`.
- Error strings currently locked by tests: `"requires a session uuid"`,
  `"invalid --conversation-id '{value}'"`, `"--no-color was replaced by --color=no"`,
  `"use --color=<mode>"`, `"expected auto, no, or yes"`,
  `"--log-file requires a path value"`, `"--log-level requires a value"`,
  `"invalid --log-level ..."`. Decision: accept clap's formatting for these and update
  assertions, except distinctive semantic phrases preserved via `value_parser`.
- `exit_with_error` (`main.rs:372-378`) flushes `LogGuards` before `std::process::exit`;
  clap's built-in error exit would skip this. Pattern: `match Cli::try_parse_from(env::args())`
  → `Err(e)` → print `e` → flush guards → exit 2 (or clap's code for `DisplayHelp`/
  `DisplayVersion`, which should exit 0). `--version` stays custom (`BUILD_INFO.render()`,
  exit 0, stderr empty; asserted by `tests/cli_version.rs`).
- Color env logic (`TERM`, `NO_COLOR`, `should_use_cli_color` at `main.rs:65-80`) is not
  argument parsing — keep it as-is (or move beside `cli.rs` helpers) and keep its matrix
  tests (`main.rs:1054-1092`).
- Prompt precedence: `explicit_prompt`/`initial_prompt` (`main.rs:642-660`) —
  `--prompt` → `prompt_from_stdin()` (748-759, non-terminal read to EOF) →
  `DEFAULT_DEMO_PROMPT` (line 28). Not clap's concern; wire from parsed `Cli` fields.
- Settled decisions from planning: new phase is **Phase 11**; strict parse errors
  adopted; `src/cli.rs` in the lib; no new flags beyond `--help`; duplicate `--prompt`
  is last-wins; `init` may follow global flags; the seed idea file
  `docs/ideas/cli-options-parsing.md` is **not** deleted; no ADR is created.
- Agent type is hardcoded `"default"` (`main.rs:321`, `337`); blueprint §6 documents an
  `--agent` flag that does not exist — out of scope (YAGNI; report only).
- Integration-test recipe for spawned binary: `docs/development-workflow.md`
  §"Spawning the compiled binary in an integration test" (`env!("CARGO_BIN_EXE_ylva")`,
  `env_clear()` + allowlist, TempDir as `YLVA_HOME` and cwd, `spawn_blocking` under
  multi-thread `#[tokio::test]`).
- Blueprint phase conventions: `### Phase N: Title` under `## 7. Phased Build Plan`,
  `- [x]`/`- [ ]` bullets with bold lead-ins (`**Goal:**`, `**Decision:**`), 6-space
  continuation indent, trailing `**Key crates:**` line, optional `#### Phase N outcome`
  subsection for completed phases. Status is purely checkbox state; no status field.
- `docs/plans/` is empty except `.gitkeep`; plans are rewritten in place during execution
  and deleted after execution (per git history). Plan format precedent: numbered
  `N. [ ]` steps with a **Documentation** step and a **verification gates** step.
- Coverage: `make test-coverage` enforces ≥80% on testable files; `main.rs`, `render.rs`,
  `tui_widgets.rs` are excluded (`docs/development-workflow.md` line ~18). A new
  `src/cli.rs` is *not* excluded.
- `Makefile:60-62` (`build-debug-and-run-with-logs`) invokes
  `cargo run -- ... --log-file ... --log-level trace` — a live consumer of flag syntax;
  keep these flags working.
