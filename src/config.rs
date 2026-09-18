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

//! Runtime profile configuration and layered configuration resolution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ylva_home::{ylva_home, YLVA_DIR_NAME};

mod agent;
pub mod agent_type_file;
pub mod layered;
mod profile;
mod provider;
pub mod soul;

pub use agent::AgentConfig;
pub use agent_type_file::AgentTypeDefinition;
pub use profile::{profile_config_source_path, ProfileConfig, ToolSandboxConfig};
pub use provider::{ProviderConfig, ProviderConnectionConfig};

/// File name of a profile's configuration inside its profile directory.
pub const PROFILE_CONFIG_FILE_NAME: &str = "config.toml";

/// File name of a profile's optional environment file.
pub const PROFILE_ENV_FILE_NAME: &str = ".env";

/// Directory containing all named profile directories.
pub const PROFILES_DIRECTORY_NAME: &str = "profiles";

/// Profile used when none is named explicitly.
pub const DEFAULT_PROFILE_NAME: &str = "default";

/// Errors surfaced while loading a profile.
///
/// A missing config file for an **explicitly** named profile
/// (`--profile <name>`) is [`ConfigError::ProfileNotFound`] — a hard error,
/// never a silent fallback, because the user asked for that specific
/// profile. Only the *implicit* default profile (no `--profile` flag; see
/// [`ProfileConfig::load_or_default`]) treats a missing file as "not
/// configured yet" and falls back to [`ProfileConfig::default`]. An
/// unreadable or malformed file is always an error, named or not: silently
/// ignoring a config file the user did write would hide their intent.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("profile '{profile_name}' not found: no config file at {}", path.display())]
    ProfileNotFound { profile_name: String, path: PathBuf },
    #[error("cannot read configuration file at {}: {source}", path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed profile config at {}: {source}", path.display())]
    Malformed {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("malformed agent-type file at {}: {message}", path.display())]
    MalformedAgentType { path: PathBuf, message: String },
    #[error(
        "agent type '{agent_type}' has conflicting {scope}-scope files at {} and {}",
        first_path.display(),
        second_path.display()
    )]
    ConflictingAgentTypeFiles {
        agent_type: String,
        scope: String,
        first_path: PathBuf,
        second_path: PathBuf,
    },
    #[error(
        "agent type '{agent_type}' has an ambiguous {scope}-scope definition: \
         inline table at {} conflicts with file {}"
        ,
        toml_path.display(),
        file_path.display()
    )]
    AmbiguousAgentTypeDefinition {
        agent_type: String,
        scope: String,
        toml_path: PathBuf,
        file_path: PathBuf,
    },
    #[error("cannot read profile environment at {}: {source}", path.display())]
    EnvironmentUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed profile environment at {}: {message}", path.display())]
    EnvironmentMalformed { path: PathBuf, message: String },
    #[error(
        "profile config at {} references missing environment variable '{name}'",
        path.display()
    )]
    MissingEnvironmentVariable { path: PathBuf, name: String },
    #[error(
        "profile config at {} contains invalid environment variable reference '{reference}'",
        path.display()
    )]
    InvalidEnvironmentReference { path: PathBuf, reference: String },
    #[error(
        "profile config at {} has an invalid sandbox rule for tool '{tool}' \
         ([tool.{tool}].rules[{index}]): expected {expected}",
        path.display()
    )]
    InvalidSandboxRuleKind {
        path: PathBuf,
        tool: String,
        index: usize,
        expected: String,
    },
    #[error(
        "profile config at {} has a misplaced wildcard in [tool.{tool}].rules[{index}].command: \
         `*`/`**` only match trailing arguments, so they may only appear as the last element",
        path.display()
    )]
    MisplacedCommandGlobWildcard {
        path: PathBuf,
        tool: String,
        index: usize,
    },
    #[error("cannot resolve the home directory for ~/.ylva")]
    HomeDirectoryNotFound,
}

/// `~/.ylva/profiles`, the production root of all profile directories.
///
/// The Ylva home comes from [`ylva_home`]: the dedicated `YLVA_HOME`
/// environment variable before the operating system's home directory.
pub fn default_profiles_root() -> Result<PathBuf, ConfigError> {
    let home = ylva_home().ok_or(ConfigError::HomeDirectoryNotFound)?;
    Ok(home.join(PROFILES_DIRECTORY_NAME))
}

/// Path of one profile's configuration file.
pub fn profile_config_path(profiles_root: &Path, profile_name: &str) -> PathBuf {
    profiles_root
        .join(profile_name)
        .join(PROFILE_CONFIG_FILE_NAME)
}

/// Path of one profile's optional environment file.
pub fn profile_env_path(profiles_root: &Path, profile_name: &str) -> PathBuf {
    profiles_root.join(profile_name).join(PROFILE_ENV_FILE_NAME)
}

/// Load the environment visible to a profile.
///
/// Values from the profile-local `.env` file are used as defaults and
/// exported process values always win, including explicitly exported empty
/// values. The returned map is also used for `${NAME}` interpolation in the
/// profile TOML. This compatibility wrapper loads only the supplied home
/// profile path; startup uses [`load_profile_environment_at_workspace_root`]
/// to add the project-scoped file.
pub fn load_profile_environment(
    profile_env_path: Option<&Path>,
) -> Result<HashMap<String, String>, ConfigError> {
    load_profile_environment_at_workspace_root(profile_env_path, None, DEFAULT_PROFILE_NAME)
}

/// Load a profile's home and project environment files plus process variables.
///
/// The merge order is home `.env`, project `.env`, then exported process
/// variables. Later values win, so project values override home values while
/// process variables retain their existing highest precedence.
pub fn load_profile_environment_at_workspace_root(
    profile_env_path: Option<&Path>,
    workspace_root: Option<&Path>,
    profile_name: &str,
) -> Result<HashMap<String, String>, ConfigError> {
    let mut values = HashMap::new();
    if let Some(path) = profile_env_path {
        load_dotenv_file(path, &mut values)?;
    }
    if let Some(workspace_root) = workspace_root {
        let project_environment_path = workspace_root
            .join(YLVA_DIR_NAME)
            .join(PROFILES_DIRECTORY_NAME)
            .join(profile_name)
            .join(PROFILE_ENV_FILE_NAME);
        load_dotenv_file(&project_environment_path, &mut values)?;
    }
    values.extend(process_environment_values());
    let environment_path = profile_env_path.map(|path| path.display().to_string());
    tracing::info!(
        home_path = ?environment_path,
        project = workspace_root.is_some(),
        variables = values.len(),
        "profile environment loaded"
    );
    Ok(values)
}

fn load_dotenv_file(path: &Path, values: &mut HashMap<String, String>) -> Result<(), ConfigError> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ConfigError::EnvironmentUnreadable {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    if !metadata.is_file() {
        return Err(ConfigError::EnvironmentMalformed {
            path: path.to_path_buf(),
            message: "path is not a file".to_string(),
        });
    }

    let entries =
        dotenvy::from_path_iter(path).map_err(|error| ConfigError::EnvironmentMalformed {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    for entry in entries {
        let (name, value) = entry.map_err(|error| ConfigError::EnvironmentMalformed {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        values.insert(name, value);
    }
    Ok(())
}

fn process_environment_values() -> HashMap<String, String> {
    std::env::vars().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_environment_path_is_next_to_profile_config() {
        let profiles_root = Path::new("/tmp/ylva/profiles");

        assert_eq!(
            profile_env_path(profiles_root, "work"),
            PathBuf::from("/tmp/ylva/profiles/work/.env")
        );
    }

    #[test]
    fn project_profile_environment_overrides_home_profile_environment() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let home_profiles_root = temporary_directory.path().join("home/profiles");
        let home_environment = profile_env_path(&home_profiles_root, "work");
        std::fs::create_dir_all(home_environment.parent().expect("home environment parent"))
            .expect("create home environment directory");
        std::fs::write(&home_environment, "SHARED=home\nHOME_ONLY=home\n")
            .expect("write home environment");

        let workspace_root = temporary_directory.path().join("project");
        let project_environment = workspace_root
            .join(YLVA_DIR_NAME)
            .join(PROFILES_DIRECTORY_NAME)
            .join("work")
            .join(PROFILE_ENV_FILE_NAME);
        std::fs::create_dir_all(
            project_environment
                .parent()
                .expect("project environment parent"),
        )
        .expect("create project environment directory");
        std::fs::write(
            &project_environment,
            "SHARED=project\nPROJECT_ONLY=project\n",
        )
        .expect("write project environment");

        let values = load_profile_environment_at_workspace_root(
            Some(&home_environment),
            Some(&workspace_root),
            "work",
        )
        .expect("load merged environment");

        assert_eq!(values.get("SHARED").map(String::as_str), Some("project"));
        assert_eq!(values.get("HOME_ONLY").map(String::as_str), Some("home"));
        assert_eq!(
            values.get("PROJECT_ONLY").map(String::as_str),
            Some("project")
        );
    }
}
