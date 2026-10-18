# Code review remediation: one finding at a time

## Context

- Component: Ylva (Rust TUI agent harness, crate `ylva`, `[[bin]]` + `[lib]`).
- Origin: code review of non-test Rust code in `src/` (about 72k lines), dated
  2026-10-17. Method: `cargo clippy --all-targets` (clean), pattern counts, four
  area reviews (tools/sandbox, agent core/LLM/HTTP, TUI, config/store/CLI) and
  four lens reviews (`rust-skills`, `security-review`, `code-reviewer`).
  Test code, `tests/`, build scripts and runtime behavior were not reviewed.
- Goal: remediate every finding sequentially. Each finding is one self-contained
  unit: failing test first (TDD), fix, docs, verification gate, then done.
  Finish and tick one finding before starting the next.
- Priorities: **High** = security or terminal-state defect; **Medium** =
  robustness, resource-bound or data-safety defect; **Low** = maintainability or
  API-surface debt.
- Confidence: findings marked **verified** were re-checked directly; all others
  come from area reviews and must be confirmed by reading the code first. If a
  finding does not reproduce, mark it `[x]` with the note "not reproducible"
  and move on.
- Themes: unbounded buffers fed by remote/subprocess output; secret-bearing
  files created with umask-default permissions; non-atomic or non-durable file
  writes; approval checks not re-applied at use time; oversized modules.
- Acceptance criteria: every step below is `[x]`; `make format lint` and
  `make test-coverage` pass; affected docs updated; `rumdl check .` clean on
  changed Markdown.
- Constraints (from `AGENTS.md`): no file edits beyond the current finding; no
  git branch/commit/stage without explicit user consent; full descriptive
  identifiers (no abbreviations); license header on every new source file;
  file-per-module layout (no `mod.rs`); keep `main.rs` thin; anything tests in
  `tests/` need must be re-exported via `src/lib.rs`; keep the coverage
  threshold from `docs/development-workflow.md`; check `docs/known-test-issues.md`
  before writing tests involving `tracing`, cwd or integration-test side
  effects; ask before creating a new doc file.
- Rejected claims (do not implement):
  - "Secret-bearing `.env` files are tracked": `.ylva/.gitignore` ignores `.env`
    and `git ls-files .ylva` lists none.
  - "Unchecked `f64` to `u64` retry delay" (`src/http.rs:806`): capped by
    `RETRY_MAX_SECONDS` and the jitter factor before the cast; no NaN/overflow.
  - Tests: no missing license headers, no cwd-race violations, no further
    sleep misuse were found.

## Implementation Plan

Workflow for an implementing agent: work strictly top to bottom. Each numbered
step is one finding. Inside a step, run the sub-steps in order. Flip `[ ]` to
`[x]` immediately after finishing each step or sub-step and rewrite this file in
place so progress is resumable across sessions. Per-finding gate (the last
sub-step of every finding): `make format lint`, then `make test-coverage`
(single mandatory test gate; do not also run `make test`).

Order: dependency fixes and small isolated high-value items first, then
design-dependent security items, then bounds/timeouts, then robustness, then
debt. Findings that touch the same file are kept adjacent.

### Phase 1: quick wins

1. [x] **D1 (Medium)** - Drop `reqwest 0.11`/`hyper 0.14`/`h2 0.3` via `jsonschema`.
   1. [x] Confirm `jsonschema` is used only at `src/agent/tools.rs:686`
      (`JSONSchema::compile`) and that no tool schema uses external `$ref`.
   2. [x] Set `jsonschema = { version = "0.17", default-features = false }` in
      `Cargo.toml`; run `cargo tree -i h2` to confirm `h2 0.3` is gone.
   3. [x] Confirm tool-argument validation tests still pass (add a test that a
      schema with an external `$ref` is not resolved, if cheap).
   4. [x] Docs: none beyond the H01 note (remote/file `$ref` resolution is now
      off).
   5. [x] Gate.
2. [x] **D2 (Medium)** - Update `h2 0.4.15` to `>=0.4.16` (RUSTSEC-2026-0258).
   1. [x] `cargo update -p h2`; verify only `Cargo.lock` changed for h2.
   2. [x] `cargo update -p rustls` to `>=0.23.46`. Not compiled (lockfile
      only), but `cargo audit` fails on it, so it is required for a clean
      `make audit`.
   3. [x] Gate.
3. [x] **D3 (Low)** - Add `cargo audit` to the verification workflow.
   1. [x] The `audit` target already exists in `Makefile` (`cargo audit --quit`, typo fixed to `--quiet`,
      committed in 5944f37); keep it and wire it into the documented
      workflow and CI.
   2. [x] Add `audit.toml` (or `.cargo/audit.toml`) ignoring accepted warnings
      `RUSTSEC-2025-0141` (`bincode`, via `syntect`) and, temporarily,
      `RUSTSEC-2024-0449` (`unsafe-libyaml`, via `serde_yaml`; removed again by
      D4), each with a reason comment. Ask the user before creating it as a
      new file if unsure of location.
   3. [x] Docs: `docs/development-workflow.md` - `cargo audit` step and
      `audit.toml` policy.
   4. [x] Gate.
4. [x] **H02 (High)** - Terminal left in raw mode if initialization fails midway.
   Where: `src/tui_driver/terminal.rs:114-131` (**verified**).
   1. [x] Test (or extract a testable seam) proving cleanup runs when a step
      after `enable_raw_mode()` fails.
   2. [x] Create the restore guard immediately after `enable_raw_mode()`; its
      `Drop` undoes raw mode, alternate screen and mouse capture, whichever
      were enabled.
   3. [x] Docs: `docs/implementation-notes.md` if terminal lifecycle is
      described.
   4. [x] Gate.
5. [x] **M07 (Medium)** - Secret-bearing files use default permissions.
   Where: `src/init_command.rs:239-247`,
   `src/store/session_file.rs:121-136,214-229` (**verified**: no `0o600`/`mode(...)`
   in production code). Profile `.env` files hold API keys; transcripts can
   hold secrets.
   1. [x] Unix-gated tests: created `.env` and session files have mode `0600`,
      created directories `0700`.
   2. [x] Create with `OpenOptionsExt::mode(0o600)` and
      `DirBuilderExt::mode(0o700)` under `#[cfg(unix)]`.
   3. [x] Docs: guarantee in `docs/persistence.md` and `docs/configuration.md`.
   4. [x] Gate.
6. [x] **M08 + S07 (Medium/Low)** - One atomic, durable replace helper (SSOT).
   Where: `src/store/writer.rs:158-176` (no `sync_all` before `rename`),
   `src/init_command.rs:196-218` (`.gitignore` written in place),
   `src/profile_shim.rs:159-171` (written, then made executable),
   `src/store/archive.rs:171-191` (the model: create-new temp, `sync_all`,
   rename, directory sync).
   1. [x] Add a shared helper (temp file in same directory, `sync_all`,
      permissions, `rename`, directory sync), extracted from the archive
      publisher. Put it in an existing suitable module (ask before adding a new
      doc; a new source file is fine with license header).
   2. [x] Tests: success, rename failure leaves original intact, mode applied
      before publish.
   3. [x] Migrate `writer.rs`, `init_command.rs`, `profile_shim.rs` and
      `archive.rs` to the helper.
   4. [x] Docs: `docs/persistence.md`, `docs/implementation-notes.md`
      (durability guarantees, helper).
   5. [x] Gate.

### Phase 2: security design

7. [x] **H01 (High)** - Web fetch has no redirect hop policy; private
   addresses reachable.
   Where: `src/agent/tools/backends/web_fetch/native.rs:20-22,78-107`
   (**verified**: no redirect policy configured); related
   `network_target()` in `src/http.rs:198-201` validates scheme and authority
   only, so loopback, link-local (`169.254.169.254`), RFC1918 and IPv6-local
   targets are reachable when a broad network rule approves them.
   Decision: the approval gate stays on the **requested URL only**; per-hop
   approval prompts are rejected (redirect targets are often unknown or
   runtime-dependent, e.g. CDNs, so prompts would be frequent and hard to
   judge). Instead apply a non-interactive hop policy.
   1. [x] Read `docs/architecture-design-records/`, `docs/features.md` and
      `docs/configuration.md` (redirect caveat) for the current policy (the
      ADR directory was empty on 2026-10-17). No ADR is written (user
      decision, 2026-10-17); the decision above and the docs below are the
      record.
   2. [x] Tests with a mock server: hop limit enforced; non-http(s) scheme hop
      refused; redirect to an approved or unknown host is followed without a
      new prompt; private, loopback, link-local and reserved addresses
      rejected on every hop including the first (after DNS resolution) unless
      explicitly opted in; a hop to a different host that an explicit `deny`
      rule matches is refused.
   3. [x] Disable automatic `reqwest` redirects; follow manually with a hop
      limit, scheme check and resolved-address check per hop. Evaluate
      explicit `deny` rules (never prompt) for cross-host hops. Pin the
      resolved address for the connection to avoid DNS rebinding.
   4. [x] Permit private/reserved addresses via a named, optionally
      target-scoped `network_private_address` rule (default refused; replaces
      the earlier global config flag).
   5. [x] Docs: `docs/features.md`, `docs/configuration.md` (replace the
      redirect caveat, document the flag), `docs/implementation-notes.md`.
   6. [x] Gate.
8. [x] **M-H03 (Medium, downgraded from High)** - Write/edit approval and
   mutation separated (TOCTOU).
   Where: `src/agent/tools/write.rs:290-302,380-395`,
   `src/agent/tools/edit.rs:411-427,503-526`.
   Severity triage (2026-10-17):
   - Tool calls of one run are executed sequentially, so the only concurrent
     actors are subagents (`spawn_subagent`, up to `max_concurrent_subagents`)
     and external processes of the same user.
   - The sandbox check canonicalizes the path at approval time
     (`src/sandbox/path.rs`), but `tokio::fs::write` later follows the raw
     path, so a symlink or parent swap in between redirects the write.
   - The window is wide only when user approval is pending (seconds to
     minutes); auto-approved writes have a negligible window.
   - Realistic attacker: a subagent (or its `bash`) planting a symlink while
     the parent's write approval is pending. Impact is bounded to what the
     process user may write, and a symlink escaping the workspace is the
     case the sandbox rules are meant to gate.
   - Result: Medium. Do the cheap hardening; skip an exhaustive test matrix.
   1. [x] Tests: symlink or parent swapped between approval and write is
      detected and the write refused (Unix-gated); one deterministic test
      per tool using a hook/seam between check and mutation.
   2. [x] Re-resolve the canonical path right before the write and re-run the
      sandbox path check on it (refuse on mismatch); use no-follow open flags
      (`O_NOFOLLOW`) on Unix for the final component.
   3. [x] Docs: `docs/implementation-notes.md` (approval/mutation flow, the
      residual parent-directory race that remains without `openat`-style
      handling).
   4. [x] Gate.
   5. [x] Addendum: `read` had the same approval-to-open gap (`ReadCall` now
      uses `ApprovedTarget::open_for_read`); `search` and `glob` are
      deliberately left out of scope.

### Phase 3: bounds and timeouts

9. [x] **M01 (Medium)** - Bash output queued without bound.
   Where: `src/agent/tools/bash.rs:465-498,542-556`. Reader tasks push every
   line into an unbounded channel; the cap applies only after exit.
   1. [x] Test: a command producing output far above the cap keeps memory
      bounded and still returns the capped result.
   2. [x] Enforce the byte/line cap in the readers (keep draining, discard the
      excess) or use a bounded channel.
   3. [x] Docs: `docs/features.md` / `docs/implementation-notes.md` (output
      bounds).
   4. [x] Gate.
10. [x] **M02 (Medium)** - Web fetch buffers media bodies before enforcing the limit.
    Where: `src/agent/tools/backends/web_fetch/native.rs:119-156`.
    1. [x] Test with an oversized mock body: rejected without exceeding the limit.
    2. [x] Reject when `raw_body.len() + chunk.len()` exceeds the limit, before
       extending the buffer.
    3. [x] Docs: as in M01. Gate.
    4. [x] Refactor: extracted `BoundedBuffer` (`src/bounded_buffer.rs`) from
       `OutputCollector` as the shared bounded accumulator; M03 reuses it.
11. [x] **M03 (Medium)** - Error response bodies read unbounded.
    Where: `src/http.rs:697-703` (`response.bytes().await` loads everything
    before truncation to 512 characters).
    1. [x] Test with a large error body: only a bounded prefix is read.
    2. [x] Accumulate chunks up to the snippet limit, then stop.
    3. [x] Gate.
12. [x] **M04 (Medium)** - Compaction and title output have no size limit.
    Where: `src/agent/compaction.rs:528-546`, `src/session_title.rs:228-247`.
    1. [x] Tests: runaway summary fails compaction keeping existing history;
       runaway title is truncated.
    2. [x] Stop after a bounded multiple of the expected size.
    3. [x] Gate.
13. [x] **M05 (Medium)** - HTTP and title requests lack explicit timeouts.
    Where: `src/http.rs:371-462`, `src/session_title.rs:146-175`. **Partly
    verified**: title generation has a timeout only at
    `session_title.rs:499`; callers such as web fetch pass `http_timeout`.
    1. [x] Determine which provider paths are actually unbounded.
    2. [x] Test (stalled mock server) for each unbounded path.
    3. [x] Set a connect timeout on the shared client and a per-attempt timeout
       on provider and title requests; cancel the stream on expiry.
    4. [x] Docs: `docs/configuration.md` if a timeout becomes configurable.
       Gate.
14. [x] **M06 (Medium)** - Provider error snippets may echo sensitive data.
    Where: `src/http.rs:777-785` (up to 512 characters of the body reach logs
    and the UI). Low confidence; depends on diagnostics policy.
    1. [x] Decide the policy (keep, redact, or log-only at debug). If the choice
       is non-obvious, ask the user.
       Decided by the user: keep. Showing the provider error in the UI and
       logs is deliberate functionality.
    2. [x] No code change; documented in `docs/implementation-notes.md`.
       Gate.
15. [ ] **S02 (Medium)** - Full request URLs are logged and echoed in errors.
    Where: `src/http.rs:437,443,461,472,488` (`tracing` fields) and the
    error strings built from `{url}` (`request to {url} failed…`,
    `status_error`), which reach the UI and possibly the model context. URLs
    may carry query-string keys, signed parameters, search terms or userinfo.
    Decision: redact in logs and error messages; keep the raw query in the
    explicitly enabled `YLVA_REQUEST_DUMP_PATH` dump (opt-in debug output
    that already stores `query` separately and redacts headers/media only).
    Redacted form: scheme, host, path, query parameter names with values
    replaced by `<REDACTED>` (e.g. `?q=<REDACTED>&page=<REDACTED>`); userinfo
    and fragment dropped.
    1. [ ] Tests (see `docs/known-test-issues.md` for `tracing` tests): logged
       text and returned error text contain scheme, host, path and parameter
       names but no query values, userinfo or fragment; the request dump still
       contains the full query.
    2. [ ] Add one display helper (e.g. `redacted_url_for_display`) in
       `src/http.rs` and use it for every `tracing` URL field and error
       string (do after M06; same file).
    3. [ ] Docs: `docs/cli-options.md` (dump keeps raw query strings),
       `docs/implementation-notes.md` (URL redaction policy).
    4. [ ] Gate.

### Phase 4: robustness and correctness

16. [ ] **M09 (Medium)** - Writer state after a failed title rewrite.
    Where: `src/store/writer.rs:152-176`. If the reopen after `rename` fails,
    the writer keeps a handle to the replaced inode and may keep appending to
    an unpublished file.
    1. [ ] Test: simulated reopen failure marks the writer failed and surfaces a
       persistent error on later writes.
    2. [ ] Mark the writer failed (fail closed). Gate.
17. [ ] **M10 (Medium)** - Editor command runs through `sh -c`.
    Where: `src/profile_command.rs:112-129`. `VISUAL`/`EDITOR` are
    user-controlled, same trust level as `git`; a quoting robustness issue more
    than a vulnerability.
    1. [ ] Decide: parse without shell evaluation, or document the behavior
       (minimum). Implement with a test for quoted/space-containing paths if
       parsed.
    2. [ ] Docs: `docs/configuration.md` or `docs/cli-options.md`. Gate.
18. [ ] **M11 (Medium)** - Text selection ignores display width.
    Where: `src/presenter/mouse.rs:84-107,149-177`. Columns are used as
    `Vec<char>` indices, so wide (CJK) and zero-width graphemes select or copy
    the wrong text.
    1. [ ] Tests with CJK, combining and emoji lines.
    2. [ ] Map columns to display-width boundaries using the helpers in
       `src/markdown.rs`. Gate.
19. [ ] **M12 (Medium)** - Render cache retention is quadratic.
    Where: `src/conversation_render_cache.rs:182-186`.
    1. [ ] Build a `HashSet<EntrySlot>` once before the `retain` calls; existing
       tests must pass (add one if retention is uncovered). Gate.
20. [ ] **S01 (Medium)** - Unknown config keys silently ignored.
    Where: `src/config/profile.rs:39,119,130,179`, `src/config/provider.rs:23`,
    `src/config/web_search.rs:60,86,99,112`. A typo such as `api_bsae_url` is
    dropped and the default is used silently; in sandbox/approval rule tables
    a misspelled key can silently weaken a restriction. This also contradicts
    "a config the user did write is never silently ignored"
    (`docs/configuration.md`). The current "unknown keys are ignored" rule
    exists for forward compatibility (a file written for a newer Ylva still
    loads), so do not use blanket `deny_unknown_fields`.
    Decision: warn instead of fail, except in rule tables. Warnings must be
    visible in the TUI: terminal output printed before startup is wiped by the
    alternate-screen redraw and `tracing` goes to a log file. Reuse the
    startup-notice channel of the summarizer capacity warnings
    (`src/agent_factory.rs`; consolidated notice shown in the TUI without a
    prompt, see `docs/features.md` "Summarizer capacity warnings").
    1. [ ] Tests: an unknown key produces a warning naming the file, the full
       key path and a "did you mean" suggestion (edit distance); known keys
       produce no warning; an unknown key in a sandbox/approval rule table is
       a hard error naming file and key; with strict mode enabled, warnings
       become errors before the TUI starts (plain terminal error, exit code);
       the TUI startup notice contains the warnings; the CLI driver prints
       them to stderr.
    2. [ ] Detect unknown keys at load time (e.g. via `serde_ignored` or a
       comparison against the known key set; check each struct for flattened
       or extension fields) and return them as structured warnings (file, key
       path, suggestion) from config loading; do not print or log at the
       detection site. Use `deny_unknown_fields` only on rule-table structs.
    3. [ ] Surface the warnings: one consolidated startup notice in the TUI
       conversation view (via the existing capacity-notice channel); stderr in
       the CLI driver; plus a `tracing::warn!` copy in the log. On runtime
       config reload show only new warnings.
    4. [ ] Add an opt-in strict mode (`--strict-config` or config key) that
       turns unknown-key warnings into errors, for CI.
    5. [ ] Docs: `docs/configuration.md` (replace the "Unknown keys are
       ignored" bullet; document warnings, rule-table errors and strict
       mode), `docs/features.md` (where warnings appear),
       `docs/cli-options.md` if a flag is added. Gate.
21. [ ] **S05 (Medium)** - Provider kind is a free string.
    Where: `src/config/provider.rs:24`; engine, model and agent-type names in
    `src/config/profile.rs`.
    1. [ ] Parse `provider` into an enum at load time so typos fail at load;
       newtypes for names only if a YAGNI check justifies them.
    2. [ ] Tests and `docs/configuration.md`. Gate. (Do after S01: same files.)
22. [ ] **S03 (Medium)** - Lossy `as usize`/`as u16` casts on runtime values.
    Where: `web_fetch/native.rs:153,172,220` (`u64` to `usize`, relevant on
    32-bit), `src/tui_widgets.rs:232-286`, `src/tui_widgets/overlays.rs`
    (`usize` to `u16` heights, offsets, widths).
    1. [ ] Tests for values beyond `u16::MAX`.
    2. [ ] Use `try_from` with explicit clamping to terminal bounds. Gate.
23. [ ] **S04 (Medium)** - Detached tasks with unobserved failure.
    Where: `src/agent/request.rs:78,129`, `src/agent/runtime.rs:204` (uncertain:
    shutdown may rely on channel closure), `src/tui_driver/terminal.rs:150`
    (low).
    1. [ ] Confirm each site; skip those that rely on channel closure by design.
    2. [ ] Keep handles (or a `JoinSet`) in the owner, log join errors, await on
       teardown; add a test where a task panics. Gate.
24. [x] **S06 (Low-Medium)** - Shim pruning deletes any unclaimed regular
    file. **Closed: intended behavior, no change.**
    Where: `src/profile_shim.rs:152,174,266-287`. Ylva owns the shim
    directories (`$YLVA_HOME/shims/`, `<workspace-root>/.ylva/shims/`), so any
    regular file in them that no profile claims is Ylva's to remove. This is
    documented in `docs/configuration.md` ("Managing profiles and shims");
    subdirectories are left alone. No marker, no test, no doc change needed.

### Phase 5: maintainability debt

25. [ ] **L01 (Low)** - Panics on runtime state.
    Where: `bash.rs:456-463,550-556` (stdout/stderr take, exit status),
    `web_fetch/native.rs:51-53` (client build), `store.rs:228,331,531,587,673,899`
    and `store/archive.rs:167` (missing parent, poisoned mutex),
    `markdown.rs:448-455` (non-empty precondition).
    1. [ ] Replace each runtime-reachable `expect`/`unwrap` with `ToolError` or
       `StoreError`, or recover from poisoned mutexes; tests where reachable.
       Gate.
26. [ ] **S08 (Low)** - Abbreviated identifier `buf` in `src/markdown.rs:242-243`.
    1. [ ] Rename to `code_block_content`. Gate.
27. [ ] **S09 (Low)** - Fixed 10 ms sleep in `tests/cli_skills.rs:93`.
    1. [ ] Replace with an event-driven wait, or document why polling is
       required. Gate.
28. [ ] **M13 (Medium)** - `main.rs` is not thin.
    Where: `src/main.rs:77-469` violates the project rule; profile loading,
    store opening, engine construction and prompt handling live there.
    1. [ ] Move startup orchestration into library modules re-exported via
       `src/lib.rs`; keep argument parsing, event loop and key handling in
       `main.rs`.
    2. [ ] Add integration tests for the moved logic.
    3. [ ] Docs: `docs/implementation-notes.md`. Gate.
    4. [ ] Commit boundary: pure refactor, nothing else in the change set;
       ask the user before committing (no behavior change, separately
       revertable).
29. [ ] **L02 (Low)** - Oversized production modules.
    Where (approximate non-test size): `cli_driver.rs` (1668 total),
    `tui_driver.rs` (~935), `store.rs` (987), `markdown.rs` (~830),
    `tui_reducer_state.rs` (~685), tool files `bash.rs`, `edit.rs`, `glob.rs`,
    `search.rs`, `web_fetch.rs`, `web_search.rs` (1000-1400 lines each incl.
    inline tests).
    Split one module per sub-step, behavior-preserving, tests green after each;
    gate after each sub-step.
    1. [ ] `cli_driver.rs`
    2. [ ] `tui_driver.rs` (event loop, session switching, title handling)
    3. [ ] `store.rs`
    4. [ ] `markdown.rs`
    5. [ ] `tui_reducer_state.rs`
    6. [ ] Tool files (argument validation, execution, result formatting,
       backends): `bash.rs`, `edit.rs`, `glob.rs`, `search.rs`, `web_fetch.rs`,
       `web_search.rs`.
    7. [ ] Docs: `docs/implementation-notes.md` module map.
    8. [ ] Commit boundary: pure refactor only; ask the user before
       committing, ideally one commit per module split (sub-steps 1-6).
30. [ ] **L03 (Low)** - Wide public library surface.
    Where: `src/lib.rs:16-105`; internal modules (`tui_*`, `cli_driver`,
    `agent_factory`) are public.
    1. [ ] Rule: re-export only what `tests/` uses. Make unused modules
       `pub(crate)`; consider a `#[doc(hidden)]` test-support module. Do after
       M13 and L02 (they move code). Gate.
    2. [ ] Commit boundary: visibility/re-export change only; ask the user
       before committing.
31. [ ] **D4 (Low)** - Replace deprecated `serde_yaml` (depends on
    `unsafe-libyaml`, RUSTSEC-2024-0449) with `serde-saphyr` 2.x. Not urgent.
    Spike (2026-10-17) verified: typed structs incl. untagged enums, `Option`,
    `Vec`, `HashMap` and block scalars work; duplicate keys (also unknown ones)
    error by default; alias bombs hit the default budget; deep nesting and
    garbage input return errors without panics; no `unsafe-libyaml` in the
    tree. YAML is parsed in two places only:
    `src/config/agent_type_file.rs:168` (typed `AgentConfig`) and
    `src/skills.rs:350-374` (untyped `serde_yaml::Value`).
    Use `Options { no_schema: true, with_snippet: false, ..Default::default() }`
    for both.
    1. [ ] Tests first (both paths): duplicate key (incl. unknown key) is
       rejected; unquoted numeric/bool `name`/`description` rejected
       ("must be quoted") while quoted values pass; `name: ~` rejected; empty
       and non-mapping documents rejected; unknown fields ignored; malformed
       YAML names the file and line; agent `can_spawn`/`timeout_secs`
       accept their bool/number-or-string forms.
    2. [ ] `skills.rs`: replace the `Value` extraction with a typed struct
       (`name: String`, `description: String`, unknown fields ignored) and keep
       the existing name/description validation and error messages.
       `agent_type_file.rs`: swap the `from_str` call.
    3. [ ] Update tests that assert `serde_yaml` error wording (messages are
       now single-line "… at line N, column M"; empty input reports
       "unexpected end of input" instead of "missing field").
    4. [ ] Note behavior differences in the docs where user-visible: `yes`/`no`
       parse as booleans for `can_spawn`; `strict_booleans` stays off because
       `bool_or_string` already accepts string forms.
    5. [ ] Remove `serde_yaml` from `Cargo.toml`; drop the temporary
       RUSTSEC-2024-0449 entry from `audit.toml` (D3); `make audit` clean for
       it. Docs: `docs/implementation-notes.md` (YAML parsing policy and
       options). Gate.
32. [ ] **S10 (Low)** - Remaining dependency advisories.
    Verified 2026-10-17 (`cargo audit`, RustSec DB of that day): 6
    vulnerabilities (`h2 0.3.27` and `h2 0.4.15` RUSTSEC-2026-0258; `rustls
    0.23.43` RUSTSEC-2026-0285/-0346/-0348/-0349) and 2 warnings (`bincode`
    RUSTSEC-2025-0141, `unsafe-libyaml` RUSTSEC-2024-0449).
    - `h2 0.3.27`: reachable via `jsonschema 0.17` -> `reqwest 0.11` ->
      `hyper 0.14`; cleared by D1.
    - `h2 0.4.15`: reachable via `reqwest 0.12` -> `hyper 1.x`; cleared by D2.
    - `rustls 0.23.43`: only in `Cargo.lock` (via `hyper-rustls`/`tokio-rustls`
      of an unused `reqwest` feature); `cargo tree --target all -e all -i rustls`
      is empty, so it is not compiled. `cargo audit` scans the lockfile and
      still fails on it, so D2 must update it for a clean audit.
    - `bincode 1.3.3` (via `syntect 5.3`): unmaintained, allow-listed (D3).
    - `unsafe-libyaml 0.2.11` (via `serde_yaml`): cleared by D4.
    1. [ ] Re-run `make audit` after D1, D2 and D4: expect only the `bincode`
       warning (allow-listed in `audit.toml`) and exit code 0. If anything else
       remains, add it here or fix it.

### Close-out

33. [ ] Final documentation pass: confirm these were updated by the steps above
    - `docs/development-workflow.md` (D3), `docs/features.md`,
      `docs/implementation-notes.md`, `docs/persistence.md`,
      `docs/configuration.md`.
    Verify the commit history keeps pure refactors (M13, L02, L03) separate
    from behavior-changing fixes (`git log --stat` review) and report any
    mixed commit. Run `rumdl check .`; run the full gate once more.
34. [ ] Delete or archive this plan only with user consent.

## Implementation Hints

- Dependency audit (`cargo audit`, RustSec DB 2026-10-17, `Cargo.lock`):

  | Crate | Advisory | Reachable | Action |
  | --- | --- | --- | --- |
  | `h2 0.4.15` | RUSTSEC-2026-0258 | Yes, via `reqwest 0.12` | D2 |
  | `h2 0.3.27` | RUSTSEC-2026-0258 | Yes, via `jsonschema 0.17` -> `reqwest 0.11` -> `hyper 0.14`; no 0.3 fix | D1 |
  | `rustls 0.23.43` | RUSTSEC-2026-0285, -0346, -0348, -0349 | No (lockfile only; `cargo tree --target all -e all -i rustls` empty; TLS uses `hyper-tls`/native) | update in D2 (needed for clean `cargo audit`) |
  | `bincode 1.3.3` | RUSTSEC-2025-0141 (unmaintained) | Yes, via `syntect` | allow-list (D3) |
  | `unsafe-libyaml 0.2.11` | RUSTSEC-2024-0449 (unmaintained) | Yes, via `serde_yaml 0.9` | D4 (migrate to `serde-saphyr`) |

- `jsonschema` default features enable `resolve-http` (pulls `reqwest 0.11`) and
  `resolve-file`; disabling them also stops schemas from fetching URLs or reading
  files, narrowing H01.
- Positive patterns to preserve: `store/archive.rs` publishes durably and is the
  model for the shared helper; `store/session_file.rs` repairs only trailing
  corruption on append/resume and keeps pure reads read-only; a dedicated writer
  task keeps persistence off the streaming/TUI path; clippy is clean and
  production code has no `unsafe`.
- Verification policy: `make format lint`, then `make test-coverage` (runs the
  full suite and enforces the coverage threshold). Markdown: `rumdl check .`,
  `rumdl fmt [PATH]`.
