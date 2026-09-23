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

use clap::Parser;

use ylva::llm::provider_registry::{ProviderEnv, ProviderRegistry, ProviderSelection};
use ylva::logging::init_logging;
use ylva::{
    AgentFactory, App, Cli, CliColorMode, CliCommand, ConsoleApp, Engine, EngineImpl,
    ProfileConfig, TuiApp, DEFAULT_TUI_TOOLS,
};

/// Fallback demo prompt used when neither `--prompt` nor piped stdin supplies
/// the initial user turn.
const DEFAULT_DEMO_PROMPT: &str = "What makes the sky look blue?";

fn run_init_command(workspace: bool, force: bool) -> Result<ylva::InitReport, String> {
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

fn main() {
    // `--version` pre-empts everything, including parse errors: the rich
    // BUILD_INFO rendering replaces clap's own version flag
    // (`disable_version_flag` on `Cli`).
    if std::env::args()
        .skip(1)
        .any(|argument| argument == "--version")
    {
        println!("{}", ylva::BUILD_INFO.render());
        return;
    }

    // Parse errors exit before logging is initialised, so there is no log
    // guard to flush on this path — mirroring the historical color/logging
    // validation order.
    let cli = match Cli::try_parse_from(std::env::args()) {
        Ok(cli) => cli,
        Err(error) if error.use_stderr() => {
            eprintln!("{error}");
            std::process::exit(2);
        }
        Err(error) => {
            print!("{error}");
            std::process::exit(0);
        }
    };

    if let Some(CliCommand::Init { workspace, force }) = &cli.command {
        match run_init_command(*workspace, *force) {
            Ok(report) => println!("{}", report.summary()),
            Err(message) => {
                eprintln!("error: {message}");
                std::process::exit(2);
            }
        }
        return;
    }

    let cli_color_mode = match cli.resolved_color_mode() {
        Ok(color_mode) => color_mode,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };
    let log_config = cli.log_config();
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
    let use_tui = stdout_is_terminal && !cli.no_tui;
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
    let explicit_profile_name = cli.profile.as_deref();
    let profile_environment =
        match load_selected_profile_environment(explicit_profile_name, workspace_root.as_deref()) {
            Ok(environment) => environment,
            Err(message) => {
                exit_with_error(&mut log_guard, message);
            }
        };
    let mut profile = match load_profile(
        explicit_profile_name,
        &profile_environment,
        workspace_root.as_deref(),
    ) {
        Ok(profile) => profile,
        Err(message) => {
            exit_with_error(&mut log_guard, message);
        }
    };
    let profile_path = match selected_profile_path(explicit_profile_name, workspace_root.as_deref())
    {
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

    let conversation_id = cli.conversation_id;

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
                            selected_profile_context(
                                explicit_profile_name,
                                workspace_root.as_deref()
                            )
                        ),
                    );
                }
            }
        };
        if use_tui {
            factory = factory.with_allowed_tools(DEFAULT_TUI_TOOLS);
        }
        for tool in cli.auto_approve_tools() {
            factory = factory.with_auto_approve(tool);
        }
        factory = factory.with_system_prompt(root_system_prompt.clone());
        factory
    };

    if let Err(error) = factory_without_store.validate_profile_agent_engines(&profile) {
        exit_with_error(
            &mut log_guard,
            format!(
                "{}: {error}",
                selected_profile_context(explicit_profile_name, workspace_root.as_deref())
            ),
        );
    }

    let explicit_profile_name = explicit_profile_name.map(str::to_string);
    let selected_profile_name = explicit_profile_name
        .clone()
        .unwrap_or_else(|| ylva::DEFAULT_PROFILE_NAME.to_string());
    let prompt_from_flag = cli.prompt.clone();

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
            let explicit_prompt = explicit_prompt(prompt_from_flag.clone());
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
            let prompt = initial_prompt(prompt_from_flag);
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
    explicit_profile_name: Option<&str>,
    environment: &std::collections::HashMap<String, String>,
    workspace_root: Option<&std::path::Path>,
) -> Result<ProfileConfig, String> {
    match explicit_profile_name {
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

/// Describe the profile file whose configuration is being materialized.
///
/// `ProfileConfig` intentionally remains independent of its on-disk source,
/// so startup adds this context when a later profile-construction step fails.
fn selected_profile_context(
    explicit_profile_name: Option<&str>,
    workspace_root: Option<&std::path::Path>,
) -> String {
    let profile_name = explicit_profile_name.unwrap_or(ylva::DEFAULT_PROFILE_NAME);
    match selected_profile_path(explicit_profile_name, workspace_root) {
        Ok(Some(path)) => format!("profile '{profile_name}' at {}", path.display()),
        Ok(None) => format!("profile '{profile_name}' (built-in default)"),
        Err(error) => format!("profile '{profile_name}' (config path unavailable: {error})"),
    }
}

/// Resolve the profile config path selected by the same layering used to load
/// the active profile.
fn selected_profile_path(
    explicit_profile_name: Option<&str>,
    workspace_root: Option<&std::path::Path>,
) -> Result<Option<std::path::PathBuf>, String> {
    let profile_name = explicit_profile_name.unwrap_or(ylva::DEFAULT_PROFILE_NAME);
    let profiles_root = match ylva::default_profiles_root() {
        Ok(profiles_root) => profiles_root,
        Err(error) if explicit_profile_name.is_some() => {
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
fn load_selected_profile_environment(
    explicit_profile_name: Option<&str>,
    workspace_root: Option<&std::path::Path>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let profile_name = explicit_profile_name.unwrap_or(ylva::DEFAULT_PROFILE_NAME);
    let profile_env_path = match ylva::default_profiles_root() {
        Ok(root) => Some(ylva::profile_env_path(&root, profile_name)),
        Err(error) if explicit_profile_name.is_some() => {
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

/// The explicitly supplied initial user turn, if any: `--prompt <text>`, else
/// piped stdin read to EOF. Returns `None` when neither was given (or stdin is
/// a terminal / whitespace-only). Used by the TUI, which starts with an empty
/// input box unless the user supplied a turn.
fn explicit_prompt(prompt_from_flag: Option<String>) -> Option<String> {
    let prompt = prompt_from_flag.or_else(prompt_from_stdin)?;
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
fn initial_prompt(prompt_from_flag: Option<String>) -> String {
    explicit_prompt(prompt_from_flag).unwrap_or_else(|| DEFAULT_DEMO_PROMPT.to_string())
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
    fn initial_prompt_prefers_arg_over_default() {
        assert_eq!(initial_prompt(Some("  hi  ".to_string())), "hi");
    }

    #[test]
    fn explicit_prompt_is_none_without_user_turn() {
        assert_eq!(explicit_prompt(None), None);
    }

    #[test]
    fn explicit_prompt_returns_trimmed_arg() {
        assert_eq!(
            explicit_prompt(Some("  hi  ".to_string())),
            Some("hi".to_string())
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
