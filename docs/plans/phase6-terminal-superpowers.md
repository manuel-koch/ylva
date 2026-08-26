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
8. `spawn_subagent` tool + `ChildAgentManager` (one child at a time, no nesting)
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

- `bash` executes shell commands; orphaned grandchildren are killed on timeout/cancel via
  `tokio::process::Command::process_group(0)` + `killpg`.
- `search` finds files and text using pure-Rust `ignore` + `regex`; no `rg` binary required.
- `edit` applies search-and-replace and line-range patches; shows a unified diff before
  approval; writes to disk only after approval is granted.
- `glob` supports `git_ignore`, `only_count`, `limit`, `only_files`, `only_dirs` flags;
  returns truncation metadata when `limit` is active.
- `clarify` shows a question banner (+ numbered choices if provided) in TUI mode; the model
  may supply up to 5 pre-populated choices; the user can pick one or type freely; `clarify`
  is absent from the `tools` array in headless mode.
- `Sandbox` enforces: deny reads/globs outside workspace root; deny dot-path access; defaults
  are hardcoded (TOML config loading deferred to Phase 7).
- `ApprovalCheck` (AUTO_APPROVED / AUTO_DENIED { reason } / USER_APPROVAL_REQUIRED) is
  returned by every `Tool::needs_approval(&self, args: &Value)` call; agent loop acts
  accordingly. The sandbox is embedded in each tool via `SandboxContext`.
- Approval modal and diff view appear for `bash`, `write`, `edit` before execution.
- `spawn_subagent` spawns exactly one child at a time; a concurrent call is queued and
  executed automatically after the current child finishes.
- `finish` terminates the child's ReAct loop; unstructured fallback activates on timeout or
  max iterations.
- `child_start` / `child_result` JSONL records are appended to the parent session file.
- Viewing a child's conversation: full-screen drill-in with non-scrolling breadcrumb header
  and "← Back (Ctrl+B)" button; parent continues processing messages and tools while a
  child is active.
- Parent chat shows a clickable spawn-bubble; clicking it navigates to the child conversation.
- TUI auto-switches to child view when the child needs user input; auto-returns to the
  previous view only if the switch was automatic (manual navigation stays).
- Child interactive pauses take precedence over parent interactive pauses.
- `--allow-child-agents` enables child spawning in headless mode (disabled by default).
- All existing tests pass; new modules meet ≥80% line-coverage threshold.
- `docs/features.md`, `docs/configuration.md`, and `docs/keybindings.md` are updated
  (`Ctrl+B` back-to-parent keybinding added in Phase 6.8).

**Key constraints:**

- Binary + library split: anything integration tests need must be re-exported via `src/lib.rs`.
- Module layout: file-per-module Rust 2018+ style (`foo.rs`, not `foo/mod.rs`).
- `main.rs` stays thin (event loop, key handling only).
- `AgentFactory::create(profile, agent_type, role)` is the single construction path.
- `Role::Child` never receives `spawn_subagent` in its `ToolRegistry` (structural enforcement).
- One child at a time; second spawn while one is active is queued (one slot); executed
  automatically when the current child's `finish_rx` fires.
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
- Safety inheritance: child agents inherit parent `--auto-approve-*` flags.
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

1. Add to `[dependencies]` in `Cargo.toml`:
   - `nix = { version = "0.29", features = ["signal", "process"] }` under
     `[target.'cfg(unix)'.dependencies]`
   - `ignore = "0.4"`
   - `regex = "1"`
   - `similar = { version = "2", features = ["text"] }`

2. Create `src/sandbox.rs`. Define `ApprovalCheck`, `SandboxRule`, `Sandbox`, and
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
   /// Evaluation: canonicalize path → walk rules in order → last match wins.
   /// Implicit zeroth verdict: UserApprovalRequired (nothing matched = ask human).
   pub struct SandboxContext {
       sandbox: Arc<RwLock<Sandbox>>,
       rules: Vec<SandboxRule>,  // named rules prepended; glob list appended
   }
   impl SandboxContext {
       /// Phase 6 defaults: dot_path = deny, outside_workspace = deny.
       pub fn new(sandbox: Arc<RwLock<Sandbox>>) -> Self { ... }
       pub fn check_path(&self, path: &Path) -> ApprovalCheck { ... }
   }
   ```

3. Introduce `Section` enum in `src/agent/tools.rs` alongside `ContentSection`:

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

4. Update `default_registry()` in `src/agent/tools.rs` to accept
   `sandbox: Arc<RwLock<Sandbox>>`. Each tool is constructed with
   `SandboxContext::new(Arc::clone(&sandbox))`.

5. Update the `Tool` trait: replace `fn needs_approval(&self) -> bool` with
   `fn needs_approval(&self, args: &Value) -> ApprovalCheck`. Default: return
   `UserApprovalRequired`. Update every existing implementor (`ReadTool`, `WriteTool`,
   `GlobTool` in `src/agent/tools/fs.rs`; `TestTool` in the unit-test module). Each
   concrete tool struct gains `sandbox_context: SandboxContext`.

6. Wire `ApprovalCheck` into the agent loop (`src/agent.rs`) and `src/cli_driver.rs`:
   - `AutoApproved` → execute immediately, no pause.
   - `AutoDenied { reason }` → send `ToolError` with the reason; no pause.
   - `UserApprovalRequired` → existing pause/`ToolPending` path (unchanged).

   Construct `Arc::new(RwLock::new(Sandbox::new()))` once in `AgentLoop::new` and
   pass to `default_registry(sandbox)`. Add `session_id: Uuid` field to `AgentLoop`,
   generated via `Uuid::new_v4()` in `new()`.

   Update every `default_registry()` call site (in `src/agent.rs`, `src/agent_factory.rs`,
   and tests) atomically in the same commit as steps 4–6.

7. Update render path for `pending_content_sections()`: in `tui_driver.rs` / `ui.rs`,
   when a tool is pending approval and `call.pending_content_sections()` returns
   `Section::Diff(...)` entries, render them with `+` lines in green and `−` lines in red
   in a scrollable overlay above the approval footer. `Section::Content(...)` entries render
   as before (line numbers + syntax highlighting). Collapse to a one-line summary if
   terminal height is below 20 rows.

8. Write unit tests:
   - `SandboxContext::check_path`: outside-workspace denied; dot-path denied; later
     `allow` glob overrides earlier deny (last-match wins); no-match → `UserApprovalRequired`.
   - `WriteTool::needs_approval`: always returns `UserApprovalRequired` regardless of path.
   - `Section` round-trips: existing tool calls still produce `Section::Content` sections.
   - Agent loop: `AutoApproved` skips pause; `AutoDenied` emits `ToolError`.

9. Run `make test` + `make test-coverage`. Fix regressions before continuing.

---

### Phase 6.1 — `bash` Tool

**Goal:** Shell command execution with process-group kill and 30-second timeout.

10. Create `src/agent/tools/bash.rs`. Define:

    ```rust
    #[derive(JsonSchema, Deserialize)]
    struct BashArgs { command: String }
    ```

    Implement `BashTool` + `BashCall`. `BashTool` embeds `sandbox_context: SandboxContext`.
    In `BashCall::run`:
    - Spawn via `tokio::process::Command::new("bash").arg("-c").arg(&command)`
      with `.process_group(0)`.
    - Capture pgid immediately after spawn via `.id()`.
    - Wrap in `tokio::time::timeout(Duration::from_secs(DEFAULT_BASH_TIMEOUT_SECS), ...)`.
    - On timeout or `CancellationToken` fire: kill the process group via
      `nix::sys::signal::killpg(Pid::from_raw(pgid as i32), Signal::SIGKILL)`.
    - Return combined stdout + stderr on success; `ToolError` on non-zero exit or timeout.
    - `needs_approval(&self, _args)` returns `UserApprovalRequired` unconditionally.
    - Define `DEFAULT_BASH_TIMEOUT_SECS: u64 = 30` alongside `DEFAULT_TOOL_TIMEOUT_SECS`.

11. Register `BashTool` in `default_registry()`.

12. Write unit tests: happy path (`echo`), timeout kills child process group, cancellation
    token kills child process group.

13. Update `docs/features.md`: add `bash` entry (approval level, timeout, process-group kill).

14. Run `make test` + `make test-coverage`.

---

### Phase 6.2 — `search` Tool (pure Rust)

**Goal:** Text search using `ignore` + `regex`; no external binary.

15. Create `src/agent/tools/search.rs`. Define:

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
    - For each file: read UTF-8 lossy, collect matches with line numbers and
      `context_lines` before/after.
    - Return results as `Section::Content(ContentSection)` entries (one per matching file).

    `needs_approval(&self, args)`:
    - If `args.path` is `Some(p)`: return `sandbox_context.check_path(p)`.
    - If `args.path` is `None` (defaults to cwd, inside workspace): return `AutoApproved`.

16. Register `SearchTool` in `default_registry()`.

17. Write unit tests: match found, no match, invalid regex, case-insensitive, context lines,
    explicit out-of-workspace path returns `AutoDenied`.

18. Update `docs/features.md`: add `search` entry.

19. Run `make test` + `make test-coverage`.

---

### Phase 6.3 — Enhanced `glob` Tool

**Goal:** Filtering flags and structured result metadata per `docs/ideas/glob-tool-filtering.md`.

20. Extend `GlobArgs` in `src/agent/tools/fs.rs`:

    ```rust
    #[serde(default = "default_true")] git_ignore: bool,  // default: true
    #[serde(default)] only_count: bool,
    #[serde(default)] only_files: bool,
    #[serde(default)] only_dirs: bool,
    #[serde(default)] limit: Option<usize>,
    ```

21. Update `GlobTool::run`:
    - When `git_ignore` is `true` (default), use `ignore::WalkBuilder`; else use `glob` crate.
    - Apply `only_files` / `only_dirs` filters.
    - Apply `limit`; set `truncated: true` + `total_matches` in metadata when capped.
    - When `only_count`: return count in metadata; content is empty string.

22. Add `fn metadata(&self) -> Option<serde_json::Value> { None }` default method to
    `ToolCallResult` trait. Override in `GlobResult` to return truncation/count metadata.
    Serialize metadata into the tool-role message alongside content.

23. Update `GlobTool::needs_approval(&self, args)` to call
    `sandbox_context.check_path(resolved_root)` on the input path root only (before
    traversal); return `UserApprovalRequired` if the root falls outside the workspace.
    Do not check individual result paths — listing paths is not content access.

24. Write unit tests for all new flags, truncation metadata, and approval check.

25. Update `docs/features.md`: update `glob` entry with new flags.

26. Run `make test` + `make test-coverage`.

---

### Phase 6.4 — `edit` Tool

**Goal:** Line-based or search-and-replace file patching; diff preview via
`pending_content_sections()`; write only after approval.

27. Create `src/agent/tools/edit.rs`. Define:

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
        LineRange { start_line: usize, end_line: usize, new_string: String },
    }
    ```

    `EditCall` is constructed with the parsed args. At construction time (before approval):
    - Read the current file content.
    - Apply the patch in memory (`ToolError` if `old_string` not found or line range
      out of bounds).
    - Compute unified diff via `similar::TextDiff`.
    - Store both `patched_content: String` and `diff: String` as fields on `EditCall`.

    `EditCall::pending_content_sections()` returns `vec![Section::Diff(DiffSection {
    label: Some(path.clone()), content: self.diff.clone() })]`.

    `EditCall::run()` writes `self.patched_content` to disk (called only after approval).

    `EditTool::needs_approval(&self, args)` calls `sandbox_context.check_path(path)`.

28. Register `EditTool` in `default_registry()`.

29. Write unit tests: `SearchReplace` happy path and diff output, not-found error,
    `LineRange` happy path and diff output, out-of-bounds error,
    `pending_content_sections()` returns `Section::Diff`.

30. Update `docs/features.md`: add `edit` entry (two modes, diff preview, approval flow).
    Remove the `write` WIP note — `write` is fully implemented and approval-gated.

31. Run `make test` + `make test-coverage`.

---

### Phase 6.5 — `clarify` Tool

**Goal:** Interactive model-to-user question; optional multiple-choice (≤5); excluded in headless.

32. Create `src/agent/tools/clarify.rs`. Define:

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
    by `TuiApp` at startup only.

33. Add `AgentCommand::AnswerClarify(String)` to `src/agent.rs`. Agent loop handles
    `clarify` calls specially:
    - Pause loop; emit `UiUpdate::ClarifyPending { question, choices }`.
    - Await `AgentCommand::AnswerClarify(answer)` or `AgentCommand::Stop`.
    - Inject the answer as the tool result; resume.

34. Add `TuiMode::ClarifyPending { question: String, choices: Option<Vec<String>> }` to
    `src/app_state.rs`. Add `DomainEvent::ClarifyPending` and handle in `presenter.rs`.

35. In `tui_driver.rs` / `render.rs`: when `TuiMode::ClarifyPending`, render a yellow
    banner with the question text. If `choices` is `Some`, render numbered items `1.`–`5.`.
    Pressing `1`–`5` populates the input field with the chosen text. `Enter` sends
    `AgentCommand::AnswerClarify(input)`.

36. Write unit tests: choice-count validation (≤5 ok, 6 → error); presenter tests for the
    `ClarifyPending` state transition.

37. Update `docs/features.md`: add `clarify` entry (banner, choices, headless exclusion).

38. Run `make test` + `make test-coverage`.

---

### Phase 6.6 — Approval UX (diff overlay in TUI)

**Goal:** Render `Section::Diff` sections from `pending_content_sections()` during tool
approval. The `ApprovalCheck` loop wiring was already done in Phase 6.0 step 6.

39. In `render.rs` / `ui.rs`: during `TuiMode::ToolApproval`, call
    `pending_call.pending_content_sections()` and render each section:
    - `Section::Diff(d)`: scrollable diff overlay above the footer — `+` lines green,
      `−` lines red, context lines default style. Collapses to a one-line summary if
      terminal height is below 20 rows.
    - `Section::Content(c)`: render as a read-only code block (existing path).
    Both types scroll with `PageUp`/`PageDown`.

40. Expose `pending_content_sections()` on the `PendingTool` state in `AppState` so
    `render.rs` can call it without holding a full `Box<dyn ToolCall>` beyond the driver.
    Add `sections: Vec<Section>` to `AppState::pending_tool_sections`, populated in
    `tui_driver.rs` when `UiUpdate::ToolPending` arrives.

41. Update `docs/features.md`: document diff overlay and section rendering.

42. Run `make test` + `make test-coverage`.

---

### Phase 6.7 — `spawn_subagent` + `ChildAgentManager` + `finish` Tool

**Goal:** One-level agent delegation; child runs concurrently with parent; parent continues
processing messages and tools while a child is active.

43. Implement `Role::Child` construction in `AgentFactory::create`
    (`src/agent_factory.rs`). Remove `create_returns_err_for_child_role` test; replace
    with a positive assertion. Wire `briefing`, `parent_cancel`, `subagent_ui_tx`, and
    the `finish_tx: mpsc::Sender<FinishResult>` from `Role::Child` fields.

44. Create `src/child_agent_manager.rs`:

    ```rust
    pub struct ChildAgentManager {
        handle: Option<JoinHandle<anyhow::Result<Context>>>,
        run_id: Option<Uuid>,
        finish_tx: mpsc::Sender<FinishResult>,  // sender end; AgentLoop holds receiver
        child_cancel: Option<CancellationToken>,
        pending_spawn: Option<SpawnRequest>,    // queued if a child is already running
    }
    impl ChildAgentManager {
        /// AgentLoop creates the mpsc channel; passes Sender here, keeps Receiver.
        pub fn new(finish_tx: mpsc::Sender<FinishResult>) -> Self { ... }
        /// If a child is already running, the request is queued (one slot only;
        /// returns Ok with queued run_id).
        /// The queued spawn executes automatically when the current child's finish_rx fires.
        pub fn spawn(...) -> Result<Uuid, ChildAgentError> { ... }
        pub fn cancel(&mut self) { /* fire child_cancel */ }
        pub fn is_running(&self) -> bool { ... }
    }
    ```

    `AgentLoop` creates `mpsc::channel::<FinishResult>()` at startup; passes `Sender`
    to `ChildAgentManager::new` and keeps `Receiver` in its own `select!` loop.

45. Create `src/agent/tools/spawn_subagent.rs`. `SpawnSubagentArgs { task, briefing,
    agent_type }`. `needs_approval` returns `UserApprovalRequired`. After approval, the
    agent loop calls `child_agent_manager.spawn(...)`, which returns a `run_id` immediately
    (child may start now or be queued). The tool returns `{ "status": "spawned",
    "run_id": "<uuid>" }` to the LLM immediately — the child's `finish` result arrives
    later as an async observation (see step 47).

46. Create `src/agent/tools/finish.rs`. `FinishArgs { status, result, error, sources }`.
    `FinishTool::run` sends `FinishResult` via `finish_tx` and returns a success envelope.
    Child loop exits unconditionally on `ChildFinish` (exempt from iteration cap). Injected
    only into `Role::Child` registries.

47. Update `AgentLoop` (`src/agent.rs`):
    - Hold `child_agent_manager: Option<ChildAgentManager>` (present when
      `role = Root && agent_type.can_spawn`).
    - Add `finish_rx` arm to `tokio::select!`. On `FinishResult` received: inject the
      result as an async observation into the parent conversation context, then re-prompt
      the parent LLM. If a pending spawn is queued in `ChildAgentManager`, start it now.
    - Add `subagent_ui_rx` arm for live child tokens → `UiUpdate`.
    - On `ToolCall("spawn_subagent")`: pause, emit `UiUpdate::ToolPending`, await
      approval, call `child_agent_manager.spawn(...)`, return `{ "status": "spawned",
      "run_id": "<uuid>" }` immediately to the LLM. The parent loop continues normally.
    - Unstructured fallback: if child's `JoinHandle` completes without `finish` being called
      (timeout or max iterations), extract last `Message::Assistant` text, wrap as
      `FinishResult { status: "unstructured", ... }` and inject as an async observation.
    - `Role::Child`: `spawn_subagent` structurally absent from registry.
    - Parent processes user messages and runs its own tools concurrently with the child;
      no suspension of the parent ReAct loop while a child is active.

48. Write unit tests: `spawn` queues second spawn when one is already running (does not
    error); queued spawn starts automatically after first child's `finish_rx` fires;
    cancellation fires child token; `finish_rx` receives result when child calls `finish`;
    unstructured fallback when `finish` never called.

49. Update `docs/features.md`: add child-agent delegation section.
    Update `docs/configuration.md` with `can_spawn` key.

50. Run `make test` + `make test-coverage`.

---

### Phase 6.8 — Drill-in TUI Conversation Model

**Goal:** Child conversation as a first-class full-screen view; spawn-bubble in parent
chat; auto-switch only on child interactive pauses.

51. Add `session_id` is already in `AgentLoop` after Phase 6.0 step 6. Add
    `UiUpdate` variants emitted from `ChildAgentManager` via `subagent_ui_tx`:
    `ChildAgentStarted { run_id, agent_type, task }`,
    `ChildAgentToken(String)`,
    `ChildAgentDone { status, result }` in `src/agent.rs`.

52. Define `ConversationView` in `src/app_state.rs`:

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

53. Update `presenter.rs`:
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

54. Update `render.rs`:
    - When `active_view == Child`: render `child_messages` in the main chat area.
      Add a **non-scrolling 1-row header**:
      `🔍 <agent_type>: "<task>" [← Back (Ctrl+B)]`.
    - When `active_view == Root`: render `root_messages`. Spawn-bubble has a distinct
      border (magenta); mouse-click on it sets `active_view = Child` (manual navigation,
      `auto_switched_to_child = false`).
    - Input box is always active; while `active_view == Child`, `Enter` routes to the
      child's approval/clarify flow, not the parent's prompt queue.

55. Add `Ctrl+B` keybinding for "back to parent" in `presenter.rs`. Document in
    `docs/keybindings.md`.

56. Write presenter unit tests: spawn does not auto-switch view; interactive child pause
    auto-switches; manual navigation prevents auto-return; back-to-parent keybinding works.

57. Update `docs/features.md`: document drill-in model, spawn-bubble, back navigation,
    auto-switch rules. Update `docs/keybindings.md`.

58. Run `make test` + `make test-coverage`.

---

### Phase 6.9 — Child-Agent Persistence

**Goal:** Durable `child_start` / `child_result` records in the parent session file.

59. Create `src/store.rs` (Phase 6 minimal subset; full JSONL session store is Phase 7).
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

60. Pass a `Arc<Store>` into `AgentLoop`. Call `append_child_start` from
    `ChildAgentManager::spawn` and `append_child_result` from the `finish_rx` handler.
    Persistence errors are logged (non-fatal); they must not crash the agent.

61. Write unit tests using `tempfile::tempdir()`: correct JSON shapes; two sequential calls
    produce two valid JSONL lines; single-writer correctness confirmed by re-parsing lines.

62. Update `docs/features.md`: document persistence records.

63. Run `make test` + `make test-coverage`.

---

### Phase 6.10 — Headless `--allow-child-agents` + Final Documentation

**Goal:** Enable child spawning in headless mode; finalise all documentation.

64. Add `--allow-child-agents` boolean CLI flag in `src/main.rs` (via `clap`, default
    `false`). Pass through to `ConsoleApp` / `cli_driver`. When `false`, `ChildAgentManager`
    is `None` in `AgentLoop`; `spawn_subagent` is absent from the active tool registry.

65. When `--allow-child-agents` is active in headless mode, emit to stderr:
    - `Spawned child agent '<type>' for task '<task>'\n` on spawn.
    - `Child agent '<type>' completed: <status>\n` on finish.
    - Safety inheritance: child agents inherit parent `--auto-approve-*` flags.

66. Update `docs/features.md`: finalise all WIP notes from sub-phases; add
    `--allow-child-agents` documentation; verify every Phase 6 feature is described.

67. Update `docs/configuration.md`: `--allow-child-agents` flag; `can_spawn` agent-type
    key; `Sandbox` Phase 6 defaults (TOML config Phase 7).

68. Run `rumdl fmt docs/features.md docs/keybindings.md docs/configuration.md
    docs/plans/phase6-terminal-superpowers.md` and fix any findings.

69. Run `make test` + `make test-coverage` — full suite must pass, all new modules ≥80%.

## Consistency Issues

**[high] `Role::Child` construction currently returns `Err`.**
The existing test `create_returns_err_for_child_role` in `src/agent_factory.rs` line 147
asserts that `AgentFactory::create` with `Role::Child` returns an error (Phase 5 left
it unimplemented). Phase 6.7 step 43 removes that test and replaces it with a positive
assertion. This must be resolved before any child-agent steps (6.7–6.9) compile.

**[low] `default_registry()` signature change.**
Phase 6.0 step 4 changes the signature to `default_registry(sandbox: Arc<RwLock<Sandbox>>)`.
Every call site (`src/agent.rs`, `src/agent_factory.rs`, tests) must be updated atomically in
the same commit to avoid compilation failures mid-refactor.

**[low] `Section` enum is a breaking change to two trait methods.**
Phase 6.0 step 3 changes `pending_content_sections()` and `content_sections()` return types
from `Vec<ContentSection>` to `Vec<Section>`. All existing tools, all rendering paths in
`tui_driver.rs` / `ui.rs`, and all test assertions on these methods must be updated in the
same commit. Mechanical wrapping `Section::Content(...)` — but easy to miss in tests.

**[low] `clarify` flow must not conflate with tool-approval flow.**
`ClarifyTool` uses `TuiMode::ClarifyPending` + `AgentCommand::AnswerClarify`; standard
tools use `TuiMode::ToolApproval` + `ApproveTool`/`DenyTool`. The agent loop must branch
on tool name before emitting the pending UiUpdate. Ensure no path sends `ToolPending` for
a `clarify` call or `ClarifyPending` for a non-clarify call.

## Implementation Hints

- **Existing `ToolRegistry`:** `src/agent/tools.rs` — `register`, `filter`, `schemas`,
  `default_registry`. Phase 6.0 adds `sandbox: Arc<RwLock<Sandbox>>` parameter.
- **Existing `Role` enum:** `src/agent.rs` lines ~445–458. `Role::Child` fields are stubs;
  Phase 6.7 step 43 fills them in.
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
- **Concurrent parent + child model:** `spawn_subagent` returns `{ "status": "spawned",
  "run_id": "..." }` immediately; the parent ReAct loop continues. When the child calls
  `finish`, `AgentLoop` injects the result as an async observation and re-prompts the
  parent LLM. A second `spawn` while a child is running is queued in
  `ChildAgentManager::pending_spawn`; it starts automatically when `finish_rx` fires.
  Parent approval takes lower precedence than child approval (child shown first).
- **`ChildAgentManager` channel ownership:** `AgentLoop` creates the `mpsc` channel
  at startup; passes `Sender<FinishResult>` to `ChildAgentManager::new`; keeps
  `Receiver<FinishResult>` in its own `select!` loop. This design is forward-compatible
  with child-in-child (future phases).
- **`Store` concurrency:** `Mutex<HashMap<Uuid, BufWriter<File>>>` — one entry per session
  file; serialize all writes to the same file through the mutex.
- **`session_id: Uuid`:** added to `AgentLoop` in Phase 6.0 step 6 (`Uuid::new_v4()` in
  `new()`); used by `Store` in Phase 6.9.
- **Drill-in auto-switch rule:** TUI stays in parent view on child spawn; parent remains
  active (processes messages and tools concurrently with child); auto-switches to child
  view only when the child needs user input (approval / clarify); auto-returns to parent
  only if the switch was automatic (manual click on spawn-bubble = no auto-return).
- **`AgentCommand::AnswerClarify(String)`:** new variant; `ApproveTool` match arms unchanged.
- **Coverage gate:** `make test-coverage` fails below 80% — run after each sub-phase.
- **ADR directory:** `docs/architecture-design-records/` is currently empty. Record a new
  ADR for decisions that deviate from the blueprint: `clarify` multiple-choice; drill-in
  model (vs. sidebar panel); pure-Rust `search`; `Section` enum; `SandboxContext` design.
- **Blueprint references:** §4D (tools, process-group), §4E (safety/approval), §4G
  (child lifecycle), §4H (JSONL persistence), §6 (checklist at lines 2594–2631).
- **Ideas files:** `docs/ideas/glob-tool-filtering.md` (Phase 6.3) and
  `docs/ideas/read-tool-auto-approve.md` (Phase 6.0 sandbox model).
