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

use std::path::{Path, PathBuf};

use crate::config::soul::DEFAULT_SOUL;
use crate::config::{
    ConfigError, DEFAULT_PROFILE_NAME, PROFILES_DIRECTORY_NAME, PROFILE_CONFIG_FILE_NAME,
};
use crate::ylva_home::YLVA_DIR_NAME;

const DEFAULT_PROFILE_STARTER: &str = r#"# Ylva profile configuration.
# This file is a starter. Uncomment and fill in the provider connection and
# engine examples below when this profile needs runtime settings.

default_engine = "default"
theme = "dark"

# [provider_connections.example]
# provider = "ollama"
# api_base_url = "https://example.invalid"

# [engines.default]
# provider_connection = "example"
# model = "replace-me"
"#;

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
            self.root.display()
        )];
        for path in &self.written {
            lines.push(format!("Wrote {}", path.display()));
        }
        for path in &self.skipped {
            lines.push(format!("Skipped {} (already exists)", path.display()));
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
    for (relative_path, contents) in home_files() {
        seed_file(ylva_home, &relative_path, contents, force, &mut report)?;
    }
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
    seed_file(
        &workspace_config_root,
        &default_profile_config_relative_path(),
        DEFAULT_PROFILE_STARTER,
        force,
        &mut report,
    )?;
    Ok(report)
}

fn home_files() -> [(PathBuf, &'static str); 2] {
    [
        (PathBuf::from("SOUL.md"), DEFAULT_SOUL),
        (
            default_profile_config_relative_path(),
            DEFAULT_PROFILE_STARTER,
        ),
    ]
}

fn default_profile_config_relative_path() -> PathBuf {
    PathBuf::from(PROFILES_DIRECTORY_NAME)
        .join(DEFAULT_PROFILE_NAME)
        .join(PROFILE_CONFIG_FILE_NAME)
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

    #[test]
    fn home_init_seeds_the_standard_files_and_is_idempotent_without_force() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let first = run_init(temporary_directory.path(), false).expect("initialization succeeds");
        assert_eq!(first.written.len(), home_files().len());
        assert!(first.skipped.is_empty());

        let second =
            run_init(temporary_directory.path(), false).expect("second initialization succeeds");
        assert!(second.written.is_empty());
        assert_eq!(second.skipped.len(), home_files().len());
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
    fn workspace_init_seeds_only_the_shared_profile_starter() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");

        let report = run_init_workspace(temporary_directory.path(), false)
            .expect("workspace initialization");
        let expected = temporary_directory
            .path()
            .join(YLVA_DIR_NAME)
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME)
            .join(PROFILE_CONFIG_FILE_NAME);

        assert_eq!(report.written, vec![expected.clone()]);
        assert!(expected.is_file());
        assert!(!temporary_directory.path().join(".ylva/SOUL.md").exists());
        assert!(std::fs::read_to_string(expected)
            .expect("read placeholder")
            .contains("example.invalid"));
    }
}
