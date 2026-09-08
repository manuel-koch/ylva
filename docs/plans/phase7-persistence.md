# Phase 7: Polish & Persistence

An implementing agent must flip `[ ]` to `[x]` immediately after completing
each numbered step below and rewrite this file in place, so progress is
resumable across sessions.

## Context

**System:** the `ylva` Rust TUI agent-harness (binary + library crate, same
name, per `docs/development-workflow.md` / `AGENTS.md`). This plan implements
`docs/blueprint.md` → §7 **"Phase 7: Polish & Persistence"** (currently all
unchecked `[ ]`), the phase blueprint tags "usable daily driver."

**Goal / motivation:** Phases 0–6 built a working headless + TUI agent loop
with tools, child-agent delegation, and interface seams (`Engine`/
`AgentFactory`), but nothing is persisted across process restarts, code
highlighting is a single hardcoded theme, and there is no way to configure
engines/profiles/theme from a file or issue quick in-chat commands. Phase 7
closes these gaps so the agent is durable and configurable enough for daily
use.

**Scope note (deliberately narrowed during planning):** the blueprint's §4G
*Config-Driven Types* section and §4C *Tiered Context State Machine* describe
much larger systems than Phase 7's checklist bullets ever asked for. To keep
this plan honest and avoid scope creep, three things were carved out into
their own new blueprint phases (inserted into `docs/blueprint.md` §7 as part
of this planning pass, shifting the former "Phase 8: CLI Surface" to "Phase
10"):

- **Phase 8: Full Configuration & Persona System** — the three-layer
  `.ylva`/`~/.ylva`/embedded-`config/` fallback chain, `ylva init`/`ylva init
  --project`, `SOUL.md` persona loading, global `agents/*.md` type files,
  `.env` auto-loading, and per-tool sandbox rule configuration.
- **Phase 9: Context Compaction & Token Budgeting** — `TieredMessage`/
  `MessageTier`, token counting (`tiktoken-rs`), `maybe_compact`'s
  hysteresis trigger/target algorithm, and tool-result truncation at
  insertion.
- A third built-in `syntect` theme ("monokai") is **not** a phase item at
  all — no bundled `syntect` default theme is named "monokai", so it needs a
  separately-sourced/licensed asset. It is tracked as a standalone idea in
  `docs/ideas/monokai-theme.md`, not scheduled to any phase.

This plan (Phase 7 itself) covers only: the `Store`/JSONL persistence layer
(including child-agent records), a narrow single-file config loader, the two
bundled themes (dark/light) with existing live-rendering behavior unchanged,
and three slash commands (`/models`, `/tools`, `/clear`/`/reset` — `/save` was
dropped and `/agent` is deferred, see Implementation Hints).

**Acceptance criteria (definition of done for this plan):**

- Conversations (and child-agent `child_start`/`child_result` records) are
  durably written as append-only JSONL under `~/.ylva/chats/<project_key>/`
  per §4H, through a single `Store` writer used for both message types (no
  separate child-only persistence path — this explicitly finishes the item
  dropped from `docs/plans/phase6-terminal-superpowers.md` at "Phase 6.12").
- An existing session can be resumed via an explicit `--conversation-id
  <uuid>` (no auto-resume, no picker) in both headless and TUI modes; an id
  that does not match an existing session file is a **hard error**, never a
  silent new-session fallback.
- `Store::list_sessions(project_key)` enumerates persisted sessions
  read-only, and an automated end-to-end test (spawning the real `ylva`
  binary against a mock provider, not just unit-testing `Store` in
  isolation) proves a full headless run round-trips through to a readable,
  correctly-shaped `.jsonl` session file.
- `~/.ylva/profiles/<name>/config.toml` is loaded at startup and populates
  `ProfileConfig`/`EngineConfig`/`AgentConfig` (default model, theme,
  keybindings, per-agent-type engine assignment) instead of being
  hand-constructed in-process only. This is a **flat single-file load**, not
  the full §4G three-layer/`ylva init`/`SOUL.md` system (Phase 8).
- The TUI ships two selectable `syntect` themes (dark = `base16-ocean.dark`,
  light = `base16-ocean.light`) instead of the single hardcoded theme.
- `/models`, `/tools`, `/clear` (alias `/reset`) are recognized as slash
  commands when typed as the first character of the TUI input box, each
  producing a result bubble in the conversation on confirm.
- All new behavior is covered by tests satisfying
  `docs/development-workflow.md`'s ≥80% coverage gate, and `make lint` /
  `make test` are clean.
- Every doc named in the **Implementation Plan**'s documentation steps is
  updated in the same commit as the code it describes (`AGENTS.md` →
  *"Docs are part of the change set"*).

**Key constraints:**

- `Context` ownership rule (HARD, §4C): the outer agent loop is the **sole
  writer** to `Context`; the inner streaming task never holds a mutable
  reference/lock to it. This plan does not touch `Context`'s tiering (that's
  Phase 9), but the `Store`-forwarding added here must still respect the
  rule — forward committed messages from the same outer-loop call sites that
  already own `Context` mutation, never from inside the streaming task.
- §4H durability bar is **process-crash-safe, not power-cut-safe**: flush
  per line, no `fsync` per line; a dedicated writer task (mpsc channel) keeps
  store writes off the TUI/streaming hot path (§4H decision 10).
- §4H decision 7 requires a per-session advisory lock (`flock`, exclusive,
  non-blocking `try_lock`) taken on **every** session-file open (fresh or
  resume); conflicting opens must fail fast with a clear error, never
  silently interleave writes. Use `nix::fcntl::flock` (already a
  `[target.'cfg(unix)'.dependencies]` entry for signal/process handling) —
  no new locking crate.
- §4H decision 6 requires repair-on-open (truncate a corrupt trailing JSON
  line before append-resume) and that **pure reads never mutate** a file.
- Reader tolerance (§4H decision 4): `#[serde(tag = "type")]`, never
  `#[serde(deny_unknown_fields)]`, so old/new readers stay forward- and
  backward-compatible.
- `docs/known-test-issues.md` constraints apply to any new test: tests that
  call `tracing::subscriber::with_default`/`init_logging`, or that read
  `std::env::current_dir()`, must hold `SUBSCRIBER_LOCK`
  (`src/test_support.rs`); any side effect that must be suppressed under
  test (clipboard, network, file I/O) must be injected via a trait/closure,
  not a `#[cfg(not(test))]` guard, because integration tests do not get
  `cfg(test)` in library code.
- Coding convention: full descriptive names, no abbreviations
  (`AGENTS.md`); avoid the "params struct" anti-pattern documented in
  `docs/development-workflow.md` when a function's argument list grows —
  work through the listed alternatives (extract a real struct only if it is
  a genuinely reusable concept, split the function, drop derivable args,
  move the function onto its type) before reaching for
  `#[expect(clippy::too_many_arguments)]`.

**Concrete starting point:** open `src/config.rs` (currently pure in-memory
`AgentConfig`/`ProfileConfig` structs with no file-loading code — see
Implementation Hints) and `docs/blueprint.md` §4H ("Persistence & Chat
History") side by side; the first test to run after step 1 lands is
`cargo test store::` (new module, see step 2) followed by `make lint`.

## Implementation Plan

### Store & persistence (§4H, §4G *Persistence*)

1. [ ] Add `sha2`, `chrono`, and `dirs` to `[dependencies]` in `Cargo.toml`
   (all three named in the blueprint's Phase 7 "Key crates" list and not yet
   present). `sha2` computes the `project_key` content-hash of the
   canonicalized CWD (§4H decision 2); `chrono` formats the `meta` header's
   `created_at`/session timestamps; `dirs::home_dir()` resolves `~/.ylva/`
   cross-platform — on unix this reads the `HOME` env var, which is exactly
   what lets step 8's integration test isolate a test run's persisted data
   into a `tempfile::TempDir` by overriding `HOME` on the spawned child
   process, without touching the real user's `~/.ylva/`.
2. [ ] Create `src/store.rs` with:
   - `StoreEvent` enum: `Meta`, `MessageEvent { msg: Message }`,
     `ChildStart`, `ChildResult` variants, `#[serde(tag = "type")]` (never
     `deny_unknown_fields`), each variant carrying its own `v: u32` field
     per §4H decision 4's example JSON shapes.
   - A `meta` header record: `type: "meta"`, `v`, `schema_version`,
     `app_version` (reuse `src/build_info.rs`'s existing version constant —
     inspect that file before adding a new one), `created_at`, `cwd_hash`,
     `engine`, and an auto-generated `title` (first user message, truncated
     to a configurable length — §4H decision 8).
   - `project_key(cwd: &Path) -> String`: SHA-256 hex digest of the
     **canonicalized** CWD (symlinks resolved, `..` collapsed), reusing
     `SandboxContext::canonicalize_best_effort` (`src/sandbox.rs`) rather
     than reimplementing canonicalization — not git root (§4H decision 2).
     This means the same physical directory always maps to one
     `project_key` regardless of which symlink or OS path quirk (e.g.
     macOS's `/var` → `/private/var`) it was reached through; two genuinely
     different cwds (even within the same repo) still get different
     `project_key`s, per decision 2's accepted per-launch-directory
     grouping.
   - Session path helper: `~/.ylva/chats/<project_key>/<session_id>.jsonl`,
     where `~` resolves via `dirs::home_dir()` (step 1) — this is the seam
     step 8's integration test uses to redirect persistence into a tempdir
     by overriding the spawned process's `HOME`, with no test-only code
     path or `#[cfg(test)]` guard needed.
   - `Store::create(cwd, session_id) -> Store` (fresh session) and
     `Store::resume(cwd, session_id) -> Result<Store, StoreError>` (existing
     session; `StoreError::SessionNotFound` when the file does not exist —
     resuming a nonexistent id is a **hard error**, never a silent
     fresh-session fallback), both acquiring the per-session advisory lock
     (`nix::fcntl::flock`, exclusive, non-blocking) on open (§4H decision 7);
     return a typed error (`"session '<uuid>' is open in another Ylva
     process"`) on conflict, not an anyhow-wrapped OS error, so callers can
     distinguish lock conflicts, missing sessions, and other I/O failures.
   - Repair-on-open (§4H decision 6): on `resume`, if the final line fails
     JSON parse, truncate the corrupt tail before allowing further appends;
     a `Store` opened for listing/read-only use never mutates the file — the
     bad line is skipped in memory only.
   - A dedicated writer: `Store::sender(&self) -> mpsc::Sender<StoreEvent>`
     feeding an internal writer task that serializes each event to one JSON
     line + `\n`, appends, and calls `flush()` (no per-line `fsync` — §4H
     decision 10). The agent loop enqueues events in commit order; the
     writer task appends them serially, preserving order without blocking
     the loop.
3. [ ] Wire `AgentLoop` (`src/agent.rs`) to hold a `Store` sender and forward
   every **committed** message (`push_user`, `push_assistant` once
   `Complete`/`Interrupted`, tool call + result) to it as a `MessageEvent` —
   never the live `partial` stream (§4H decision 3). Locate the existing
   `push_*` call sites found during exploration (`context.push_assistant(...)`
   around lines ~975/1034/1060/2072 of `src/agent.rs` at time of writing) and
   add the store-forwarding call alongside each commit point, from the same
   outer-loop call sites that already own `Context` mutation (per the
   ownership rule in Key Constraints).
4. [ ] Wire `ChildAgentManager` (`src/child_agent_manager.rs`) to append a
   `ChildStart` record to the **parent** session's `Store` sender before a
   child run begins, and a `ChildResult` record at completion (status,
   result, sources) — shapes given in §4G *Persistence*. This finishes the
   item explicitly deferred from `docs/plans/phase6-terminal-superpowers.md`
   ("Phase 6.12", dropped because there was no main-session persistence to
   attach to yet).
   - Also write the completed child transcript **once, at completion**, to
     its own `<sub-session_id>.jsonl` under the same `project_key`, with its
     own `meta` header (auto-title = the child's `task`) — §4H decision 9.
     Not streamed live.
5. [ ] Add `--conversation-id <uuid>` handling in `src/cli_driver.rs` /
   `src/tui_driver.rs` (or wherever CLI args are currently parsed pre-Phase-10
   `clap` — inspect `src/main.rs` first) so a supplied id calls
   `Store::resume` (hard error if not found, step 2) instead of
   `Store::create`. Absent the flag, both headless and TUI start a fresh
   session (no auto-resume, no picker — §4H decision 5).
6. [ ] Tests: `Store` round-trip (write events, reopen, verify order and
   content), lock-conflict error on double-open, corrupt-trailing-line
   repair-on-resume vs. no-mutation-on-read-only-open, `project_key` is
   stable for the same CWD and differs across CWDs, `project_key` is
   identical for two different symlink paths that resolve to the same
   canonical directory but differs for two genuinely different directories,
   child-agent `child_start`/`child_result` records land in the parent file with a
   correctly linked `parent_session`, `--conversation-id` resumes and
   appends rather than overwriting, `--conversation-id <nonexistent-uuid>`
   fails with a clear error rather than creating a new session.
7. [ ] Add `Store::list_sessions(project_key: &str) -> Result<Vec<SessionSummary>,
   StoreError>` to `src/store.rs`: scan
   `~/.ylva/chats/<project_key>/*.jsonl`, parse only each file's first
   (`meta`) line into a small `SessionSummary { session_id, created_at,
   title, engine }` (never the full transcript — this is a read-only
   listing helper, not a resume), skip and `tracing::warn!` any file whose
   first line fails to parse as `meta` rather than failing the whole
   listing (consistent with repair-on-open's read-never-mutates rule), and
   sort results newest-first by `created_at`. This promotes the read-only
   helper identified during planning (used by a future
   `--conversation-id`-not-found error message listing what sessions *do*
   exist, and by a future interactive picker per §4H decision 8) from a
   deferred idea into shipped Phase 7 scope, and gives step 8's integration
   test a real public API to assert persistence through, instead of
   hand-parsing directory listings in test code.
8. [ ] Add an automated end-to-end persistence test,
   `tests/store_persistence_e2e.rs`, that spawns the real compiled `ylva`
   binary (`env!("CARGO_BIN_EXE_ylva")`, the same pattern `tests/
   cli_version.rs` already uses) rather than driving `AgentLoop` in-process
   (`tests/cli_driver.rs`'s approach) or exercising only the HTTP provider
   layer (`tests/http_provider.rs`) — this is the level the manual
   `tests/e2e/*.zsh` scripts exercise by hand today, made deterministic and
   CI-safe:
   - Start a `tests/common::mock_llm_server::MockLlmServer` (already used by
     `tests/http_provider.rs`) with a short scripted response (e.g.
     `content_frame("...")`, `end_after_frames: true`) — a fake
     OpenAI-compatible endpoint, no live network, no real API key.
   - Create a `tempfile::TempDir` and set it as **both** the spawned
     process's `HOME` (`Command::env`) and its working directory
     (`Command::current_dir`): `HOME` isolates `~/.ylva/chats/` from the
     real user's data (per step 1's `dirs::home_dir()` resolution), and a
     fixed cwd gives a deterministic, reproducible `project_key`.
   - Point the child process at the mock server the same way the manual
     scripts point at a real backend: `Command::env("OLLAMA_BASE_URL",
     server.base_url())` (mirrors `tests/e2e/ollama-local/.envrc`'s
     `OLLAMA_BASE_URL`, reusing `select_provider`'s existing Ollama-first
     env-var precedence in `src/main.rs` — no new provider-selection code
     needed).
   - Run headless (`--no-tui --prompt "<fixed test prompt>"`) and wait for
     a successful exit status.
   - Assert persistence through the **public API**, not by guessing a
     filename: call `Store::list_sessions(&project_key(&tempdir_path))`
     (step 7) and assert exactly one `SessionSummary` exists, then open
     that session's `.jsonl` path directly and parse every line as a
     `StoreEvent` (step 2's `#[serde(tag = "type")]` enum) to assert: the
     first line is `Meta` with `cwd_hash` equal to
     `project_key(&tempdir_path)`; at least one `MessageEvent` is a user
     message containing the sent prompt text; at least one `MessageEvent`
     is a completed assistant message with non-empty text. Keep these
     assertions structural (event kinds/counts/fields present), never an
     exact-string match on the assistant's wording, so the same assertion
     helper works against any provider's output.
   - Factor the shared logic into a `run_persistence_e2e(provider_env:
     &[(&str, &str)]) -> ...` helper so the default (always-run, CI-safe)
     test supplies `[("OLLAMA_BASE_URL", &server.base_url())]`, and add
     `#[ignore]`-gated variant tests (`persistence_e2e_ollama_local`,
     `persistence_e2e_ollama_cloud`, `persistence_e2e_openrouter`) that
     instead forward whichever of `OLLAMA_BASE_URL`/`OLLAMA_MODEL`/
     `OLLAMA_API_KEY`/`OPENROUTER_*` are already set in the *test-runner's
     own* process env (mirroring `tests/e2e/{ollama-local,ollama-cloud,
     openrouter}/.envrc`'s exact variable names) into the child process,
     and early-return (`eprintln!` + skip, not `panic!`) when the relevant
     variable is unset — so `cargo test` in CI never depends on a live
     network, but a developer who has `direnv allow`ed one of those
     `tests/e2e/*` dirs can run `cargo test --test store_persistence_e2e --
     --ignored` locally against a real backend, reusing the same
     assertion helper with no duplicated provider-wiring or assertion code
     between the deterministic and live-opt-in variants.
9. [ ] Documentation: replace the stub `docs/persistence.md` (currently just
   a title and one-line description) with the concrete on-disk layout,
   `StoreEvent` shapes, `project_key` derivation, `list_sessions` (step 7),
   and the lock/resume contract; add a `--conversation-id` entry to
   `docs/cli-options.md`; add a "Store & persistence" subsection to
   `docs/implementation-notes.md` describing `src/store.rs` internals for
   contributors; update `docs/features.md` if it documents session
   lifecycle user-facing behavior; add a short note to
   `docs/development-workflow.md`'s "Integration-test shared helpers"
   section describing the new spawn-the-binary + `MockLlmServer` pattern
   (step 8) and its `#[ignore]`-gated live-provider variants, so future
   contributors know this pattern exists before writing a second one.

### Config hierarchy — narrow scope (§4G *Config-Driven Types*, Phase 7 bullet only)

10. [ ] Add `toml` to `[dependencies]` in `Cargo.toml` (blueprint Phase 7 key
   crate). Do **not** add the `config` crate for this narrow, single-file
   load — a flat `toml::from_str` into the existing structs needs no
   multi-source merge library; revisit only if Phase 8's layered system
   later needs real merge semantics (YAGNI).
11. [ ] Extend `src/config.rs` to load `~/.ylva/profiles/<name>/config.toml`
   into the existing `ProfileConfig`/`AgentConfig` structs (add
   `#[derive(Deserialize)]` alongside the existing `Debug, Clone`), plus a
   new `default_model`/`theme`/`keybindings` surface matching the Phase 7
   bullet ("default model, theme, keybindings, and agent-type engine
   assignments"). This is a **flat load of one file** — no project `.ylva/`
   discovery, no `ylva init`, no `SOUL.md`, no `agents/*.md` (all Phase 8).
   Keep `EngineConfig` (`src/engine.rs`) as the field-level SSOT for
   sampling parameters per its existing doc comment; the loader populates
   it, it does not duplicate its fields.
12. [ ] Wire `AgentFactory` (`src/agent_factory.rs`) to build its
    `HashMap<String, Arc<dyn Engine>>` from the loaded TOML rather than
    hand-constructed `EngineConfig` values, resolving the documented
    `default_engine`/`compact_engine` fallback (`docs/configuration.md`
    already specifies this resolution — keep it as the SSOT and update it
    only if the loader changes the contract).
13. [ ] Update `src/engine.rs`'s `EngineConfig` doc comment (currently:
    "`model`, `temperature`, `context_length`, `api_base_url`, and `api_key`
    are advisory in Phase 5 ... Phase 7 wires these fields into provider
    construction from TOML config, making `EngineConfig` the true SSOT for
    sampling parameters") so its phase claim is accurate the moment it's
    read, not just true in hindsight once steps 11–12 happen to land: state
    that Phase 7 wires these five fields from the narrow flat single-file
    loader (steps 10–12) into `AgentFactory`'s provider construction,
    making `EngineConfig` the SSOT for *these fields* specifically — and
    name what later phases add on top without displacing that SSOT: Phase 8
    (Full Configuration & Persona System) adds the three-layer project/
    home/embedded fallback chain and persona/`agents/*.md` sourcing that
    populate the same struct through additional file layers; Phase 9
    (Context Compaction & Token Budgeting) reads `context_length` to drive
    `maybe_compact`'s budget without adding new `EngineConfig` fields. This
    keeps the doc comment honest about scope as more phases ship, instead
    of requiring a reader to cross-reference the blueprint to know which
    claims are still Phase-7-only. Resolves the Consistency Issues entry
    about this doc comment (see below).
14. [ ] Tests: TOML fixture loads into the expected `ProfileConfig`, missing
    file falls back to an in-code default profile (never a hard error),
    malformed TOML produces a clear error, an `AgentConfig` engine override
    still resolves correctly through `AgentFactory`.
15. [ ] Documentation: update `docs/configuration.md`'s "Phase 7 will wire
    them into provider construction from a TOML config file" note (under
    `EngineConfig`) to describe the shipped flat-file loader, its exact
    path, and precedence; update `docs/cli-options.md` if a `--profile`
    flag's resolution changes; do not claim Phase 8's three-layer/`SOUL.md`
    behavior here — that stays documented as future work until Phase 8
    ships.

### Rendering polish (§4F)

16. [ ] In `src/highlight.rs`, replace the hardcoded
    `THEME_SET.get_or_init(...).themes["base16-ocean.dark"]` in
    `highlight_theme()` with a parameter (`ThemeName::Dark | Light`,
    resolving to the bundled `base16-ocean.dark`/`base16-ocean.light`
    entries in `ThemeSet::load_defaults()` — confirm both keys exist via a
    quick test before wiring) sourced from the loaded config's `theme` field
    (step 11). Keep the existing per-call memoization (`HIGHLIGHT_CACHE`) but
    include the active theme in the cache key (`highlight_cache_key`), since
    a theme switch must invalidate previously-cached highlighted lines. Do
    **not** add a third theme — see `docs/ideas/monokai-theme.md`.
17. [ ] Wire the resolved theme from config through to every
    `highlighted_lines_cached` call site (`src/markdown.rs`/
    `src/tui_widgets.rs`).
18. [ ] Tests: theme switch changes the cache key and produces different
    styled output for the same content; `highlight_theme` resolves each of
    the two theme names to a distinct loaded `Theme`.
19. [ ] Documentation: update `docs/features.md`'s rendering section (and
    `docs/configuration.md` if `theme` becomes a documented config key) to
    describe the two selectable themes; note that a third ("monokai") theme
    is tracked in `docs/ideas/monokai-theme.md`, not yet implemented. Live
    Markdown/`syntect` rendering during streaming is already shipped
    behavior (§4F) and needs no doc change here — see
    `docs/ideas/defer-markdown-parsing-fallback.md` for why no
    plain-text-while-streaming fallback is planned.

### Slash commands

20. [ ] Add a slash-command parser triggered only when `/` is the first
    character of the TUI draft-message input (per
    `docs/ideas/slash-commands.md`'s trigger rule, so it never fires inside
    pasted code — matches `docs/blueprint.md` §6 "Commands" for headless/CLI
    consistency). The authoritative command set for this plan is `/models`,
    `/tools`, `/clear` (alias `/reset`) — `/save` is dropped and `/agent` is
    deferred, see Implementation Hints. Locate the input-handling code in
    `src/tui_driver.rs`/`tui_reducer_state.rs` before choosing where the
    parser lives. Per the idea doc: a recognized command may open a small
    overlay above the conversation list while the user is still typing (e.g.
    `/models`' candidate list, described in step 22); confirming (sending)
    the command performs its action and always produces a result bubble in
    the conversation list — decide per command in steps 21–23 whether that
    bubble is persisted to the `Store` (step 2) or stays transient UI state
    (see Implementation Hints for the default: informational bubbles are
    transient, session-affecting ones are committed). A leading `/` that
    does not match one of these three commands (or their aliases) is
    **not** a parser error — the parser only special-cases the fixed
    allowlist and otherwise falls through untouched, so the draft is sent
    as normal chat input to the LLM (see step 24).
21. [ ] Implement `/clear` (alias `/reset` — both trigger the same handler):
    reset the current in-memory `Context` and start a fresh `Store` session
    (mirrors §4H's "fresh session" semantics but triggered mid-run rather
    than at launch). The confirmation bubble is transient (not persisted to
    the old session, since a new session has already started).
22. [ ] Implement `/models`: call the already-implemented
    `Provider::list_models()` (§4A, `src/llm.rs`) to populate a candidate
    overlay (per step 20's overlay behavior) with the models available from
    the active provider, and switch the active agent's engine to the
    selected model on confirm — this both fulfills the listing groundwork
    already documented in `docs/features.md` ("`/models` command (coming
    soon)") and the idea doc's "select the current provider and model."
    Supersedes the `SwitchModel` action placeholder referenced in
    `docs/blueprint.md` §4B. The switch-confirmation bubble is committed to
    the `Store` (it changes which engine subsequent turns use, which is
    session-affecting).
23. [ ] Implement `/tools`: display the currently allowed tool list for the
    active agent (`AgentConfig::allowed_tools` resolution, already
    documented in `docs/configuration.md`) as a transient, informational
    bubble (not persisted — it reports existing state, it doesn't change
    it).
24. [ ] Tests: each command (and its aliases) is recognized only when it is
    the first character of the input box (not mid-message, not mid-paste);
    an unrecognized `/foo` is **not** intercepted — it is sent to the LLM
    as ordinary plain user input, exactly like a message with no leading
    `/`, since the command set is a fixed, small allowlist and anything
    outside it is just chat text (e.g. a user genuinely typing "/foo" as
    part of a sentence); `/clear` and `/reset` both reach the same handler;
    `/models`' overlay lists the candidates returned by a stubbed
    `Provider::list_models()` and confirms the correct engine switch; each
    command's effect is observable via the reducer state
    (`tui_reducer_state.rs` tests already exercise this
    pattern).
25. [ ] Documentation: add a "Slash commands" section to `docs/features.md`
    covering `/models`, `/tools`, `/clear`/`/reset`, replacing the old
    "`/models` command (coming soon)" placeholder subsection; add the
    commands to `docs/keybindings.md` if that doc covers input-box
    interactions; update `docs/ideas/slash-commands.md` — fold the shipped
    commands out of it and trim the idea file to only the not-yet-implemented
    `/agent` idea (it already never mentioned `/save`, so no removal needed
    there); update `docs/blueprint.md` §4B's `SwitchModel` comment and §6
    "Commands" line if either still needs adjusting once the real command
    name/behavior is finalized (both were already aligned to `/models`
    during this planning pass).

### Final verification

26. [ ] Run `make format`, `make lint`, `make test-coverage` (per
    `docs/development-workflow.md`) and fix any findings; confirm the
    ≥80% coverage gate still passes for every non-excluded file touched by
    this plan.
27. [ ] Re-read `docs/blueprint.md` §7 Phase 7 and flip every corresponding
    `[ ]` to `[x]` in that section once its item is verified shipped and
    documented.

## Implementation Hints

- Crate/version facts: `Cargo.toml` already depends on `serde_json`
  (`preserve_order`), `syntect` (`dump-load` feature already enabled,
  bundled themes only — `base16-ocean.dark` and `base16-ocean.light` both
  exist among `ThemeSet::load_defaults()`'s bundled set, alongside
  `base16-eighties.dark`/`base16-mocha.dark`/`InspiredGitHub`/`Solarized
  (dark)`/`Solarized (light)`; no "monokai" entry — see
  `docs/ideas/monokai-theme.md`), `nix` (unix-only, `signal`/`process`
  features, not yet `fcntl`/file-locking — step 2/3 add the `flock` usage),
  `textwrap`, `pulldown-cmark`, `ratatui`,
  `tracing`/`tracing-subscriber`/`tracing-appender`. `sha2`, `toml`,
  `chrono` are genuinely new additions needed for this plan; `config` and
  `tiktoken-rs` (both named in the blueprint's original, now-superseded
  Phase 7 "Key crates" line) are deliberately **not** added here — `config`
  is deferred as YAGNI for a single flat file, and `tiktoken-rs` moved to
  Phase 9 with the rest of token counting/compaction.
- `src/config.rs` (103 lines) currently holds only in-memory
  `AgentConfig`/`ProfileConfig` structs (`#[derive(Debug, Clone)]`, no
  `Deserialize`, no file I/O) with unit tests constructing them by hand —
  there is no existing TOML-loading code to extend, only to add.
- `docs/persistence.md` currently is a two-line stub ("# Ylva agent-harness
  persistence" + one description line) — it is the designated target for
  step 9's content, not a file to create fresh.
- `docs/plans/` currently only contains a `.gitkeep` — no prior
  `phase6-terminal-superpowers.md` plan file exists in the repo at
  exploration time even though the blueprint's Phase 7 bullet references it
  by name ("Phase 6.12"); if it is truly absent, step 4's "supersedes"
  language in this plan is descriptive only — there is no sidecar file to
  edit/delete for that supersession.
- `project_key`'s canonicalization decision reuses existing code:
  `src/sandbox.rs`'s `SandboxContext::canonicalize_best_effort` (private
  fn, called from `Sandbox::new`/`check_path`/elsewhere) already resolves
  symlinks via `std::fs::canonicalize` with a best-effort fallback that
  walks up to the nearest existing ancestor when the full path doesn't
  exist yet, canonicalizes that ancestor, and reattaches the non-existent
  suffix (used today for macOS's `/var` → `/private/var` and similar
  quirks). Either reuse this function directly (making it `pub(crate)` if
  it is currently private to `sandbox.rs`) or extract it to a shared
  location (e.g. a small `path` utility module) if `store.rs` and
  `sandbox.rs` both need it — do not reimplement canonicalization logic a
  second time (SSOT).
- `src/highlight.rs` (183 lines) already implements syntax highlighting with
  a thread-local content-addressed cache (`HIGHLIGHT_CACHE`,
  `highlight_cache_key(content, file_extension)`) and a hardcoded single
  theme (`highlight_theme()` returns `themes["base16-ocean.dark"]` from
  `ThemeSet::load_defaults()`); Phase 6 already shipped highlighting itself,
  so Phase 7's "polish" here is genuinely scoped to dark/light selection and
  cache-key theme-awareness, not building highlighting from scratch.
- `src/tui_widgets.rs` line ~693 already calls `render_markdown` on
  `state.partial_assistant_text` every frame (inside the block guarded by
  `if !state.partial_assistant_text.is_empty()` around line 689) — this is
  the shipped live-rendering behavior kept as-is by this plan (§4F was
  updated to document it as the intended design rather than a temporary
  state; see `docs/ideas/defer-markdown-parsing-fallback.md` for why no
  "plain text while streaming" change is planned here).
- `src/agent.rs`: `Message`/`AssistantStatus` enums are defined around lines
  310–347; `Context::push_assistant`/`push_user` around lines 463–474;
  commit-point call sites for `push_assistant` currently sit around lines
  975, 1034, 1060, 2072 (worth re-`grep`-ping before editing, since line
  numbers shift).
- `src/engine.rs`'s `EngineConfig` doc comment already states the Phase 7
  contract explicitly: "Phase 7 wires these fields into provider
  construction from TOML config, making `EngineConfig` the true SSOT for
  sampling parameters" — this is the authoritative statement of what "done"
  means for steps 11–12, beyond the blueprint bullet text.
- `docs/configuration.md` already documents `ProfileConfig`/`AgentConfig`/
  `EngineConfig` field tables; its `compact_engine` and per-tool sandbox
  `[tool.*]` notes now point at Phase 9 and Phase 8 respectively (updated as
  part of this planning pass, since those two features moved out of Phase
  7) — only the `EngineConfig` provider-construction note still points at
  Phase 7, matching this plan's steps 11–12.
- `/save` was in the blueprint's original Phase 7 slash-command bullet but
  is dropped from this plan: the `Store` already flushes every committed
  line automatically (§4H decision 10), and no distinct behavior for a
  separate user-triggered "save" was identified — `docs/blueprint.md`'s
  Phase 7 section has been updated accordingly to list only `/models`,
  `/tools`, `/clear` (alias `/reset`); `/agent` is tracked separately in
  `docs/ideas/slash-commands.md` as deferred, not dropped.
- `docs/known-test-issues.md` documents three specific pitfalls relevant to
  this plan's new tests: tracing-subscriber isolation (`SUBSCRIBER_LOCK`),
  process-global cwd mutation (also `SUBSCRIBER_LOCK`, relevant because
  `project_key` hashing reads `current_dir()`), and `cfg(test)` not reaching
  library code from integration tests (relevant to any `Store`
  file-I/O/flock behavior an integration test wants to exercise —
  side-effects must be injectable, not `#[cfg(not(test))]`-guarded).
- `docs/development-workflow.md` lists the exact `make` targets to run
  (`make format`, `make lint`, `make test-coverage`, `make test`) and states
  `tui_widgets.rs`, `render.rs`, `main.rs` are excluded from the coverage
  gate because they require a live terminal — not directly relevant to this
  plan since it no longer touches `tui_widgets.rs` (the streaming-render
  change was dropped, see above), but still relevant if slash-command input
  handling (steps 20–23) turns out to live in `tui_widgets.rs` rather than
  `tui_reducer_state.rs`.
- `docs/blueprint.md` §7 now has this phase sequence after this planning
  pass: Phase 7 (this plan, narrowed) → Phase 8 (Full Configuration &
  Persona System, new) → Phase 9 (Context Compaction & Token Budgeting,
  new) → Phase 10 (CLI Surface, renumbered from the former Phase 8). All
  in-blueprint cross-references to the moved compaction/`SOUL.md` material
  (in §4C and §4G) were updated to point at Phase 9/Phase 8 respectively as
  part of this planning pass — a future plan for either new phase can rely
  on those blueprint sections already being consistent.
