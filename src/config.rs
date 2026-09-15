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

use crate::ylva_home::ylva_home;

mod agent;
pub mod layered;
mod profile;
mod provider;
pub mod soul;

pub use agent::AgentConfig;
pub use profile::{profile_config_source_path, ProfileConfig};
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
/// profile TOML.
pub fn load_profile_environment(
    profile_env_path: Option<&Path>,
) -> Result<HashMap<String, String>, ConfigError> {
    let mut values = HashMap::new();
    if let Some(path) = profile_env_path {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(process_environment_values());
            }
            Err(source) => {
                return Err(ConfigError::EnvironmentUnreadable {
                    path: path.to_path_buf(),
                    source,
                });
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
    }
    values.extend(process_environment_values());
    let environment_path = profile_env_path
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    tracing::info!(
        path = %environment_path,
        variables = values.len(),
        "profile environment loaded"
    );
    Ok(values)
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
}
