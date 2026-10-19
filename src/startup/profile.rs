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

//! Selected-profile loading for startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::home_relative_path::display_path;
use crate::{Cli, ProfileConfig};

/// Load the selected profile, or return exit-worthy error text.
///
/// An explicitly named profile (`--profile <name>`) with no config file is a
/// hard error: the user asked for that specific profile, so silently
/// substituting the in-code default would hide a real typo/misconfiguration.
/// Only the implicit default profile (no `--profile` flag at all) tolerates a
/// missing file, so Ylva still runs out of the box without ever writing one.
pub(super) fn load_profile(
    explicit_profile_name: Option<&str>,
    environment: &HashMap<String, String>,
    workspace_root: Option<&Path>,
) -> Result<ProfileConfig, String> {
    match explicit_profile_name {
        Some(profile_name) => {
            let profiles_root =
                crate::default_profiles_root().map_err(|error| error.to_string())?;
            ProfileConfig::load_with_environment_at_workspace_root(
                &profiles_root,
                profile_name,
                environment,
                workspace_root,
            )
            .map_err(|error| error.to_string())
        }
        None => {
            let profiles_root = match crate::default_profiles_root() {
                Ok(profiles_root) => profiles_root,
                Err(error) => {
                    tracing::warn!(%error, "no home directory; using the default profile");
                    return Ok(ProfileConfig::default());
                }
            };
            ProfileConfig::load_or_default_with_environment_at_workspace_root(
                &profiles_root,
                crate::DEFAULT_PROFILE_NAME,
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
pub(super) fn selected_profile_context(
    explicit_profile_name: Option<&str>,
    workspace_root: Option<&Path>,
) -> String {
    let profile_name = explicit_profile_name.unwrap_or(crate::DEFAULT_PROFILE_NAME);
    match selected_profile_path(explicit_profile_name, workspace_root) {
        Ok(Some(path)) => format!("profile '{profile_name}' at {}", display_path(&path)),
        Ok(None) => format!("profile '{profile_name}' (built-in default)"),
        Err(error) => format!("profile '{profile_name}' (config path unavailable: {error})"),
    }
}

/// Resolve the profile config path selected by the same layering used to load
/// the active profile.
pub(super) fn selected_profile_path(
    explicit_profile_name: Option<&str>,
    workspace_root: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    let profile_name = explicit_profile_name.unwrap_or(crate::DEFAULT_PROFILE_NAME);
    let profiles_root = match crate::default_profiles_root() {
        Ok(profiles_root) => profiles_root,
        Err(error) if explicit_profile_name.is_some() => {
            return Err(error.to_string());
        }
        Err(_) => return Ok(None),
    };
    crate::profile_config_source_path(&profiles_root, profile_name, workspace_root)
        .map_err(|error| error.to_string())
}

/// Load the merged environment belonging to the selected profile.
///
/// Missing `.env` files are normal; malformed or unusable files are startup
/// errors. Process environment values take precedence over profile-file
/// values.
pub(super) fn load_selected_profile_environment(
    explicit_profile_name: Option<&str>,
    workspace_root: Option<&Path>,
) -> Result<HashMap<String, String>, String> {
    let profile_name = explicit_profile_name.unwrap_or(crate::DEFAULT_PROFILE_NAME);
    let profile_env_path = match crate::default_profiles_root() {
        Ok(root) => Some(crate::profile_env_path(&root, profile_name)),
        Err(error) if explicit_profile_name.is_some() => {
            return Err(format!("cannot resolve profile environment: {error}"));
        }
        Err(error) => {
            tracing::warn!(%error, "no home directory; using process environment");
            None
        }
    };
    crate::load_profile_environment_at_workspace_root(
        profile_env_path.as_deref(),
        workspace_root,
        profile_name,
    )
    .map_err(|error| format!("cannot load profile environment: {error}"))
}

pub(super) fn apply_cli_profile_overrides(cli: &Cli, profile: &mut ProfileConfig) {
    if let Some(max_concurrent_subagents) = cli.max_concurrent_subagents {
        profile.apply_max_concurrent_subagents_override(max_concurrent_subagents);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cli_max_concurrent_subagents_overrides_profile_and_zero_means_unlimited() {
        let mut profile = ProfileConfig {
            max_concurrent_subagents: Some(2),
            ..ProfileConfig::default()
        };
        let cli = Cli::try_parse_from(["ylva", "--max-concurrent-subagents", "7"])
            .expect("positive override parses");

        apply_cli_profile_overrides(&cli, &mut profile);

        assert_eq!(profile.max_concurrent_subagents_limit(), Some(7));

        let unlimited_cli = Cli::try_parse_from(["ylva", "--max-concurrent-subagents", "0"])
            .expect("zero override parses");
        apply_cli_profile_overrides(&unlimited_cli, &mut profile);
        assert_eq!(profile.max_concurrent_subagents_limit(), None);
    }
}
