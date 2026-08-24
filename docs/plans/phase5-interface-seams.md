# Phase 5: Interface Seams

## Context

**System:** Ylva — a Rust TUI agent-harness (`src/` crate named `ylva`).

**Goal:** Introduce the runtime interface layer described in `docs/blueprint.md §4A`
and `docs/class-diagram.mmd` so that Phase 6 child-agent work builds against the
correct seams (`AgentFactory`, `Engine`) from the start rather than against the
Phase 1 `Provider`-direct wiring.

**Motivation:** The current code wires `AgentLoop` directly to `Arc<dyn Provider>`.
Phase 6 requires child agents to be constructed via `AgentFactory::create`, which
in turn requires `AgentLoop` to depend on `Arc<dyn Engine>` (not `Arc<dyn Provider>`).
Phase 5 introduces all the missing types so Phase 6 has stable seams to build against.

**Scope of change:**

1. **`Engine` trait + `EngineImpl`** — thin wrapper around `Arc<dyn Provider>` +
   `EngineConfig`; exposes `chat()` (delegates to provider) and `config()`.
2. **`AgentLoop` migration** — replace `Arc<dyn Provider>` field with
   `Arc<dyn Engine>`; callers in `tui_driver`, `cli_driver`, and `main.rs` must
   construct an `Engine` and pass it.
3. **`AgentFactory` struct** — owns `HashMap<String, Arc<dyn Engine>>` built from
   config; single public method `create(profile, agent_type, role) -> AgentLoop`.
4. **`App` trait** — `async fn run(self) -> anyhow::Result<u8>`; `TuiApp` and
   `ConsoleApp` implement it; `main.rs` dispatches on mode.
5. **`Provider::list_models()` and `Provider::name()`** — extend the `Provider` trait
   with `list_models() -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>>` and
   `fn name(&self) -> &str`. `MockProvider` returns `"mock"` and an empty model list.
   `OpenAiProvider` stores its name at construction. Real `list_models` HTTP
   implementation is deferred to the phase that exercises the `/models` slash command.
6. **Config refactor** — introduce `EngineConfig` struct (fields: `provider`,
   `model`, `temperature: Option<f32>`, `context_length`, `api_base_url`, `api_key`);
   rename the existing env-var-based `provider_type` concept accordingly; add `ProfileConfig`
   (fields: `default_engine`, `compact_engine: Option<String>`, `agents: HashMap<String, AgentConfig>`).
7. **Child-agent construction stub** — expose `Role` enum (`Root`, `Child`) so
   Phase 6 can use `AgentFactory::create(profile, agent_type, Role::Child)`
   without a direct `AgentLoop::new()` call. Phase 5 only needs `Role::Root`
   wired end-to-end; `Role::Child` returns `Err` from `AgentFactory::create`.

**Acceptance criteria:**

- `make test` passes.
- `make test-coverage` passes (≥80% line coverage on testable files).
- `cargo clippy -- -D warnings` passes.
- `AgentLoop` no longer holds `Arc<dyn Provider>` directly; it holds `Arc<dyn Engine>`.
- `TuiApp` and `ConsoleApp` implement `App`; `main.rs` dispatches through it.
- `AgentFactory::create(profile, agent_type, Role::Root)` is the only call site
  that constructs an `AgentLoop` for the root agent.
- `Provider::list_models()` is declared on the trait; `MockProvider` returns `Ok(vec![])`.
- `Provider::name()` is declared on the trait; `MockProvider` returns `"mock"`;
  `OpenAiProvider` returns its stored name (`"ollama"` or `"openrouter"`).
- Real `OpenAiProvider::list_models()` HTTP implementation is deferred to the phase
  that introduces the `/models` slash command.

**Key constraints:**

- No new crate dependencies (`tokio`, `reqwest`, `serde` are already present).
- The binary + library split must be preserved: `main.rs` stays thin; new types
  live in `src/lib.rs`-reachable modules; integration tests in `tests/` must be
  able to reach them.
- Rust 2018+ file-per-module style: `foo.rs`, not `foo/mod.rs`.
- All public identifiers use full, descriptive names (no abbreviations).
- `EngineConfig` is the SSOT for sampling parameters; `AgentLoop` reads them via
  `engine.config()` after this phase.
- `compact_engine` is optional and resolves to `default_engine` at runtime
  (Phase 7 feature; stub only in Phase 5).
- Docs are part of the change set: every affected doc must be updated in the same
  commit.

## Implementation Plan

### Step 1 — Introduce `ModelInfo` and extend `Provider` trait

- In `src/llm.rs`, add:

  ```rust
  pub struct ModelInfo {
      pub id: String,
      pub name: Option<String>,
      pub context_length: Option<usize>,
  }
  ```

- Extend the `Provider` trait with two new methods:

  ```rust
  fn name(&self) -> &str;

  fn list_models(
      &self,
  ) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>>;
  ```

  Use `BoxFuture<'_, …>` (from the `futures` crate, already in `Cargo.toml`) to
  match the existing `Provider::chat` signature and keep `Arc<dyn Provider>` valid.
- Implement both on `MockProvider`: `name()` returns `"mock"`; `list_models()` returns `Ok(vec![])`.
- Implement on `OpenAiProvider`:
  - Add `name: String` as a new field; update `OpenAiProvider::new` to accept
    `name: String` as the first parameter. All call sites in `main.rs` pass
    `"ollama"` or `"openrouter"` explicitly.
  - `name()` returns `self.name.as_str()`.
  - `list_models()`: add `Box::pin(async { anyhow::bail!("list_models not yet implemented") })` stub.
- Export `ModelInfo` from `src/lib.rs`.
- Write unit tests in `src/llm.rs`:
  - `MockProvider::name()` returns `"mock"`.
  - `MockProvider::list_models()` returns an empty `Vec`.

### Step 2 — Introduce `EngineConfig`, `Engine` trait, and `EngineImpl`

- Create `src/engine.rs`.
- Define:

  ```rust
  pub struct EngineConfig {
      pub provider: String,
      pub model: String,
      pub temperature: Option<f32>,
      pub context_length: usize,
      pub api_base_url: Option<String>,
      pub api_key: Option<String>,
  }
  ```

  `temperature: None` means the provider uses the model's own default; `Some(v)` overrides it.

  ```rust
  pub trait Engine: Send + Sync {
      fn chat<'a>(
          &'a self,
          messages: Vec<Message>,
          tools: Vec<ToolSchema>,
          cancel: CancellationToken,
      ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>>;

      fn config(&self) -> &EngineConfig;
  }

  pub struct EngineImpl {
      provider: Arc<dyn Provider>,
      config: EngineConfig,
  }
  ```

- Add a `pub fn new(provider: Arc<dyn Provider>, config: EngineConfig) -> Self` constructor to `EngineImpl`.
- Implement `Engine` for `EngineImpl`:
  - `chat()` delegates to `self.provider.chat(messages, tools, cancel)` and returns
    its `BoxFuture` directly — no re-wrapping needed. Note: `model` and `temperature`
    from `EngineConfig` are informational in Phase 5; `OpenAiProvider` already bakes
    these in at construction time. Phase 7's factory will use them when constructing
    `OpenAiProvider` from TOML.
  - `config()` returns `&self.config`.
- Add `pub mod engine;` to `src/lib.rs` and re-export `Engine`, `EngineConfig`,
  `EngineImpl`.
- Write unit tests: confirm `EngineImpl::config()` returns the injected config;
  confirm `EngineImpl::chat()` delegates to `MockProvider`.

### Step 3 — Migrate `AgentLoop` from `Arc<dyn Provider>` to `Arc<dyn Engine>`

- In `src/agent.rs`:
  - Replace `provider: Arc<dyn Provider>` field with `engine: Arc<dyn Engine>`.
  - Update `AgentLoop::new(engine: Arc<dyn Engine>, registry: Arc<ToolRegistry>)`
    signature.
  - Replace all `self.provider.chat(…)` call sites with `self.engine.chat(…)`.
  - Remove the `use crate::llm::{Provider, …}` import if no longer needed directly.
- Update `pub use agent::{…}` re-exports in `src/lib.rs` as needed.
- Update `run_tui<B>` in `src/tui_driver.rs`:
  - Replace `provider: Arc<dyn Provider>, registry: Arc<ToolRegistry>` parameters
    with `agent: AgentLoop, ui_rx: mpsc::Receiver<UiUpdate>` — `run_tui` no longer
    constructs the `AgentLoop`; it receives a pre-built one.
  - Remove the `AgentLoop::new(…)` call at the top of `run_tui`.
  - Update `tests/tui_state.rs`: replace `Arc::new(provider), registry` arguments
    with `AgentLoop::new(Arc::new(EngineImpl::new(provider, config)), registry)`.
- Update `run_cli_to` in `src/cli_driver.rs`:
  - Replace only the `provider: Arc<dyn Provider>` parameter with
    `agent: AgentLoop, ui_rx: mpsc::Receiver<UiUpdate>`. **Keep** `registry: Arc<ToolRegistry>`
    — it is still used inside the function body for tool-title lookup and approval checks.
  - New signature:
    `run_cli_to(agent: AgentLoop, ui_rx: mpsc::Receiver<UiUpdate>,`
    `registry: Arc<ToolRegistry>, initial_prompt: String, out: &mut dyn Write)`
  - Remove the `AgentLoop::new(…)` call at the top of `run_cli_to`.
  - Update `run_cli` (public signature unchanged: still accepts `Arc<dyn Provider>` and
    `Arc<ToolRegistry>`): build `AgentLoop::new(engine, registry.clone())` in its body
    before calling `run_cli_to`; mark the site with
    `// TODO Phase 5 Step 9: replace with AgentFactory`. The `engine` is built inline
    from `EngineImpl::new(provider, config)` using a temporary `EngineConfig`.
  - Update `tests/cli_driver.rs`: replace `provider, registry` arguments with a
    pre-built `AgentLoop` and its `ui_rx`, keeping `registry` as a separate argument.
- Fix remaining compilation errors in `src/main.rs` by wrapping the existing
  `Arc<dyn Provider>` in `EngineImpl::new(provider, config)` temporarily. Mark
  each site with `// TODO Phase 5 Step 9: replace with AgentFactory`.
- Update all existing `AgentLoop` unit tests to pass an `Arc<dyn Engine>` built
  from `MockProvider` + a test `EngineConfig`.

### Step 4 — Introduce `Role` enum

- In `src/agent.rs`, define:

  ```rust
  pub enum Role {
      Root,
      Child, // fields added in Phase 6
  }
  ```

- Export `Role` from `src/lib.rs`.
- No behaviour change in Phase 5; `Role::Child` is a unit variant.

### Step 5 — Introduce `ProfileConfig` and `AgentConfig`

- Create `src/config.rs`.
- Define:

  ```rust
  pub struct AgentConfig {
      pub engine: Option<String>,
      pub allowed_tools: Option<Vec<String>>,
      pub timeout_secs: Option<u64>,
      pub persona: Option<String>,
      pub can_spawn: bool,
  }

  pub struct ProfileConfig {
      pub default_engine: String,
      pub compact_engine: Option<String>,
      pub agents: HashMap<String, AgentConfig>,
  }
  ```

- These structs are plain data; no TOML parsing yet (Phase 7 adds file I/O).
- Export both from `src/lib.rs`.
- Write a simple unit test: construct a `ProfileConfig` and verify field access.

### Step 6 — Introduce `AgentFactory`

- Create `src/agent_factory.rs`.
- Define:

  ```rust
  pub struct AgentFactory {
      engines: HashMap<String, Arc<dyn Engine>>,
  }

  impl AgentFactory {
      pub fn new(engines: HashMap<String, Arc<dyn Engine>>) -> Self { … }

      pub fn create(
          &self,
          profile: &ProfileConfig,
          agent_type: &str,
          role: Role,
      ) -> anyhow::Result<(AgentLoop, mpsc::Receiver<UiUpdate>)> { … }
  }
  ```

- `create` resolves the engine:
  1. Look up `profile.agents.get(agent_type)` → `AgentConfig.engine`.
  2. Fall back to `profile.default_engine`.
  3. Return `Err(…)` if the resolved engine name is not in `self.engines`.
- `create` builds an `AgentLoop` with the resolved engine and `default_registry()`.
  Tool filtering (`allowed_tools`) is **deferred to Phase 6** (YAGNI).
- For `Role::Child`, return `Err` via
  `anyhow::bail!("Role::Child is not yet implemented")` (Phase 6 will implement it).
  Do not use `todo!()` — it panics and aborts the test process.
- Add `pub mod agent_factory;` to `src/lib.rs` and re-export `AgentFactory`.
- Write unit tests:
  - `create` returns `Ok(…)` for `Role::Root` with a known engine name.
  - `create` returns `Err(…)` for an unknown engine name.

### Step 7 — Introduce `App` trait in `src/app.rs`

- Create `src/app.rs` with:

  ```rust
  pub trait App {
      async fn run(self) -> anyhow::Result<u8>;
  }
  ```

  The `u8` is an exit code (`0` = success). `TuiApp` and `ConsoleApp` both return `0u8`
  on clean exit; errors propagate via `Err`. `main.rs` converts the result to
  `std::process::exit`.

- Add `pub mod app;` to `src/lib.rs` and re-export `App`.

### Step 8 — Implement `TuiApp` and `ConsoleApp`

- In `src/tui_driver.rs`:
  - Add `pub struct TuiApp` implementing `App`:
    - Fields: `factory: Arc<AgentFactory>`, `profile: ProfileConfig`,
      `agent_type: String`, `explicit_prompt: Option<String>`.
    - `run(self) -> anyhow::Result<u8>` absorbs `run_tui_on_stdout` logic: sets up the
      crossterm terminal, calls `factory.create(&profile, &agent_type, Role::Root)?`
      to get `(agent, ui_rx)`, then delegates to `run_tui(&mut terminal, agent, ui_rx, …)`.
  - Demote `run_tui_on_stdout` — its body moves into `TuiApp::run()`; remove the
    standalone public function.
  - `run_tui<B>` stays `pub` and now accepts `(agent: AgentLoop, ui_rx: mpsc::Receiver<UiUpdate>)`
    (refactored in Step 3); no further changes here.
- In `src/cli_driver.rs`:
  - Add `pub struct ConsoleApp` implementing `App`:
    - Fields: `factory: Arc<AgentFactory>`, `profile: ProfileConfig`,
      `agent_type: String`, `prompt: String`,
      `writer: Box<dyn std::io::Write + Send>`.
    - `run(self) -> anyhow::Result<u8>` calls
      `factory.create(&profile, &agent_type, Role::Root)?` to get `(agent, ui_rx)`,
      then delegates to `run_cli_to(agent, ui_rx, self.prompt, &mut *self.writer)`.
  - `run_cli_to` stays `pub` and now accepts `(agent: AgentLoop, ui_rx: …)` (refactored
    in Step 3); no further changes here.
  - Update `run_cli` to construct `ConsoleApp` or build an `AgentLoop` before calling
    `run_cli_to`; do not break its public signature.
- Export `TuiApp` and `ConsoleApp` from `src/lib.rs`.

### Step 9 — Refactor `main.rs` to dispatch via `App`

- In `src/main.rs`:
  - Remove all `// TODO Phase 5 Step 9` temporary wrappers introduced in Step 3.
  - `select_provider()` no longer needs to return a name string separately —
    `provider.name()` provides it. `EngineConfig.provider` is set from `provider.name()`.
  - Construct an `EngineConfig` inline from the env-var-derived values, setting
    `provider` from `provider.name().to_string()`, `model` from the relevant env var,
    `temperature: None` (model default), and `context_length: 128_000`.
  - Wrap the provider in `EngineImpl::new(provider, engine_config)`.
  - Construct `ProfileConfig` inline:

    ```rust
    ProfileConfig {
        default_engine: "default".to_string(),
        compact_engine: None,
        agents: HashMap::from([(
            "default".to_string(),
            AgentConfig {
                engine: None, // resolves to default_engine
                allowed_tools: None,
                timeout_secs: None,
                persona: None,
                can_spawn: true,
            },
        )]),
    }
    ```

  - Wrap `AgentFactory` in `Arc` **once, before the conditional**, so it can be
    passed to both branches without a move conflict:

    ```rust
    let factory = Arc::new(AgentFactory::new(HashMap::from([
        ("default".to_string(), engine as Arc<dyn Engine>),
    ])));
    ```

  - Dispatch using the **existing boolean conditional** from `main.rs`
    (`is_terminal && !--no-tui`); do not introduce a `Mode` enum (deferred to Phase 8).
    Handle `anyhow::Result<u8>` explicitly — do not use `?` inside `block_on`:

    ```rust
    let result = runtime.block_on(async {
        if std::io::stdout().is_terminal() && !args.contains(&"--no-tui".to_string()) {
            TuiApp::new(Arc::clone(&factory), profile, "default".to_string(), explicit_prompt)
                .run().await
        } else {
            ConsoleApp::new(Arc::clone(&factory), profile, "default".to_string(), prompt,
                            Box::new(std::io::stdout()))
                .run().await
        }
    });
    match result {
        Ok(exit_code) => {
            if exit_code != 0 {
                std::process::exit(exit_code as i32);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    }
    ```

- Run `cargo build` to confirm everything compiles.

### Step 10 — Documentation updates

- Update `docs/features.md`: document `Engine`/`AgentFactory`/`App` seams as
  implemented features.
- Update `docs/configuration.md`: document `EngineConfig` and `ProfileConfig`
  fields (TOML loading comes in Phase 7; the data shape is now stable).
- Update `docs/blueprint.md` Phase 5 checklist: mark each bullet `[x]` once
  implemented. Do not alter any other phase's bullets.
- Run `rumdl fmt docs/features.md docs/configuration.md docs/blueprint.md` and
  fix any findings from `rumdl docs/`.

### Step 11 — Final verification

- Run `make test` — all tests must pass.
- Run `make test-coverage` — must remain ≥80%.
- Run `cargo clippy -- -D warnings` — zero warnings.
- Run `rumdl docs/` — zero findings.
- Commit all source and doc changes in a single commit.

## Implementation Hints

- **`run_tui<B>`** (`src/tui_driver.rs:377`) is the integration-test seam for the TUI —
  after Step 3 it accepts `(agent: AgentLoop, ui_rx: mpsc::Receiver<UiUpdate>)` instead
  of `provider` + `registry`. It stays `pub`. `run_tui_on_stdout` is removed; its logic
  moves into `TuiApp::run()`.
- **`run_cli_to`** (`src/cli_driver.rs:103`) is the integration-test seam for the CLI —
  after Step 3 its signature is:
  `(agent: AgentLoop, ui_rx: Receiver<UiUpdate>, registry: Arc<ToolRegistry>,`
  `initial_prompt: String, out: &mut dyn Write)`.
  `registry` is kept because the function body looks up tool titles and approval flags.
  It stays `pub`. Tests updated in Step 3 to pre-build `AgentLoop` and pass `registry` separately.
- **`EngineConfig.provider` string values**: populated from `provider.name()` in `main.rs`
  (`"ollama"`, `"openrouter"`, or `"mock"`). These strings are the future SSOT for
  `OpenAiProvider::list_models` endpoint dispatch.
- **`Provider::name()`**: `OpenAiProvider` stores name as a new `String` field; add
  `name: String` as the first parameter to `OpenAiProvider::new`. Update all call sites
  in `main.rs` (`"ollama"`, `"openrouter"`, `"mock"`).
- **`AgentFactory::create` for `Role::Child`**: use `anyhow::bail!` not `todo!()` — returns
  `Err` rather than panicking, which is safe in tests.
- **`BoxFuture` pattern**: `Provider::chat` already uses `BoxFuture<'a, …>` from the `futures`
  crate (`futures = "0.3"` in `Cargo.toml`). Both `Provider::list_models` and `Engine::chat`
  must use the same `BoxFuture` return type (not `impl Future`) so `Arc<dyn Provider>` and
  `Arc<dyn Engine>` remain valid. `MockProvider::list_models` implementation:
  `Box::pin(async { Ok(vec![]) })`.
- **Current `AgentLoop::new` signature** (`src/agent.rs:455`):
  `pub fn new(provider: Arc<dyn Provider>, registry: Arc<ToolRegistry>) -> (Self, mpsc::Receiver<UiUpdate>)`
  — change `provider` to `engine: Arc<dyn Engine>`.
- **`AgentLoop` internal provider call sites** (`src/agent.rs:826`, `:876`): both call
  `self.provider.chat(…)` — change to `self.engine.chat(…)`.
- **`Provider` trait** (`src/llm.rs:73`): uses `BoxFuture<'a, …>` return position
  (not `impl Future`); `MockProvider` is the test double — match its `chat()` impl
  pattern for `list_models`.
- **`OpenAiProvider`** lives in `src/llm/openai_provider.rs`. Its `list_models` implementation
  is deferred; add a `todo!()` stub to satisfy the trait bound in Phase 5.
- **`tui_driver.rs`**: search for `AgentLoop::new` to find construction sites that must
  move to `AgentFactory::create` in Step 8.
- **`cli_driver.rs`**: `run_cli` and `run_cli_to` (`src/cli_driver.rs:93`, `:103`)
  currently hold `Arc<dyn Provider>`; after Step 3 they accept a pre-built `AgentLoop`.
- **`main.rs`**: after Phase 5, `select_provider()` still produces `Arc<dyn Provider>`.
  `EngineConfig.provider` is populated from `provider.name()`. The engine is built via
  `EngineImpl::new(provider, engine_config)` with `temperature: None` and
  `context_length: 128_000`. `Arc<AgentFactory>` is created once before the `if/else`
  dispatch. `ProfileConfig` is constructed inline with `default_engine = "default"` and
  `can_spawn = true` for the default agent type.
- **`EngineImpl::new`**: `pub fn new(provider: Arc<dyn Provider>, config: EngineConfig) -> Self`.
  `model`/`temperature` in `EngineConfig` are informational in Phase 5; `OpenAiProvider`
  already bakes these in at construction time. Phase 7's factory will use them when building
  `OpenAiProvider` from TOML config.
- **`AgentFactory::create` return type**: `anyhow::Result<(AgentLoop, mpsc::Receiver<UiUpdate>)>`.
  Callers must propagate or handle the error at startup.
- **Tool filtering** (`AgentConfig.allowed_tools`): not applied in Phase 5; `default_registry()`
  is always used. Phase 6 adds the filtering when `spawn_subagent` is introduced.
- **Module layout**: `src/engine.rs`, `src/agent_factory.rs`, `src/config.rs`, `src/app.rs`
  (file-per-module, no subdirectory `mod.rs`).
- **Re-exports via `src/lib.rs`**: integration tests in `tests/` cannot reach `src/main.rs`;
  all new public types must be re-exported through `src/lib.rs`.
- **Test coverage threshold**: ≥80% line coverage enforced by `make test-coverage`;
  `ui.rs` and `main.rs` are excluded. New modules need sufficient unit tests.
- **No new crate dependencies** needed; `tokio`, `reqwest`, `serde`, `tokio-util`,
  `tokio_util::sync::CancellationToken` are already in `Cargo.toml`.
- **Markdown linter**: `rumdl` with `.rumdl.toml`; autofix with `rumdl fmt <PATH>`.
