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

//! Shell shims: one executable per profile that runs `ylva --profile <name>`.
//!
//! The shims directory belongs to Ylva. [`plan_shims`] decides every change from the
//! profiles' `config.toml` files before [`apply_shim_plan`] writes or removes anything.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};

use crate::config::{is_valid_profile_name, profile_config_path, DEFAULT_PROFILE_NAME};
use crate::home_relative_path::display_path;

pub const SHIMS_DIRECTORY_NAME: &str = "shims";
pub const SHIM_NAME_KEY: &str = "shim_name";
const SHIM_FILE_NAME_PREFIX: &str = "ylva-";
const YLVA_PROGRAM_NAME: &str = "ylva";

const WORKSPACE_PREAMBLE: &str = r#"cd "$(dirname "$0")/../.." || exit 2
"#;

/// Whether `name` may name a shim file: a profile-style name other than `ylva`.
pub fn is_valid_shim_name(name: &str) -> bool {
    is_valid_profile_name(name) && name != YLVA_PROGRAM_NAME
}

/// Which shell script a scope's shims use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimTemplate {
    /// Shims in the Ylva home's `shims/` that run `ylva` in the caller's environment.
    Home,
    /// Shims in `<workspace>/.ylva/shims/` that `cd` to the workspace root first, then
    /// run `ylva` in the caller's environment.
    Workspace,
}

/// Where one scope keeps its profiles and shims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimDirectories {
    pub profiles_root: PathBuf,
    pub shims_directory: PathBuf,
    pub template: ShimTemplate,
}

/// A profile whose `config.toml` is about to be written, so its claim counts before any write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingProfile {
    pub name: String,
    pub config_text: String,
}

/// One shim to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedShim {
    pub profile_name: String,
    pub path: PathBuf,
    pub script: String,
}

/// Every file change a reconcile makes, computed before any of them is applied.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ShimPlan {
    /// Shims to write, sorted by file name.
    pub shims: Vec<PlannedShim>,
    /// Unclaimed regular files to remove, sorted by path.
    pub prunes: Vec<PathBuf>,
}

/// Compute the shim plan for a scope. Fails, changing nothing, on any config problem.
///
/// `pending` stands in for a profile whose `config.toml` is about to be written.
pub fn plan_shims(
    directories: &ShimDirectories,
    pending: Option<&PendingProfile>,
) -> anyhow::Result<ShimPlan> {
    if !cfg!(unix) {
        return Ok(ShimPlan::default());
    }
    let mut profile_names: BTreeSet<String> = profile_directory_names(&directories.profiles_root)
        .with_context(|| format!("cannot list {}", display_path(&directories.profiles_root)))?
        .into_iter()
        .collect();
    if let Some(pending) = pending {
        profile_names.insert(pending.name.clone());
    }

    let mut claims: BTreeMap<String, String> = BTreeMap::new();
    for profile_name in &profile_names {
        let config_path = profile_config_path(&directories.profiles_root, profile_name);
        let table = match pending.filter(|pending| pending.name == *profile_name) {
            Some(pending) => parse_config_table(&config_path, &pending.config_text)?,
            None => read_config_table(&config_path)?,
        };
        if !is_valid_profile_name(profile_name) {
            continue;
        }
        let explicit_shim_name = explicit_shim_name(&config_path, &table)?;
        if profile_name == DEFAULT_PROFILE_NAME {
            if explicit_shim_name.is_some() {
                bail!(
                    "{} sets {SHIM_NAME_KEY}, but the default profile has no shim",
                    display_path(&config_path)
                );
            }
            continue;
        }
        let file_name = match explicit_shim_name {
            Some(shim_name) => {
                if !is_valid_shim_name(&shim_name) {
                    bail!(
                        "invalid {SHIM_NAME_KEY} {shim_name:?} in {}; use letters, digits, '-' or '_', not 'ylva'",
                        display_path(&config_path)
                    );
                }
                shim_name
            }
            None => format!("{SHIM_FILE_NAME_PREFIX}{profile_name}"),
        };
        if let Some(other_profile_name) = claims.insert(file_name.clone(), profile_name.clone()) {
            bail!(
                "profiles {other_profile_name} and {profile_name} both claim the shim {file_name}; give one of them a different {SHIM_NAME_KEY}"
            );
        }
    }

    let mut shims = Vec::new();
    for (file_name, profile_name) in &claims {
        let path = directories.shims_directory.join(file_name);
        ensure_regular_file_or_absent(&path)?;
        shims.push(PlannedShim {
            profile_name: profile_name.clone(),
            path,
            script: render_script(directories.template, profile_name),
        });
    }
    let prunes = unclaimed_regular_files(&directories.shims_directory, &claims)?;
    Ok(ShimPlan { shims, prunes })
}

/// Write the planned shims as executable files, then remove the pruned ones.
pub fn apply_shim_plan(directories: &ShimDirectories, plan: &ShimPlan) -> anyhow::Result<()> {
    if !plan.shims.is_empty() {
        std::fs::create_dir_all(&directories.shims_directory).with_context(|| {
            format!(
                "cannot create {}",
                display_path(&directories.shims_directory)
            )
        })?;
    }
    for shim in &plan.shims {
        std::fs::write(&shim.path, &shim.script)
            .with_context(|| format!("cannot write {}", display_path(&shim.path)))?;
        make_executable(&shim.path)
            .with_context(|| format!("cannot make {} executable", display_path(&shim.path)))?;
    }
    for path in &plan.prunes {
        std::fs::remove_file(path)
            .with_context(|| format!("cannot remove {}", display_path(path)))?;
    }
    Ok(())
}

/// Prepend a `shim_name` key to a starter `config.toml`, ahead of any table header.
pub fn config_text_with_shim_name(starter_text: &str, shim_name: &str) -> String {
    format!("{SHIM_NAME_KEY} = \"{shim_name}\"\n{starter_text}")
}

/// Whether `shims_directory` is one of the entries in `path_variable`.
pub fn is_on_path(shims_directory: &Path, path_variable: Option<&OsStr>) -> bool {
    path_variable.is_some_and(|value| {
        std::env::split_paths(value).any(|entry| entry.as_path() == shims_directory)
    })
}

/// Shell line that puts `shims_directory` on `PATH`.
pub fn path_export_line(shims_directory: &Path) -> String {
    export_path_line(&display_path(shims_directory))
}

/// `export` line for a display path. A leading `~/` stays unquoted so the shell expands it.
fn export_path_line(directory: &str) -> String {
    if let Some(rest) = directory.strip_prefix("~/") {
        return format!("export PATH=~/{}:\"$PATH\"", shell_word(rest));
    }
    if directory.chars().all(is_plain_path_character) {
        format!("export PATH=\"{directory}:$PATH\"")
    } else {
        format!("export PATH={}:\"$PATH\"", single_quote(directory))
    }
}

/// Sorted names of the directories directly below `profiles_root`; empty when it is missing.
pub fn profile_directory_names(profiles_root: &Path) -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(profiles_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

fn explicit_shim_name(config_path: &Path, table: &toml::Table) -> anyhow::Result<Option<String>> {
    let Some(value) = table.get(SHIM_NAME_KEY) else {
        return Ok(None);
    };
    match value.as_str() {
        Some(shim_name) => Ok(Some(shim_name.to_string())),
        None => bail!(
            "{SHIM_NAME_KEY} in {} must be a string",
            display_path(config_path)
        ),
    }
}

fn read_config_table(config_path: &Path) -> anyhow::Result<toml::Table> {
    match std::fs::read_to_string(config_path) {
        Ok(text) => parse_config_table(config_path, &text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(error) => {
            Err(error).with_context(|| format!("cannot read {}", display_path(config_path)))
        }
    }
}

fn parse_config_table(config_path: &Path, text: &str) -> anyhow::Result<toml::Table> {
    toml::from_str(text).with_context(|| format!("{} is not valid TOML", display_path(config_path)))
}

fn ensure_regular_file_or_absent(path: &Path) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => bail!(
            "{} exists and is not a regular file; move it aside before reconciling shims",
            display_path(path)
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot inspect {}", display_path(path))),
    }
}

fn unclaimed_regular_files(
    shims_directory: &Path,
    claims: &BTreeMap<String, String>,
) -> anyhow::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(shims_directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot list {}", display_path(shims_directory)))
        }
    };
    let mut prunes = Vec::new();
    for entry in entries {
        let entry = entry?;
        let is_claimed = entry
            .file_name()
            .to_str()
            .is_some_and(|file_name| claims.contains_key(file_name));
        if entry.file_type()?.is_file() && !is_claimed {
            prunes.push(entry.path());
        }
    }
    prunes.sort();
    Ok(prunes)
}

fn render_script(template: ShimTemplate, profile_name: &str) -> String {
    let preamble = match template {
        ShimTemplate::Home => "",
        ShimTemplate::Workspace => WORKSPACE_PREAMBLE,
    };
    format!("#!/bin/sh\n{preamble}exec ylva --profile '{profile_name}' \"$@\"\n")
}

fn is_plain_path_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
}

/// Leave plain text as-is; quote anything else for the shell.
fn shell_word(text: &str) -> String {
    if text.chars().all(is_plain_path_character) {
        text.to_string()
    } else {
        single_quote(text)
    }
}

fn single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
