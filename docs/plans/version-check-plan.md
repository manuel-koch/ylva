# Daily new-release check with startup-logo hint

## Context

Ylva (Rust TUI agent-harness, crate `ylva`, repo `https://github.com/manuel-koch/ylva`) has no way
to tell users that a newer release exists. Goal: at TUI startup, check the GitHub Releases API at
most once per day and, if a newer version than the running one exists, render a hint with the new
version and the release page link in the startup logo block. The timestamp of the last check and the
version found are cached in a file in the Ylva home so subsequent starts (within 24 h) make no
network request but can still show the hint.

Reference implementation: `/Users/manuelkoch/workspace/gitover-rust-tui` (`src/ops.rs`
`fetch_latest_release_tag` / `github_releases_url`, `src/main.rs` `semver_gt` and
`handle_release_check_result`, `src/state.rs` persisted `last_notified_release_version`). It queries
`https://api.github.com/repos/<owner>/<repo>/releases/latest` in a background thread, reads
`tag_name`, strips a leading `v`, compares numerically, and silently ignores network errors.

Acceptance criteria:

- TUI start performs at most one release-check request per 24 h, off the UI-critical path
  (non-blocking, failures silent apart from `tracing` logs).
- Cache file in the Ylva home stores last-check timestamp and latest version found.
- When a newer version is known (fresh or cached), the startup logo shows the new version and the
  release URL.
- No hint when current >= latest, on network failure with no cached newer version, or when the cache
  is corrupt (corrupt cache is treated as absent).
- Tests do not touch the network or the real home; docs updated.

Constraints: follow `AGENTS.md` (license headers, full descriptive names, file-per-module, no
`mod.rs`, `make format lint` + `make test-coverage`, update docs in the same commit, rumdl for
markdown). Integration tests cannot rely on `cfg(test)` guards in `src/`, so the network call must
be injectable (known-test-issues).

## Implementation Plan

Implementing agent: flip `[ ]` to `[x]` immediately after completing each step and rewrite this file
in place so progress is resumable across sessions.

1. [x] Read `docs/known-test-issues.md`, `docs/development-workflow.md` (testing, license headers),
   `docs/implementation-notes.md` header guideline and `docs/architecture-design-records/` for
   conflicts; record any ADR needed.
2. [x] Add `src/version_check.rs` (re-export from `src/lib.rs` as needed) with pure core:
       `is_newer_version(current, latest)` (strip leading `v`, numeric `major.minor.patch` compare
       via parsing; pre-releases ignored: non-numeric suffix means not newer), cache struct
       `VersionCheckCache { last_checked_unix: i64, latest_version: Option<String> }`
       (`enabled: bool` default true; read via `toml`, tolerant of unknown/missing fields), and
       `should_check(now, cache) -> bool` (24 h interval, also true when the timestamp is in the
       future or the cache is absent).
3. [x] Add cache persistence in the same module: path `<ylva_home>/version-check.toml` via
       `crate::ylva_home::ylva_home()`; read returns `None` on missing/corrupt; write uses
       `private_fs::replace_file` (mode `0600`) (content produced by `toml_edit` edit-in-place) so
       the write is atomic; write failures only logged.
4. [x] Add the fetch behind an injectable trait/closure (e.g. `ReleaseFetcher`) with a production
   `reqwest` implementation: GET `https://api.github.com/repos/manuel-koch/ylva/releases/latest`
   (derive owner/repo from `env!("CARGO_PKG_REPOSITORY")`), headers `Accept:
   application/vnd.github+json`, user agent `crate::http::OUTBOUND_USER_AGENT`, short timeouts (e.g.
   5 s connect/10 s total), parse `tag_name`. Use `html_url` or `<repository>/releases/tag/<tag>` as
   the link.
5. [x] Add orchestrator
       `check_for_new_version(fetcher, now, current_version) -> Option<NewVersionHint>`
       (`NewVersionHint { version, release_url }`): load cache; if due, fetch, then write cache
       (also update the timestamp on failure to avoid retry storms); return a hint when the
       resulting latest version is newer than `BUILD_INFO.version`.
6. [x] Wire into startup (`src/startup.rs`, TUI branch near
       `TuiApp::new(...).with_display_metadata`): read the cached hint synchronously (cheap file
       read) for instant display, and spawn a background tokio task for the network refresh when
       due; deliver the result to the TUI as an event (see `src/events.rs`) so the logo updates when
       the check finishes. Skip entirely for the headless console path and when disabled
       (`enabled = false` or `YLVA_NO_VERSION_CHECK`).
7. [x] Add `new_version_hint: Option<NewVersionHint>` to the TUI state: `TuiState` in
       `src/tui_render_state.rs`, its source in `src/tui_reducer_state/app_state.rs`, and the
       projection in `src/render.rs` (`project_for_draw`).
8. [x] Render in `append_logo` (`src/tui_widgets/chat.rs`): extra centered line under the `profile:`
       line, e.g. `vX.Y.Z available: <url>`, in a highlight theme color added in
       `src/theme.rs` (alongside `build_info_color`). `append_logo` is also called from
       `src/tui_widgets/chat/cache.rs` (~line 359): the hint must be part of that render-cache key
       so a late-arriving result redraws the logo. Also add a `New version:` row to the help dialog
       (`src/tui_widgets/help.rs`, next to the `Profile:` row, ~line 171). Logo-height/layout tests
       must still pass.
9. [x] Add the hint to `--version`: in `version_report()` (`src/main.rs`, `--version` is handled
       before clap parsing and logging) append a `vX.Y.Z available: <url>` line using
       the cache only, with no network call and no tokio runtime; absent when disabled, no cache, or
       not newer. Keep the cache read/compare in `src/version_check.rs` so `main.rs` stays thin. Do
       not add the hint to the `/status` dialog. Add a unit test for the report text with a temp
       `YLVA_HOME`-independent injected cache path (avoid mutating process env, see
       `docs/known-test-issues.md`).
10. [x] Unit tests (`src/version_check/tests.rs` or `src/version_check.rs` tests, per repo layout):
        version comparison table (v-prefix, longer/shorter, equal, older, garbage), `should_check`
        boundaries, cache roundtrip/corrupt/missing, orchestrator with a stub fetcher (skips fetch
        within 24 h, fetches when stale, failure keeps cached version, writes cache). Use temp dirs,
        not the real home. Add a render test in `src/tui_widgets/chat/tests.rs` for the logo hint
        present/absent.
11. [x] Documentation: `docs/features.md` (new feature), `docs/persistence.md` (new
        `~/.ylva/version-check.toml` file, format, mode `0600`, corrupt-tolerance),
        `docs/configuration.md` and/or `docs/cli-options.md` (`--version` hint, `enabled` flag and
        `YLVA_NO_VERSION_CHECK`), `docs/implementation-notes.md` (only the non-obvious: injectable
        fetcher, why the timestamp is also written on failure), `CONTEXT.md` if a new domain term is
        introduced. `CHANGELOG.md` is generated by release-plz; do not edit.
12. [x] Resolve doc inconsistency: `docs/features.md` (~line 530) lists every outbound HTTP request
        as model listing, chat/SSE and Ollama metadata and claims the shared `User-Agent`; add the
        GitHub release check to that list and state it is opt-out. Grep `README.md` and `docs/` for
        any statement that Ylva contacts only model providers or makes no other network calls, and
        amend.
13. [x] Run `rumdl fmt` on changed markdown, then `make format lint` and `make test-coverage`; no
        new dependencies expected, so `make audit` is not needed unless `Cargo.toml` changes.

Execution notes: the hint lives in `TuiDisplayMetadata` (not a separate `TuiState` field) so it
survives session switches; `version_report()` is in `src/startup.rs`; the logo render test is in
`tests/tui_render_state/bubbles.rs`.

## Implementation Hints

- Opt-out: `enabled = true` (default) in `version-check.toml`, alongside `last_checked_unix` and
  `latest_version`; env `YLVA_NO_VERSION_CHECK=1` also disables and wins over the file. A missing or
  unreadable file counts as enabled with no cache. Cache writes must edit in place with `toml_edit`
  (already a dependency) so `enabled` and unknown keys survive.
- Decisions: opt-out enabled by default; pre-releases ignored (plain `x.y.z` compare); the
  last-check timestamp advances even on fetch failure (cached `latest_version` kept); hint shown in
  the startup logo, the help dialog and `--version` (not `/status`); cache is global and TOML
  (`version-check.toml`).
- Ylva home: `src/ylva_home.rs::ylva_home()` (`YLVA_HOME` override, else `~/.ylva`); existing
  top-level file precedent: `~/.ylva/trusted-workspaces.toml` (mode `0600`), documented in
  `docs/persistence.md` under "Workspace trust store".
- Atomic private writes: `src/private_fs.rs::replace_file(path, contents, unix_mode)`;
  `create_private_dir_all`. Time: `src/unix_time.rs::unix_now()`; `chrono` also available.
- HTTP: `reqwest` 0.13 (json feature) already a dependency; `src/http.rs` has `OUTBOUND_USER_AGENT`
  and `provider_client_builder`; `redacted_url_for_display` is the URL form for tracing fields. No
  `semver` crate is a direct dependency; keep a small hand-rolled comparison (as gitover) to avoid
  new deps.
- Current version: `BUILD_INFO.version` (`src/build_info.rs`) / `env!("CARGO_PKG_VERSION")` (0.35.0
  at planning time). Release tags look like `v0.35.0` (`CHANGELOG.md` compare links).
- Logo: `src/tui_widgets/chat.rs::append_logo` renders wordmark, `v<version>  <commit>`,
  `profile: <name>` then a spacer; shown only when the root conversation is empty and not in a
  subagent view. Display data flows via `TuiDisplayMetadata` (`src/tui_reducer_state/panels.rs`),
  built in `src/startup.rs` (~line 376), cloned in `src/render.rs` and
  `src/tui_driver/event_loop.rs`. `TuiDisplayMetadata` derives `Eq`, so adding fields there must
  stay `Eq`.
- Colors: `src/theme.rs` (`logo_color`, `build_info_color`).
- Gitover shows a popup once per version; here the requirement is a passive logo hint instead.
- `cfg(test)` is not set for library code during integration tests, hence the injectable fetcher.
- Module style: `foo.rs` plus `foo/` directory for submodules, never `mod.rs`.
