# Development workflow

After implementing new functionality or fixing bugs, run `make format lint` and the
single mandatory test gate, `make test-coverage`.

`make test-coverage` runs the full test suite and enforces the coverage
threshold, so running `make test` in addition is redundant and is not required.

For new features come up with appropriate test case(s) to verify them.

Bugfixes MUST add or strengthen a test that fails before the fix and passes after.

If test runs exhibit flaky results/errors, the tests must fixed immediately,
don't postpone fixing (pre-existing) flaky tests!

Coverage threshold: **≥80% aggregate line coverage** is enforced by
`make test-coverage` (below the threshold the command fails).

The following `make` targets are available:

```shell
make format                          # Enforce consistent formatting on known files
make format-rust                     # Enforce consistent formatting on Rust files
make format-markdown                 # Enforce consistent formatting on Markdown files
make lint                            # cargo check + cargo clippy — fix all warnings before committing
make test-coverage                   # mandatory final test gate: run tests + print per-file coverage summary; fails if <80%
make test-coverage-missing           # same as test-coverage but also prints uncovered line numbers
make test                            # optional: cargo test — all tests must pass (no coverage check)
make test-e2e-persistence              # run live-provider persistence tests; missing variables fail
make build-debug                     # cargo build (debug)
make build-debug-and-run             # build debug binary and launch it
make build-debug-and-run-with-logs   # build debug binary and launch it with tracing logs enabled
make build-release                   # cargo build (release)
make build-release-and-run           # build release binary and launch it
make install                         # cargo install --path . → ~/.cargo/bin/ylva
make tag-version                     # tag HEAD with version from Cargo.toml
make outdated-dependencies           # cargo update --dry-run — show available upgrades
make upgrade-dependencies            # cargo update — apply dependency upgrades to Cargo.lock
```

The `Makefile` at the workspace root is the source of truth for `make` targets.
This section is a documentation mirror — when a target is added, removed, renamed,
or its command changes, update this section in the same commit (see
[AGENTS](../AGENTS.md) → *Docs are part of the change set*).
Other documents that reference `make` targets must delegate here rather than
re-list targets, otherwise the lists drift the next time the `Makefile` changes.

## Rust linter

Lint suppression (clippy or rustc, e.g. `dead_code`/`unused_variables`) is a
case-by-case call by the user — never a blanket `#![allow(...)]` — applied to
the specific item that triggers it:

```rust
#[expect(clippy::too_many_arguments)]
fn some_fn(...) { ... }
```

Use `#[expect(...)]`, not `#[allow(...)]`: it only suppresses the lint while
the lint actually fires. Once the code changes so it no longer fires, rustc
warns `unfulfilled_lint_expectations` — remove the now-obsolete attribute (or
replace it with a freshly justified one).

`make lint` runs `--all-features`; also check default-features `cargo clippy`
for suppressions near `#[cfg(feature = ...)]` code, since expectations can be
fulfilled under one feature set and not the other.

### Anti-pattern: "params struct" to silence `too_many_arguments`

Do not introduce a single-use struct whose only job is to pack a function's
arguments together so the function itself takes one struct parameter instead
of many — especially when the body immediately destructures it back into the
same local names, e.g.:

```rust
// Don't do this just to dodge the lint:
struct FooParams<'a> {
    a: &'a str,
    b: bool,
    c: Option<u16>,
}

fn foo(params: FooParams<'_>) {
    let FooParams { a, b, c } = params; // immediately unpacked, no benefit
    ...
}
```

This adds a type and a destructuring block with no readability or reuse
benefit over the plain argument list it replaces. Before reaching for
`#[expect(...)]`, work through these in order:

1. **Extract a real struct** if the arguments are only ever used together as
   one meaningful, reusable value across multiple call sites (a real cohesive
   concept, not a lint dodge) — see `ToolBubbleArgs`/`ToolDonePayload` for
   examples of this being justified.
2. **Split the function** if it is doing more than one job (Fowler's "Long
   Parameter List"/"Long Method" smells) — each half then takes fewer args.
3. **Drop derivable args** — an argument computable from another argument
   already in the list (or from `self`/existing state) doesn't need to be
   passed in separately; compute it inside the function instead.
4. **Move the function onto the type it operates on** (`impl` method) if
   several args are really just that type's own fields — `&self`/`&mut self`
   then replaces those positional args.

Only once none of these genuinely fits — the arguments are unrelated,
single-use, and the function's responsibility is already minimal — suppress
the lint with `#[expect(clippy::too_many_arguments)]` as shown above, rather
than inventing a throwaway struct.

### Anti-pattern: magic values in docstring and strings

We should not add magic values to docstring or string when the real value
is actually a known constant we use in the implementation.
Thos texts will drift when the constant value gets changed !

Either rephrase the text without the constant value ( if that still makes
sense ) or embed the constant value in the docstring ( by reference ) or
format the string when the code runs ( to include the constant value ).

## Logging

Diagnostics are emitted through the `tracing` facade macros
(`trace!`/`debug!`/`info!`/`warn!`/`error!`) with structured fields — never
`println!`/`eprintln!` (console output is user-facing) and never a subscriber
call; `src/logging.rs` owns the subscriber (level filter, a single file in the
`text` or `json` format selected via `--log-format`).

Correlate related events inside a `tracing` span so every log line is
attributable to its context. E.g. tool execution runs inside a `tool_execute`
span (carrying `tool` and `tool_call_id` fields) with a nested `tool_args` span.
Span context renders inline in text and as the `spans` array in JSONL (see
`docs/features.md`). Use spans the same way for other long-running scopes
(e.g. a request/stream), and only for data safe to log.

Sensitive values (API keys, credentials) MUST NOT be written to the log: call
sites MUST reduce them to a presence indicator (`set`/`unset`/`???`/`***`) before passing
them to a `tracing` macro.

### Tracing subscriber isolation in tests

See [known-test-issues.md — Tracing subscriber isolation](known-test-issues.md#tracing-subscriber-isolation-in-tests)
for the root cause, required lock acquisition rules, and `run_locked` usage.

### Process-global cwd mutation in tests

See [known-test-issues.md — cwd mutation](known-test-issues.md#process-global-cwd-mutation-in-tests)
for why tests that read `current_dir()` must also hold `SUBSCRIBER_LOCK`.

## Planned implementations

Each implementation is driven by a dedicated plan document kept in
`docs/plans/<topic>.md` — one file per subject/feature. A plan states:

- the goal and scope (what "done" means),
- an ordered, independently-verifiable task checklist,
- for each task, the tests that must verify it,
- a **documentation step** that lists every `docs/*` file the change touches.

The plan is the source of truth for the work.

## Plan execution

When executing a plan:

- complete tasks in order; verify each via the test suite and linter before
  moving on (Testing Pyramid / TDD semantics).
- each committed change bundles implementation + tests + the **documentation
  update named in the plan's documentation step** — never commit code without
  its doc changes (see [AGENTS](../AGENTS.md) → *Docs are part of the change set*).
- before executing, check the change against the recorded decisions in
  [architecture-design-records](../docs/architecture-design-records/) and keep
  the implementation consistent with them; introduce a new record when the
  change deviates.
- if a plan doc is found to be out of date, update the plan to match the work;
  stale docs are bugs.

A plan step is "done" only when its docs are updated too. Partial completion is
not completion.

## Integration-test shared helpers (`tests/common`)

Shared test-support code lives in `tests/common.rs` and is pulled into each
integration test (`tests/*.rs`) via `mod common;`, with helpers reached as
`common::<item>`.

- **Location.** `tests/common.rs`, **not** `tests/common/mod.rs` — a
  `tests/common.rs` + `tests/common/` directory pair is the established layout;
  adding `tests/common/mod.rs` collides and triggers `E0761]::file for module
  \`common\` found at both ...`.
- **Submodules.** Larger helpers go in `tests/common/<name>.rs` and are wired
  from `tests/common.rs` with `#[path = "common/<name>.rs"] pub mod <name>;`
  (see `mock_llm_server`).
- **Dead code.** Cargo compiles `tests/common.rs` into *every* test crate that
  declares `mod common;`, so any helper unused by a given test crate would warn
  as dead code. Therefore `tests/common.rs` and every submodule under
  `tests/common/` must start with `#![allow(dead_code)]`
  (see `tests/common/mock_llm_server.rs` for the established form).
- **No duplication.** Add a new shared helper to `tests/common.rs` (small) or a
  new `tests/common/<name>.rs` submodule (larger); never copy a helper across
  test files. SSOT applies here.

### Spawning the compiled binary in an integration test

Most integration tests drive an `AgentLoop` in-process. When the behavior under
test only exists in the assembled product — argument parsing, provider
selection from the environment, or where a file lands under `${YLVA_HOME}` — spawn
the real binary instead:

- Launch it via `Command::new(env!("CARGO_BIN_EXE_ylva"))` (see
  `tests/cli_version.rs` for the minimal form, and
  `tests/store_persistence_e2e.rs` for the full pattern).
- Give it a `tempfile::TempDir` as **both** `YLVA_HOME` (`Command::env`) and its
  working directory (`Command::current_dir`): `YLVA_HOME` names the Ylva home
  directly (`<tempdir>/chats`, `<tempdir>/profiles`, and the fixed cwd makes
  any cwd-derived key deterministic. `YLVA_HOME` redirects Ylva's state and nothing else.
- Point it at a `common::mock_llm_server::MockLlmServer` through the same
  environment variable a user would use (`OLLAMA_BASE_URL`).
- Isolate the child's environment with an **allowlist**: `Command::env_clear()`
  first, then re-add only what the binary needs (`PATH`, `YLVA_HOME`, and the
  variables the test itself is exercising). A blocklist of `env_remove` calls
  stops isolating the test the day a variable is added or renamed, so nothing
  from the developer's or CI's ambient environment may survive by default.
- Because `Command::output()` blocks, run it from `spawn_blocking` under a
  `#[tokio::test(flavor = "multi_thread")]`, otherwise the mock server's accept
  loop is starved.
- **Live-provider variants.** Add `#[ignore]`d tests that forward whichever of
  `OLLAMA_*`/`OPENROUTER_*` the *test runner's own* environment already has
  (the names `tests/e2e/*/.envrc` export) into the same assertion helper, and
  fail with a clear configuration error when the required variable is unset.
  CI stays offline because the variants remain ignored; a developer who
  `direnv allow`ed one of the `tests/e2e/*` directories runs
  `cargo test --test store_persistence_e2e -- --ignored` against a real backend
  with no duplicated wiring or assertions.

## Unit-test shared helpers (`src/test_support.rs`)

Large owner-local suites use `#[cfg(test)] mod tests;` with
`<owner>/tests.rs`; responsibility-specific descendants live under
`<owner>/tests/`, sharing fixtures from that local parent (see
`src/llm/engine/tests.rs`, `src/tui_reducer_state/tests.rs` and
`src/presenter/tests.rs`). Presenter live/replay buffer comparisons and
cross-input effect-order assertions remain at the facade, alongside shared
local fixtures; its keyboard, mouse, domain and bubble suites live in
`presenter/tests/`. Driver frame/title/history orchestration and cross-owner
clarify routing assertions remain in `tui_driver/tests.rs`, alongside shared
fixtures; effects, input, models, terminal and update suites live in
`tui_driver/tests/`. Widget rendering suites follow the bubble, chat, footer,
help and tool-bubble owners under `tui_widgets/`; small pure selection,
diff and path-label suites remain beside their implementations.
`tests/tui_render_state.rs` retains its integration target and uses
`tui_widgets/chat/cache.rs` as its large source-backed highlighting fixture.
Persistence event, session-file, writer and replay suites live under
`store/tests/`, with shared fixtures and cross-owner lifecycle assertions in
`store/tests.rs`. Sandbox suites live beside their private rule, path, command
and network owners under `sandbox/<owner>/tests.rs`, keeping private helpers
private; they share the sandbox fixture from `sandbox/tests.rs`, which also
holds cross-owner assertions.
Small boundary suites can remain beside the
extracted implementation (see `src/agent/tools/read/stream.rs` and
`src/agent/tools/read/windows.rs`). Cross-owner assertions stay at the facade.

Helpers shared only by unit tests (`#[cfg(test)]` modules inside `src/*.rs`)
live in `src/test_support.rs`, wired from `src/lib.rs` via
`#[cfg(test)] mod test_support;` so the module is compiled into test builds
only. It currently provides `SharedLog` and `capture_logs`: `SharedLog` is a
synchronous in-memory `tracing` writer, and `capture_logs` runs an async body
under it and returns the captured text. The in-memory buffer is deliberate — a
file-backed `tracing_appender::non_blocking` writer races its worker thread, so
its output is not deterministic point-in-time.

Since integration tests run in their own crate, they cannot use this module —
shared integration-test helpers go in `tests/common` (see above). Never copy
`SharedLog`/`capture_logs` (or any other helper) into a per-module test block;
SSOT applies here as well.

## `cfg(test)` does not reach library code from integration tests

See [known-test-issues.md — cfg(test) does not reach library code](known-test-issues.md#cfgtest-does-not-reach-library-code-from-integration-tests)
for the explanation and the required abstraction pattern.
