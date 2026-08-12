# Convert modules to the file-per-module style

## Context

### System / component being changed

The **Ylva** Rust crate (`Cargo.toml` name `ylva`, `[[bin]]` + `[lib]`). The
change is a **source-tree convention refactor**: adopt Rust 2018+'s
file-per-module layout (`foo.rs` + `foo/bar.rs`) in place of the legacy
`foo/mod.rs` + `foo/bar.rs` style, and pin the convention as a project
guideline so all future modules — including the `src/agent/tools/` tree the
pending Phase 3 plan (`docs/plans/phase3-agent-loop-with-tools.md`) introduces —
follow it without a separate decision.

This plan is **executed before** `docs/plans/phase3-agent-loop-with-tools.md`.
Consequently the Phase 3 plan is itself a **target of planned changes**: every
path reference it contains that assumes the old `mod.rs` style must be rewritten
to the new style so a Phase 3 implementer reading only the Phase 3 plan lands on
the right files.

### Goal and motivation

- **Single module convention.** Today the tree mixes styles: `src/agent.rs` is
  already flat (new style), while `src/llm/mod.rs` and `tests/common/mod.rs`
  use the legacy `mod.rs` style. The mix is a Fowler "inconsistency" code smell
  and a recurring trap for new modules (`mod.rs` is required reading to know
  what a directory exports; the flat file surfaces it directly).
- **Forward-looking.** Phase 3 will split `src/agent.rs` into a `src/agent/`
  directory with a `tools/` submodule. Establishing the convention now means
  Phase 3 writes `src/agent.rs` + `src/agent/tools.rs` + `src/agent/tools/fs.rs`
  instead of `src/agent/mod.rs` + `src/agent/tools/mod.rs` + …, and the
  blueprint's §3 file tree stops advertising `mod.rs`.
- **KISS / SSOT.** One rule ("prefer `foo.rs` over `foo/mod.rs`"), stated once
  in `AGENTS.md`, enforced everywhere. Rust resolves both identically, so this
  is a pure move with no semantic change.

### Acceptance criteria (definition of done)

- No `mod.rs` file exists under `src/` or `tests/` after the change
  (`git ls-files 'src/**/mod.rs' 'tests/**/mod.rs'` returns nothing).
- `src/llm/mod.rs` is moved to `src/llm.rs` (contents unchanged modulo the
  intra-crate path references the move implies — see Implementation Hints).
- `tests/common/mod.rs` is moved to `tests/common.rs` and a
  `#[path = "common/mock_llm_server.rs"]` attribute is added as the
  first line (with an inline comment above it explaining the
  Cargo-imposed reason — see *Consistency Issues* and Implementation
  Plan step 2).
- `cargo build`, `make lint`, `make test`, and `make test-coverage` (≥80% on
  testable files) all pass unchanged from before the refactor.
- `AGENTS.md` gains a short "Module layout" subsection under *Coding
  Conventions* stating the rule, the rationale, and the explicit "use `foo.rs`
  - `foo/` for submodules; never `foo/mod.rs`" phrasing.
- `README.md` reflects the convention if (and only if) it currently shows a
  `mod.rs` layout; otherwise it is left untouched (it is a 6-line intro today
  with no module tree — verify, do not invent).
- `docs/blueprint.md` §3 "Project Structure" tree is rewritten so every
  `mod.rs` entry becomes the corresponding `foo.rs` entry (e.g.
  `llm/mod.rs` → `llm.rs`, `agent/mod.rs` → `agent.rs`,
  `agent/tools/mod.rs` → `agent/tools.rs`), and the §7 Phase 1 "File layout"
  row in the decisions table is reconciled to the new convention.
- `docs/plans/phase3-agent-loop-with-tools.md` is updated in the same commit so
  every path it references matches the new layout (see Implementation Plan
  step 5 for the exhaustive list). A Phase 3 implementer reading only that file
  must never encounter a `mod.rs` path.
- `docs/features.md`, `docs/configuration.md`, `docs/keybindings.md`,
  `docs/development-workflow.md`, `docs/release-notes-workflow.md`,
  `docs/persistence.md`, `docs/naming-research.md`, `docs/class-diagram.mmd`,
  and `docs/architecture-design-records/` are checked for `mod.rs` references;
  any found are rewritten. If none are found, the file is left untouched
  (do not fabricate references).
- Markdown touched by this change passes `rumdl fmt` against `.rumdl.toml`.

### Key constraints

- **No semantic changes.** Module resolution in Rust 2018 (edition = 2021 in
  `Cargo.toml`) treats `foo.rs` and `foo/mod.rs` as identical. The refactor is
  a `git mv` plus path-reference fixups; no types, traits, function bodies,
  visibility, or test assertions change. `src/lib.rs`'s `pub mod agent; pub mod
  llm;` declarations need **no edit** — they resolve to whichever file exists.
- **`src/agent.rs` stays a flat file in this plan.** Phase 3 is what splits it
  into a directory. This plan only (a) sets the convention Phase 3 must follow
  and (b) fixes Phase 3's plan doc so it follows it. Do **not** create
  `src/agent/` here — that is Phase 3's step 3.
- **`tests/common/mod.rs` move is NOT mechanical.** Cargo auto-discovers
  `tests/common.rs` as a top-level test crate, while the integration tests
  (`tests/steering.rs`, `tests/interrupt.rs`, `tests/http_provider.rs`)
  load it as an inline submodule via `mod common;`. The two crate-root
  contexts disagree on `mod mock_llm_server;` resolution, so the move
  requires a `#[path = "common/mock_llm_server.rs"]` attribute on
  `tests/common.rs` to reconcile them. See *Consistency Issues* and
  Implementation Plan step 2 for the full resolution. No
  `#[path]` attribute is in use today, so the move adds one
  attribute (not a test-side edit); the three `mod common;` declarations
  resolve to `tests/common.rs` unchanged.
- **`docs/blueprint.md` §3 is a forward-looking design tree**, not a snapshot
  of the current tree. It currently advertises `llm/mod.rs`, `agent/mod.rs`,
  `agent/tools/mod.rs`, `context.rs`, `widgets/`, `store.rs`, etc. — many of
  which do not exist yet. Only the `mod.rs` references are in scope for this
  change; do not add or remove other entries (Phase 3 owns the agent/tools
  split; future phases own their own files). The §7 Phase 1 "File layout"
  decision-table row mentions `src/agent.rs`, `src/llm.rs` (already the target
  style) — reconcile any contradiction with §3 after the §3 rewrite.
- **Phase 3 plan is a peer plan, not yet executed.** Updates to
  `docs/plans/phase3-agent-loop-with-tools.md` are doc-only edits to its
  Implementation Hints and Implementation Plan path strings; they must not
  alter Phase 3's design decisions, acceptance criteria, or step ordering. If a
  Phase 3 step's *semantics* depend on a `mod.rs`-vs-`foo.rs` choice, that is a
  Phase 3 bug to flag here, not to silently redesign (see Consistency Issues).
- **ADRs.** `docs/architecture-design-records/` contains only `.gitkeep` today
  (verified). No recorded decision covers module layout. This plan does not
  introduce an ADR (the convention lives in `AGENTS.md`); if the user later
  wants an ADR, that is a separate task.

### Concrete starting point

Open `src/llm/mod.rs`. The whole refactor pivots on this one file: move it to
`src/llm.rs`, then chase every `mod.rs` path reference in `src/`, `tests/`,
and `docs/`. The first verification is `cargo build` immediately after the
`git mv` (before any doc edits) — it must compile with zero edits to `.rs`
files, proving the move is semantically a no-op.

## Implementation Plan

1. **Move `src/llm/mod.rs` → `src/llm.rs`.** Use `git mv` so history is
   preserved. Do **not** touch `src/llm/openai_provider.rs` (its
   `pub mod openai_provider;` declaration in `mod.rs` moves with the file and
   still resolves). Run `cargo build`; expect success with no source edits.
   If `cargo build` fails, stop — the failure means an intra-crate path is
   hard-coded somewhere (search for `llm/mod` in `src/`); fix the reference,
   not the move.

2. **Move `tests/common/mod.rs` → `tests/common.rs`.** This step is
   **NOT mechanical** — see *Consistency Issues* below for the chosen
   resolution (Option A: add `#[path]` attribute).

   Concretely:

   1. `git mv tests/common/mod.rs tests/common.rs` (history-preserving rename).
   2. Add `#[path = "common/mock_llm_server.rs"]` as the **first line** of
      `tests/common.rs`, immediately above `pub mod mock_llm_server;`.
   3. Add a one-line inline comment immediately above the `#[path]`
      attribute: `// Cargo compiles this file as a top-level test crate named 'common'; the attribute forces the submodule resolution to match what the other tests expect via `mod common;`. See AGENTS.md "Module layout" for context.`
      (The comment is local-to-the-line and points readers at AGENTS.md
      for the full rule; the AGENTS.md bullet stays generic.)
   4. `cargo test --no-run`; expect success. Verify both that the
      `common` test binary compiles AND that the three other
      integration tests (`tests/steering.rs`, `tests/interrupt.rs`,
      `tests/http_provider.rs`) compile — they use `mod common;` and
      need the `#[path]` attribute to find `mock_llm_server.rs`.
   5. Confirm `tests/common.rs` line 1 is `#[path = "common/mock_llm_server.rs"]`
      and line 2 is the inline comment (catch a typo in the attribute
      before moving on).
   6. Confirm `tests/common/mock_llm_server.rs` exists at that path
      (do **not** move it — the `#[path]` attribute requires it
      there).

3. **Confirm the tree is clean.** Run
   `git ls-files 'src/**/mod.rs' 'tests/**/mod.rs'` — it must return nothing.
   Also run `rg --no-heading -t rust 'mod\.rs' src/ tests/` to confirm no
   intra-source string references to `mod.rs` remain (doc comments included).
   Any hit is a leftover comment to fix in this step.

4. **Update `AGENTS.md`** under *Coding Conventions* (after the "Binary +
   library split" paragraph): add a "Module layout" subsection stating:
   - Prefer `foo.rs` for a module's root; place submodules in a sibling
     `foo/` directory (e.g. `src/llm.rs` + `src/llm/openai_provider.rs`).
   - Do **not** use `foo/mod.rs`.
   - Rationale: the flat file surfaces a module's exports without opening a
     directory; Rust 2018+ resolves both identically, so this is convention
     only.
   - Applies to `src/` and `tests/` alike, with one documented exception:
     integration-test shared helpers under `tests/<name>/` may use
     `#[path = "<name>/<sub>.rs"]` on the helper's root file because Cargo
     also auto-discovers `tests/<name>.rs` as a top-level test crate and the
     two crate-root contexts disagree on submodule paths. The single
     instance in this repo today is `tests/common.rs`. See the inline
     comment above the `#[path]` attribute for the in-context explanation.
   Keep it to ~5 bullets; no prose essay (AGENTS.md style is bullets).

5. **Update `docs/plans/phase3-agent-loop-with-tools.md`** so every path
   matches the new layout. The exhaustive set of edits (search the file for
   each left-hand string and replace with the right-hand string):
   - `src/llm/mod.rs` → `src/llm.rs` (appears in step 4 bullet 1, and in
     Implementation Hints bullets referencing `Provider` trait at
     `src/llm/mod.rs:23`, `MockProvider` at `src/llm/mod.rs:39`,
     `serialize_messages` at `src/llm/mod.rs:92`, and "Existing
     `MockProvider` script" at `src/llm/mod.rs:44`). Preserve the line
     numbers after the path — they refer to the current file, which is now
     `src/llm.rs` with identical content (the `git mv` keeps line numbers
     stable). After the move, re-confirm each cited line number still points
     at the cited item; if any drifted (it should not, since content is
     unchanged), update the line number.
   - `src/agent/tools/mod.rs` (step 3 bullet 1: "`mod.rs`: a `Tool` trait …")
     → `src/agent/tools.rs`. The bullet describes the *file's contents*, so
     rephrase as "`tools.rs`: a `Tool` trait …". Keep the rest of the bullet
     verbatim.
   - `src/agent/tools/` (step 3 header "Add the `tools` module at
     `src/agent/tools/`") → `src/agent/tools.rs` + `src/agent/tools/fs.rs`.
     The header should read "Add the `tools` module at `src/agent/tools.rs`"
     (the `fs.rs` submodule lives at `src/agent/tools/fs.rs` per the new
     convention; the bullet for `fs.rs` already names the file, just fix its
     directory prefix implicitly by the header rewrite).
   - `tests/common/mod.rs` (Implementation Hints, "Test helpers" bullet:
     "`tests/common/mod.rs` provides `collect_until`") → `tests/common.rs`.
   - The Phase 3 plan's "Concrete starting point" says "create a
     `src/agent/tools/` module (blueprint §3 layout)". Reword to "create a
     `src/agent/tools.rs` module + `src/agent/tools/fs.rs` submodule
     (blueprint §3 layout, file-per-module style per `AGENTS.md`)".
   - Any other occurrence of `mod.rs` in the Phase 3 plan (run
     `rg 'mod\.rs' docs/plans/phase3-agent-loop-with-tools.md` after the
     edits above and fix stragglers).
   Do **not** touch Phase 3's acceptance criteria, key constraints, step
   ordering, Proposed Improvements, or Consistency Issues — those are design
   content, out of scope for a path-rename.

6. **Update `docs/blueprint.md`** §3 "Project Structure" tree:
   - `llm/mod.rs` → `llm.rs` (keep the inline comment).
   - `agent/mod.rs` → `agent.rs`.
   - `agent/tools/mod.rs` → `agent/tools.rs`.
   - Leave `context.rs`, `widgets/*`, `store.rs`, `cli.rs`, `app.rs`,
     `events.rs`, `config.rs`, `ui.rs`, `main.rs`, `openai_provider.rs`,
     `fs.rs`, `shell.rs`, `search.rs`, and the inline comments untouched —
     they are not `mod.rs` files.
   Re-check the §7 Phase 1 decisions-table row ("File layout"):
   `Flat files for Phase 1 (src/agent.rs, src/llm.rs); directories when >150
   lines (Q24)` — this already matches the new convention; no edit needed
   beyond confirming it no longer contradicts §3 (after the §3 rewrite, it
   does not).

7. **Sweep the rest of `docs/` for `mod.rs` references.** Run
   `rg --no-heading 'mod\.rs' docs/ README.md AGENTS.md`. Expected hits after
   steps 4–6: none. If any file still references `mod.rs` (e.g.
   `docs/features.md`, `docs/development-workflow.md`,
   `docs/configuration.md`, `docs/keybindings.md`,
   `docs/release-notes-workflow.md`, `docs/persistence.md`,
   `docs/naming-research.md`, `docs/class-diagram.mmd`,
   `docs/class-diagram.mmd.svg`), rewrite the path string to the new layout.
   If a doc shows a *tree diagram* with `mod.rs`, rewrite the tree
   consistently with §3. Do not invent references that are not there.

8. **Verify `README.md`.** It is a 6-line intro with no module tree today
   (read it before editing). If it contains no `mod.rs` reference, leave it
   untouched. If a future edit has added one, rewrite it. Report which case
   applied in the commit message.

9. **Run the full verification suite:**
   - `cargo fmt` (the `git mv` should not change formatting, but run it to
     be safe; commit any whitespace-only diffs separately if they appear).
   - `make lint` (cargo check + clippy) — must be warning-free.
   - `make test` — all tests pass unchanged.
   - `make test-coverage` — ≥80% on testable files, unchanged from before.
   - `rumdl fmt docs/ AGENTS.md README.md` — markdown lint clean.
   - `git ls-files 'src/**/mod.rs' 'tests/**/mod.rs'` — empty.
   Fix all findings before reporting done.

## Proposed Improvements

- **Pin the convention with an ADR.** This plan deliberately puts the rule in
  `AGENTS.md` (KISS: one place to read). If the project later grows
  multi-crate or workspace structure, consider an ADR in
  `docs/architecture-design-records/` recording the decision so external
  contributors see it without reading `AGENTS.md`. Defer until a second crate
  arrives (YAGNI today).
- **Add a CI guard.** A one-line `git ls-files 'src/**/mod.rs'
  'tests/**/mod.rs' | grep -q . && exit 1` step in CI would prevent
  regressions. This plan does not add CI (no CI config exists in-repo today);
  flag it for whoever owns CI setup.

## Consistency Issues

- [low] **`docs/blueprint.md` §3 vs §7 Phase 1 row.** Today §3 advertises
  `mod.rs` files while the §7 row names `src/agent.rs`/`src/llm.rs` (flat).
  The contradiction is pre-existing, not introduced by this plan. Step 6
  resolves it by bringing §3 in line with §7 and the new convention. No
  blocker; noted so the implementer does not think §3 and §7 disagree after
  the change.
- [low] **Phase 3 plan's "Concrete starting point" mentions
  `src/agent/tools/` (a directory) while its step 2 keeps types in
  `src/agent.rs`.** Under the new style this is fine — Rust 2018+ allows
  `src/agent.rs` (module root) and `src/agent/tools.rs` (submodule) to
  coexist without a `src/agent/mod.rs`. Step 5 rewords the Phase 3 plan to
  make this explicit so an implementer does not fabricate a `src/agent/mod.rs`
  to "bridge" them. No design change to Phase 3.
- [medium] **`tests/common/mod.rs` cannot become `tests/common.rs` without
  a Cargo conflict.** Empirical reproduction (see Implementation Hints)
  shows that any `tests/<name>.rs` file is automatically compiled by
  Cargo as a top-level integration test crate named `<name>`. That crate's
  crate root is `tests/<name>.rs`, so `mod foo;` inside it looks for
  `tests/foo.rs` — not `tests/<name>/foo.rs`. But the integration tests
  that `mod common;` still want `mod mock_llm_server;` in
  `tests/common.rs` to resolve to `tests/common/mock_llm_server.rs`.
  No single file location satisfies both. **Resolution: Option A.**
  Add `#[path = "common/mock_llm_server.rs"]` to `tests/common.rs`
  (smallest diff, retains the new convention everywhere; the "no
  `#[path]` attributes" rule is relaxed as a Cargo-imposed exception).
  An inline comment above the attribute and a generic bullet in
  `AGENTS.md` *Module layout* document the rule for future readers;
  the `docs/blueprint.md` §3 file tree stays clean (no quirk note).
  The empty `common` test binary that Cargo produces from
  `tests/common.rs` is accepted as a harmless dead artifact (no
  `#[test]` functions, so it adds <1s to `cargo test --no-run` and
  zero runtime cost).

## Implementation Hints

- **`Cargo.toml`** sets `edition = "2021"`. Rust 2018+ resolves
  `pub mod llm;` in `src/lib.rs` to **either** `src/llm.rs` **or**
  `src/llm/mod.rs`; moving the file requires no `Cargo.toml` or `lib.rs`
  edit. Verified: `src/lib.rs` lines 1–2 are `pub mod agent; pub mod llm;`.
- **Current `mod.rs` files in the tree** (verified via
  `git ls-files '**/mod.rs'`): exactly two — `src/llm/mod.rs` and
  `tests/common/mod.rs`. `src/agent.rs` is already flat. No other `mod.rs`
  exists under `src/` or `tests/`.
- **`src/llm/mod.rs` content** (8334 bytes, 236 lines): declares
  `pub mod openai_provider;` (line 9), `pub use openai_provider::OpenAiProvider;`
  (line 11), the `Provider` trait (line 23), `MockProvider` (line 39, impl at
  50), and `serialize_messages` (line 92). None of these reference
  `llm/mod.rs` by string; the file is self-contained and moves as-is.
- **`tests/common/mod.rs`**: provides `collect_until` and the
  `mock_llm_server` submodule (`mod mock_llm_server;` resolving to
  `tests/common/mock_llm_server.rs`).

  **Verified during execution: the move is NOT mechanical and creates
  an unavoidable Cargo conflict.** Empirical reproduction in
  `/tmp/rust-mod-test` with a minimal library crate containing exactly
  `tests/common.rs` + `tests/common/mock_llm_server.rs` + a sibling
  integration test that uses `mod common;`:

  - `cargo test --no-run` fails with **two** `E0583` errors from the
    same `tests/common.rs:1` declaration, but with **different**
    suggested paths:
    - `(test "common")` — Cargo also compiles `tests/common.rs` as a
      top-level integration test crate (its mere presence in `tests/`
      triggers that). For that crate, `mod mock_llm_server;` looks
      for `tests/mock_llm_server.rs` (same directory as the crate
      root).
    - `(test "other")` — the sibling test that does `mod common;`
      loads `tests/common.rs` as a submodule. For that crate,
      `mod mock_llm_server;` looks for `tests/common/mock_llm_server.rs`
      (sibling directory of the file).

  No single file location satisfies both. Three ways out:

  1. Add `#[path = "common/mock_llm_server.rs"]` to `tests/common.rs`
     so both crates agree on the submodule path (works; adds one
     `#[path]` attribute — the plan's "no `#[path]` attributes" rule
     is the *verification* claim, not an absolute ban; discuss in step
     2).
  2. Keep `tests/common/mod.rs` and accept that the convention does
     not apply to integration-test shared helpers (one documented
     exception in `AGENTS.md`).
  3. Rename `tests/common/` to `tests/<other-name>/` (e.g.
     `tests/shared_helpers/`) and update the three integration tests
     that declare `mod common;` to declare the new module name. Larger
     diff but convention-clean.

  **Resolution chosen: option 1** (add `#[path = "common/mock_llm_server.rs"]`
  to `tests/common.rs`). See *Consistency Issues* for the rationale and
  Implementation Plan step 2 for the exact edits.
- **`src/lib.rs` re-exports** (`pub use agent::{…}; pub use llm::{…}`) are
  path-based on the *module* (`agent`, `llm`), not on file paths — they need
  no edit.
- **`docs/blueprint.md` §3 tree** (lines 332–358) is the only place a
  file-tree diagram advertises `mod.rs`. The §7 Phase 1 decisions row
  (line 2710) already uses `src/agent.rs`/`src/llm.rs`.
- **`docs/plans/phase3-agent-loop-with-tools.md`** path references to update
  are enumerated exhaustively in Implementation Plan step 5; an
  `rg 'mod\.rs' docs/plans/phase3-agent-loop-with-tools.md` after applying
  them must return no hits.
- **`AGENTS.md`** currently has a *Coding Conventions* section ending at the
  "Keep `main.rs` thin" paragraph (line 84). The new "Module layout"
  subsection appends directly after it, before the *Guidelines for
  Implementation, Testing and Bugfixing* heading (line 86).
- **`rumdl`** config (`.rumdl.toml`) sets `line-length = 100`; the
  `AGENTS.md` addition and blueprint §3 rewrite must stay within it.
  `rumdl fmt` will auto-wrap if not.
- **No ADR covers module layout** today
  (`docs/architecture-design-records/` contains only `.gitkeep`, verified).
  The convention is recorded in `AGENTS.md` only; no ADR action is required
  by this plan.
