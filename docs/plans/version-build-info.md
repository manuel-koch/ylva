# Plan: Version and Build Info

## Context

### System / component being changed

The **Ylva agent-harness** (Rust). Phases 1–3 are complete (headless agent
with a real HTTP provider and a ReAct tool loop); the TUI has **not** landed
yet (Phase 4 per `docs/blueprint.md` §7). This plan covers a small,
self-contained enhancement: exposing **build provenance** of the compiled
binary — `version`, `git_commit`, `build_timestamp`, `profile` — through a
`build_info` module that the CLI and (later) the TUI surface on demand.

Concretely:

- The build pipeline (`cargo build`) must embed four values into the binary
  via `env!`/`option_env!`: `CARGO_PKG_VERSION` (already free at compile
  time), the current `HEAD` commit hash, the UTC build timestamp, and the
  build profile (debug/release).
- The CLI must print this info when invoked with `--version`.
- The TUI (Phase 4) must show the same info via a keybinding + popup
  "About" dialog. **The TUI has not landed yet**, so this plan only
  records the TUI contract; the TUI implementation is Phase 4 work.

### Goal and motivation

- Make it trivial to answer "which exact build is running on this host?"
  when debugging, filing issues, or verifying a release tag. Today the
  answer requires `git describe` and a clock comparison against an unknown
  compile time.
- Provide a stable machine- and human-readable version string that a
  release pipeline (and `make tag-version`) can correlate with tags.
- Establish the **source of truth** (a single `build_info` struct in
  `src/build_info.rs`) so the TUI, the CLI, and any future diagnostic
  command read identical values.

### Acceptance criteria (definition of done)

- `src/build_info.rs` defines a **`const BUILD_INFO: BuildInfo`** whose
  fields are all `&'static str`: `version`, `git_commit`,
  `build_timestamp_utc`, `profile` (exact field names to be decided in
  implementation). The struct derives `Debug, Clone, Copy, PartialEq, Eq`
  and is populated from compile-time `env!`/`option_env!` values.
- `version` is sourced from `CARGO_PKG_VERSION` (`Cargo.toml`,
  currently `0.3.1`).
- `git_commit` is the short (8-character) SHA-1 of `HEAD` at compile time,
  derived from `git rev-parse --short=8 HEAD` and injected via a `build.rs` (or
  `cargo:rerun-if-changed` + a committed git command) so the value lives
  in the binary. Fall back to `"unknown"` when not in a git checkout
  (e.g. a `cargo install` from a tarball).
- `build_timestamp_utc` is the UTC compile-time timestamp in
  `2026-08-13T16:45:00+Z` shape (the trailing `+Z` is literal, matching
  the requirement). Captured via a `build.rs` timestamp (see the
  `time` vs `chrono` decision in Implementation Hints) and
  embedded as a string — this guarantees the value reflects when the
  binary was **built**, not when it was first run.
- `profile` is `debug` or `release`, derived from the build profile in
  `build.rs` (see Proposed Improvements item 1, now adopted).
- `BuildInfo` is reachable from `src/lib.rs` (re-exported) so
  integration tests can verify the field shapes.
- The CLI (`src/main.rs`) handles `--version`: it prints the `BuildInfo`
  (one field per line, in the order version / commit / timestamp /
  profile) to **stdout** and **exits immediately with status `0`**,
  short-circuiting before the Tokio runtime, the provider, and the agent
  loop are touched. `--version` coexists with — and simply precedes —
  `--prompt`/stdin: whichever arg order, `--version` wins and exits.
  Any other CLI behavior is unchanged (still `--prompt`, stdin, or the
  demo prompt).
- The TUI contract: when the TUI lands in Phase 4, a dedicated keybinding
  opens a popup "About" dialog rendering the same `BuildInfo` values.
  This plan records the contract; the actual TUI work is Phase 4
  (`docs/blueprint.md` §7).
- `make lint`, `make test`, and `make test-coverage` (≥80% on testable
  files; `main.rs` excluded) pass.
- The markdown docs that mention version/build info are updated in the
  same commit (`docs/features.md` and any other doc that lists
  what `--version` does).

### Key constraints

- **Compile-time provenance, not runtime.** `git_commit` and
  `build_timestamp_utc` must be **embedded** in the binary at compile
  time, not computed at startup. Computing them at startup would give the
  wrong value (the binary carries its build info; running it on a
  different host with `git` not on PATH must still report the right hash).
- **`main.rs` stays thin** (AGENTS.md): argument parsing for `--version`
  lives next to the existing `--prompt` and stdin handling; the heavy
  lifting (formatting, field order) lives in `build_info`.
- **Hermetic tests.** `git_commit` derivation is unit-tested with a
  fixture or by reading from the compile-time constant — no live `git
  rev-parse` in tests.
- **Toolchain impact is minimal and build-only.** The timestamp needs
  one small date formatter inside `build.rs`; this is a
  `build-dependencies` entry only (per the `time` vs `chrono` decision in
  Implementation Hints), never a runtime dependency of the binary.
- **Coverage threshold.** The new module sits above the threshold; the
  test plan in step 6 covers the formatting helpers, `BUILD_INFO`,
  and the unknown-commit fallback. The `--version` CLI path
  lives in `main.rs` (excluded from coverage) but is covered by an
  integration test that spawns the binary.
- **TUI scope is recorded, not implemented.** The TUI does not yet
  exist; this plan only fixes the contract so Phase 4 can wire to a
  stable struct. The TUI keybinding itself (which key, the dialog
  layout, the close behavior) is Phase 4.
- **No re-tagging, no release-process changes.** `make tag-version`
  already uses `Cargo.toml` `version`; the new `git_commit` field is
  complementary, not a replacement for tags.

### Concrete starting point

Open `src/lib.rs` and add a new `pub mod build_info;` plus a re-export
of `BuildInfo` (and `BUILD_INFO`). Then create `src/build_info.rs` and
`build.rs` (the latter at the crate root, picked up automatically by
Cargo). Finally, wire `--version` into the early branch of `main()` in
`src/main.rs:25`. The first test to run is `cargo build` (must compile)
followed by `cargo test`.

## Implementation Plan

1. **Add a `build.rs`** at the repo root that:
   - Runs `git rev-parse --short=8 HEAD` and writes the trimmed short SHA-1
     (8 hex chars — short form, per adopted decision) to `OUT_DIR` as
     `GIT_COMMIT`. When `git` is missing or the working
     directory is not a git checkout (`!Command::new("git").status().is_ok()`
     or the rev-parse fails), write the literal string `"unknown"`.
   - Writes the current UTC timestamp formatted as `"%Y-%m-%dT%H:%M:%S+Z"`
     (the trailing `+Z` is literal, per the requirement) to `OUT_DIR`
     as `BUILD_TIMESTAMP_UTC`. The `build.rs` re-runs when the source
     tree changes (`cargo:rerun-if-changed=build.rs`); it does **not**
     need a `rerun-if-changed=.git/HEAD` because builds already run
     after a checkout update.
   - Writes the build profile as `PROFILE` from `std::env::var("PROFILE")`.
   - Uses `std::process::Command` for `git` and the `time` crate for the
     timestamp formatter (see the `time` vs `chrono` decision in
     Implementation Hints).
   - Decide the env var name(s) the binary will read (`YLVA_GIT_COMMIT`,
     `YLVA_BUILD_TIMESTAMP_UTC`, `YLVA_PROFILE`); document in a doc
     comment on the `build_info` module so they are discoverable.

2. **Create `src/build_info.rs`**:
   - Define `pub const BUILD_INFO: BuildInfo` where the struct holds
     `&'static str` for each of the four fields (`version`, `git_commit`,
     `build_timestamp_utc`, `profile`). A `const` avoids a function call
     and future consumers can use it in `const` contexts (Proposed
     Improvements item 4, now adopted). The struct derives
     `Debug, Clone, Copy, PartialEq, Eq` so tests can compare against
     fixtures and `main.rs` can format trivially.
   - Use `env!` for values the build script always writes (timestamp,
     profile) and for the git commit (its fallback is the literal
     default `"unknown"`). Missing values then surface as a compile
     error rather than a silent `""`.

3. **Re-export `BuildInfo` from `src/lib.rs`** (the binary+library
   split: integration tests live in `tests/` and cannot `use` items
   from `src/main.rs`). Add a small `tests/build_info.rs` integration
   test that:
   - Asserts `BUILD_INFO.version` equals `env!("CARGO_PKG_VERSION")`.
   - Asserts `git_commit` is either 8 hex chars or the literal
     `"unknown"`.
   - Asserts `build_timestamp_utc` matches the regex
     `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\+Z$` (anchor the `+Z` so
     a plain `+00:00` style would fail; the requirement is explicit
     about the literal `+Z`).
   - Asserts `profile` is `"debug"` or `"release"`. (This integration
     test sees `BUILD_INFO` as compiled; a separate unit test below
     covers the shape of a hand-built `BuildInfo` fixture.)

4. **Add a `format` helper** on `BuildInfo` (e.g. `fn render(&self) ->
   String`) that returns the canonical multi-line representation used by
   the CLI: `version: <v>\ncommit: <hash>\nbuilt:  <ts>\nprofile: <p>`
   (field labels chosen so they are stable for scripts to parse). Unit-test
   the rendering with a hand-built `BuildInfo` fixture (do **not** read from
   `BUILD_INFO` in unit tests so the assertion stays deterministic across
   timezones / build machines) — this unit test is what asserts the
   `profile` field shape in isolation.

5. **Wire `--version` into `src/main.rs`**:
   - At the very top of `main()` (before any runtime setup, before
     provider selection), inspect `std::env::args()`. If **any** argument
     equals `--version`, print `BUILD_INFO.render()` to **stdout** and
     `return` (exits with status `0`) — regardless of whether other args
     like `--prompt` are present. `--version` always wins and exits
     immediately (Open Question resolved: it coexists with, and precedes,
     the normal flow). Do **not** start the Tokio runtime, the provider,
     or the agent loop on this path.
   - The `--version` branch must sit **above** the runtime setup in
     `main()`: currently `src/main.rs:25` builds the Tokio runtime and
     `AgentLoop` and only later processes args. The short-circuit
     must come first so `--version` never spins up the runtime or
     selects a provider (see Implementation Hints).
   - Add an integration test in `tests/cli_version.rs` that:
   - Builds the binary via `assert_cmd` or shells out to
     `cargo run --quiet -- --version` (prefer `assert_cmd` for
     hermetic capture; if it is not already a dev-dependency, add it
     to `Cargo.toml`).
      - Asserts the exit code is `0`, stdout contains the `Version:`
        line with `v0.3.1`, contains a `Commit:` line with either a
        8-char hex hash or `unknown`, a `Built:` line matching the
        timestamp regex above, and a `Profile:` line with `debug` or
        `release`.
      - Asserts the binary **does not** emit any provider wiring to
        stderr in this path (i.e. no `[ylva] provider=…` line — the
        version path must short-circuit before `select_provider`).
      - Add a second case: invoke with both `--version --prompt "hi"` and
        assert it still exits `0` and prints only the build info (the
        "always wins" resolution).

6. **Wire `BuildInfo` access for the future TUI** without building the
   TUI itself. Add a thin `pub use build_info::BuildInfo;` and document
   in a module doc comment: "Phase 4 (TUI) will render this in an 'About'
   popup via a dedicated keybinding; the contract here is final." No
   changes to the TUI code (there is none yet); the doc note is the
   contract anchor.

7. **Documentation step** (docs are part of the change set — update in
   the same commit):
   - `docs/features.md`: add a bullet under "General" describing
     `--version` (CLI flag, prints version / commit / build timestamp /
     profile from `BUILD_INFO`; exits 0 without starting the runtime)
     and a forward-reference to Phase 4 for the TUI About popup.
   - `docs/keybindings.md`: add a placeholder row for the future TUI
     "About" keybinding (which key is Phase 4's decision; record it
     here only when the TUI lands — for now, add a section heading
     "TUI About dialog (Phase 4)" with a one-line note that the
     keybinding TBD will show `BuildInfo`).
   - `docs/configuration.md`: unaffected (no config file involved).
   - `docs/blueprint.md`: under §7 Phase 1, add a checkoff for
     "Build provenance surfaced via `--version`". (The TUI popup line
     goes under §7 Phase 4 when the TUI lands — do not pre-check it
     here.)
   - `docs/architecture-design-records/`: still empty (only
     `.gitkeep`); no ADR covers build provenance. No new ADR is
     warranted for this small a change (a one-liner about a
     `build_info` module does not merit an ADR — it is a single
     source of truth, not an architectural decision). If a reviewer
     disagrees, propose an ADR; do not create one preemptively.

8. **Verify** with `make lint`, `make test`, and `make test-coverage`
   (≥80% on testable files; `main.rs` excluded). Run `rumdl fmt` on the
   touched markdown and re-run `rumdl` to confirm zero findings. Fix all
   findings before reporting done.

## Proposed Improvements

- **Add `--version=json`** as a sibling flag that emits the same fields
  as a one-line JSON object (`{"version":..., "commit":..., "built":...}`)
  for tooling to parse without regex. Optional and small; consider in a
  follow-up.
- **Consider `clap`** for CLI parsing instead of hand-rolling `--version`
  alongside `--prompt` and the stdin fallback. The current codebase
  deliberately avoids `clap` in early phases (Phase 3 §7 scope: "no
  config parsing, no TOML, no CLI"), so this is a Phase 5+ cleanup. Not
  part of this plan.

## Implementation Hints

- **`--version` must short-circuit above the runtime setup.** The current
  `src/main.rs:25` flow builds the Tokio runtime and the `AgentLoop` and
  only later processes arguments (the `--prompt` and stdin handling run
  after the runtime). The `--version` branch must be placed *before* all of
  that so `--version` never starts the runtime or selects a provider. This
  is the first pre-runtime escape hatch in `main.rs`. No ADR change is
  required.
- **`build.rs` location and wiring**: Cargo automatically picks up
  `build.rs` at the crate root; no `Cargo.toml` change is required
  beyond the optional build-dependency. The script must `println!` the
  values it wants the binary to see (`cargo:rustc-env=NAME=value`); the
  binary then reads them with `env!("NAME")`.
- **Reading the env vars in `build_info.rs`**: prefer `env!` over
  `option_env!` for the values `build.rs` always writes, so missing
  values surface as a compile error rather than a silent `""`. Use
  `option_env!` only if a missing value is a legitimate state
  (e.g. for an optional feature flag).
- **`time` vs `chrono` for the UTC timestamp (pros/cons):**
  - Hand-rolled `std`: **pros** — zero dependencies, no lockfile churn.
    **cons** — `SystemTime::now()` is epoch-based; breaking down to a
    UTC date needs ~20 lines of leap-year-ish arithmetic, error-prone and
    hard to review.
  - `time` crate: **pros** — small (`OffsetDateTime::now_utc().format`),
    no transitive deps, focused API, stable. **Cons** — one new
    `build-dependencies` entry and its feature flags can be fiddly (need
    `formatting` feature for `format`).
  - `chrono` crate: **pros** — the de-facto standard, `chrono::Utc::now()`
    is one line. **Cons** — much heavier (wider API, larger transitive
    dependency graph) for a single timestamp; brings more than needed.
  - **Decision: `time` is the recommended choice** for a single UTC
    formatter — tiny footprint and a clean API. Use it as a
    `build-dependencies` entry only, never in the runtime crate. Add it
    in the same commit as `build.rs`.
- **Build profile injection**: in `build.rs`,
  `std::env::var("PROFILE")` returns `debug`/`release` (Cargo sets it for
  build scripts). Inject via `cargo:rustc-env=YLVA_PROFILE=$PROFILE` and
  read with `env!("YLVA_PROFILE")`.
- **Git commit fallback**: `git` may be missing on the build host, or
  the source may come from a tarball (no `.git/`). Catch the
  `std::io::Error` and the non-zero exit code; do not panic in
  `build.rs` — a build script that panics fails the whole `cargo
  build` with a confusing error. Write `"unknown"` and let the binary
  decide how to present it.
- **Test the `BuildInfo` shape, not the values**: unit tests should
  not assert a specific SHA-1 or timestamp (those change on every
  commit). Assert the *shape*: 8 hex chars or `"unknown"`, the
  timestamp regex, and the version equals `CARGO_PKG_VERSION`.
- **Spawning the binary in integration tests**: the existing `tests/`
  tree uses `assert_cmd` only if it is already a dev-dep; check
  `Cargo.toml` first. If it is not, prefer
  `std::process::Command::new(env!("CARGO_BIN_EXE_ylva"))` (Cargo sets
  this env var for integration tests) to avoid adding a new dev-dep.
- **Coverage**: `src/build_info.rs` must be covered (the regex/timestamp
  helpers, the fallback path, the renderer). `src/main.rs` is excluded
  by `Makefile`; the `--version` short-circuit is covered by the
  integration test in `tests/cli_version.rs`. `build.rs` is not a
  normal Rust source file for coverage purposes (cargo-llvm-cov
  typically ignores it); assert the `build.rs` behavior via the binary
  it produces.
- **Lint**: `clippy::needless_return` and `clippy::redundant_else` may
  fire on the `--version` short-circuit. Use the idiomatic early
  return (`if arg == "--version" { …; return; }`) and let clippy guide
  any micro-cleanup.
- **Field ordering in `BuildInfo::render()`**: pick a stable order
  (`version`, `commit`, `built`) and document it; downstream scripts
  may parse the output. Changing the order is a breaking change for
  any such script.
- **AGENTS.md compliance**: `main.rs` stays thin — the formatting
  logic lives in `build_info`, `main.rs` only calls
  `println!("{}", BUILD_INFO.render())` and returns. No
  comments in source per AGENTS.md.
