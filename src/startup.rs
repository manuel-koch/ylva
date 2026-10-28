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

//! Startup orchestration for the `ylva` binary.
//!
//! `main.rs` parses arguments, dispatches subcommands and owns logging and
//! process exit. Everything between "arguments parsed" and "app finished"
//! lives here and reports failures as `Err(String)` (the text printed after
//! `error: `) instead of terminating the process.

mod engines;
mod profile;
mod prompt;
mod session;

use std::io::IsTerminal;
use std::sync::Arc;

use crate::tui_driver::FavoriteSaveSetup;
use crate::{AgentFactory, App, Cli, CliColorMode, ConsoleApp, TuiApp, DEFAULT_TUI_TOOLS};
use engines::{apply_provider_env_to_profile, engines_from_environment, load_provider_env};
use profile::{
    apply_cli_profile_overrides, load_profile, load_selected_profile_environment,
    selected_profile_context, selected_profile_path, trusted_profile_workspace_root,
};
use prompt::{explicit_prompt, initial_prompt};
use session::{open_store, print_resume_hint, resolve_root_agent_type};

/// Outcome of a completed app run.
pub struct StartupRun {
    /// Session persisted by this run, for log-span scoping of the final
    /// outcome handling.
    pub session_id: Option<uuid::Uuid>,
    /// The app's exit code, or the error text of a failed run.
    pub result: Result<u8, String>,
}

/// `--version` output: build provenance plus the Ylva home and workspace root
/// this run would use, resolved the same way as the normal startup path.
pub fn version_report() -> String {
    let workspace_root = std::env::current_dir()
        .ok()
        .and_then(|current_directory| crate::discover_workspace_root(&current_directory));
    crate::BUILD_INFO
        .render_with_locations(crate::ylva_home().as_deref(), workspace_root.as_deref())
}

/// `ylva trust [--revoke]`: approve (or withdraw approval of) the current
/// workspace configuration of the selected profile; returns the report text.
pub fn run_trust_command(
    revoke: bool,
    explicit_profile_name: Option<&str>,
) -> Result<String, String> {
    use crate::config::workspace_trust::{revoke_workspace_trust, trust_workspace};

    let current_directory =
        std::env::current_dir().map_err(|error| format!("cannot resolve cwd: {error}"))?;
    let workspace_root = crate::discover_workspace_root(&current_directory).ok_or_else(|| {
        "no workspace found: run inside a workspace with a .ylva directory or a git repository"
            .to_string()
    })?;
    let ylva_home = crate::ylva_home()
        .ok_or_else(|| "cannot resolve the home directory for ~/.ylva".to_string())?;
    let profile_name = explicit_profile_name.unwrap_or(crate::DEFAULT_PROFILE_NAME);
    let workspace_display = crate::home_relative_path::display_path(&workspace_root);
    if revoke {
        let removed = revoke_workspace_trust(&ylva_home, &workspace_root, profile_name)
            .map_err(|error| error.to_string())?;
        return Ok(if removed {
            format!("Revoked trust for profile '{profile_name}' in {workspace_display}")
        } else {
            format!("Profile '{profile_name}' in {workspace_display} was not trusted")
        });
    }
    let files = trust_workspace(&ylva_home, &workspace_root, profile_name)
        .map_err(|error| error.to_string())?;
    if files.is_empty() {
        return Ok(format!(
            "Nothing to trust: {workspace_display} has no workspace configuration for profile '{profile_name}'"
        ));
    }
    let listing = files
        .iter()
        .map(|file| format!("  {}", crate::home_relative_path::display_path(file)))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "Trusted profile '{profile_name}' in {workspace_display}. Any effective edit to these files revokes the trust:\n{listing}"
    ))
}

/// Seed starter files; returns the report text, which for a workspace ends
/// with a notice when Ylva will refuse the (changed) workspace configuration
/// until `ylva trust` approves it.
///
/// Seeding never trusts automatically: the workspace may hold other files
/// (`.env`, agents) that came from a clone and were not reviewed.
pub fn run_init_command(workspace: bool, force: bool) -> Result<String, String> {
    if workspace {
        let current_directory =
            std::env::current_dir().map_err(|error| format!("cannot resolve cwd: {error}"))?;
        let workspace_root =
            crate::discover_workspace_root(&current_directory).unwrap_or(current_directory);
        let report =
            crate::run_init_workspace(&workspace_root, force).map_err(|error| error.to_string())?;
        let mut summary = report.summary();
        if let Some(notice) = crate::config::workspace_trust::workspace_trust_notice(
            &workspace_root,
            crate::DEFAULT_PROFILE_NAME,
        ) {
            summary.push_str("\n\n");
            summary.push_str(&notice);
        }
        Ok(summary)
    } else {
        let ylva_home = crate::ylva_home()
            .ok_or_else(|| "cannot resolve the home directory for ~/.ylva".to_string())?;
        crate::run_init(&ylva_home, force)
            .map(|report| report.summary())
            .map_err(|error| error.to_string())
    }
}

/// Decide whether CLI status lines and listings may use ANSI styling.
///
/// This is intentionally evaluated only for CLI output. The TUI keeps its
/// existing ratatui palette regardless of the selected CLI color mode.
pub fn should_use_cli_color(
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

/// [`should_use_cli_color`] evaluated against the real stdout and process
/// environment.
pub fn cli_color_enabled(color_mode: CliColorMode) -> bool {
    should_use_cli_color(
        std::io::stdout().is_terminal(),
        color_mode,
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("NO_COLOR").is_some(),
    )
}

/// Run the TUI or headless console app selected by the parsed arguments.
///
/// Prints the resume hint to stderr once the app returns, whether it
/// succeeded or failed. Setup failures are `Err`; an app failure is carried in
/// [`StartupRun::result`] so the caller can scope its handling to the session.
pub fn run(
    cli: &Cli,
    cli_color_mode: CliColorMode,
    tokio_runtime: &tokio::runtime::Runtime,
) -> Result<StartupRun, String> {
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let use_tui = stdout_is_terminal && !cli.no_tui;
    let cli_color_output = !use_tui && cli_color_enabled(cli_color_mode);
    tracing::info!(app = if use_tui { "tui" } else { "cli" }, "starting");

    let current_directory = std::env::current_dir().ok();
    let workspace_root = current_directory
        .as_deref()
        .and_then(crate::discover_workspace_root);
    let ylva_home = crate::ylva_home();
    let skills = crate::skill_command::discover()
        .and_then(|catalog| {
            crate::skill_command::report_diagnostics(&catalog, &mut std::io::stderr())?;
            Ok(Arc::new(catalog))
        })
        .map_err(|error| error.to_string())?;
    // Export the resolved Ylva home so `$YLVA_HOME/…` sentinel sources expand
    // verbatim for path-like tools (`create_call`) and for `bash` subagent
    // processes alike. `ylva_home()` already answered the same question, but
    // the default `~/.ylva` value existed only inside its return value while
    // `YLVA_HOME` stayed unset in the process environment. Idempotent when the
    // variable was already set explicitly.
    if let Some(ylva_home) = ylva_home.as_deref() {
        std::env::set_var(crate::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home);
    }
    let explicit_profile_name = cli.profile.as_deref();
    let profile_workspace_root =
        trusted_profile_workspace_root(explicit_profile_name, workspace_root.as_deref())?;
    let profile_environment = load_selected_profile_environment(
        explicit_profile_name,
        profile_workspace_root.as_deref(),
    )?;
    let (mut profile, favorite_writer) = load_profile(
        explicit_profile_name,
        &profile_environment,
        profile_workspace_root.as_deref(),
    )?;
    for warning in &profile.config_warnings {
        tracing::warn!(%warning, "unknown configuration key");
    }
    if cli.strict_config {
        if let Some(notice) = profile.config_warnings_notice() {
            return Err(format!("{notice}\n(--strict-config)"));
        }
    }
    apply_cli_profile_overrides(cli, &mut profile);
    let profile_path =
        selected_profile_path(explicit_profile_name, profile_workspace_root.as_deref())?;
    let root_system_prompt =
        crate::compose_root_system_prompt(workspace_root.as_deref(), current_directory.as_deref())
            .map_err(|error| error.to_string())?;
    crate::highlight::set_active_theme(profile.theme);
    tracing::info!(theme = ?profile.theme, "theme selected");
    let provider_env = load_provider_env(&profile_environment)?;
    apply_provider_env_to_profile(&mut profile, &provider_env);

    let conversation_id = cli.conversation_id;
    // `--clone` turns `--conversation-id` into the copy source: the run then
    // starts a brand-new session replaying that source's event stream.
    let clone_session = cli.clone;

    // Resolved before the initial engine is chosen and before any session
    // metadata is written: an explicit `--agent` always wins, a resume
    // otherwise continues with the type the session last stored, and a fresh
    // session starts as `default`.
    let root_agent_type = resolve_root_agent_type(cli.agent.as_deref(), conversation_id, &profile)?;
    tracing::info!(
        agent_type = %root_agent_type.agent_type,
        explicit = cli.agent.is_some(),
        resumed = conversation_id.is_some() && !clone_session,
        cloned = clone_session,
        "root agent type resolved"
    );

    let factory_without_store = {
        // Engines come from the profile's `[engines.*]` tables when it defines
        // any; a profile that defines none (including the in-code default used
        // when no config file exists) falls back to the environment-derived
        // provider set.
        let mut factory = if profile.engines.is_empty() && profile.provider_connections.is_empty() {
            let engines = engines_from_environment(
                &provider_env,
                &profile.default_engine,
                profile.default_model.as_deref(),
            )
            .map_err(|error| error.to_string())?;
            AgentFactory::new(engines)
        } else {
            AgentFactory::from_profile(&profile).map_err(|error| {
                format!(
                    "{}: {error}",
                    selected_profile_context(
                        explicit_profile_name,
                        profile_workspace_root.as_deref()
                    )
                )
            })?
        };
        if use_tui {
            factory = factory.with_allowed_tools(DEFAULT_TUI_TOOLS);
        }
        for tool in cli.auto_approve_tools() {
            factory = factory.with_auto_approve(tool);
        }
        factory = factory
            .with_system_prompt(root_system_prompt.clone())
            .with_skill_catalog(skills, use_tui);
        factory
    };

    factory_without_store
        .validate_profile_agent_engines(&profile)
        .and_then(|()| factory_without_store.validate_profile_skills(&profile))
        .map_err(|error| {
            format!(
                "{}: {error}",
                selected_profile_context(explicit_profile_name, profile_workspace_root.as_deref())
            )
        })?;

    let explicit_profile_name = explicit_profile_name.map(str::to_string);
    let selected_profile_name = explicit_profile_name
        .clone()
        .unwrap_or_else(|| crate::DEFAULT_PROFILE_NAME.to_string());
    let prompt_from_flag = cli.prompt.clone();

    let (result, session_hint) = tokio_runtime.block_on(async {
        // Resolved once, up front, so both a fresh session's `meta` header
        // and the `model_changed` record below (written on every start,
        // fresh or resumed) use the exact same provider-connection/model.
        // The engine follows the selected root agent type, not the profile
        // default, so `--agent` and a restored type run on their own engine.
        let initial_engine_name = profile.engine_name_for(&root_agent_type.agent_type);
        let initial_engine_config = factory_without_store.engine_config(initial_engine_name);
        // The store is opened inside the Tokio runtime: it spawns the writer task
        // that owns the session file.
        let store = open_store(
            conversation_id,
            clone_session,
            initial_engine_config.as_ref(),
            &root_agent_type.agent_type,
        )?;
        // Recorded on every start — fresh session and resume alike — not
        // only on a fresh session: resuming under a different `--profile` or
        // an edited config can resolve the selected agent type's engine to a
        // different provider/model than the session's history last showed,
        // and that must be visible without re-deriving it from config.
        if let Some(config) = &initial_engine_config {
            store.record_model_changed(&config.provider_connection, &config.model);
        }
        // An explicit `--agent` that changed the type a *resumed* session was
        // already using becomes the latest stored selection, so later resumes
        // without the flag continue under it. A fresh session already carries
        // its type in `meta`, so it needs no extra record.
        // A clone already carries its effective type in `meta` (the copy
        // writes it at creation), so it needs no selection record of its own.
        if conversation_id.is_some()
            && !clone_session
            && root_agent_type.override_changes_active_type
        {
            store.record_agent_type_changed(&root_agent_type.agent_type);
        }
        // Captured before `store` moves into the factory, so it survives to
        // print the resume hint after the app returns.
        let session_hint = Arc::new(std::sync::Mutex::new(Some(
            crate::app::SessionExitSnapshot {
                session_id: store.session_id(),
                title: store.title(),
            },
        )));
        let factory = Arc::new(factory_without_store.with_store(store));
        let result = if use_tui {
            // The TUI starts with an empty input box. An explicit `--prompt` (or
            // piped stdin) still auto-sends, but the hardcoded demo prompt is only
            // for the headless path — it must not pre-populate the interactive UI.
            let explicit_prompt = explicit_prompt(prompt_from_flag.clone());
            let favorite_save_setup = FavoriteSaveSetup::from_profile(favorite_writer, &profile);
            TuiApp::new(
                factory,
                profile,
                root_agent_type.agent_type.clone(),
                explicit_prompt,
            )
            .with_exit_snapshot(Arc::clone(&session_hint))
            .with_favorite_save_setup(favorite_save_setup)
            .with_display_metadata(crate::TuiDisplayMetadata {
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
                root_agent_type.agent_type.clone(),
                prompt,
                Box::new(std::io::stdout()),
            )
            .with_exit_snapshot(Arc::clone(&session_hint))
            .with_color_output(cli_color_output)
            .run()
            .await
        };
        let snapshot = session_hint.lock().expect("exit snapshot mutex").clone();
        Ok::<_, String>((result, snapshot))
    })?;
    // Printed whether the run succeeded or failed, so a user can always
    // resume the session that was just persisted (the TUI has already
    // restored the terminal by this point; the hint goes to stderr alongside
    // every other status line).
    if let Some(snapshot) = &session_hint {
        print_resume_hint(snapshot, explicit_profile_name.as_deref());
    }
    Ok(StartupRun {
        session_id: session_hint.as_ref().map(|snapshot| snapshot.session_id),
        result: result.map_err(|error| error.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
