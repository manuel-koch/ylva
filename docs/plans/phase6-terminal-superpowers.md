# Phase 6: Terminal Superpowers

## Context

**System:** Ylva — a Rust-based terminal AI coding agent (TUI + headless CLI).
The codebase ships as a binary + library split (`src/main.rs` + `src/lib.rs`, crate `ylva`).
After Phase 5 the agent has named engines, `AgentFactory`, `Engine`/`EngineImpl`,
`App` trait, `Role::Root`/`Role::Child` stubs, and `ToolRegistry` with `fs` tools
(`read`, `write`, `glob`).

**Goal:** Promote Ylva from a chat relay with file tools to a true coding agent by adding:

1. `bash` shell-execution tool (process-group kill on timeout/cancel, 30s default timeout)
2. `search` tool — pure-Rust implementation using `ignore` (`.gitignore`-aware traversal)
   and `regex` (no external binary dependency)
3. `edit` tool (search-and-replace or line-range patch with unified diff preview)
4. `clarify` tool — TUI: question banner with optional multiple-choice (≤5 options);
   headless: excluded from the LLM's `tools` array entirely
5. Sandbox — `ApprovalCheck` enum + `Sandbox` struct (shared via `Arc<RwLock<Sandbox>>`)
   with hardcoded defaults for path-dependent approval. Each tool embeds a `SandboxContext`
   (built from the tool's own config + `Arc<RwLock<Sandbox>>` at construction). The `Tool`
   trait's `needs_approval` signature becomes `(&self, args: &Value) -> ApprovalCheck` —
   the sandbox is inside the tool, not threaded through the call site.
6. Enhanced `glob` tool — adds `git_ignore`, `only_count`, `limit`, `only_files`,
   `only_dirs` flags; structured `ToolCallResult` metadata for truncation signalling
7. Human-in-the-loop: approval modal for `bash` (Strict); diff view + approval for
   `write`/`edit` (Ask); `clarify` interactive banner (Interactive)
8. `spawn_subagent` tool + `ChildAgentManager` (unbounded FIFO queue; one child active at
   a time; no nesting)
9. `finish` tool (child-only; unstructured fallback if never called)
10. Child-agent persistence records (`child_start` / `child_result` JSONL) in parent session
11. Drill-in TUI conversation model: child agent replaces main view with breadcrumb header
    and "back" button; parent chat shows a clickable spawn-bubble; auto-switch on interactive
    child pauses (approval / `clarify`); parent continues running concurrently while a child
    is active; `spawn_subagent` returns immediately and child `finish` arrives as an async
    observation
12. `--allow-child-agents` headless flag; stderr status lines for child lifecycle

**Motivation:** Phase 5 delivered the interface seams (`AgentFactory`, `Engine`, `Role`
stubs). Phase 6 fills in the tool implementations, safety machinery, and delegation
mechanics, making Ylva capable of editing code autonomously while remaining human-supervised.

**Acceptance criteria / definition of done:**

- `bash` executes shell commands; stdout and stderr are merged in true arrival order
  (concurrent async reads); orphaned grandchildren are killed on timeout/cancel via
  `tokio::process::Command::process_group(0)` + `killpg`; output capped at 100 KB
  (first chunk kept); `tool_output_truncated` and `tool_output_truncated_at_bytes`
  metadata fields present when capped.
- `search` finds files and text using pure-Rust `ignore` + `regex`; no `rg` binary required.
  Results in grep style (`<file>:<line>: <content>`); binary files (null byte in first 8 KB)
  skipped silently; output capped at 100 KB with `tool_output_truncated` metadata.
- `edit` applies search-and-replace (`SearchReplace` replaces first occurrence; `ToolError`
  if not found) and line-range patches (1-based, documented in schema; `ToolError` if
  `start_line > end_line` or file does not exist); shows a unified diff before approval;
  writes to disk only after approval is granted.
- `glob` supports `git_ignore`, `only_count`, `limit`, `only_files`, `only_dirs` flags;
  returns `tool_output_truncated` / `tool_output_total_matches` metadata when `limit` is
  active; `ToolError` if both `only_files` and `only_dirs` are set.
- `clarify` shows a question banner (+ numbered choices if provided) in TUI mode; the model
  may supply up to 5 pre-populated choices; the user can pick one or type freely; `clarify`
  is absent from the `tools` array in headless mode.
- `Sandbox` enforces: deny reads/globs outside workspace root; deny dot-path access; defaults
  are hardcoded (TOML config loading deferred to Phase 7).
- `ApprovalCheck` (AUTO_APPROVED / AUTO_DENIED { reason } / USER_APPROVAL_REQUIRED) is
  returned by every `Tool::needs_approval(&self, args: &Value)` call; agent loop acts
  accordingly. The sandbox is embedded in each tool via `SandboxContext`.
- Approval modal and diff view appear for `bash`, `write`, `edit` before execution.
- `spawn_subagent` runs one child at a time; additional calls are queued in an unbounded
  FIFO and executed automatically after the current child finishes. Returns
  `{ "status": "spawned", "run_id": "..." }` when the child starts immediately, or
  `{ "status": "queued", "run_id": "..." }` when it is queued behind an active child.
- `spawn_subagent` requires human approval by default (`UserApprovalRequired`);
  `--auto-approve-spawn` bypasses this. Children inherit the parent's `--auto-approve-spawn`
  and `--auto-approve-write` flags. No blanket `--auto-approve-all` flag exists.
- `finish` terminates the child's ReAct loop (uses same max-iterations limit as parent);
  unstructured fallback activates on timeout or max iterations.
- `child_start` / `child_result` JSONL records are appended to the parent session file.
  Cancelled children (parent stopped, or queue drained) write a `child_result` record
  with `status: "cancelled"`.
- Viewing a child's conversation: full-screen drill-in with non-scrolling breadcrumb header
  and "← Back (Ctrl+B)" button; parent continues processing messages and tools while a
  child is active.
- Parent chat shows a clickable spawn-bubble (mouse); clicking it navigates to the child
  conversation. A keyboard-driven `/agents` command is deferred to a future phase.
- While in child view and the child is not paused for input, the input box is disabled with
  a muted `"Waiting for child agent…"` hint; input is not buffered.
- TUI auto-switches to child view when the child needs user input; auto-returns to the
  previous view only if the switch was automatic (manual navigation stays).
- Child interactive pauses take precedence over parent interactive pauses.
- `--allow-child-agents` enables child spawning in headless mode (disabled by default).
- `--auto-approve-spawn` bypasses approval for `spawn_subagent`; children inherit it.
- All existing tests pass; new modules meet ≥80% line-coverage threshold.
- `docs/features.md`, `docs/configuration.md`, and `docs/keybindings.md` are updated
  (`Ctrl+B` back-to-parent keybinding added in Phase 6.10).

**Key constraints:**

- Binary + library split: anything integration tests need must be re-exported via `src/lib.rs`.
- Module layout: file-per-module Rust 2018+ style (`foo.rs`, not `foo/mod.rs`).
- `main.rs` stays thin (event loop, key handling only).
- `AgentFactory::create(profile, agent_type, role)` is the single construction path.
- `Role::Child` never receives `spawn_subagent` in its `ToolRegistry` (structural enforcement).
- One active child at a time; additional `spawn_subagent` calls are queued in an unbounded
  FIFO; each queued child starts automatically when the previous child's `finish_rx` fires.
- No nesting: delegation depth limit of one (Phase 6; future phases may relax this).
- `clarify` multiple-choice is capped at 5 options; enforced in JSON schema via
  `maxItems: 5` and validated at call-parse time in `create_call`.
- `Sandbox` in Phase 6 uses hardcoded defaults; TOML `[tool.*]` config loading is Phase 7.
- `similar` crate for unified diff generation.
- `ignore` + `regex` crates for `search` (no external binary dependency).
- `nix` crate (Unix target only) for `killpg`; `tokio::process::Command::process_group(0)`
  is available on tokio ≥ 1.35 (current lockfile resolves ≥ 1.53).
- `AgentCommand::AnswerClarify(String)` is a new dedicated variant; existing `ApproveTool`
  match arms are not changed.
- Safety inheritance: child agents inherit parent `--auto-approve-write` and
  `--auto-approve-spawn` flags. No blanket `--auto-approve-all` flag.
- `spawn_subagent` always requires human approval (`UserApprovalRequired`) unless
  `--auto-approve-spawn` is set.
- Persistence format: JSONL append-only, no SQLite (blueprint §4H).
- `Section` enum (`Section::Content(ContentSection)` / `Section::Diff(DiffSection)`) replaces
  bare `Vec<ContentSection>` on `pending_content_sections()` and `content_sections()`; both
  traits return `Vec<Section>` after Phase 6.0.
- `AgentLoop` owns `mpsc::Receiver<FinishResult>`; `ChildAgentManager` holds `Sender` + `JoinHandle`.
- `AgentLoop` gains `session_id: Uuid` generated in `new()`.
- `Store` uses `Mutex<BufWriter<File>>` per session file (not bare `append(true)`).

## Implementation Plan

Each sub-phase is independently committable and verifiable. Complete all steps within
a sub-phase — implementation + tests + named doc updates — before starting the next.
Run `make test-coverage` at the end of every sub-phase to confirm ≥80% line coverage.

---

### Phase 6.0 — Foundations (Crates, Sandbox, Section, ApprovalCheck loop wiring)

**Goal:** All compile-time and runtime foundations every subsequent sub-phase depends on.
No user-visible behavior. Must be fully green before any tool work begins.

- [x] 1. Add to `[dependencies]` in `Cargo.toml`:
  - `nix = { version = "0.29", features = ["signal", "process"] }` under
    `[target.'cfg(unix)'.dependencies]`
  - `ignore = "0.4"`
  - `regex = "1"`
  - `similar = { version = "2", features = ["text"] }`

- [x] 2. Create `src/sandbox.rs`. Define `ApprovalCheck`, `SandboxRule`, `Sandbox`, and
  `SandboxContext` (last-match-wins evaluation; see
  `docs/ideas/read-tool-auto-approve.md` for the full model):

   ```rust
   pub enum ApprovalCheck {
       AutoApproved,
       AutoDenied { reason: String },
       UserApprovalRequired,
   }

   pub struct SandboxRule {
       pub verdict: ApprovalCheck,
       pub pattern: glob::Pattern,  // matched against canonicalized absolute path
   }

   /// Workspace root only. Phase 7 adds per-tool TOML config.
   pub struct Sandbox { pub workspace_root: PathBuf }
   impl Sandbox { pub fn new() -> Self { /* workspace_root = std::env::current_dir() */ } }

   /// Per-tool context. Built once at construction; re-reads Sandbox on each check.
   /// Evaluation: normalize path (workspace_root.join(path), collapse ".." without
   /// hitting filesystem) → walk rules in order → last match wins.
   /// Path does not need to exist on disk; normalization alone determines workspace membership.
   /// Returns None when no rule fires (sandbox abstains); the tool applies its own default.
   pub struct SandboxContext {
       sandbox: Arc<RwLock<Sandbox>>,
       rules: Vec<SandboxRule>,  // named rules prepended; glob list appended
   }
   impl SandboxContext {
       /// Phase 6 defaults: dot_path = deny, outside_workspace = deny.
       pub fn new(sandbox: Arc<RwLock<Sandbox>>) -> Self { ... }
       pub fn check_path(&self, path: &Path) -> Option<ApprovalCheck> { ... }
   }
   ```

- [x] 3. Introduce `Section` enum in `src/agent/tools.rs` alongside `ContentSection`:

   ```rust
   pub struct DiffSection {
       pub label: Option<String>,
       pub content: String,   // unified diff text (from `similar`)
   }

   pub enum Section {
       Content(ContentSection),
       Diff(DiffSection),
   }
   ```

  Change `ToolCall::pending_content_sections()` and `ToolCallResult::content_sections()`
  to return `Vec<Section>` instead of `Vec<ContentSection>`. Add default implementations
  returning `vec![]`. Update every existing implementor to wrap `ContentSection` in
  `Section::Content(...)` — mechanical, no logic change.

  **Breaking-change note:** this is a coordinated change — all existing tools, all
  rendering paths in `tui_driver.rs` / `ui.rs`, and all test assertions on these methods
  must be updated in the same commit to avoid compilation failures mid-refactor.

- [x] 4. Update `default_registry()` in `src/agent/tools.rs` to accept
  `sandbox: Arc<RwLock<Sandbox>>`. Each tool is constructed with
  `SandboxContext::new(Arc::clone(&sandbox))`.

  **Call-site atomicity:** every call site (`src/agent.rs`, `src/agent_factory.rs`, and
  tests) must be updated in the same commit as steps 4–6 to keep the codebase compilable.

- [x] 5. Update the `Tool` trait: replace `fn needs_approval(&self) -> bool` with
  `fn needs_approval(&self, args: &Value) -> ApprovalCheck`. Update every existing
  implementor (`ReadTool`, `WriteTool`, `GlobTool` in `src/agent/tools/fs.rs`; `TestTool`
  in the unit-test module). Each concrete tool struct gains `sandbox_context: SandboxContext`.

  Each tool calls `sandbox_context.check_path()` which returns `Option<ApprovalCheck>`:
  - `None` → sandbox abstains; tool applies its own default (`AutoApproved` for reads,
    `UserApprovalRequired` for writes).
  - `Some(verdict)` → explicit sandbox opinion; tool honours it unconditionally.

  This keeps `UserApprovalRequired` as a first-class rule verdict (e.g. `user:*.secret`
  globs) rather than conflating it with "no rule fired".

- [x] 6. Wire `ApprovalCheck` into the agent loop (`src/agent.rs`) and `src/cli_driver.rs`:
  - `AutoApproved` → execute immediately, no pause.
  - `AutoDenied { reason }` → send `ToolError` with the reason; no pause.
  - `UserApprovalRequired` → existing pause/`ToolPending` path (unchanged).

    Construct `Arc::new(RwLock::new(Sandbox::new()))` once in `AgentLoop::new` and
    pass to `default_registry(sandbox)`. Add `session_id: Uuid` field to `AgentLoop`,
    generated via `Uuid::new_v4()` in `new()`.

    Update every `default_registry()` call site (in `src/agent.rs`, `src/agent_factory.rs`,
    and tests) atomically in the same commit as steps 4–6.

- [x] 7. Update render path for `pending_content_sections()`: in `tui_driver.rs` / `ui.rs`,
  when a tool is pending approval and `call.pending_content_sections()` returns
  `Section::Diff(...)` entries, render them with `+` lines in green and `−` lines in red
  in a scrollable overlay above the approval footer. `Section::Content(...)` entries render
  as before (line numbers + syntax highlighting). Collapse to a one-line summary if
  terminal height is below 20 rows.

- [x] 8. Write unit tests:
  - `SandboxContext::check_path`: outside-workspace denied; dot-path denied; later
    `allow` glob overrides earlier deny (last-match wins); no-match → `UserApprovalRequired`.
  - `WriteTool::needs_approval`: always returns `UserApprovalRequired` regardless of path.
  - `Section` round-trips: existing tool calls still produce `Section::Content` sections.
  - Agent loop: `AutoApproved` skips pause; `AutoDenied` emits `ToolError`.

- [x] 9. Run `make test` + `make test-coverage`. Fix regressions before continuing.

---

### Phase 6.1 — `bash` Tool

**Goal:** Shell command execution with process-group kill and 30-second timeout.

- [x] 10. Create `src/agent/tools/bash.rs`. Define:

    ```rust
    #[derive(JsonSchema, Deserialize)]
    struct BashArgs { command: String }
    ```

  Implement `BashTool` + `BashCall`. `BashTool` embeds `sandbox_context: SandboxContext`.
  In `BashCall::run`:
  - Spawn via `tokio::process::Command::new("bash").arg("-c").arg(&command)`
    with `.process_group(0)`.
  - Capture pgid immediately after spawn via `.id()`.
  - Read stdout and stderr concurrently via two async tasks, merging lines in arrival
    order (true interleaved output). Buffer up to 100 KB total; once the cap is hit,
    stop buffering and discard remaining output.
  - Wrap in `tokio::time::timeout(Duration::from_secs(DEFAULT_BASH_TIMEOUT_SECS), ...)`.
  - On timeout or `CancellationToken` fire: kill the process group via
    `nix::sys::signal::killpg(Pid::from_raw(pgid as i32), Signal::SIGKILL)`.
  - Return buffered output on success; include `tool_output_truncated: true` and
    `tool_output_truncated_at_bytes: <n>` in result metadata when the cap was hit.
    Return `ToolError` on non-zero exit or timeout.
  - `needs_approval(&self, _args)` returns `UserApprovalRequired` unconditionally
    (or `AutoApproved` when `--auto-approve-bash` is set — **not** in Phase 6).
  - Define `DEFAULT_BASH_TIMEOUT_SECS: u64 = 30` alongside `DEFAULT_TOOL_TIMEOUT_SECS`.
  - Define `BASH_OUTPUT_LIMIT_BYTES: usize = 100 * 1024`.

- [x] 11. Register `BashTool` in `default_registry()`.

- [x] 12. Write unit tests: happy path (`echo`), timeout kills child process group, cancellation
  token kills child process group, output truncated at 100 KB cap (`tool_output_truncated`
  metadata present), non-zero exit returns `ToolError`.

- [x] 13. Update `docs/features.md`: add `bash` entry (approval level, timeout, process-group kill).

- [x] 14. Run `make test` + `make test-coverage`.

---

### Phase 6.2 — `search` Tool (pure Rust)

**Goal:** Text search using `ignore` + `regex`; no external binary.

- [x] 15. Create `src/agent/tools/search.rs`. Define:

    ```rust
    #[derive(JsonSchema, Deserialize)]
    struct SearchArgs {
        pattern: String,
        path: Option<String>,
        #[serde(default)]
        case_sensitive: bool,
        #[serde(default)]
        context_lines: usize,
    }
    ```

  `SearchTool` embeds `sandbox_context: SandboxContext`. In `SearchCall::run`:
  - Build `regex::Regex` (respecting `case_sensitive`).
  - Use `ignore::WalkBuilder` rooted at `path` (or cwd) with `.git_ignore(true)`.
  - Skip files that appear binary: if the first 8 KB of a file contains a null byte,
    omit it from results silently (same heuristic as `git grep` and `rg`).
  - For each file: read UTF-8 lossy, collect matches with line numbers and
    `context_lines` before/after.
  - Format results in grep style: `<filepath>:<line_number>: <line_content>`.
    Context lines are prefixed with `<filepath>-<line_number>- <content>`.
    One `ContentSection` per matching file, label set to the file path.
  - Cap total output at 100 KB (first chunk kept); set `tool_output_truncated: true`
    and `tool_output_truncated_at_bytes: <n>` in result metadata when capped.

    `needs_approval(&self, args)`:
  - If `args.path` is `Some(p)`: return `sandbox_context.check_path(p)`.
  - If `args.path` is `None` (defaults to cwd, inside workspace): return `AutoApproved`.

- [x] 16. Register `SearchTool` in `default_registry()`.

- [x] 17. Write unit tests: match found, no match, invalid regex, case-insensitive, context lines,
  explicit out-of-workspace path returns `AutoDenied`, binary file skipped (null byte),
  output truncated at 100 KB cap (`tool_output_truncated` metadata present).

- [x] 18. Update `docs/features.md`: add `search` entry.

- [x] 19. Run `make test` + `make test-coverage`.

---

### Phase 6.3 — Enhanced `glob` Tool

**Goal:** Filtering flags and structured result metadata per `docs/ideas/glob-tool-filtering.md`.

- [x] 20. Extend `GlobArgs` in `src/agent/tools/fs.rs`:

    ```rust
    #[serde(default = "default_true")] git_ignore: bool,  // default: true
    #[serde(default)] only_count: bool,
    #[serde(default)] only_files: bool,
    #[serde(default)] only_dirs: bool,
    #[serde(default)] limit: Option<usize>,
    ```

- [x] 21. Update `GlobTool::run`:
  - When `git_ignore` is `true` (default), use `ignore::WalkBuilder`; else use `glob` crate.
  - Return `ToolError` immediately if both `only_files` and `only_dirs` are `true`
    (contradictory combination).
  - Apply `only_files` / `only_dirs` filters.
  - Apply `limit`; set `tool_output_truncated: true` and `tool_output_total_matches: <n>`
    in metadata when capped.
  - When `only_count`: return count via `tool_output_total_matches` in metadata;
    content is empty string.

- [x] 22. Add `fn metadata(&self) -> Option<serde_json::Value> { None }` default method to
  `ToolCallResult` trait. Override in `GlobResult` to return truncation/count metadata
  using the `tool_output_*` prefix (`tool_output_truncated`, `tool_output_total_matches`).
  Serialize metadata into the tool-role message alongside content.

- [x] 23. Update `GlobTool::needs_approval(&self, args)` to call
  `sandbox_context.check_path(resolved_root)` on the input path root only (before
  traversal); return `UserApprovalRequired` if the root falls outside the workspace.
  Do not check individual result paths — listing paths is not content access.

- [x] 24. Write unit tests for all new flags, truncation metadata (`tool_output_*` keys),
  `ToolError` on contradictory `only_files + only_dirs`, and approval check.

- [x] 25. Update `docs/features.md`: update `glob` entry with new flags.

- [x] 26. Run `make test` + `make test-coverage`.

---

### Phase 6.4 — `edit` Tool

**Goal:** Line-based or search-and-replace file patching; diff preview via
`pending_content_sections()`; write only after approval.

- [x] 27. Create `src/agent/tools/edit.rs`. Define:

    ```rust
    #[derive(JsonSchema, Deserialize)]
    struct EditFileArgs {
        path: String,
        #[serde(flatten)]
        mode: EditMode,
    }

    #[derive(JsonSchema, Deserialize)]
    #[serde(untagged)]
    enum EditMode {
        SearchReplace { old_string: String, new_string: String },
        /// Line numbers are 1-based (documented in JSON schema description).
        /// Returns ToolError if start_line > end_line.
        LineRange { start_line: usize, end_line: usize, new_string: String },
    }
    ```

  `EditCall` is constructed with the parsed args. At construction time (before approval):
  - Return `ToolError` immediately if the file does not exist (use `write` to create new files).
  - Read the current file content.
  - `SearchReplace`: replace the **first** occurrence of `old_string`; return `ToolError`
    if not found.
  - `LineRange`: validate `start_line <= end_line` (1-based) and range is within file
    bounds; return `ToolError` otherwise.
  - Compute unified diff via `similar::TextDiff`.
  - Store both `patched_content: String` and `diff: String` as fields on `EditCall`.

    `EditCall::pending_content_sections()` returns `vec![Section::Diff(DiffSection {
    label: Some(path.clone()), content: self.diff.clone() })]`.

    `EditCall::run()` writes `self.patched_content` to disk (called only after approval).

    `EditTool::needs_approval(&self, args)` calls `sandbox_context.check_path(path)`.

- [x] 28. Register `EditTool` in `default_registry()`.

- [x] 29. Write unit tests: `SearchReplace` happy path (first occurrence replaced) and diff output,
  not-found error, multiple-occurrence replaces only first,
  `LineRange` happy path and diff output, out-of-bounds error, `start_line > end_line` error,
  non-existent file returns `ToolError`,
  `pending_content_sections()` returns `Section::Diff`.

- [x] 30. Update `docs/features.md`: add `edit` entry (two modes, diff preview, approval flow).
  Remove the `write` WIP note — `write` is fully implemented and approval-gated.

- [x] 31. Run `make test` + `make test-coverage`.

---

### Phase 6.5 — `clarify` Tool

**Goal:** Interactive model-to-user question; optional multiple-choice (≤5); excluded in headless.

- [x] 32. Create `src/agent/tools/clarify.rs`. Define:

    ```rust
    #[derive(JsonSchema, Deserialize)]
    struct ClarifyArgs {
        question: String,
        #[serde(default)]
        context: Option<String>,
        #[serde(default)]
        #[schemars(length(max = 5))]
        choices: Option<Vec<String>>,  // max 5; enforced in schema and validated in create_call
    }
    ```

  `ClarifyTool::needs_approval` returns `UserApprovalRequired` (Interactive flow).
  `create_call` validates `choices.len() <= 5`; returns `ToolError` if exceeded.
  `ClarifyTool` is **not** registered in `default_registry()`; injected into the registry
  by `TuiApp` at startup only. If a `clarify` call arrives in headless mode despite this
  (e.g. session replay), return `ToolError` with message
  `"clarify is not available in headless mode"`.

- [x] 33. Add `AgentCommand::AnswerClarify(String)` to `src/agent.rs`. Agent loop handles
  `clarify` calls specially:
  - Pause loop; emit `UiUpdate::ClarifyPending { question, choices }`.
  - Await `AgentCommand::AnswerClarify(answer)` or `AgentCommand::Stop`.
  - Inject the answer as the tool result; resume.

- [x] 34. Add `TuiMode::ClarifyPending { question: String, choices: Option<Vec<String>> }` to
  `src/app_state.rs`. Add `DomainEvent::ClarifyPending` and handle in `presenter.rs`.

- [x] 35. In `tui_driver.rs` / `render.rs`: when `TuiMode::ClarifyPending`, render a yellow
  banner with the question text. If `choices` is `Some`, render numbered items `1.`–`5.`.
  Pressing `1`–`5` populates the input field with the chosen text. `Enter` sends
  `AgentCommand::AnswerClarify(input)`.

- [x] 36. Write unit tests: choice-count validation (≤5 ok, 6 → error); presenter tests for the
  `ClarifyPending` state transition.

- [x] 36a. Verify that the agent loop branches on tool name before emitting the pending `UiUpdate`:
      `ClarifyTool` must emit `UiUpdate::ClarifyPending` and route through
      `AgentCommand::AnswerClarify`; all other tools must emit `UiUpdate::ToolPending` and
      route through `ApproveTool`/`DenyTool`. No path may send `ToolPending` for a `clarify`
      call or `ClarifyPending` for a non-clarify call. Add regression tests for both branches.

- [x] 37. Update `docs/features.md`: add `clarify` entry (banner, choices, headless exclusion).

- [x] 38. Run `make test` + `make test-coverage`.

---

### Phase 6.6 — Markdown rendering for chat bubbles and clarify banner

**Goal:** Render Markdown in assistant messages, user messages, clarify `question`, and
clarify `context` fields. Plain text fallback is preserved when content contains no
Markdown syntax.

Currently `append_assistant_lines`, `append_user_lines`, and `draw_clarify_banner` all
call `textwrap::wrap` on raw text, producing no formatting. This phase replaces that with
a lightweight Markdown-to-`ratatui` `Line` conversion.

- [x] 39. Add `tui-markdown` (or `pulldown-cmark` with a custom `ratatui` renderer) to
  `Cargo.toml`. Prefer a crate that emits `Vec<Line<'static>>` directly; avoid pulling in
  a full browser-side HTML pipeline. Evaluate `tui-markdown` v0.x before choosing.

- [x] 40. Create `src/markdown.rs`: a `render_markdown(text: &str, width: usize) ->
  Vec<Line<'static>>` function that converts Markdown to styled `ratatui` lines. Supported
  elements (minimum viable set):
  - Paragraphs with hard-wrapped lines at `width` columns.
  - **Bold** and *italic* via `Modifier::BOLD` / `Modifier::ITALIC`.
  - `inline code` via a distinct foreground color (reuse the existing
    `INLINE_CODE_FG` constant or introduce one).
  - Fenced code blocks: rendered as-is in a dim style, no syntax highlighting.
  - Bullet lists: `•` prefix, indented continuation lines.
  - Numbered lists: `N.` prefix, indented continuation lines.
  - Headings: bold, prefixed with `#` / `##` etc. (no decorative bars).
  - Blockquotes: `│` prefix in `DarkGray`.
  - Horizontal rules: a line of `─` characters spanning `width`.
  - Anything else passes through as unstyled text (no crash, no data loss).

- [x] 41. Replace `textwrap::wrap` in `append_assistant_lines` and
  `append_streaming_lines` (`ui.rs`) with `render_markdown`. Keep the existing
  left-border bar (`▌`), background fill, and `interrupted` / dim styling — only the
  inner text spans change.

- [x] 42. Replace `textwrap::wrap` in `append_user_lines` (`ui.rs`) with
  `render_markdown`. User messages often contain code snippets or lists pasted from
  terminals; rendering them formatted improves readability.

- [x] 43. Replace `textwrap::wrap` for `question` and `context` in
  `draw_clarify_banner` (`ui.rs`) with `render_markdown`. Bold on question lines and
  dim on context lines are already applied via `patch_style` — keep those overrides.

- [x] 44. Unit-test `render_markdown` in `src/markdown.rs`: cover bold/italic, inline
  code, fenced block, bullet list, numbered list, heading, blockquote, horizontal rule,
  and plain text (regression guard). Use `ANSI`-free assertions on span content and
  modifiers.

- [x] 45. Update `docs/features.md`: document Markdown rendering in chat bubbles and
  clarify banner.

- [x] 46. Run `make test` + `make test-coverage`.

---

### Phase 6.7 — Thinking-block bubble

**Goal:** Surface LLM thinking output (extended-thinking / reasoning tokens) in the TUI
as a collapsible bubble that renders its content as Markdown. Thinking content must flow
through the streaming pipeline without affecting the assistant text or tool-call logic.

Currently `StreamEvent` has no thinking variant and `UiUpdate` carries no thinking
payload. This phase adds the full path from the SSE stream to a rendered bubble.

- [x] 47. Add `StreamEvent::Thinking(String)` to the `pub enum StreamEvent` in
  `src/agent.rs`. Update `MockLlm` in `src/llm.rs` to include a matching
  `MockEvent::Thinking(String)` variant so tests can inject thinking tokens.

- [x] 48. In `src/llm/openai_provider.rs`, parse Anthropic-style
  `content_block_delta` events where `delta.type == "thinking_delta"` (field
  `delta.thinking`) and OpenAI-style `reasoning_content` deltas, emitting
  `StreamEvent::Thinking(fragment)`. All other SSE payloads are unaffected.

- [x] 49. Add `UiUpdate::AppendThinking(String)` to `pub enum UiUpdate` in
  `src/agent.rs`. In the `StreamEvent::Thinking` arm of the agent stream loop
  (`src/agent.rs`), send this update; do **not** append to `partial` (the
  assistant text accumulator).

- [x] 50. In `src/app_state.rs`, add `partial_thinking_text: String` alongside
  `partial_assistant_text`. Handle `UiUpdate::AppendThinking` by appending to it.
  On `UiUpdate::StreamDone`, if `partial_thinking_text` is non-empty, push a
  `ChatEntry::ThinkingMessage { text: partial_thinking_text.drain(..).collect(),
  collapsed: true }` **before** the assistant bubble. On `UiUpdate::StreamStarted`,
  reset `partial_thinking_text`.

- [x] 51. Add `ChatEntry::ThinkingMessage { text: String, collapsed: bool }` to the
  `ChatEntry` enum in `src/app_state.rs`. Persist `collapsed` state per entry;
  toggle with a keypress or click on the bubble header (handled in Phase 6.7 step 53).

- [x] 52. In `src/ui.rs`, add `append_thinking_lines` (mirrors `append_assistant_lines`
  structure). Use a distinct border color (`THINKING_BORDER_COLOR`, dim cyan) and
  background (`THINKING_BG_COLOR`). When `collapsed`, render only a single summary
  line: `▌  💭 Thinking  (N lines)` where N is the line count of the full text.
  When expanded, render the content via `render_markdown` (Phase 6.6). Add a
  streaming variant `append_streaming_thinking_lines` for live thinking output
  (same style, no collapse toggle while streaming).

- [x] 53. Wire collapse toggle in `src/presenter.rs` / `src/tui_driver.rs`: clicking the
  collapsed thinking header line toggles `collapsed`. Mirror the existing tool-bubble
  collapse pattern. (No `T` keyboard shortcut: the TUI has no cursor/hover concept over
  chat bubbles, and a bare `T` binding would break normal typing — mouse-click only, per
  explicit user decision.)

- [x] 54. Update `docs/features.md`: document the thinking bubble, collapse behaviour,
  and that thinking-token emission is a model capability (not provider-specific) — models
  must be configured with extended-thinking / reasoning enabled to produce this output.

- [x] 55. Run `make test` + `make test-coverage`.

---

### Phase 6.8 — Rename `app_state.rs` → `tui_reducer_state.rs` / `tui_state.rs` → `tui_render_state.rs`

**Goal:** Align module file names with their actual role in the reducer/render-projection
split. `app_state.rs` is TUI-only canonical reducer state mutated by `presenter::update`;
`tui_state.rs` is a disposable, rebuilt-every-frame render projection with no reducer of
its own. Neither name communicates that today. Public API (`AppState`, `TuiState`, and all
other re-exported type names) is unchanged — this is a module-path rename only.

- [x] 56. Rename `src/app_state.rs` to `src/tui_reducer_state.rs`. Update `src/lib.rs`:
  `pub mod app_state;` → `pub mod tui_reducer_state;`; `pub use app_state::{...}` →
  `pub use tui_reducer_state::{...}` (exported type names — `AppState`, `ChatEntry`,
  `TuiMode`, etc. — are unchanged).

- [x] 57. Rename `src/tui_state.rs` to `src/tui_render_state.rs`. Update `src/lib.rs`:
  `pub mod tui_state;` → `pub mod tui_render_state;`; `pub use tui_state::TuiState;` →
  `pub use tui_render_state::TuiState;` (type name `TuiState` unchanged).

- [x] 58. Update every internal reference across `src/ui.rs`, `src/render.rs`,
  `src/tui_driver.rs`, `src/presenter.rs`, `src/agent.rs`, and the two renamed files
  themselves: `crate::app_state::` → `crate::tui_reducer_state::`; `crate::tui_state::` →
  `crate::tui_render_state::`. Covers fully-qualified call sites (e.g.
  `STREAMING_THINKING_ENTRY_INDEX`, `result_section_ids`, `TuiMode`, `ResultSection`) as
  well as `use` statements.

- [x] 59. Update module-level `//!` doc comments and intra-doc links
  (`` [`crate::app_state::AppState`] ``, `` [`crate::tui_state::TuiState`] ``) inside
  `src/tui_reducer_state.rs` and `src/tui_render_state.rs` to reference the new module
  paths and file names.

- [ ] 60. Add a new ADR in `docs/architecture-design-records/` documenting the
  reducer-state vs. render-projection split and the naming rationale: `tui_reducer_state`
  is the canonical state mutated only via `presenter::update`; `tui_render_state` is a
  disposable, rebuilt-every-frame render snapshot with no reducer of its own.

- [x] 61. Update `docs/features.md` and `docs/blueprint.md`: replace `src/app_state.rs` /
  `src/tui_state.rs` references with `src/tui_reducer_state.rs` / `src/tui_render_state.rs`.
  Leave already-completed (`[x]`) historical checklist entries elsewhere in this plan file
  that mention the old paths unchanged — they describe what was true at the time.

- [x] 62. Rename `tests/tui_state.rs` to `tests/tui_render_state.rs` for consistency (it
  imports only the public `ylva::` API, so no internal path changes are required beyond
  the file rename).

- [x] 63. Run `make test` + `make test-coverage`.

---

### Phase 6.9 — `spawn_subagent` + `ChildAgentManager` + `finish` Tool

**Goal:** One-level agent delegation; child runs concurrently with parent; parent continues
processing messages and tools while a child is active.

- [ ] 64. Implement `Role::Child` construction in `AgentFactory::create`
  (`src/agent_factory.rs`). Remove `create_returns_err_for_child_role` test (line 147 in
  `src/agent_factory.rs`, left failing since Phase 5); replace with a positive assertion.
  This must be done before any other child-agent steps (6.7–6.10) can compile. Wire
  `briefing`, `parent_cancel`, `subagent_ui_tx`, and the
  `finish_tx: mpsc::Sender<FinishResult>` from `Role::Child` fields.

- [ ] 65. Create `src/child_agent_manager.rs`:

    ```rust
    pub struct ChildAgentManager {
        handle: Option<JoinHandle<anyhow::Result<Context>>>,
        run_id: Option<Uuid>,
        finish_tx: mpsc::Sender<FinishResult>,  // sender end; AgentLoop holds receiver
        child_cancel: Option<CancellationToken>,
        queue: VecDeque<SpawnRequest>,           // unbounded FIFO; drained one-by-one
    }
    impl ChildAgentManager {
        /// AgentLoop creates the mpsc channel; passes Sender here, keeps Receiver.
        pub fn new(finish_tx: mpsc::Sender<FinishResult>) -> Self { ... }
        /// If a child is already running, enqueues the request (unbounded FIFO).
        /// Returns `SpawnStatus::Started(run_id)` or `SpawnStatus::Queued(run_id)`.
        pub fn spawn(...) -> SpawnStatus { ... }
        /// Cancel the running child and drain the queue. Called when parent stops.
        pub fn cancel_all(&mut self) { /* fire child_cancel; clear queue */ }
        pub fn is_running(&self) -> bool { ... }
    }
    ```

  `AgentLoop` creates `mpsc::channel::<FinishResult>()` at startup; passes `Sender`
  to `ChildAgentManager::new` and keeps `Receiver` in its own `select!` loop.

- [ ] 66. Create `src/agent/tools/spawn_subagent.rs`. `SpawnSubagentArgs { task, briefing,
  agent_type }`. `needs_approval` returns `UserApprovalRequired` (or `AutoApproved`
  when `--auto-approve-spawn` is set). After approval, the agent loop calls
  `child_agent_manager.spawn(...)`. The tool returns `{ "status": "spawned", "run_id": "<uuid>" }`
  when the child starts immediately, or `{ "status": "queued", "run_id": "<uuid>" }` when
  it is enqueued behind an active child. The child's `finish` result arrives later as a
  tool result message tied to this `spawn_subagent` call id (see step 47).

- [ ] 67. Create `src/agent/tools/finish.rs`. `FinishArgs { status, result, error, sources }`.
  `FinishTool::run` sends `FinishResult` via `finish_tx` and returns a success envelope.
  Child loop exits unconditionally on `ChildFinish` (exempt from iteration cap). Injected
  only into `Role::Child` registries.

- [ ] 68. Update `AgentLoop` (`src/agent.rs`):
  - Hold `child_agent_manager: Option<ChildAgentManager>` (present when
    `role = Root && agent_type.can_spawn`).
  - Add `finish_rx` arm to `tokio::select!`. On `FinishResult` received: inject the
    result as a **tool result message** (role: `tool`, tool_call_id: the original
    `spawn_subagent` call id) into the parent conversation context. Enqueue it for
    the normal ReAct loop to pick up on its next iteration — no forced re-prompt.
    If the next request is queued in `ChildAgentManager`, start it now.
  - On `AgentCommand::Stop`: call `child_agent_manager.cancel_all()` (fires child
    `CancellationToken`, drains queue); write a `child_result` JSONL record with
    `status: "cancelled"` for the running child (if any). Persistence errors are
    logged and non-fatal.
  - Add `subagent_ui_rx` arm for live child tokens → `UiUpdate`.
  - On `ToolCall("spawn_subagent")`: pause, emit `UiUpdate::ToolPending`, await
    approval, call `child_agent_manager.spawn(...)`, return
    `{ "status": "spawned"/"queued", "run_id": "<uuid>" }` immediately to the LLM.
    The parent loop continues normally.
  - Unstructured fallback: if child's `JoinHandle` completes without `finish` being called
    (timeout or max iterations — same cap as parent), extract last `Message::Assistant`
    text, wrap as `FinishResult { status: "unstructured", ... }` and inject as a tool
    result message tied to the original `spawn_subagent` call id.
  - `Role::Child`: `spawn_subagent` structurally absent from registry.
  - Parent processes user messages and runs its own tools concurrently with the child;
    no suspension of the parent ReAct loop while a child is active.

- [ ] 69. Write unit tests: `spawn` returns `Queued` when a child is already running; multiple
  queued spawns are drained in FIFO order after each child finishes; `cancel_all` fires
  child token and empties the queue; `finish_rx` receives result when child calls `finish`;
  cancelled child writes `child_result` with `status: "cancelled"`;
  unstructured fallback when `finish` never called.

- [ ] 70. Update `docs/features.md`: add child-agent delegation section.
  Update `docs/configuration.md` with `can_spawn` key.

- [ ] 71. Run `make test` + `make test-coverage`.

---

### Phase 6.10 — Drill-in TUI Conversation Model

**Goal:** Child conversation as a first-class full-screen view; spawn-bubble in parent
chat; auto-switch only on child interactive pauses.

- [ ] 72. Add `session_id` is already in `AgentLoop` after Phase 6.0 step 6. Add
  `UiUpdate` variants emitted from `ChildAgentManager` via `subagent_ui_tx`:
  `ChildAgentStarted { run_id, agent_type, task }`,
  `ChildAgentToken(String)`,
  `ChildAgentDone { status, result }` in `src/agent.rs`.

- [ ] 73. Define `ConversationView` in `src/app_state.rs`:

    ```rust
    pub enum ConversationView {
        Root,
        Child { run_id: Uuid, agent_type: String, task: String },
    }
    ```

  Add to `AppState`:
  - `active_view: ConversationView` (default `Root`)
  - `child_messages: Vec<Message>`
  - `auto_switched_to_child: bool`

- [ ] 74. Update `presenter.rs`:
  - On `ChildAgentStarted`: create a spawn-bubble entry in `root_messages` (distinct
    style: magenta, shows task + "running…"); initialize `child_messages = vec![]`.
    **Do not change `active_view`** — user stays in parent view until they click or
    until an interactive pause fires.
  - On `ChildAgentToken`: append token to `child_messages`.
  - On `ChildAgentDone`: update spawn-bubble status to done/error.
    If `active_view == Child` and `auto_switched_to_child`, switch back to `Root` and
    clear `auto_switched_to_child`.
  - On `ClarifyPending` or `ToolPending` from child when `active_view == Root`:
    set `auto_switched_to_child = true`, switch to `Child` view.
    If `active_view` is already `Child`: set `auto_switched_to_child = false` (user
    navigated manually; no auto-return).
  - When child interactive pause resolves: auto-return to `Root` only if
    `auto_switched_to_child` is `true`.
  - Child interactive pauses take precedence over any simultaneous parent pending tool:
    auto-switch to child; resume parent pending state after child resolves.

- [ ] 75. Update `render.rs`:
  - When `active_view == Child`: render `child_messages` in the main chat area.
    Add a **non-scrolling 1-row header**:
    `🔍 <agent_type>: "<task>" [← Back (Ctrl+B)]`.
  - When `active_view == Root`: render `root_messages`. Spawn-bubble has a distinct
    border (magenta); mouse-click on it sets `active_view = Child` (manual navigation,
    `auto_switched_to_child = false`). A `/agents` slash-command for keyboard-driven
    child navigation is deferred to a future phase.
  - Input box behaviour while `active_view == Child`:
    - Child paused for approval/clarify: input active; `Enter` routes to the child flow.
    - Child running (not paused): input disabled; show muted hint
      `"Waiting for child agent…"`. User input is not buffered.

- [ ] 76. Add `Ctrl+B` keybinding for "back to parent" in `presenter.rs`. Document in
  `docs/keybindings.md`.

- [ ] 77. Write presenter unit tests: spawn does not auto-switch view; interactive child pause
  auto-switches; manual navigation prevents auto-return; back-to-parent keybinding works.

- [ ] 78. Update `docs/features.md`: document drill-in model, spawn-bubble, back navigation,
  auto-switch rules. Update `docs/keybindings.md`.

- [ ] 79. Run `make test` + `make test-coverage`.

---

### Phase 6.11 — Child-Agent Persistence

**Goal:** Durable `child_start` / `child_result` records in the parent session file.

- [ ] 80. Create `src/store.rs` (Phase 6 minimal subset; full JSONL session store is Phase 7).
  Define:

    ```rust
    pub struct Store {
        /// One entry per session file, serialized via Mutex<BufWriter<File>>.
        files: Mutex<HashMap<Uuid, BufWriter<File>>>,
    }
    impl Store {
        pub fn append_child_start(&self, session_id, run_id, agent_type, task)
            -> anyhow::Result<()>
        pub fn append_child_result(&self, session_id, run_id, status, result, sources)
            -> anyhow::Result<()>
    }
    ```

  Both methods: acquire the `Mutex`; open or reuse a `BufWriter<File>` for
  `~/.ylva/sessions/<session_id>.jsonl` (create dirs if absent); append one JSONL line;
  flush. Follow blueprint §4H shapes:

    ```json
    {"type":"child_start","v":1,"id":"<run-uuid>","parent_session":"<uuid>","agent_type":"...","task":"...","created_at":"..."}
    {"type":"child_result","v":1,"id":"<run-uuid>","parent_session":"<uuid>","status":"...","result":"...","sources":[...],"completed_at":"..."}
    ```

- [ ] 81. Pass a `Arc<Store>` into `AgentLoop`. Call `append_child_start` from
  `ChildAgentManager::spawn` and `append_child_result` from the `finish_rx` handler.
  Persistence errors are logged (non-fatal); they must not crash the agent.

- [ ] 82. Write unit tests using `tempfile::tempdir()`: correct JSON shapes; two sequential calls
  produce two valid JSONL lines; single-writer correctness confirmed by re-parsing lines.

- [ ] 83. Update `docs/features.md`: document persistence records.

- [ ] 84. Run `make test` + `make test-coverage`.

---

### Phase 6.12 — Headless `--allow-child-agents`

**Goal:** Enable child spawning in headless mode with safety-flag inheritance.

- [ ] 85. Add `--allow-child-agents` boolean CLI flag in `src/main.rs` (via `clap`, default
  `false`). Add `--auto-approve-spawn` boolean flag (default `false`). Pass both through
  to `ConsoleApp` / `cli_driver`. When `--allow-child-agents` is `false`, `ChildAgentManager`
  is `None` in `AgentLoop`; `spawn_subagent` is absent from the active tool registry.
  When `--auto-approve-spawn` is `true`, `spawn_subagent::needs_approval` returns
  `AutoApproved`; children inherit this flag alongside `--auto-approve-write`.

- [ ] 86. When `--allow-child-agents` is active in headless mode, emit to stderr:
  - `Spawned child agent '<type>' for task '<task>'\n` on spawn.
  - `Queued child agent '<type>' for task '<task>'\n` when enqueued.
  - `Child agent '<type>' completed: <status>\n` on finish.
  - Safety inheritance: child agents inherit parent `--auto-approve-write` and
    `--auto-approve-spawn` flags.

- [ ] 87. Run `make test` + `make test-coverage`.

---

### Phase 6.13 — Token-Efficient Tool Schemas

**Goal:** Minimise the token cost of every tool description and argument schema sent to
the LLM. Descriptions must convey the full API contract (what the tool does, every
argument, defaults, constraints, error conditions) in the fewest possible tokens —
no prose filler, no motivational context, no examples beyond what is strictly necessary
to disambiguate.

Principles:

- Start with a tight one-line summary of what the tool does.
- List arguments inline as `arg (type, default): constraint — effect` rather than in
  full sentences.
- Replace hedging language ("you can", "this allows", "note that") with direct
  imperative or declarative statements.
- Remove all examples that duplicate information already in the type/constraint.
- Cap total description length: aim for ≤120 tokens per tool, ≤30 tokens per argument.
- [ ] 88. Audit every `fn description(&self) -> &str` implementation across all tools in
  `src/agent/tools/` and `src/agent/tools.rs`. Rewrite each to be minimal: one-liner
  summary, followed only by what cannot be inferred from the JSON schema alone
  (e.g. side-effects, error conditions, non-obvious defaults). Remove prose entirely.
- [ ] 89. Audit every `#[schemars(description = "...")]` and `///` doc-comment on
  `JsonSchema`-derived argument structs (`ReadFileArgs`, `WriteFileArgs`, `GlobArgs`,
  `BashArgs`, `SearchArgs`, `EditFileArgs`, `ClarifyArgs`, etc.). Rewrite to
  `"type — purpose; constraint"` format. Strip full sentences.
- [ ] 90. Run `make lint` to confirm no compilation errors from description changes.

---

### Phase 6.14 — Final Documentation

**Goal:** Finalise all documentation for Phase 6; ensure every feature, flag, and
ADR is recorded.

- [ ] 91. Update `docs/features.md`: finalise all WIP notes from sub-phases; add
  `--allow-child-agents` documentation; verify every Phase 6 feature is described.

- [ ] 92. Update `docs/configuration.md`: `--allow-child-agents` flag; `--auto-approve-spawn`
  flag; `can_spawn` agent-type key; `Sandbox` Phase 6 defaults (TOML config Phase 7).

- [ ] 93. Run `rumdl fmt docs/features.md docs/keybindings.md docs/configuration.md
  docs/plans/phase6-terminal-superpowers.md` and fix any findings.

- [ ] 94. Run `make test` + `make test-coverage` — full suite must pass, all new modules ≥80%.

## Implementation Hints

- **Existing `ToolRegistry`:** `src/agent/tools.rs` — `register`, `filter`, `schemas`,
  `default_registry`. Phase 6.0 adds `sandbox: Arc<RwLock<Sandbox>>` parameter.
- **Existing `Role` enum:** `src/agent.rs` lines ~445–458. `Role::Child` fields are stubs;
  Phase 6.9 step 43 fills them in.
- **`AgentFactory::create` test at line 147 of `src/agent_factory.rs`:** replaced in step 43.
- **Existing tool timeout:** `DEFAULT_TOOL_TIMEOUT_SECS = 5` in `src/agent.rs`; define
  `DEFAULT_BASH_TIMEOUT_SECS: u64 = 30` alongside it.
- **`tokio::process::Command::process_group()`:** available on tokio ≥ 1.35; lockfile
  resolves ≥ 1.53 — no `pre_exec` fallback needed.
- **New crates (not yet in `Cargo.toml`):** `nix` (Unix target only), `ignore`, `regex`,
  `similar`. `which` and `walkdir` are **not** needed.
- **`write` tool:** already fully implemented in `src/agent/tools/fs.rs` with
  `needs_approval() -> true`; not a stub. Phase 6.0 updates it to the new
  `needs_approval(&self, args) -> ApprovalCheck` signature, returning
  `UserApprovalRequired` unconditionally (writes are always destructive).
- **`Section` / `DiffSection`:** Phase 6.0 step 3. `EditCall::pending_content_sections()`
  returns `vec![Section::Diff(DiffSection { label: Some(path), content: diff })]`.
  Existing tools wrap their returns in `Section::Content(...)`.
- **`pending_content_sections()` rendering:** `tui_driver.rs` stores
  `Vec<Section>` on `AppState::pending_tool_sections` when `ToolPending` fires; `render.rs`
  pattern-matches on `Section::Diff` vs `Section::Content` for coloured vs line-numbered display.
- **`Sandbox` / `SandboxContext` (last-match wins):** full model in
  `docs/ideas/read-tool-auto-approve.md`. Phase 6 defaults: `dot_path = deny`,
  `outside_workspace = deny`; glob list empty; workspace root = cwd (no traversal).
  Phase 7 loads `[tool.*]` TOML config.
- **`search` `needs_approval`:** `AutoApproved` when `path` is `None` (cwd = inside
  workspace); `sandbox_context.check_path(path)` when `path` is `Some`.
- **`glob` `git_ignore` default:** `true` (matching `search`'s default behaviour).
  `GlobTool::needs_approval` checks only the input root path before traversal; it does
  not inspect individual result paths (listing paths is not content access).
  Returns `ToolError` immediately if both `only_files` and `only_dirs` are `true`.
  Metadata keys use the `tool_output_*` prefix: `tool_output_truncated`,
  `tool_output_total_matches`. All tools with output caps use the same prefix
  (`tool_output_truncated`, `tool_output_truncated_at_bytes`).
- **Concurrent parent + child model:** `spawn_subagent` returns
  `{ "status": "spawned"/"queued", "run_id": "..." }` immediately; the parent ReAct loop
  continues. When the child calls `finish`, `AgentLoop` injects the result as a **tool
  result message** (role: `tool`, tool_call_id: original `spawn_subagent` call id) and
  enqueues it for the normal ReAct loop — no forced re-prompt. Additional `spawn` calls
  while a child is running are enqueued in an unbounded FIFO (`VecDeque`) inside
  `ChildAgentManager`; each starts automatically when the previous child's `finish_rx`
  fires. Parent approval takes lower precedence than child approval (child shown first).
- **`ChildAgentManager` channel ownership:** `AgentLoop` creates the `mpsc` channel
  at startup; passes `Sender<FinishResult>` to `ChildAgentManager::new`; keeps
  `Receiver<FinishResult>` in its own `select!` loop. This design is forward-compatible
  with child-in-child (future phases).
- **`Store` concurrency:** `Mutex<HashMap<Uuid, BufWriter<File>>>` — one entry per session
  file; serialize all writes to the same file through the mutex.
- **`session_id: Uuid`:** added to `AgentLoop` in Phase 6.0 step 6 (`Uuid::new_v4()` in
  `new()`); used by `Store` in Phase 6.11.
- **Drill-in auto-switch rule:** TUI stays in parent view on child spawn; parent remains
  active (processes messages and tools concurrently with child); auto-switches to child
  view only when the child needs user input (approval / clarify); auto-returns to parent
  only if the switch was automatic (manual click on spawn-bubble = no auto-return).
- **`AgentCommand::AnswerClarify(String)`:** new variant; `ApproveTool` match arms unchanged.
- **Coverage gate:** `make test-coverage` fails below 80% — run after each sub-phase.
- **ADR directory:** `docs/architecture-design-records/` is currently empty. Record two
  new ADRs in Phase 6.14: one for pure-Rust `search` (no external binary dependency),
  one for `SandboxContext` design (last-match-wins, normalization without filesystem hit,
  Phase 6 hardcoded defaults). UX decisions (drill-in model, `Section` enum, `clarify`
  choices) do not warrant ADRs.
- **Blueprint references:** §4D (tools, process-group), §4E (safety/approval), §4G
  (child lifecycle), §4H (JSONL persistence), §6 (checklist at lines 2594–2631).
- **Ideas files:** `docs/ideas/glob-tool-filtering.md` (Phase 6.3) and
  `docs/ideas/read-tool-auto-approve.md` (Phase 6.0 sandbox model).
