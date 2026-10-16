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

//! `ylva profile` actions: list, create, delete, and edit profile directories.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::ProfileCommand;
use crate::config::layered::discover_workspace_root;
use crate::config::{
    is_valid_profile_name, profile_config_path, profile_env_path, ConfigError,
    DEFAULT_PROFILE_NAME, PROFILES_DIRECTORY_NAME,
};
use crate::exit_code::ExitCode;
use crate::home_relative_path::display_path;
use crate::init_command::{
    seed_profile_starters, seed_workspace_gitignore, InitReport, DEFAULT_PROFILE_STARTER,
};
use crate::profile_shim::{
    apply_shim_plan, config_text_with_shim_name, is_on_path, path_export_line, plan_shims,
    profile_directory_names, PendingProfile, ShimDirectories, ShimPlan, ShimTemplate,
    SHIMS_DIRECTORY_NAME,
};
use crate::ylva_home::{ylva_home, YLVA_DIR_NAME};

const SUCCESS: u8 = ExitCode::Success as u8;
const GENERAL_ERROR: u8 = ExitCode::GeneralError as u8;
const USAGE_ERROR: u8 = ExitCode::UsageError as u8;

const IMPLICIT_DEFAULT_MARK: &str = "(implicit default)";
const INVALID_NAME_MARK: &str = "(invalid name)";
const NO_CONFIG_MARK: &str = "(no config.toml)";
const INVALID_CONFIG_MARK: &str = "(invalid config)";

/// Where a profile command reads and writes profile directories and shims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileScope {
    /// The Ylva home, whose profiles live in `<ylva home>/profiles/`.
    Home { ylva_home: PathBuf },
    /// A workspace, whose profiles live in `<workspace>/.ylva/profiles/`.
    Workspace { workspace_config_root: PathBuf },
}

impl ProfileScope {
    fn shim_directories(&self) -> ShimDirectories {
        let (scope_root, template) = match self {
            Self::Home { ylva_home } => (ylva_home.clone(), ShimTemplate::Home),
            Self::Workspace {
                workspace_config_root,
            } => (workspace_config_root.clone(), ShimTemplate::Workspace),
        };
        ShimDirectories {
            profiles_root: scope_root.join(PROFILES_DIRECTORY_NAME),
            shims_directory: scope_root.join(SHIMS_DIRECTORY_NAME),
            template,
        }
    }

    fn profiles_root(&self) -> PathBuf {
        self.shim_directories().profiles_root
    }

    /// The scope's name as shown in `profile list`.
    fn label(&self) -> &'static str {
        match self {
            Self::Home { .. } => "home",
            Self::Workspace { .. } => "workspace",
        }
    }
}

/// Editor variables read from the process environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorSettings {
    pub visual: Option<String>,
    pub editor: Option<String>,
}

impl EditorSettings {
    /// The editor command from `VISUAL`, then `EDITOR`. Blank values count as unset.
    fn command(&self) -> Option<&str> {
        [self.visual.as_deref(), self.editor.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|command| !command.is_empty())
    }
}

/// Process environment a profile command reads: editor variables and `PATH`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileEnvironment {
    pub editor: EditorSettings,
    pub path_variable: Option<OsString>,
}

/// Starts an editor on a file and reports whether it exited successfully.
pub trait EditorLauncher {
    fn launch(&self, editor_command: &str, file_path: &Path) -> io::Result<bool>;
}

/// Runs the editor through `sh -c`, so commands with arguments such as `code --wait` work.
pub struct ShellEditorLauncher;

impl EditorLauncher for ShellEditorLauncher {
    fn launch(&self, editor_command: &str, file_path: &Path) -> io::Result<bool> {
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("{editor_command} \"$1\""))
            .arg("sh")
            .arg(file_path)
            .status()?;
        Ok(status.success())
    }
}

/// Run one `ylva profile` action, resolving the scope and editor from the environment.
pub fn run(
    command: &ProfileCommand,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    let environment = ProfileEnvironment {
        editor: EditorSettings {
            visual: std::env::var("VISUAL").ok(),
            editor: std::env::var("EDITOR").ok(),
        },
        path_variable: std::env::var_os("PATH"),
    };
    if let ProfileCommand::List { workspace: false } = command {
        return match merged_list_scopes() {
            Ok(scopes) => list(&scopes, output),
            Err(error) => refuse(diagnostics, &format!("{error:#}")),
        };
    }
    let scope = match resolve_scope(workspace_requested(command)) {
        Ok(Some(scope)) => scope,
        Ok(None) => {
            return refuse(
                diagnostics,
                &format!(
                    "no workspace root found; run inside a directory with {} or .ylva",
                    crate::GIT_ENTRY_NAME,
                ),
            );
        }
        Err(error) => return refuse(diagnostics, &format!("{error:#}")),
    };
    run_in_scope(
        command,
        &scope,
        &environment,
        &ShellEditorLauncher,
        output,
        diagnostics,
    )
}

/// Run one action in an already resolved scope. I/O failures exit with `UsageError`.
pub fn run_in_scope(
    command: &ProfileCommand,
    scope: &ProfileScope,
    environment: &ProfileEnvironment,
    launcher: &dyn EditorLauncher,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    match execute(command, scope, environment, launcher, output, diagnostics) {
        Ok(code) => Ok(code),
        Err(error) => refuse(diagnostics, &format!("{error:#}")),
    }
}

fn execute(
    command: &ProfileCommand,
    scope: &ProfileScope,
    environment: &ProfileEnvironment,
    launcher: &dyn EditorLauncher,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    let name = command_name(command);
    if let Some(name) = name {
        if !is_valid_profile_name(name) {
            return refuse(
                diagnostics,
                &format!("invalid profile name {name:?}; use letters, digits, '-' or '_'"),
            );
        }
    }
    match command {
        ProfileCommand::List { .. } => list(std::slice::from_ref(scope), output),
        ProfileCommand::Create {
            name,
            force,
            shim_name,
            ..
        } => create(
            scope,
            environment,
            name,
            *force,
            shim_name.as_deref(),
            output,
            diagnostics,
        ),
        ProfileCommand::Delete { name, dry_run, .. } => {
            delete(scope, name, *dry_run, output, diagnostics)
        }
        ProfileCommand::Edit { name, .. } => {
            edit(scope, name, environment, launcher, output, diagnostics)
        }
        ProfileCommand::Shims { .. } => shims(scope, environment, output, diagnostics),
    }
}

/// Scopes of the merged `profile list`, highest priority first: the workspace when the
/// current directory is inside one, then the Ylva home.
fn merged_list_scopes() -> anyhow::Result<Vec<ProfileScope>> {
    Ok(merge_list_scopes(
        resolve_scope(true)?,
        resolve_scope(false)?,
    ))
}

/// Drops the home scope when it reads the same profiles directory as the workspace, as it
/// does when `YLVA_HOME` points at the workspace's `.ylva`.
fn merge_list_scopes(
    workspace: Option<ProfileScope>,
    home: Option<ProfileScope>,
) -> Vec<ProfileScope> {
    let mut scopes: Vec<ProfileScope> = workspace.into_iter().collect();
    if let Some(home) = home {
        if !scopes
            .iter()
            .any(|scope| scope.profiles_root() == home.profiles_root())
        {
            scopes.push(home);
        }
    }
    scopes
}

/// Lists profiles from `scopes`, highest priority first, with a blank line between each
/// scope's rows; names are sorted within a scope. A profile whose name an earlier scope
/// already lists is marked as shadowed by that scope. Paths under the home directory are
/// shown with a leading `~`. Rows are aligned in columns without a header.
fn list(scopes: &[ProfileScope], output: &mut dyn Write) -> anyhow::Result<u8> {
    let mut entries: Vec<(String, usize)> = Vec::new();
    for (scope_index, scope) in scopes.iter().enumerate() {
        for name in profile_directory_names(&scope.profiles_root())? {
            entries.push((name, scope_index));
        }
    }
    if entries.is_empty() {
        for scope in scopes {
            writeln!(
                output,
                "No profiles in {}",
                display_path(&scope.profiles_root())
            )?;
        }
        return Ok(SUCCESS);
    }
    entries.sort_by(|(left_name, left_scope), (right_name, right_scope)| {
        (left_scope, left_name).cmp(&(right_scope, right_name))
    });
    let mut winning_scope_by_name: HashMap<&str, usize> = HashMap::new();
    for (name, scope_index) in &entries {
        winning_scope_by_name
            .entry(name.as_str())
            .or_insert(*scope_index);
    }
    let mut rows: Vec<(usize, &str, String, String)> = Vec::new();
    for (name, scope_index) in &entries {
        let name = name.as_str();
        let profiles_root = scopes[*scope_index].profiles_root();
        let winning_scope_index = winning_scope_by_name[name];
        let mut marks: Vec<String> = list_marks(&profiles_root, name)
            .into_iter()
            .map(str::to_string)
            .collect();
        if winning_scope_index < *scope_index {
            let label = scopes[winning_scope_index].label();
            marks.push(format!("(shadowed by {label})"));
        }
        let directory = display_path(&profiles_root.join(name));
        rows.push((*scope_index, name, directory, marks.join(" ")));
    }
    let name_width = rows
        .iter()
        .map(|(_, name, _, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let directory_width = rows
        .iter()
        .map(|(_, _, directory, _)| directory.chars().count())
        .max()
        .unwrap_or(0);
    let mut previous_scope_index: Option<usize> = None;
    for (scope_index, name, directory, marks) in rows {
        if previous_scope_index.is_some_and(|previous| previous != scope_index) {
            writeln!(output)?;
        }
        previous_scope_index = Some(scope_index);
        let line = if marks.is_empty() {
            format!("{name:<name_width$}  {directory}")
        } else {
            format!("{name:<name_width$}  {directory:<directory_width$}  {marks}")
        };
        writeln!(output, "{line}")?;
    }
    Ok(SUCCESS)
}

fn list_marks(profiles_root: &Path, name: &str) -> Vec<&'static str> {
    let mut marks = Vec::new();
    if name == DEFAULT_PROFILE_NAME {
        marks.push(IMPLICIT_DEFAULT_MARK);
    }
    if !is_valid_profile_name(name) {
        marks.push(INVALID_NAME_MARK);
        return marks;
    }
    let config_path = profile_config_path(profiles_root, name);
    if !config_path.exists() {
        marks.push(NO_CONFIG_MARK);
    } else if config_problem(&config_path).is_some() {
        marks.push(INVALID_CONFIG_MARK);
    }
    marks
}

fn create(
    scope: &ProfileScope,
    environment: &ProfileEnvironment,
    name: &str,
    force: bool,
    shim_name: Option<&str>,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    let shim_directories = scope.shim_directories();
    let profile_directory = shim_directories.profiles_root.join(name);
    let config_path = profile_config_path(&shim_directories.profiles_root, name);
    let environment_path = profile_env_path(&shim_directories.profiles_root, name);
    if config_path.exists() {
        if let Some(problem) = config_problem(&config_path) {
            return refuse(
                diagnostics,
                &format!("{problem}; fix it before creating {name:?}"),
            );
        }
    }
    if (config_path.exists() || environment_path.exists()) && !force {
        return refuse(
            diagnostics,
            &format!(
                "profile {name:?} already has config.toml or .env; use --force to overwrite them"
            ),
        );
    }

    let config_text = match shim_name {
        Some(shim_name) => config_text_with_shim_name(DEFAULT_PROFILE_STARTER, shim_name),
        None => DEFAULT_PROFILE_STARTER.to_string(),
    };
    let pending = PendingProfile {
        name: name.to_string(),
        config_text: config_text.clone(),
    };
    let shim_plan = plan_shims(&shim_directories, Some(&pending))?;

    let mut report = InitReport {
        root: profile_directory.clone(),
        written: Vec::new(),
        skipped: Vec::new(),
    };
    seed_profile_starters(&profile_directory, force, &config_text, &mut report)?;
    if let ProfileScope::Workspace {
        workspace_config_root,
    } = scope
    {
        seed_workspace_gitignore(workspace_config_root, false, &mut report)?;
    }
    apply_shim_plan(&shim_directories, &shim_plan)?;

    writeln!(
        output,
        "Created profile {name:?} at {}",
        display_path(&profile_directory)
    )?;
    for path in &report.written {
        writeln!(output, "Wrote {}", display_path(path))?;
    }
    for path in &report.skipped {
        writeln!(output, "Skipped {} (already exists)", display_path(path))?;
    }
    let mut created_shim = false;
    for shim in shim_plan
        .shims
        .iter()
        .filter(|shim| shim.profile_name == name)
    {
        writeln!(output, "Shim {}", display_path(&shim.path))?;
        created_shim = true;
    }
    report_removed_shims(output, &shim_plan)?;
    if created_shim {
        write_path_hint(
            output,
            &shim_directories.shims_directory,
            environment.path_variable.as_deref(),
        )?;
    }
    Ok(SUCCESS)
}

fn delete(
    scope: &ProfileScope,
    name: &str,
    dry_run: bool,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    let shim_directories = scope.shim_directories();
    let profile_directory = shim_directories.profiles_root.join(name);
    if !profile_directory.is_dir() {
        return refuse(
            diagnostics,
            &format!(
                "profile {name:?} does not exist at {}",
                display_path(&profile_directory)
            ),
        );
    }
    if let Err(error) = plan_shims(&shim_directories, None) {
        return refuse(diagnostics, &format!("{error:#}; nothing was deleted"));
    }
    let display_directory = display_path(&profile_directory);
    if dry_run {
        writeln!(output, "would delete {name:?} at {display_directory}")?;
        return Ok(SUCCESS);
    }
    std::fs::remove_dir_all(&profile_directory)?;
    let shim_plan = plan_shims(&shim_directories, None)?;
    apply_shim_plan(&shim_directories, &shim_plan)?;
    writeln!(output, "Deleted profile {name:?} at {display_directory}")?;
    report_removed_shims(output, &shim_plan)?;
    Ok(SUCCESS)
}

fn edit(
    scope: &ProfileScope,
    name: &str,
    environment: &ProfileEnvironment,
    launcher: &dyn EditorLauncher,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    let shim_directories = scope.shim_directories();
    let config_path = profile_config_path(&shim_directories.profiles_root, name);
    if !config_path.is_file() {
        return refuse(
            diagnostics,
            &format!("profile {name:?} has no config.toml; run `ylva profile create {name}` first"),
        );
    }
    let Some(editor_command) = environment.editor.command() else {
        return refuse(
            diagnostics,
            "neither VISUAL nor EDITOR is set; export one of them to choose an editor",
        );
    };
    if !launcher.launch(editor_command, &config_path)? {
        writeln!(diagnostics, "error: editor exited with a non-zero status")?;
        return Ok(GENERAL_ERROR);
    }
    let shim_plan = plan_shims(&shim_directories, None)?;
    apply_shim_plan(&shim_directories, &shim_plan)?;
    report_removed_shims(output, &shim_plan)?;
    Ok(SUCCESS)
}

fn shims(
    scope: &ProfileScope,
    environment: &ProfileEnvironment,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    if !cfg!(unix) {
        return refuse(diagnostics, "profile shims is only supported on Unix");
    }
    let shim_directories = scope.shim_directories();
    let shim_plan = plan_shims(&shim_directories, None)?;
    apply_shim_plan(&shim_directories, &shim_plan)?;
    report_removed_shims(output, &shim_plan)?;
    if shim_plan.shims.is_empty() {
        writeln!(
            output,
            "No shims in {}",
            display_path(&shim_directories.shims_directory)
        )?;
        return Ok(SUCCESS);
    }
    let name_width = shim_plan
        .shims
        .iter()
        .map(|shim| shim.profile_name.chars().count())
        .max()
        .unwrap_or(0);
    for shim in &shim_plan.shims {
        writeln!(
            output,
            "{:<name_width$}  {}",
            shim.profile_name,
            display_path(&shim.path)
        )?;
    }
    write_path_hint(
        output,
        &shim_directories.shims_directory,
        environment.path_variable.as_deref(),
    )?;
    Ok(SUCCESS)
}

/// Prints the `#` comment lines that explain how to put `shims_directory` on `PATH`.
fn write_path_hint(
    output: &mut dyn Write,
    shims_directory: &Path,
    path_variable: Option<&OsStr>,
) -> io::Result<()> {
    let shims_directory_display = display_path(shims_directory);
    writeln!(output, "# Update your PATH to use shims:")?;
    writeln!(output, "# {}", path_export_line(shims_directory))?;
    if is_on_path(shims_directory, path_variable) {
        writeln!(output, "# {shims_directory_display} is already on PATH")
    } else {
        writeln!(output, "# {shims_directory_display} is not on PATH")
    }
}

fn report_removed_shims(output: &mut dyn Write, shim_plan: &ShimPlan) -> io::Result<()> {
    for path in &shim_plan.prunes {
        writeln!(output, "Removed {}", display_path(path))?;
    }
    Ok(())
}

/// Describes why `config_path` cannot be used as a profile config, or `None` when it can.
fn config_problem(config_path: &Path) -> Option<String> {
    let text = match std::fs::read_to_string(config_path) {
        Ok(text) => text,
        Err(error) => {
            return Some(format!(
                "cannot read {}: {error}",
                display_path(config_path)
            ));
        }
    };
    match toml::from_str::<toml::Table>(&text) {
        Ok(_) => None,
        Err(error) => Some(format!(
            "{} is not valid TOML: {error}",
            display_path(config_path)
        )),
    }
}

fn refuse(diagnostics: &mut dyn Write, message: &str) -> anyhow::Result<u8> {
    writeln!(diagnostics, "error: {message}")?;
    Ok(USAGE_ERROR)
}

fn workspace_requested(command: &ProfileCommand) -> bool {
    match command {
        ProfileCommand::List { workspace }
        | ProfileCommand::Create { workspace, .. }
        | ProfileCommand::Delete { workspace, .. }
        | ProfileCommand::Edit { workspace, .. }
        | ProfileCommand::Shims { workspace } => *workspace,
    }
}

fn command_name(command: &ProfileCommand) -> Option<&str> {
    match command {
        ProfileCommand::List { .. } | ProfileCommand::Shims { .. } => None,
        ProfileCommand::Create { name, .. }
        | ProfileCommand::Delete { name, .. }
        | ProfileCommand::Edit { name, .. } => Some(name.as_str()),
    }
}

fn resolve_scope(workspace: bool) -> anyhow::Result<Option<ProfileScope>> {
    if !workspace {
        let ylva_home = ylva_home().ok_or(ConfigError::HomeDirectoryNotFound)?;
        return Ok(Some(ProfileScope::Home { ylva_home }));
    }
    let current_directory = std::env::current_dir()?;
    Ok(workspace_scope(discover_workspace_root(&current_directory)))
}

fn workspace_scope(workspace_root: Option<PathBuf>) -> Option<ProfileScope> {
    workspace_root.map(|root| ProfileScope::Workspace {
        workspace_config_root: root.join(YLVA_DIR_NAME),
    })
}

#[cfg(test)]
mod tests;
