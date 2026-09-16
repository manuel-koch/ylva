// Copyright © 2026 Manuel Koch
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::Arc;

use ylva::llm::provider_registry::{ProviderEnv, ProviderRegistry, ProviderSelection};
use ylva::logging::{init_logging, LogConfig, LogFormat};
use ylva::{
    AgentFactory, App, ConsoleApp, Engine, EngineImpl, ProfileConfig, TuiApp, DEFAULT_TUI_TOOLS,
};

/// Fallback demo prompt used when neither `--prompt` nor piped stdin supplies
/// the initial user turn.
const DEFAULT_DEMO_PROMPT: &str = "What makes the sky look blue?";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliColorMode {
    Auto,
    No,
    Yes,
}

fn run_init_command(arguments: &[String]) -> Result<ylva::InitReport, String> {
    let mut workspace = false;
    let mut force = false;
    for argument in arguments {
        match argument.as_str() {
            "--workspace" => workspace = true,
            "--force" => force = true,
            other => return Err(format!("unknown init argument '{other}'")),
        }
    }

    if workspace {
        let current_directory =
            std::env::current_dir().map_err(|error| format!("cannot resolve cwd: {error}"))?;
        let workspace_root =
            ylva::discover_workspace_root(&current_directory).unwrap_or(current_directory);
        ylva::run_init_workspace(&workspace_root, force).map_err(|error| error.to_string())
    } else {
        let ylva_home = ylva::ylva_home()
            .ok_or_else(|| "cannot resolve the home directory for ~/.ylva".to_string())?;
        ylva::run_init(&ylva_home, force).map_err(|error| error.to_string())
    }
}

/// Decide whether headless CLI status lines may use ANSI colors.
///
/// This is intentionally evaluated only for the CLI path. The TUI keeps its
/// existing ratatui palette regardless of the selected CLI color mode.
fn should_use_cli_color(
    stdout_is_terminal: bool,
    color_mode: CliColorMode,
    terminal_name: Option<&str>,
    no_color_environment: bool,
) -> bool {
    match color_mode {
        CliColorMode::Yes => true,
        CliColorMode::No => false,
        CliColorMode::Auto => {
            stdout_is_terminal
                && !no_color_environment
                && !terminal_name.is_some_and(|name| name.eq_ignore_ascii_case("dumb"))
        }
    }
}

/// Parse the CLI color mode. The last `--color=<mode>` flag wins.
fn color_mode_from_args(args: &[String]) -> Result<CliColorMode, String> {
    let mut color_mode = CliColorMode::Auto;
    for argument in args {
        if argument == "--no-color" {
            return Err("--no-color was replaced by --color=no".to_string());
        }
        if argument == "--color" {
            return Err("--color requires one of: auto, no, yes (use --color=<mode>)".to_string());
        }
        let Some(mode) = argument.strip_prefix("--color=") else {
            continue;
        };
        color_mode = match mode.to_ascii_lowercase().as_str() {
            "auto" => CliColorMode::Auto,
            "no" => CliColorMode::No,
            "yes" => CliColorMode::Yes,
            _ => {
                return Err(format!(
                    "invalid --color mode '{mode}': expected auto, no, or yes"
                ));
            }
        };
    }
    Ok(color_mode)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("{}", ylva::BUILD_INFO.render());
        return;
    }

    if args.first().is_some_and(|argument| argument == "init") {
        match run_init_command(&args[1..]) {
            Ok(report) => println!("{}", report.summary()),
            Err(message) => {
                eprintln!("error: {message}");
                std::process::exit(2);
            }
        }
        return;
    }

    let cli_color_mode = match color_mode_from_args(&args) {
        Ok(color_mode) => color_mode,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };
    let log_config = match logging_from_args(&args) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };
    // The guard must outlive the whole run so buffered log events are flushed
    // on exit. Logging is opt-in: `init_logging` installs nothing and returns
    // `None` when no `--log-file` was given.
    let mut log_guard = match init_logging(log_config) {
        Ok(guard) => guard,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };

    let stdout_is_terminal = std::io::stdout().is_terminal();
    let use_tui = stdout_is_terminal && !args.contains(&"--no-tui".to_string());
    let cli_color_output = !use_tui
        && should_use_cli_color(
            stdout_is_terminal,
            cli_color_mode,
            std::env::var("TERM").ok().as_deref(),
            std::env::var_os("NO_COLOR").is_some(),
        );
    tracing::info!(
        version = ylva::BUILD_INFO.version,
        commit = ylva::BUILD_INFO.git_commit,
        built = ylva::BUILD_INFO.build_timestamp_utc,
        profile = ylva::BUILD_INFO.profile,
        "build info"
    );
    tracing::info!(app = if use_tui { "tui" } else { "cli" }, "starting");

    let workspace_root = std::env::current_dir()
        .ok()
        .and_then(|current_directory| ylva::discover_workspace_root(&current_directory));
    let ylva_home = ylva::ylva_home();
    let profile_environment =
        match load_profile_environment_for_args(&args, workspace_root.as_deref()) {
            Ok(environment) => environment,
            Err(message) => {
                exit_with_error(&mut log_guard, message);
            }
        };
    let mut profile = match load_profile(&args, &profile_environment, workspace_root.as_deref()) {
        Ok(profile) => profile,
        Err(message) => {
            exit_with_error(&mut log_guard, message);
        }
    };
    let profile_path = match selected_profile_path(&args, workspace_root.as_deref()) {
        Ok(path) => path,
        Err(message) => {
            exit_with_error(&mut log_guard, message);
        }
    };
    let root_system_prompt = match ylva::load_soul(workspace_root.as_deref()) {
        Ok(system_prompt) => system_prompt,
        Err(error) => {
            exit_with_error(&mut log_guard, error);
        }
    };
    ylva::highlight::set_active_theme(profile.theme);
    let provider_env = match load_provider_env(&profile_environment) {
        Ok(provider_env) => provider_env,
        Err(message) => {
            exit_with_error(&mut log_guard, message);
        }
    };
    apply_provider_env_to_profile(&mut profile, &provider_env);

    let runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");

    let conversation_id = match conversation_id_from_args(&args) {
        Ok(conversation_id) => conversation_id,
        Err(message) => {
            exit_with_error(&mut log_guard, message);
        }
    };

    let auto_approve_edit = args.contains(&"--auto-approve-edit".to_string());
    let auto_approve_write = args.contains(&"--auto-approve-write".to_string());
    let auto_approve_spawn = args.contains(&"--auto-approve-spawn".to_string());

    let factory_without_store = {
        // Engines come from the profile's `[engines.*]` tables when it defines
        // any; a profile that defines none (including the in-code default used
        // when no config file exists) falls back to the environment-derived
        // provider set.
        let mut factory = if profile.engines.is_empty() && profile.provider_connections.is_empty() {
            let engines = match engines_from_environment(
                &provider_env,
                &profile.default_engine,
                profile.default_model.as_deref(),
            ) {
                Ok(engines) => engines,
                Err(error) => {
                    exit_with_error(&mut log_guard, error);
                }
            };
            AgentFactory::new(engines)
        } else {
            match AgentFactory::from_profile(&profile) {
                Ok(factory) => factory,
                Err(error) => {
                    exit_with_error(
                        &mut log_guard,
                        format!(
                            "{}: {error}",
                            selected_profile_context(&args, workspace_root.as_deref())
                        ),
                    );
                }
            }
        };
        if use_tui {
            factory = factory.with_allowed_tools(DEFAULT_TUI_TOOLS);
        }
        if auto_approve_edit {
            factory = factory.with_auto_approve_edit();
        }
        if auto_approve_write {
            factory = factory.with_auto_approve_write();
        }
        if auto_approve_spawn {
            factory = factory.with_auto_approve_spawn();
        }
        factory = factory.with_system_prompt(root_system_prompt.clone());
        factory
    };

    if let Err(error) = factory_without_store.validate_profile_agent_engines(&profile) {
        exit_with_error(
            &mut log_guard,
            format!(
                "{}: {error}",
                selected_profile_context(&args, workspace_root.as_deref())
            ),
        );
    }

    let explicit_profile_name = explicit_profile_name_from_args(&args).map(str::to_string);
    let selected_profile_name = explicit_profile_name
        .clone()
        .unwrap_or_else(|| ylva::DEFAULT_PROFILE_NAME.to_string());

    let (result, session_hint) = runtime.block_on(async {
        // Resolved once, up front, so both a fresh session's `meta` header
        // and the `model_changed` record below (written on every start,
        // fresh or resumed) use the exact same provider-connection/model.
        let initial_engine_config = factory_without_store.engine_config(&profile.default_engine);
        // The store is opened inside the runtime: it spawns the writer task
        // that owns the session file.
        let store = match open_store(conversation_id, initial_engine_config.as_ref()) {
            Ok(store) => store,
            Err(message) => {
                exit_with_error(&mut log_guard, message);
            }
        };
        // Recorded on every start — fresh session and resume alike — not
        // only on a fresh session: resuming under a different `--profile` or
        // an edited config can resolve `profile.default_engine` to a
        // different provider/model than the session's history last showed,
        // and that must be visible without re-deriving it from config.
        if let Some(config) = &initial_engine_config {
            store.record_model_changed(&config.provider_connection, &config.model);
        }
        // Captured before `store` moves into the factory, so it survives to
        // print the resume hint after the app returns.
        let session_hint = Some(store.session_id());
        let factory = Arc::new(factory_without_store.with_store(store));
        let result = if use_tui {
            // The TUI starts with an empty input box. An explicit `--prompt` (or
            // piped stdin) still auto-sends, but the hardcoded demo prompt is only
            // for the headless path — it must not pre-populate the interactive UI.
            let explicit_prompt = explicit_prompt(args);
            TuiApp::new(
                Arc::clone(&factory),
                profile,
                "default".to_string(),
                explicit_prompt,
            )
            .with_display_metadata(ylva::TuiDisplayMetadata {
                profile_name: selected_profile_name.clone(),
                profile_path: profile_path.clone(),
                ylva_home: ylva_home.clone(),
                workspace_root: workspace_root.clone(),
            })
            .run()
            .await
        } else {
            let prompt = initial_prompt(args);
            ConsoleApp::new(
                Arc::clone(&factory),
                profile,
                "default".to_string(),
                prompt,
                Box::new(std::io::stdout()),
            )
            .with_color_output(cli_color_output)
            .run()
            .await
        };
        (result, session_hint)
    });
    // Printed whether the run succeeded or failed, and before any
    // `std::process::exit` below, so a user can always resume the session
    // that was just persisted (the TUI has already restored the terminal by
    // this point; the hint goes to stderr alongside every other status line).
    if let Some(session_id) = session_hint {
        print_resume_hint(session_id, explicit_profile_name.as_deref());
    }
    match result {
        Ok(exit_code) => {
            if exit_code != 0 {
                drop(log_guard.take());
                std::process::exit(exit_code as i32);
            }
        }
        Err(error) => {
            exit_with_error(&mut log_guard, error);
        }
    }
}

/// Report a fatal error after logging has been initialized.
///
/// `std::process::exit` skips destructors, so explicitly dropping the
/// non-blocking log guard is required before exiting or the diagnostic can
/// remain buffered and never reach the selected logfile.
fn exit_with_error(log_guard: &mut Option<ylva::LogGuards>, message: impl std::fmt::Display) -> ! {
    let message = message.to_string();
    tracing::error!(%message, "fatal error");
    eprintln!("error: {message}");
    drop(log_guard.take());
    std::process::exit(2);
}

/// Print the session id and a copy-pasteable command that resumes it.
fn print_resume_hint(session_id: uuid::Uuid, explicit_profile_name: Option<&str>) {
    eprintln!("{}", resume_hint_text(session_id, explicit_profile_name));
}

/// Pure core of [`print_resume_hint`], kept separate so its exact wording is
/// unit-testable without capturing stderr.
///
/// `--profile <name>` is included only when it was given explicitly on this
/// run: the implicit default profile needs no flag to resume with the same
/// configuration, and echoing a name the user never typed would be
/// misleading.
fn resume_hint_text(session_id: uuid::Uuid, explicit_profile_name: Option<&str>) -> String {
    let mut hint =
        format!("\nSession: {session_id}\nResume with: ylva --conversation-id {session_id}");
    if let Some(profile_name) = explicit_profile_name {
        hint.push_str(&format!(" --profile {profile_name}"));
    }
    hint
}

/// Load the runtime profile, or exit-worthy error text.
///
/// An explicitly named profile (`--profile <name>`) with no config file is a
/// hard error: the user asked for that specific profile, so silently
/// substituting the in-code default would hide a real typo/misconfiguration.
/// Only the implicit default profile (no `--profile` flag at all) tolerates a
/// missing file, so Ylva still runs out of the box without ever writing one.
fn load_profile(
    args: &[String],
    environment: &std::collections::HashMap<String, String>,
    workspace_root: Option<&std::path::Path>,
) -> Result<ProfileConfig, String> {
    match explicit_profile_name_from_args(args) {
        Some(profile_name) => {
            let profiles_root = ylva::default_profiles_root().map_err(|error| error.to_string())?;
            ProfileConfig::load_with_environment_at_workspace_root(
                &profiles_root,
                profile_name,
                environment,
                workspace_root,
            )
            .map_err(|error| error.to_string())
        }
        None => {
            let profiles_root = match ylva::default_profiles_root() {
                Ok(profiles_root) => profiles_root,
                Err(error) => {
                    tracing::warn!(%error, "no home directory; using the default profile");
                    return Ok(ProfileConfig::default());
                }
            };
            ProfileConfig::load_or_default_with_environment_at_workspace_root(
                &profiles_root,
                ylva::DEFAULT_PROFILE_NAME,
                environment,
                workspace_root,
            )
            .map_err(|error| error.to_string())
        }
    }
}

/// The profile name explicitly requested via `--profile <name>`, if any. This
/// is what distinguishes a hard error (the user asked for a specific,
/// missing profile) from the tolerant implicit-default fallback in
/// [`load_profile`]: with no `--profile` flag, `None` is returned, not the
/// resolved default profile name.
fn explicit_profile_name_from_args(args: &[String]) -> Option<&str> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--profile" {
            if let Some(value) = iter.next() {
                return Some(value);
            }
        }
    }
    None
}

/// Describe the profile file whose configuration is being materialized.
///
/// `ProfileConfig` intentionally remains independent of its on-disk source,
/// so startup adds this context when a later profile-construction step fails.
fn selected_profile_context(args: &[String], workspace_root: Option<&std::path::Path>) -> String {
    let profile_name = explicit_profile_name_from_args(args).unwrap_or(ylva::DEFAULT_PROFILE_NAME);
    match selected_profile_path(args, workspace_root) {
        Ok(Some(path)) => format!("profile '{profile_name}' at {}", path.display()),
        Ok(None) => format!("profile '{profile_name}' (built-in default)"),
        Err(error) => format!("profile '{profile_name}' (config path unavailable: {error})"),
    }
}

/// Resolve the profile config path selected by the same layering used to load
/// the active profile.
fn selected_profile_path(
    args: &[String],
    workspace_root: Option<&std::path::Path>,
) -> Result<Option<std::path::PathBuf>, String> {
    let profile_name = explicit_profile_name_from_args(args).unwrap_or(ylva::DEFAULT_PROFILE_NAME);
    let profiles_root = match ylva::default_profiles_root() {
        Ok(profiles_root) => profiles_root,
        Err(error) if explicit_profile_name_from_args(args).is_some() => {
            return Err(error.to_string());
        }
        Err(_) => return Ok(None),
    };
    ylva::profile_config_source_path(&profiles_root, profile_name, workspace_root)
        .map_err(|error| error.to_string())
}

/// Load the merged environment belonging to the selected profile.
///
/// Missing `.env` files are normal; malformed or unusable files are startup
/// errors. Process environment values take precedence over profile-file
/// values.
fn load_profile_environment_for_args(
    args: &[String],
    workspace_root: Option<&std::path::Path>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let profile_name = explicit_profile_name_from_args(args).unwrap_or(ylva::DEFAULT_PROFILE_NAME);
    let profile_env_path = match ylva::default_profiles_root() {
        Ok(root) => Some(ylva::profile_env_path(&root, profile_name)),
        Err(error) if explicit_profile_name_from_args(args).is_some() => {
            return Err(format!("cannot resolve profile environment: {error}"));
        }
        Err(error) => {
            tracing::warn!(%error, "no home directory; using process environment");
            None
        }
    };
    ylva::load_profile_environment_at_workspace_root(
        profile_env_path.as_deref(),
        workspace_root,
        profile_name,
    )
    .map_err(|error| format!("cannot load profile environment: {error}"))
}

fn load_provider_env(
    environment: &std::collections::HashMap<String, String>,
) -> Result<ProviderEnv, String> {
    Ok(ProviderEnv::from_values(environment))
}

/// Supply missing API keys for explicitly configured connections and legacy
/// flat engines from the selected profile environment. Explicit values in
/// `config.toml` remain authoritative.
fn apply_provider_env_to_profile(profile: &mut ProfileConfig, provider_env: &ProviderEnv) {
    for provider_config in profile.provider_connections.values_mut() {
        provider_env.apply_missing_api_key_to_provider(provider_config);
    }
    for engine_config in profile.engines.values_mut() {
        if !profile
            .provider_connections
            .contains_key(&engine_config.provider_connection)
        {
            provider_env.apply_missing_api_key(engine_config);
        }
    }
}

/// Build one engine for every provider configured by the environment.
///
/// The first provider retains the profile's default engine name and therefore
/// remains the initial active engine. Additional providers use their provider
/// names as engine names so the runtime router can switch to them.
fn engines_from_environment(
    environment: &ProviderEnv,
    default_engine_name: &str,
    default_model: Option<&str>,
) -> anyhow::Result<HashMap<String, Arc<dyn Engine>>> {
    let selections = ProviderRegistry::all_from_environment(environment);
    let mut engines = HashMap::new();
    for (index, selection) in selections.into_iter().enumerate() {
        let provider_connection = selection.provider_connection().to_string();
        let mut engine_name = if index == 0 {
            default_engine_name.to_string()
        } else {
            provider_connection.clone()
        };
        if engines.contains_key(&engine_name) {
            engine_name = format!("{provider_connection}-{index}");
        }

        log_provider_selection(&selection);
        let engine_config = selection.engine_config(default_model);
        let provider = ProviderRegistry::provider_from_environment(selection)?;
        engines.insert(
            engine_name,
            Arc::new(EngineImpl::new(provider, engine_config)) as Arc<dyn Engine>,
        );
    }
    Ok(engines)
}

/// Open the session file this run appends to.
///
/// With `--conversation-id <uuid>`, the existing session is re-attached for
/// append; an unknown id is a hard error, never a silent fresh-session
/// fallback. Without the flag, a brand-new session is started — Ylva never
/// auto-resumes and never shows a startup picker.
///
/// Fails when the Ylva home cannot be resolved or the session file cannot be
/// created or resumed. Persistence is mandatory for runtime sessions; there
/// is no transient, non-persisted mode.
fn open_store(
    conversation_id: Option<uuid::Uuid>,
    initial_engine_config: Option<&ylva::EngineConfig>,
) -> Result<ylva::Store, String> {
    let chats_root =
        ylva::default_chats_root().map_err(|error| format!("cannot resolve Ylva home: {error}"))?;
    let cwd = std::env::current_dir().map_err(|error| format!("cannot resolve cwd: {error}"))?;
    let store = match conversation_id {
        Some(conversation_id) => ylva::Store::resume(&chats_root, &cwd, conversation_id)
            .map_err(|error| error.to_string())?,
        None => {
            let (provider_connection, model) = initial_engine_config
                .map(|config| (config.provider_connection.as_str(), config.model.as_str()))
                .unwrap_or(("", ""));
            ylva::Store::create(
                &chats_root,
                &cwd,
                uuid::Uuid::new_v4(),
                provider_connection,
                model,
            )
            .map_err(|error| error.to_string())?
        }
    };
    tracing::info!(
        session_id = %store.session_id(),
        path = %store.path().display(),
        resumed = conversation_id.is_some(),
        "session store opened"
    );
    Ok(store)
}

/// Extract and validate `--conversation-id <uuid>`.
///
/// A missing value or an unparseable uuid is a hard error: silently starting
/// a fresh session would hide the user's intent to continue a specific one.
fn conversation_id_from_args(args: &[String]) -> Result<Option<uuid::Uuid>, String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--conversation-id" {
            let value = iter
                .next()
                .ok_or_else(|| "--conversation-id requires a session uuid".to_string())?;
            return uuid::Uuid::parse_str(value)
                .map(Some)
                .map_err(|error| format!("invalid --conversation-id '{value}': {error}"));
        }
    }
    Ok(None)
}

/// The explicitly supplied initial user turn, if any: `--prompt <text>`, else
/// piped stdin read to EOF. Returns `None` when neither was given (or stdin is
/// a terminal / whitespace-only). Used by the TUI, which starts with an empty
/// input box unless the user supplied a turn.
fn explicit_prompt(args: Vec<String>) -> Option<String> {
    let prompt = prompt_from_arg(&args).or_else(prompt_from_stdin)?;
    let trimmed = prompt.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// The initial user turn sent to the agent in headless mode, resolved by
/// precedence: `--prompt <text>`, else piped stdin read to EOF, else the
/// hardcoded demo prompt. stdin is only consumed when it is **not** a terminal
/// (something is piped in); no interactive prompt-read is attempted in the
/// CLI-only phase. The resolved prompt is trimmed (leading/trailing whitespace
/// removed) before it is echoed and sent.
fn initial_prompt(args: Vec<String>) -> String {
    explicit_prompt(args).unwrap_or_else(|| DEFAULT_DEMO_PROMPT.to_string())
}

/// Extract the value of the first `--prompt <text>` argument, if any.
fn prompt_from_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--prompt" {
            return iter.next().cloned();
        }
    }
    None
}

/// Build the logging configuration from the CLI arguments, parsing
/// `--log-file <path>`, `--log-level <level>`, and `--log-format <format>`.
///
/// Missing flags fall back to the `LogConfig` defaults (no file, `info` level,
/// `text` format). An unknown `--log-level` or `--log-format` value is an
/// error. Values are matched case-insensitively (`"iNFo"`, `"TEXT"` are
/// accepted); the canonical forms are lowercase. All other arguments are
/// ignored here.
fn logging_from_args(args: &[String]) -> Result<LogConfig, String> {
    let mut log_file = None;
    let mut log_level = None;
    let mut log_format = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--log-file" => {
                log_file = Some(
                    iter.next()
                        .ok_or("--log-file requires a path value")?
                        .into(),
                );
            }
            "--log-level" => {
                log_level = Some(
                    iter.next()
                        .ok_or("--log-level requires a value")?
                        .to_string(),
                );
            }
            "--log-format" => {
                log_format = Some(
                    iter.next()
                        .ok_or("--log-format requires a value")?
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    let log_level = match log_level.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => tracing::Level::INFO,
        Some("trace") => tracing::Level::TRACE,
        Some("debug") => tracing::Level::DEBUG,
        Some("info") => tracing::Level::INFO,
        Some("warn") => tracing::Level::WARN,
        Some("error") => tracing::Level::ERROR,
        Some(other) => {
            return Err(format!(
                "invalid --log-level '{other}': expected trace, debug, info, warn, or error"
            ));
        }
    };
    let log_format = match log_format
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None => LogFormat::Text,
        Some("text") => LogFormat::Text,
        Some("json") => LogFormat::Json,
        Some(other) => {
            return Err(format!(
                "invalid --log-format '{other}': expected text or json"
            ));
        }
    };
    Ok(LogConfig {
        log_file,
        log_level,
        log_format,
    })
}

/// Read all of stdin when it is not a terminal. Returns `None` when stdin is a
/// terminal (no piped input to consume) or contains only whitespace.
fn prompt_from_stdin() -> Option<String> {
    use std::io::Read;
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut buffer = String::new();
    std::io::stdin().read_to_string(&mut buffer).ok()?;
    if buffer.trim().is_empty() {
        return None;
    }
    Some(buffer)
}

/// Log provider wiring without ever logging secret values.
fn log_provider_selection(selection: &ProviderSelection) {
    match selection {
        ProviderSelection::OpenAiCompatible {
            provider_connection,
            base_url,
            model,
            api_key,
        } => {
            tracing::debug!(
                provider_connection,
                base_url,
                model,
                api_key = if api_key.is_some() { "set" } else { "unset" },
                "provider selected"
            );
        }
        ProviderSelection::Mock => {
            tracing::debug!(
                provider_connection = "mock",
                base_url = "-",
                model = "-",
                api_key = "unset",
                "provider selected"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_profile_name_from_args_is_none_without_a_profile_flag() {
        let args = vec![
            "--no-tui".to_string(),
            "--prompt".to_string(),
            "hi".to_string(),
        ];
        assert_eq!(explicit_profile_name_from_args(&args), None);
    }

    #[test]
    fn explicit_profile_name_from_args_returns_the_flags_value() {
        let args = vec!["--profile".to_string(), "work".to_string()];
        assert_eq!(explicit_profile_name_from_args(&args), Some("work"));
    }

    #[test]
    fn explicit_profile_name_from_args_is_none_when_the_flag_has_no_value() {
        let args = vec!["--profile".to_string()];
        assert_eq!(explicit_profile_name_from_args(&args), None);
    }

    #[test]
    fn profile_environment_fills_only_missing_explicit_engine_api_keys() {
        let mut profile = ProfileConfig::default();
        profile.engines.insert(
            "local".to_string(),
            ylva::EngineConfig {
                provider_connection: "ollama".to_string(),
                model: "local".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: Some("http://localhost:11434".to_string()),
                api_key: None,
                reasoning: None,
            },
        );
        profile.engines.insert(
            "hosted".to_string(),
            ylva::EngineConfig {
                provider_connection: "openrouter".to_string(),
                model: "hosted/model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: Some("https://openrouter.ai".to_string()),
                api_key: Some("explicit-key".to_string()),
                reasoning: None,
            },
        );
        let provider_env = ProviderEnv {
            ollama_api_key: Some("from-dotenv".to_string()),
            openrouter_api_key: Some("ignored".to_string()),
            ..Default::default()
        };

        apply_provider_env_to_profile(&mut profile, &provider_env);

        assert_eq!(
            profile.engines["local"].api_key.as_deref(),
            Some("from-dotenv")
        );
        assert_eq!(
            profile.engines["hosted"].api_key.as_deref(),
            Some("explicit-key")
        );
    }

    #[test]
    fn environment_builds_one_engine_for_each_configured_provider() {
        let provider_env = ProviderEnv {
            ollama_base_url: Some("http://localhost:11434".to_string()),
            ollama_model: Some("local-model".to_string()),
            openrouter_api_key: Some("test-key".to_string()),
            openrouter_model: Some("hosted/model".to_string()),
            ..Default::default()
        };

        let engines =
            engines_from_environment(&provider_env, "default", None).expect("engines build");

        assert_eq!(engines.len(), 2);
        assert_eq!(engines["default"].config().provider_connection, "ollama");
        assert_eq!(engines["default"].config().model, "local-model");
        assert_eq!(
            engines["openrouter"].config().provider_connection,
            "openrouter"
        );
        assert_eq!(engines["openrouter"].config().model, "hosted/model");
    }

    #[test]
    fn resume_hint_text_without_a_profile_names_only_the_conversation_id() {
        let session_id = uuid::Uuid::nil();
        let hint = resume_hint_text(session_id, None);
        assert_eq!(
            hint,
            format!("\nSession: {session_id}\nResume with: ylva --conversation-id {session_id}")
        );
    }

    #[test]
    fn resume_hint_text_with_an_explicit_profile_includes_it() {
        let session_id = uuid::Uuid::nil();
        let hint = resume_hint_text(session_id, Some("work"));
        assert_eq!(
            hint,
            format!(
                "\nSession: {session_id}\nResume with: ylva --conversation-id {session_id} --profile work"
            )
        );
    }

    #[test]
    fn logging_from_args_accepts_mixed_case_values() {
        let args = vec![
            "--log-level".to_string(),
            "iNFo".to_string(),
            "--log-format".to_string(),
            "Text".to_string(),
        ];
        let config = logging_from_args(&args).expect("mixed-case values accepted");
        assert_eq!(config.log_level, tracing::Level::INFO);
        assert_eq!(config.log_format, LogFormat::Text);
    }

    #[test]
    fn logging_from_args_accepts_all_levels_case_insensitively() {
        for (raw, level) in [
            ("Trace", tracing::Level::TRACE),
            ("DEBUG", tracing::Level::DEBUG),
            ("InFo", tracing::Level::INFO),
            ("Warn", tracing::Level::WARN),
            ("Error", tracing::Level::ERROR),
        ] {
            let config = logging_from_args(&["--log-level".to_string(), raw.to_string()])
                .expect("valid level");
            assert_eq!(config.log_level, level, "for input {raw:?}");
        }
    }

    #[test]
    fn logging_from_args_accepts_json_case_insensitively() {
        let config = logging_from_args(&["--log-format".to_string(), "JSON".to_string()])
            .expect("valid format");
        assert_eq!(config.log_format, LogFormat::Json);
    }

    #[test]
    fn logging_from_args_rejects_unknown_values() {
        let bad_level = logging_from_args(&["--log-level".to_string(), "verbose".to_string()]);
        assert!(bad_level.is_err());
        let bad_format = logging_from_args(&["--log-format".to_string(), "xml".to_string()]);
        assert!(bad_format.is_err());
    }

    #[test]
    fn conversation_id_is_absent_without_the_flag() {
        assert_eq!(
            conversation_id_from_args(&["--no-tui".to_string()]).expect("no flag is not an error"),
            None
        );
    }

    #[test]
    fn conversation_id_is_parsed_from_the_flag() {
        let session_id = uuid::Uuid::new_v4();
        let parsed =
            conversation_id_from_args(&["--conversation-id".to_string(), session_id.to_string()])
                .expect("a valid uuid parses");
        assert_eq!(parsed, Some(session_id));
    }

    #[test]
    fn conversation_id_without_a_value_is_an_error() {
        let error = conversation_id_from_args(&["--conversation-id".to_string()])
            .expect_err("a missing value must not be silently ignored");
        assert!(error.contains("requires a session uuid"), "{error}");
    }

    #[test]
    fn conversation_id_that_is_not_a_uuid_is_an_error() {
        let error =
            conversation_id_from_args(&["--conversation-id".to_string(), "nonsense".to_string()])
                .expect_err("an unparseable id must not start a fresh session");
        assert!(error.contains("invalid --conversation-id"), "{error}");
    }

    #[test]
    fn prompt_from_arg_reads_value_after_flag() {
        assert_eq!(
            prompt_from_arg(&["--prompt".to_string(), "hello".to_string()]),
            Some("hello".to_string())
        );
        assert_eq!(prompt_from_arg(&["--log-file".to_string()]), None);
    }

    #[test]
    fn initial_prompt_prefers_arg_over_default() {
        assert_eq!(
            initial_prompt(vec!["--prompt".to_string(), "  hi  ".to_string()]),
            "hi"
        );
    }

    #[test]
    fn explicit_prompt_is_none_without_user_turn() {
        assert_eq!(explicit_prompt(Vec::new()), None);
        assert_eq!(explicit_prompt(vec!["--log-file".to_string()]), None);
    }

    #[test]
    fn explicit_prompt_returns_trimmed_arg() {
        assert_eq!(
            explicit_prompt(vec!["--prompt".to_string(), "  hi  ".to_string()]),
            Some("hi".to_string())
        );
    }

    #[test]
    fn color_mode_defaults_to_auto_and_parses_supported_modes() {
        assert_eq!(color_mode_from_args(&[]), Ok(CliColorMode::Auto));
        assert_eq!(
            color_mode_from_args(&["--color=auto".to_string()]),
            Ok(CliColorMode::Auto)
        );
        assert_eq!(
            color_mode_from_args(&["--color=no".to_string()]),
            Ok(CliColorMode::No)
        );
        assert_eq!(
            color_mode_from_args(&["--color=yes".to_string()]),
            Ok(CliColorMode::Yes)
        );
        assert_eq!(
            color_mode_from_args(&["--color=NO".to_string()]),
            Ok(CliColorMode::No)
        );
    }

    #[test]
    fn color_mode_rejects_removed_and_invalid_syntax() {
        assert!(color_mode_from_args(&["--no-color".to_string()])
            .expect_err("removed flag should be rejected")
            .contains("--no-color was replaced by --color=no"));
        assert!(color_mode_from_args(&["--color".to_string()])
            .expect_err("missing mode should be rejected")
            .contains("use --color=<mode>"));
        assert!(color_mode_from_args(&["--color=maybe".to_string()])
            .expect_err("unknown mode should be rejected")
            .contains("expected auto, no, or yes"));
    }

    #[test]
    fn later_color_mode_flags_override_earlier_modes() {
        assert_eq!(
            color_mode_from_args(&["--color=no".to_string(), "--color=yes".to_string()]),
            Ok(CliColorMode::Yes)
        );
    }

    #[test]
    fn cli_colors_follow_the_selected_mode_and_environment() {
        assert!(should_use_cli_color(
            true,
            CliColorMode::Auto,
            Some("xterm-256color"),
            false
        ));
        assert!(!should_use_cli_color(
            false,
            CliColorMode::Auto,
            Some("xterm-256color"),
            false
        ));
        assert!(!should_use_cli_color(
            true,
            CliColorMode::No,
            Some("xterm-256color"),
            false
        ));
        assert!(!should_use_cli_color(
            true,
            CliColorMode::Auto,
            Some("dumb"),
            false
        ));
        assert!(!should_use_cli_color(
            true,
            CliColorMode::Auto,
            Some("xterm-256color"),
            true
        ));
        assert!(should_use_cli_color(
            false,
            CliColorMode::Yes,
            Some("dumb"),
            true
        ));
    }
}
