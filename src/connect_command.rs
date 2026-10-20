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

//! `ylva connect <provider>`: onboarding that adds a provider connection to
//! the profile `config.toml` and stores its secret in the profile `.env`.
//!
//! This module is provider-agnostic: each provider implements [`ConnectFlow`]
//! (see [`providers`]) and returns the connection settings and secrets to
//! store; this module validates the profile and writes the files.

mod providers;

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cli::ConnectCommand;
use crate::config::layered::discover_workspace_root;
use crate::config::{
    default_profiles_root, profile_config_path, profile_env_path, DEFAULT_PROFILE_NAME,
    PROFILES_DIRECTORY_NAME,
};
use crate::exit_code::ExitCode;
use crate::home_relative_path::display_path;
use crate::init_command::{seed_workspace_gitignore, InitReport};
use crate::ylva_home::YLVA_DIR_NAME;
use providers::{CopilotConnect, EndpointConnect};

/// A connection to add to the profile, as produced by a provider.
struct NewConnection {
    /// Provider-specific keys of the `[provider_connections.<name>]` table;
    /// `provider` itself is added from [`ConnectFlow::kind`].
    settings: toml_edit::Table,
    /// `.env` assignments (variable, value) the settings refer to.
    env_entries: Vec<(String, String)>,
}

/// Terminal access handed to a [`ConnectFlow`].
struct ConnectIo<'a> {
    prompter: &'a mut dyn Prompter,
    output: &'a mut dyn Write,
}

/// What is specific to connecting one provider kind.
trait ConnectFlow {
    /// The `provider = "..."` value; also the default connection name, whose
    /// `.env` variables may be refreshed instead of being a clash.
    fn kind(&self) -> &'static str;

    /// `.env` variables this flow will write, so a clash is reported before
    /// an expensive interaction such as a login. Clashes of the entries
    /// returned by [`ConnectFlow::connect`] are always checked afterwards.
    fn env_variables(&self, _connection_name: &str) -> Vec<String> {
        Vec::new()
    }

    /// Interact with the user and produce the connection to store.
    fn connect(
        &self,
        connection_name: &str,
        io: &mut ConnectIo<'_>,
    ) -> anyhow::Result<NewConnection>;
}

/// Source of the answers for the interactive connect flows.
pub trait Prompter {
    /// Ask a question and return the trimmed answer.
    fn ask(&mut self, question: &str) -> anyhow::Result<String>;
    /// Ask for a secret without echoing it and return the trimmed answer.
    fn ask_secret(&mut self, question: &str) -> anyhow::Result<String>;
}

/// Prompts on the terminal; with piped stdin it reads one line per answer.
pub struct TerminalPrompter;

impl TerminalPrompter {
    fn show(question: &str) -> anyhow::Result<()> {
        let mut stdout = std::io::stdout();
        write!(stdout, "{question} ")?;
        stdout.flush()?;
        Ok(())
    }

    fn read_line() -> anyhow::Result<String> {
        let mut line = String::new();
        let read = std::io::stdin().read_line(&mut line)?;
        anyhow::ensure!(read > 0, "unexpected end of input");
        Ok(line.trim().to_string())
    }

    fn read_hidden_line() -> anyhow::Result<String> {
        use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

        struct RawModeGuard;
        impl Drop for RawModeGuard {
            fn drop(&mut self) {
                let _ = crossterm::terminal::disable_raw_mode();
            }
        }
        crossterm::terminal::enable_raw_mode()?;
        let guard = RawModeGuard;
        let mut secret = String::new();
        loop {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::Enter => break,
                KeyCode::Backspace => {
                    secret.pop();
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    anyhow::bail!("cancelled");
                }
                KeyCode::Char(character) => secret.push(character),
                _ => {}
            }
        }
        // Leave raw mode first so the newline also returns the carriage.
        drop(guard);
        println!();
        Ok(secret.trim().to_string())
    }
}

impl Prompter for TerminalPrompter {
    fn ask(&mut self, question: &str) -> anyhow::Result<String> {
        Self::show(question)?;
        Self::read_line()
    }

    fn ask_secret(&mut self, question: &str) -> anyhow::Result<String> {
        use std::io::IsTerminal;
        Self::show(question)?;
        if std::io::stdin().is_terminal() {
            Self::read_hidden_line()
        } else {
            Self::read_line()
        }
    }
}

/// Run one `ylva connect` action. Progress goes to `output`, problems to
/// `diagnostics`; the return value is the process exit code.
pub fn run(
    command: &ConnectCommand,
    profile_name: Option<&str>,
    prompter: &mut dyn Prompter,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    let (flow, name, workspace, force): (Box<dyn ConnectFlow>, _, _, _) = match command {
        ConnectCommand::Copilot {
            name,
            workspace,
            force,
        } => (Box::new(CopilotConnect), name, *workspace, *force),
        ConnectCommand::Ollama {
            name,
            workspace,
            force,
        } => (
            Box::new(EndpointConnect::ollama()),
            name,
            *workspace,
            *force,
        ),
        ConnectCommand::Openrouter {
            name,
            workspace,
            force,
        } => (
            Box::new(EndpointConnect::openrouter()),
            name,
            *workspace,
            *force,
        ),
    };
    let connection_name = name.as_deref().unwrap_or(flow.kind());
    let result = (|| {
        let (profiles_root, workspace_config_root) = if workspace {
            let workspace_config_root = workspace_config_root(&std::env::current_dir()?)?;
            (
                workspace_config_root.join(PROFILES_DIRECTORY_NAME),
                Some(workspace_config_root),
            )
        } else {
            (default_profiles_root()?, None)
        };
        let profile_name = profile_name.unwrap_or(DEFAULT_PROFILE_NAME);
        let config_path = profile_config_path(&profiles_root, profile_name);
        let env_path = profile_env_path(&profiles_root, profile_name);
        // Fail before the user spends time answering questions or logging in.
        plan_connection(
            &config_path,
            &env_path,
            flow.kind(),
            connection_name,
            &flow.env_variables(connection_name),
            force,
        )?;
        let connection = flow.connect(connection_name, &mut ConnectIo { prompter, output })?;
        if let (Some(workspace_config_root), false) =
            (&workspace_config_root, connection.env_entries.is_empty())
        {
            // The workspace .env must not be committed by accident.
            let mut report = InitReport {
                root: workspace_config_root.clone(),
                written: Vec::new(),
                skipped: Vec::new(),
            };
            seed_workspace_gitignore(workspace_config_root, false, &mut report)?;
        }
        persist_connection(
            &config_path,
            &env_path,
            flow.kind(),
            connection_name,
            &connection,
            force,
        )?;
        let variables: Vec<&str> = connection
            .env_entries
            .iter()
            .map(|(variable, _)| variable.as_str())
            .collect();
        if variables.is_empty() {
            writeln!(
                output,
                "Connected '{connection_name}': connection added to {} (no secret stored)",
                display_path(&config_path)
            )?;
        } else {
            writeln!(
                output,
                "Connected '{connection_name}': secret stored as {} in {}, connection added to {}",
                variables.join(", "),
                display_path(&env_path),
                display_path(&config_path)
            )?;
        }
        anyhow::Ok(())
    })();
    match result {
        Ok(()) => Ok(ExitCode::Success as u8),
        Err(error) => {
            writeln!(diagnostics, "error: {error:#}")?;
            Ok(ExitCode::UsageError as u8)
        }
    }
}

/// Everything needed to write, validated before any file is touched.
struct ConnectionPlan {
    config_document: toml_edit::DocumentMut,
    env_text: String,
}

/// Validate that `connection_name` is free in `config.toml` and that none of
/// `env_variables` would clobber an existing `.env` entry. The default
/// connection (named like `kind`) may refresh its variables. `force` accepts
/// both an existing connection and existing variables.
fn plan_connection(
    config_path: &Path,
    env_path: &Path,
    kind: &str,
    connection_name: &str,
    env_variables: &[String],
    force: bool,
) -> anyhow::Result<ConnectionPlan> {
    let config_text = std::fs::read_to_string(config_path).map_err(|error| {
        anyhow::anyhow!(
            "cannot read {}: {error}; create the profile first with `ylva init` or `ylva profile create` (add `--workspace` for a workspace profile)",
            display_path(config_path)
        )
    })?;
    let config_document: toml_edit::DocumentMut = config_text
        .parse()
        .map_err(|error| anyhow::anyhow!("cannot parse {}: {error}", display_path(config_path)))?;
    if !force
        && config_document
            .get("provider_connections")
            .and_then(|connections| connections.get(connection_name))
            .is_some()
    {
        anyhow::bail!(
            "{} already defines [provider_connections.{connection_name}]; choose another --name or pass --force to replace it",
            display_path(config_path)
        );
    }
    let env_text = match std::fs::read_to_string(env_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => anyhow::bail!("cannot read {}: {error}", display_path(env_path)),
    };
    if connection_name != kind && !force {
        for variable in env_variables {
            if env_assigns(&env_text, variable) {
                anyhow::bail!(
                    "{} already defines {variable}; choose another --name or pass --force to overwrite it",
                    display_path(env_path)
                );
            }
        }
    }
    Ok(ConnectionPlan {
        config_document,
        env_text,
    })
}

fn env_assigns(env_text: &str, variable: &str) -> bool {
    env_text.lines().any(|line| {
        let line = line.trim_start();
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        line.split_once('=')
            .is_some_and(|(name, _)| name.trim() == variable)
    })
}

/// Set `variable=value` in dotenv text, replacing an existing assignment and
/// keeping every other line untouched.
fn env_with_assignment(env_text: &str, variable: &str, value: &str) -> String {
    let mut replaced = false;
    let mut lines: Vec<String> = env_text
        .lines()
        .map(|line| {
            if !replaced && env_assigns(line, variable) {
                replaced = true;
                format!("{variable}={value}")
            } else {
                line.to_string()
            }
        })
        .collect();
    if !replaced {
        lines.push(format!("{variable}={value}"));
    }
    lines.join("\n") + "\n"
}

/// Write the secrets to the profile `.env` and the connection to `config.toml`.
fn persist_connection(
    config_path: &Path,
    env_path: &Path,
    kind: &str,
    connection_name: &str,
    connection: &NewConnection,
    force: bool,
) -> anyhow::Result<()> {
    let env_variables: Vec<String> = connection
        .env_entries
        .iter()
        .map(|(variable, _)| variable.clone())
        .collect();
    let ConnectionPlan {
        mut config_document,
        mut env_text,
    } = plan_connection(
        config_path,
        env_path,
        kind,
        connection_name,
        &env_variables,
        force,
    )?;

    let connections = config_document
        .entry("provider_connections")
        .or_insert_with(|| {
            let mut table = toml_edit::Table::new();
            table.set_implicit(true);
            toml_edit::Item::Table(table)
        })
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`provider_connections` in config.toml is not a table"))?;
    let mut table = toml_edit::Table::new();
    table["provider"] = toml_edit::value(kind);
    for (key, item) in &connection.settings {
        table.insert(key, item.clone());
    }
    connections.insert(connection_name, toml_edit::Item::Table(table));

    if !connection.env_entries.is_empty() {
        for (variable, secret) in &connection.env_entries {
            env_text = env_with_assignment(&env_text, variable, secret);
        }
        crate::private_fs::replace_file(env_path, env_text.as_bytes(), 0o600)?;
    }
    crate::private_fs::replace_file(
        config_path,
        config_document.to_string().as_bytes(),
        existing_mode(config_path),
    )?;
    Ok(())
}

#[cfg(unix)]
fn existing_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .unwrap_or(0o644)
}

#[cfg(not(unix))]
fn existing_mode(_path: &Path) -> u32 {
    0o644
}

/// The `.ylva` directory of the workspace containing `directory`.
fn workspace_config_root(directory: &Path) -> anyhow::Result<PathBuf> {
    let workspace_root = discover_workspace_root(directory).ok_or_else(|| {
        anyhow::anyhow!(
            "no workspace root found; run inside a directory with {} or .ylva",
            crate::GIT_ENTRY_NAME
        )
    })?;
    Ok(workspace_root.join(YLVA_DIR_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_config_root_finds_the_workspace_from_a_subdirectory() {
        let directory = tempfile::tempdir().expect("tempdir");
        let nested = directory.path().join("src");
        std::fs::create_dir(&nested).expect("nested");
        std::fs::create_dir(directory.path().join(crate::GIT_ENTRY_NAME)).expect("marker");

        let found = workspace_config_root(&nested).expect("workspace root");

        assert_eq!(found, directory.path().join(YLVA_DIR_NAME));
    }

    #[test]
    fn workspace_config_root_without_a_workspace_root_is_an_error() {
        let directory = tempfile::tempdir().expect("tempdir");

        let error = workspace_config_root(directory.path()).expect_err("no workspace");

        assert!(error.to_string().contains("no workspace root found"));
    }

    fn profile_files(
        config: &str,
        env: Option<&str>,
    ) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let directory = tempfile::tempdir().expect("tempdir");
        let config_path = directory.path().join("config.toml");
        let env_path = directory.path().join(".env");
        std::fs::write(&config_path, config).expect("config");
        if let Some(env) = env {
            std::fs::write(&env_path, env).expect("env");
        }
        (directory, config_path, env_path)
    }

    fn persist_secret(
        config_path: &Path,
        env_path: &Path,
        connection_name: &str,
        variable: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        persist_secret_with_force(
            config_path,
            env_path,
            connection_name,
            variable,
            value,
            false,
        )
    }

    fn persist_secret_with_force(
        config_path: &Path,
        env_path: &Path,
        connection_name: &str,
        variable: &str,
        value: &str,
        force: bool,
    ) -> anyhow::Result<()> {
        let mut settings = toml_edit::Table::new();
        settings["api_key"] = toml_edit::value(format!("${{{variable}}}"));
        persist_connection(
            config_path,
            env_path,
            "copilot",
            connection_name,
            &NewConnection {
                settings,
                env_entries: vec![(variable.to_string(), value.to_string())],
            },
            force,
        )
    }

    pub(super) struct ScriptedPrompter {
        answers: Vec<&'static str>,
        pub(super) questions: Vec<String>,
    }

    impl ScriptedPrompter {
        pub(super) fn new(answers: &[&'static str]) -> Self {
            Self {
                answers: answers.iter().rev().copied().collect(),
                questions: Vec::new(),
            }
        }
    }

    impl Prompter for ScriptedPrompter {
        fn ask(&mut self, question: &str) -> anyhow::Result<String> {
            self.questions.push(question.to_string());
            Ok(self.answers.pop().expect("scripted answer").to_string())
        }

        fn ask_secret(&mut self, question: &str) -> anyhow::Result<String> {
            self.ask(question)
        }
    }

    #[test]
    fn persists_connection_and_token_preserving_existing_content() {
        let (_directory, config_path, env_path) = profile_files(
            "# my comment\ndefault_engine = \"x\"\n\n[engines.x]\nprovider_connection = \"c\"\n",
            Some("OTHER=1\n"),
        );

        persist_secret(
            &config_path,
            &env_path,
            "copilot",
            "COPILOT_GITHUB_TOKEN",
            "ghu_token",
        )
        .expect("persist");

        let config = std::fs::read_to_string(&config_path).unwrap();
        assert!(config.contains("# my comment"));
        assert!(config.contains("[provider_connections.copilot]"));
        assert!(config.contains("api_key = \"${COPILOT_GITHUB_TOKEN}\""));
        assert!(!config.contains("ghu_token"));
        assert_eq!(
            std::fs::read_to_string(&env_path).unwrap(),
            "OTHER=1\nCOPILOT_GITHUB_TOKEN=ghu_token\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&env_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(config.parse::<toml::Table>().is_ok());
    }

    #[test]
    fn default_connection_refreshes_the_token_but_refuses_an_existing_table() {
        let (_directory, config_path, env_path) = profile_files(
            "default_engine = \"x\"\n",
            Some("COPILOT_GITHUB_TOKEN=old\nA=b\n"),
        );

        persist_secret(
            &config_path,
            &env_path,
            "copilot",
            "COPILOT_GITHUB_TOKEN",
            "ghu_new",
        )
        .expect("refresh");
        assert_eq!(
            std::fs::read_to_string(&env_path).unwrap(),
            "COPILOT_GITHUB_TOKEN=ghu_new\nA=b\n"
        );

        let error = persist_secret(
            &config_path,
            &env_path,
            "copilot",
            "COPILOT_GITHUB_TOKEN",
            "ghu_other",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("already defines"));
        assert!(std::fs::read_to_string(&env_path)
            .unwrap()
            .contains("ghu_new"));
    }

    #[test]
    fn named_connection_refuses_an_existing_variable() {
        let (_directory, config_path, env_path) = profile_files("", None);

        persist_secret(&config_path, &env_path, "work", "TOKEN_WORK", "ghu_a").expect("persist");
        std::fs::write(&config_path, "").unwrap();

        let error = persist_secret(&config_path, &env_path, "work", "TOKEN_WORK", "ghu_b")
            .unwrap_err()
            .to_string();
        assert!(error.contains("TOKEN_WORK"));
    }

    #[test]
    fn missing_config_asks_to_create_the_profile_first() {
        let (directory, _config_path, env_path) = profile_files("", None);
        let missing = directory.path().join("missing.toml");
        assert!(
            persist_secret(&missing, &env_path, "copilot", "TOKEN", "ghu_x")
                .unwrap_err()
                .to_string()
                .contains("ylva init")
        );
    }

    #[test]
    fn plan_checks_declared_variables_but_lets_the_default_connection_refresh() {
        let (_directory, config_path, env_path) = profile_files("", Some("TOKEN=old\n"));
        let variables = vec!["TOKEN".to_string()];

        assert!(plan_connection(
            &config_path,
            &env_path,
            "copilot",
            "copilot",
            &variables,
            false
        )
        .is_ok());
        let error = plan_connection(
            &config_path,
            &env_path,
            "copilot",
            "work",
            &variables,
            false,
        )
        .err()
        .expect("clash")
        .to_string();
        assert!(error.contains("already defines TOKEN"));
        assert!(error.contains("--force"));
        assert!(
            plan_connection(&config_path, &env_path, "copilot", "work", &variables, true).is_ok()
        );
    }

    #[test]
    fn force_replaces_an_existing_connection_and_its_variable() {
        let (_directory, config_path, env_path) = profile_files("", Some("A=b\n"));
        persist_secret(&config_path, &env_path, "work", "TOKEN_WORK", "old").expect("first");

        let error = persist_secret(&config_path, &env_path, "work", "TOKEN_WORK", "new")
            .unwrap_err()
            .to_string();
        assert!(error.contains("--force"));

        persist_secret_with_force(&config_path, &env_path, "work", "TOKEN_WORK", "new", true)
            .expect("forced");

        let config = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(config.matches("[provider_connections.work]").count(), 1);
        assert_eq!(
            std::fs::read_to_string(&env_path).unwrap(),
            "A=b\nTOKEN_WORK=new\n"
        );
    }

    #[test]
    fn persists_settings_without_secret_leaving_the_env_untouched() {
        let (_directory, config_path, env_path) = profile_files("", Some("A=b\n"));
        let mut settings = toml_edit::Table::new();
        settings["api_base_url"] = toml_edit::value("http://gpu:11434");

        persist_connection(
            &config_path,
            &env_path,
            "ollama",
            "ollama",
            &NewConnection {
                settings,
                env_entries: Vec::new(),
            },
            false,
        )
        .expect("persist");

        let config = std::fs::read_to_string(&config_path).unwrap();
        assert!(config.contains("[provider_connections.ollama]"));
        assert!(config.contains("provider = \"ollama\""));
        assert!(config.contains("api_base_url = \"http://gpu:11434\""));
        assert!(!config.contains("api_key"));
        assert_eq!(std::fs::read_to_string(&env_path).unwrap(), "A=b\n");
    }
}
