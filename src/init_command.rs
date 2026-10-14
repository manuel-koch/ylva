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

//! The `ylva init` file-seeding command.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::soul::DEFAULT_SOUL;
use crate::config::{
    ConfigError, DEFAULT_PROFILE_NAME, PROFILES_DIRECTORY_NAME, PROFILE_CONFIG_FILE_NAME,
    PROFILE_ENV_FILE_NAME,
};
use crate::home_relative_path::display_path;
use crate::ylva_home::YLVA_DIR_NAME;

pub(crate) const DEFAULT_PROFILE_STARTER: &str = r#"# Ylva profile configuration.
# This file is a starter. Uncomment and fill in the provider connection and
# engine examples below when this profile needs engine-specific settings.

default_engine = "default"
theme = "dark"

# [provider_connections.example-ollama-local]
# provider = "ollama"
# api_base_url = "http://localhost:11434"

# [provider_connections.example-ollama-cloud]
# provider = "ollama"
# api_base_url = "https://ollama.com"
# put this envvar into the .env file of the profile
# api_key = "${OLLAMA_CLOUD_API_KEY}"

# [engines.default]
# provider_connection = "example-ollama-local"
# model = "replace-me"
"#;

const DEFAULT_PROFILE_ENV_STARTER: &str = r#"# Local provider secrets and other environment values for this profile.
# Examples: API keys, access tokens, and custom provider endpoints.
# Do not commit this file.
# Exported process variables take precedence over values in this file.
"#;

const WORKSPACE_GITIGNORE_HEADER: &str =
    "# Keep ylva profile environment files and chat transcripts out of version control.\n";

/// Entries every workspace `.ylva/.gitignore` must contain.
const WORKSPACE_GITIGNORE_ENTRIES: [&str; 3] = [".env", ".envrc", "chats/"];

fn workspace_gitignore_starter() -> String {
    let mut starter = String::from(WORKSPACE_GITIGNORE_HEADER);
    for entry in WORKSPACE_GITIGNORE_ENTRIES.iter() {
        starter.push_str(entry);
        starter.push('\n');
    }
    starter
}

/// Files written or skipped by one init operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    /// Directory under which the seeded files were placed.
    pub root: PathBuf,
    /// Files written by this operation.
    pub written: Vec<PathBuf>,
    /// Existing files left untouched because `force` was false.
    pub skipped: Vec<PathBuf>,
}

impl InitReport {
    /// Render a concise user-facing summary.
    pub fn summary(&self) -> String {
        let mut lines = vec![format!(
            "Initialized Ylva configuration in {}",
            display_path(&self.root)
        )];
        for path in &self.written {
            lines.push(format!("Wrote {}", display_path(path)));
        }
        for path in &self.skipped {
            lines.push(format!("Skipped {} (already exists)", display_path(path)));
        }
        if self.written.is_empty() && self.skipped.is_empty() {
            lines.push("No files were selected".to_string());
        }
        lines.join("\n")
    }
}

/// Seed the code-owned starters into the Ylva home.
pub fn run_init(ylva_home: &Path, force: bool) -> Result<InitReport, ConfigError> {
    let mut report = InitReport {
        root: ylva_home.to_path_buf(),
        written: Vec::new(),
        skipped: Vec::new(),
    };
    seed_file(
        ylva_home,
        Path::new("SOUL.md"),
        DEFAULT_SOUL,
        force,
        &mut report,
    )?;
    seed_profile_starters(
        &ylva_home
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME),
        force,
        DEFAULT_PROFILE_STARTER,
        &mut report,
    )?;
    Ok(report)
}

/// Seed the workspace-local default profile beneath `<workspace_root>/.ylva/`.
pub fn run_init_workspace(workspace_root: &Path, force: bool) -> Result<InitReport, ConfigError> {
    let workspace_config_root = workspace_root.join(YLVA_DIR_NAME);
    let mut report = InitReport {
        root: workspace_config_root.clone(),
        written: Vec::new(),
        skipped: Vec::new(),
    };
    seed_profile_starters(
        &workspace_config_root
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME),
        force,
        DEFAULT_PROFILE_STARTER,
        &mut report,
    )?;
    seed_workspace_gitignore(&workspace_config_root, force, &mut report)?;
    Ok(report)
}

/// Seed the `config.toml` and `.env` starters into `profile_directory`.
///
/// `config_text` is written as `config.toml`. Each file is written only when
/// missing unless `force` is set. Shared by `init` (for `default`) and
/// `profile create` (for any name).
pub(crate) fn seed_profile_starters(
    profile_directory: &Path,
    force: bool,
    config_text: &str,
    report: &mut InitReport,
) -> Result<(), ConfigError> {
    seed_file(
        profile_directory,
        Path::new(PROFILE_CONFIG_FILE_NAME),
        config_text,
        force,
        report,
    )?;
    seed_file(
        profile_directory,
        Path::new(PROFILE_ENV_FILE_NAME),
        DEFAULT_PROFILE_ENV_STARTER,
        force,
        report,
    )
}

/// Seed `<workspace_config_root>/.gitignore` so profile `.env` files and chat
/// transcripts stay out of version control.
///
/// A missing file is written from the starter. An existing file is kept as is,
/// and only the entries it lacks are appended, so custom lines survive. With
/// `force`, the file is overwritten with the starter.
pub(crate) fn seed_workspace_gitignore(
    workspace_config_root: &Path,
    force: bool,
    report: &mut InitReport,
) -> Result<(), ConfigError> {
    let path = workspace_config_root.join(".gitignore");
    if force || !path.exists() {
        return seed_file(
            workspace_config_root,
            Path::new(".gitignore"),
            &workspace_gitignore_starter(),
            force,
            report,
        );
    }
    let existing_contents =
        std::fs::read_to_string(&path).map_err(|source| ConfigError::Unreadable {
            path: path.clone(),
            source,
        })?;
    let existing_lines: HashSet<&str> = existing_contents.lines().map(str::trim).collect();
    let missing_entries: Vec<&str> = WORKSPACE_GITIGNORE_ENTRIES
        .iter()
        .copied()
        .filter(|entry| !existing_lines.contains(entry))
        .collect();
    if missing_entries.is_empty() {
        report.skipped.push(path);
        return Ok(());
    }
    let mut updated_contents = existing_contents;
    if !updated_contents.is_empty() && !updated_contents.ends_with('\n') {
        updated_contents.push('\n');
    }
    for entry in missing_entries {
        updated_contents.push_str(entry);
        updated_contents.push('\n');
    }
    std::fs::write(&path, updated_contents).map_err(|source| ConfigError::Unreadable {
        path: path.clone(),
        source,
    })?;
    report.written.push(path);
    Ok(())
}

fn seed_file(
    root: &Path,
    relative_path: &Path,
    contents: &str,
    force: bool,
    report: &mut InitReport,
) -> Result<(), ConfigError> {
    let path = root.join(relative_path);
    if path.exists() && !force {
        report.skipped.push(path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Unreadable {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(&path, contents).map_err(|source| ConfigError::Unreadable {
        path: path.clone(),
        source,
    })?;
    report.written.push(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SOUL.md`, the default profile's `config.toml`, and its `.env`.
    const HOME_STARTER_FILE_COUNT: usize = 3;

    #[test]
    fn home_init_seeds_the_standard_files_and_is_idempotent_without_force() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let first = run_init(temporary_directory.path(), false).expect("initialization succeeds");
        assert_eq!(first.written.len(), HOME_STARTER_FILE_COUNT);
        assert!(first.skipped.is_empty());
        let environment_path = temporary_directory
            .path()
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME)
            .join(PROFILE_ENV_FILE_NAME);
        assert_eq!(
            std::fs::read_to_string(environment_path).expect("read environment starter"),
            DEFAULT_PROFILE_ENV_STARTER
        );
        let second =
            run_init(temporary_directory.path(), false).expect("second initialization succeeds");
        assert!(second.written.is_empty());
        assert_eq!(second.skipped.len(), HOME_STARTER_FILE_COUNT);
    }

    #[test]
    fn force_reseeds_an_existing_home_file() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        run_init(temporary_directory.path(), false).expect("initialization succeeds");
        let soul_path = temporary_directory.path().join("SOUL.md");
        std::fs::write(&soul_path, "edited").expect("edit soul");

        let report = run_init(temporary_directory.path(), true).expect("forced initialization");

        assert!(report.written.contains(&soul_path));
        assert_eq!(
            std::fs::read_to_string(soul_path).expect("read reseeded soul"),
            DEFAULT_SOUL
        );
    }

    #[test]
    fn workspace_init_seeds_the_profile_starters() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");

        let report = run_init_workspace(temporary_directory.path(), false)
            .expect("workspace initialization");
        let expected_config = temporary_directory
            .path()
            .join(YLVA_DIR_NAME)
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME)
            .join(PROFILE_CONFIG_FILE_NAME);
        let expected_environment = temporary_directory
            .path()
            .join(YLVA_DIR_NAME)
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME)
            .join(PROFILE_ENV_FILE_NAME);
        let expected_gitignore = temporary_directory
            .path()
            .join(YLVA_DIR_NAME)
            .join(".gitignore");

        assert_eq!(
            report.written,
            vec![
                expected_config.clone(),
                expected_environment.clone(),
                expected_gitignore.clone()
            ]
        );
        assert!(expected_config.is_file());
        assert!(!temporary_directory.path().join(".ylva/SOUL.md").exists());
        assert!(std::fs::read_to_string(expected_config)
            .expect("read placeholder")
            .contains("example-ollama-local"));
        assert_eq!(
            std::fs::read_to_string(expected_environment).expect("read environment starter"),
            DEFAULT_PROFILE_ENV_STARTER
        );
        assert!(std::fs::read_to_string(expected_gitignore)
            .expect("read workspace gitignore")
            .lines()
            .any(|line| line == ".env"));
    }

    #[test]
    fn workspace_gitignore_starter_lists_every_entry() {
        let starter = workspace_gitignore_starter();

        for entry in WORKSPACE_GITIGNORE_ENTRIES.iter() {
            assert!(
                starter.lines().any(|line| line == *entry),
                "missing {entry}"
            );
        }
    }

    #[test]
    fn workspace_gitignore_appends_missing_entries_and_keeps_custom_lines() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let workspace_config_root = temporary_directory.path().join(YLVA_DIR_NAME);
        std::fs::create_dir_all(&workspace_config_root).expect("create workspace config root");
        let gitignore_path = workspace_config_root.join(".gitignore");
        std::fs::write(&gitignore_path, "custom-entry\n.env\n").expect("write existing gitignore");
        let mut report = InitReport {
            root: workspace_config_root.clone(),
            written: Vec::new(),
            skipped: Vec::new(),
        };

        seed_workspace_gitignore(&workspace_config_root, false, &mut report)
            .expect("append succeeds");

        assert_eq!(
            std::fs::read_to_string(&gitignore_path).expect("read gitignore"),
            "custom-entry\n.env\n.envrc\nchats/\n"
        );
        assert_eq!(report.written, vec![gitignore_path.clone()]);
        assert!(report.skipped.is_empty());

        let mut second_report = InitReport {
            root: workspace_config_root.clone(),
            written: Vec::new(),
            skipped: Vec::new(),
        };
        seed_workspace_gitignore(&workspace_config_root, false, &mut second_report)
            .expect("second append succeeds");

        assert_eq!(
            std::fs::read_to_string(&gitignore_path).expect("read gitignore"),
            "custom-entry\n.env\n.envrc\nchats/\n"
        );
        assert!(second_report.written.is_empty());
        assert_eq!(second_report.skipped, vec![gitignore_path]);
    }

    #[test]
    fn workspace_gitignore_terminates_an_unterminated_last_line_before_appending() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let workspace_config_root = temporary_directory.path().join(YLVA_DIR_NAME);
        std::fs::create_dir_all(&workspace_config_root).expect("create workspace config root");
        let gitignore_path = workspace_config_root.join(".gitignore");
        std::fs::write(&gitignore_path, "custom-entry").expect("write existing gitignore");
        let mut report = InitReport {
            root: workspace_config_root.clone(),
            written: Vec::new(),
            skipped: Vec::new(),
        };

        seed_workspace_gitignore(&workspace_config_root, false, &mut report)
            .expect("append succeeds");

        assert_eq!(
            std::fs::read_to_string(gitignore_path).expect("read gitignore"),
            "custom-entry\n.env\n.envrc\nchats/\n"
        );
    }

    #[test]
    fn workspace_gitignore_force_overwrites_with_the_starter() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let workspace_config_root = temporary_directory.path().join(YLVA_DIR_NAME);
        std::fs::create_dir_all(&workspace_config_root).expect("create workspace config root");
        let gitignore_path = workspace_config_root.join(".gitignore");
        std::fs::write(&gitignore_path, "custom-entry\n").expect("write existing gitignore");
        let mut report = InitReport {
            root: workspace_config_root.clone(),
            written: Vec::new(),
            skipped: Vec::new(),
        };

        seed_workspace_gitignore(&workspace_config_root, true, &mut report)
            .expect("forced seed succeeds");

        assert_eq!(
            std::fs::read_to_string(gitignore_path).expect("read gitignore"),
            workspace_gitignore_starter()
        );
    }
}
