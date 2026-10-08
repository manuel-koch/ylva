# Agent Guidelines for Ylva Rust TUI

A Rust-based terminal UI that implements an agent-harness.

## How to read these docs

1. **Before investigating behavior:** skim
   - [README](./README.md) (project intro and quickstart)
   - [CONTEXT](./CONTEXT.md) (Ylva's ubiquitous language and domain terms)
   - [features](./docs/features.md) (implemented feature reference)
   - [implementation-notes](./docs/implementation-notes.md) (internal mechanics
     behind the features above; module/struct-level detail for contributors)
   - [architecture-design-records](./docs/architecture-design-records/) (recorded architecture decisions)
   - [configuration](./docs/configuration.md) (how to configure Ylva profiles and engines)
   - [keybindings](./docs/keybindings.md) (keybindings implemented in the TUI)
   - [plans](./docs/plans/) (planned implementations, one file per subject/feature)
   - [known-test-issues](./docs/known-test-issues.md) (persistent quirks and required workarounds for testing)
2. **Testing:** While implementing code changes (consent given), write tests according to
   [development-workflow](./docs/development-workflow.md). Before writing tests involving
   `tracing`, cwd, or integration-test side-effects, read the relevant entries in
   [known-test-issues](./docs/known-test-issues.md).
3. **Before reporting done**, follow the verification policy in
   [development-workflow](./docs/development-workflow.md): for code changes,
   run `make format lint` and the single mandatory test gate `make test-coverage`.
   `make test-coverage` runs the full test suite and enforces the coverage
   threshold, so do not also run `make test` as a mandatory duplicate.
4. **Plan execution:** Planned implementations are executed from their plan file
   in [development-workflow](./docs/development-workflow.md) → *Plan execution*;
   update the named docs in the same commit.
5. **For release-note generation:** read
   [release-notes-workflow](./docs/release-notes-workflow.md).

## Communication

- Be concise and direct
- No preamble
- No motivational filler
- No repetition
- Prefer bullets over prose
- Keep explanations to the minimum needed

## Execution

- Admit uncertainty when appropriate
- Break large tasks into independently-verifiable units
- Parallelize independent subtasks where possible
- Execute actions via your tools
- No file edits without explicit user consent
  - Chat triggers — do NOT edit: "analyze", "discuss", "evaluate", "explore", "focus",
    "highlight", "propose", "review", "re-think", "suggest", "X doesn't work",
    "can we refactor Y", "what if Z" (and equivalent-intent phrases).
  - Imperatives — DO edit: "apply it", "do it", "go ahead", "fix it", "fix the bug",
    "implement it" (and equivalent-intent phrases)
- Don't git branch/commit/stage anything without explicit user consent

When consent applies to a multi-step task, complete all the steps the user asked
for before reporting back; partial completion is not the same as completion.

## Contributor Notes

**Docs are part of the change set.** A commit must update every doc affected
by the change in the same commit. A planned implementation's plan MUST contain
a documentation step naming the `docs/*` files it updates; executing the
implementation without those doc updates is incomplete.
If a doc doesn't exist yet, ask the user before creating a new doc file.
Out-of-date docs are treated as bugs.

If you notice a stale doc while working on something else, report it; do not fix it unless
asked (consent rule applies).

Any change that touches an area an architecture decision covers must be checked
against the recorded decisions in
([architecture-design-records](./docs/architecture-design-records/)) and kept
consistent with them; introduce a new record when the change deviates.
Using an *architecture-decision-record* skill is encouraged to analyze and
resolve plan-vs-ADR and source-vs-ADR inconsistencies and gaps.

Check markdown documents using linter `rumdl` with its configuration `.rumdl.toml`.
To automatically fix possible issues and format markdown files, run `rumdl fmt [PATH]+`.

Maintain configured test coverage threshold as noted in [development-workflow](./docs/development-workflow.md).

## Coding Conventions

Use full, descriptive names for variables, functions, methods, and types — never
abbreviations (e.g. `case_sensitive_sort` not `cs`, `config_path` not `cp`,
`format_timestamp` not `fmt_ts`).

**Binary + library split.** The project ships both a `[[bin]]` (`src/main.rs`) and a
`[lib]` (`src/lib.rs`) with the same crate name `ylva` (see `Cargo.toml`). Any
type, function, or module that integration tests in `tests/` need to reach must be
re-exported through `src/lib.rs` — `tests/*.rs` cannot `use` items from `src/main.rs`.

Keep `main.rs` thin (event loop, key handling).

**Module layout.** Use Rust 2018+ file-per-module style: `foo.rs`, not `foo/mod.rs`.
For the integration-test shared module see [Integration-test shared helpers (`tests/common`)](./docs/development-workflow.md#integration-test-shared-helpers-testscommon).

See [Logging](./docs/development-workflow.md#logging) for logging diagnostics in CLI and TUI.

## Guidelines for Implementation, Testing and Bugfixing

These are well-known concepts (**Semantic Anchors**).
Look up any unfamiliar principle via web search before applying it.
Structure your work and thinking to follow those principles:

- Code Smells by Fowler
- Resist scope creep
- KISS (Keep It Simple)
- SSOT (Single Source of Truth)
- YAGNI (You Aren't Gonna Need It)
- Clean Code by Uncle Bob
- SOLID Principles by Martin
- Clean Architecture by Martin
- Five Whys by Ohno — find root cause before fixing
- CoT (Chain of Thought)
- Occam's Razor
- TDD (Test Driven Development)
- Test Double: Mock / Spy / Stub by Meszaros
- Testing Pyramid by Cohn, Testing Trophy by Dodds
