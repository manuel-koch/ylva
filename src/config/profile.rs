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
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::highlight::ThemeName;
use crate::llm::engine::EngineConfig;
use crate::ylva_home::YLVA_DIR_NAME;

use super::{
    layered::{current_workspace_root, resolve_file_at_paths, ResolvedFile},
    load_profile_environment, AgentConfig, ConfigError, ProviderConnectionConfig,
    DEFAULT_PROFILE_NAME, PROFILES_DIRECTORY_NAME, PROFILE_CONFIG_FILE_NAME,
};

/// Top-level runtime profile that groups engine selection and per-agent settings.
///
/// This is also the deserialization target of
/// `~/.ylva/profiles/<name>/config.toml` — see [`ProfileConfig::load`].
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileConfig {
    /// Name of the engine used for all agents unless overridden by
    /// `AgentConfig::engine`.
    #[serde(default = "default_engine_name")]
    pub default_engine: String,
    /// Name of the engine used for compact/summarisation tasks. When `None`,
    /// falls back to `default_engine`. Phase 10 wires this at runtime.
    #[serde(default)]
    pub compact_engine: Option<String>,
    /// Name of the isolated utility engine used for session-title generation.
    /// When `None`, falls back to `default_engine`.
    #[serde(default)]
    pub utility_engine: Option<String>,
    /// Named provider connections, keyed by the connection name referenced by
    /// an engine's `provider_connection` field.
    #[serde(default)]
    #[serde(alias = "providers")]
    pub provider_connections: HashMap<String, ProviderConnectionConfig>,
    /// Named engine definitions this profile provides, keyed by engine name.
    /// `AgentFactory::from_profile` turns each entry into a live engine. Empty
    /// means "no engines configured": the caller falls back to its
    /// environment-derived providers unless named provider connections are
    /// configured.
    #[serde(default)]
    pub engines: HashMap<String, EngineConfig>,
    /// Model used by an engine that does not name one of its own.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Syntax-highlighting theme used by the renderers.
    #[serde(default)]
    pub theme: ThemeName,
    /// Reserved key-to-action map. Accepted and validated as a table of
    /// strings so a profile carrying one still loads, but Phase 7 ships a
    /// fixed key map (`docs/keybindings.md`) and does not apply these
    /// entries yet.
    #[serde(default)]
    pub keybindings: HashMap<String, String>,
    /// Map of agent-type name to its configuration.
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,
}

fn default_engine_name() -> String {
    DEFAULT_PROFILE_NAME.to_string()
}

fn default_agent_config() -> AgentConfig {
    AgentConfig {
        engine: None,
        description: None,
        allowed_tools: None,
        timeout_secs: None,
        persona: None,
        can_spawn: true,
    }
}

impl Default for ProfileConfig {
    /// The in-code default profile: one `default` agent type that may spawn
    /// children, no configured engines (the caller supplies its
    /// environment-derived engine), and the dark theme.
    fn default() -> Self {
        ProfileConfig {
            default_engine: default_engine_name(),
            compact_engine: None,
            utility_engine: None,
            provider_connections: HashMap::new(),
            engines: HashMap::new(),
            default_model: None,
            theme: ThemeName::default(),
            keybindings: HashMap::new(),
            agents: HashMap::from([(DEFAULT_PROFILE_NAME.to_string(), default_agent_config())]),
        }
    }
}

impl ProfileConfig {
    /// Load `<profiles_root>/<profile_name>/config.toml`, treating a missing
    /// file as a hard error.
    ///
    /// Use this for an **explicitly** named profile (`--profile <name>`): the
    /// user asked for that specific profile, so a missing file is
    /// [`ConfigError::ProfileNotFound`], never a silent fallback to defaults.
    /// For the implicit default profile, use
    /// [`ProfileConfig::load_or_default`] instead.
    pub fn load(profiles_root: &Path, profile_name: &str) -> Result<ProfileConfig, ConfigError> {
        let environment_path = super::profile_env_path(profiles_root, profile_name);
        let environment = load_profile_environment(Some(&environment_path))?;
        Self::load_with_environment(profiles_root, profile_name, &environment)
    }

    /// Load a profile using a pre-resolved environment snapshot.
    ///
    /// This is the startup path used by the binary so the same merged
    /// profile environment is used for TOML interpolation and provider
    /// discovery.
    pub fn load_with_environment(
        profiles_root: &Path,
        profile_name: &str,
        environment: &HashMap<String, String>,
    ) -> Result<ProfileConfig, ConfigError> {
        let workspace_root = current_workspace_root();
        Self::load_with_environment_at_workspace_root(
            profiles_root,
            profile_name,
            environment,
            workspace_root.as_deref(),
        )
    }

    /// Load a profile using an explicitly discovered workspace root.
    ///
    /// The binary uses this entry point so the workspace root is discovered
    /// once and shared with the root persona loader.
    pub fn load_with_environment_at_workspace_root(
        profiles_root: &Path,
        profile_name: &str,
        environment: &HashMap<String, String>,
        workspace_root: Option<&Path>,
    ) -> Result<ProfileConfig, ConfigError> {
        Self::load_file(profiles_root, profile_name, environment, workspace_root)
    }

    /// Load `<profiles_root>/<profile_name>/config.toml`, falling back to
    /// [`ProfileConfig::default`] when the file does not exist.
    ///
    /// Use this only for the *implicit* default profile (no `--profile` flag
    /// given): running without ever writing a config file is the normal,
    /// expected case there. An unreadable or malformed file is still an
    /// error.
    pub fn load_or_default(
        profiles_root: &Path,
        profile_name: &str,
    ) -> Result<ProfileConfig, ConfigError> {
        let environment_path = super::profile_env_path(profiles_root, profile_name);
        let environment = load_profile_environment(Some(&environment_path))?;
        Self::load_or_default_with_environment(profiles_root, profile_name, &environment)
    }

    /// Load a profile with a pre-resolved environment, falling back to the
    /// in-code default only when the profile file is missing.
    pub fn load_or_default_with_environment(
        profiles_root: &Path,
        profile_name: &str,
        environment: &HashMap<String, String>,
    ) -> Result<ProfileConfig, ConfigError> {
        let workspace_root = current_workspace_root();
        Self::load_or_default_with_environment_at_workspace_root(
            profiles_root,
            profile_name,
            environment,
            workspace_root.as_deref(),
        )
    }

    /// Load a profile with an explicitly discovered workspace root, falling back
    /// to the in-code defaults when no file exists.
    pub fn load_or_default_with_environment_at_workspace_root(
        profiles_root: &Path,
        profile_name: &str,
        environment: &HashMap<String, String>,
        workspace_root: Option<&Path>,
    ) -> Result<ProfileConfig, ConfigError> {
        match Self::load_file(profiles_root, profile_name, environment, workspace_root) {
            Ok(profile) => Ok(profile),
            Err(ConfigError::ProfileNotFound { path, .. }) => {
                tracing::debug!(path = %path.display(), "no profile config; using defaults");
                Ok(ProfileConfig::default())
            }
            Err(other) => Err(other),
        }
    }

    /// Shared file-reading/parsing core of [`ProfileConfig::load`] and
    /// [`ProfileConfig::load_or_default`]; a missing file is always reported
    /// as [`ConfigError::ProfileNotFound`], and it is each public entry
    /// point's job to decide whether that is fatal.
    fn load_file(
        profiles_root: &Path,
        profile_name: &str,
        environment: &HashMap<String, String>,
        workspace_root: Option<&Path>,
    ) -> Result<ProfileConfig, ConfigError> {
        let path = super::profile_config_path(profiles_root, profile_name);
        let resolved = resolve_profile_config_file(profiles_root, profile_name, workspace_root)?
            .ok_or_else(|| ConfigError::ProfileNotFound {
                profile_name: profile_name.to_string(),
                path: path.clone(),
            })?;
        let ResolvedFile {
            contents,
            layer,
            path,
        } = resolved;
        let mut document: toml::Value =
            toml::from_str(&contents).map_err(|source| ConfigError::Malformed {
                path: path.clone(),
                source,
            })?;
        interpolate_toml_strings(&mut document, environment, &path)?;
        let mut profile: ProfileConfig =
            document
                .try_into()
                .map_err(|source| ConfigError::Malformed {
                    path: path.clone(),
                    source,
                })?;
        profile.apply_default_model();
        // Specialized agent tables do not replace the built-in root agent.
        profile
            .agents
            .entry(DEFAULT_PROFILE_NAME.to_string())
            .or_insert_with(default_agent_config);
        tracing::info!(
            path = %path.display(),
            ?layer,
            provider_connections = profile.provider_connections.len(),
            engines = profile.engines.len(),
            agents = profile.agents.len(),
            "profile config loaded"
        );
        Ok(profile)
    }

    /// Fill in every engine that named no model of its own from
    /// `default_model`, so `[engines.*]` entries only have to repeat a model
    /// when they genuinely differ from the profile-wide default.
    fn apply_default_model(&mut self) {
        let Some(default_model) = self.default_model.clone() else {
            return;
        };
        for engine_config in self.engines.values_mut() {
            if engine_config.model.is_empty() {
                engine_config.model = default_model.clone();
            }
        }
    }

    /// Engine name used for compaction: `compact_engine` when set, else
    /// `default_engine` (see `docs/configuration.md`).
    pub fn compact_engine_name(&self) -> &str {
        self.compact_engine
            .as_deref()
            .unwrap_or(self.default_engine.as_str())
    }

    /// Engine name used for utility tasks such as session-title generation:
    /// `utility_engine` when set, else `default_engine`.
    pub fn utility_engine_name(&self) -> &str {
        self.utility_engine
            .as_deref()
            .unwrap_or(self.default_engine.as_str())
    }
}

/// Return the path of the profile configuration file selected by the same
/// workspace/home layering used to load the profile.
pub fn profile_config_source_path(
    profiles_root: &Path,
    profile_name: &str,
    workspace_root: Option<&Path>,
) -> Result<Option<PathBuf>, ConfigError> {
    Ok(
        resolve_profile_config_file(profiles_root, profile_name, workspace_root)?
            .map(|resolved| resolved.path),
    )
}

fn resolve_profile_config_file(
    profiles_root: &Path,
    profile_name: &str,
    workspace_root: Option<&Path>,
) -> Result<Option<ResolvedFile>, ConfigError> {
    let relative_path = PathBuf::from(PROFILES_DIRECTORY_NAME)
        .join(profile_name)
        .join(PROFILE_CONFIG_FILE_NAME);
    let home_path = profiles_root
        .join(profile_name)
        .join(PROFILE_CONFIG_FILE_NAME);
    let workspace_path = workspace_root.map(|root| root.join(YLVA_DIR_NAME).join(&relative_path));
    let built_in_path = PathBuf::from("<built-in>").join(&relative_path);
    resolve_file_at_paths(
        workspace_path.as_deref(),
        Some(&home_path),
        &built_in_path,
        None,
    )
}

fn interpolate_toml_strings(
    value: &mut toml::Value,
    environment: &HashMap<String, String>,
    path: &Path,
) -> Result<(), ConfigError> {
    match value {
        toml::Value::String(string) => {
            *string = interpolate_string(string, environment, path)?;
        }
        toml::Value::Array(values) => {
            for value in values {
                interpolate_toml_strings(value, environment, path)?;
            }
        }
        toml::Value::Table(table) => {
            for (_, value) in table.iter_mut() {
                interpolate_toml_strings(value, environment, path)?;
            }
        }
        toml::Value::Integer(_)
        | toml::Value::Float(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_) => {}
    }
    Ok(())
}

fn interpolate_string(
    value: &str,
    environment: &HashMap<String, String>,
    path: &Path,
) -> Result<String, ConfigError> {
    let mut result = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '$' || characters.peek() != Some(&'{') {
            result.push(character);
            continue;
        }

        characters.next();
        let mut name = String::new();
        let mut closed = false;
        for next in characters.by_ref() {
            if next == '}' {
                closed = true;
                break;
            }
            name.push(next);
        }
        let reference = format!("${{{name}}}");
        if !closed || !valid_environment_name(&name) {
            return Err(ConfigError::InvalidEnvironmentReference {
                path: path.to_path_buf(),
                reference,
            });
        }
        let value =
            environment
                .get(&name)
                .ok_or_else(|| ConfigError::MissingEnvironmentVariable {
                    path: path.to_path_buf(),
                    name: name.clone(),
                })?;
        result.push_str(value);
    }
    Ok(result)
}

fn valid_environment_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use crate::highlight::ThemeName;

    use super::*;

    /// Write `contents` as `<root>/<profile_name>/config.toml`.
    fn write_profile_config(root: &Path, profile_name: &str, contents: &str) {
        let path = super::super::profile_config_path(root, profile_name);
        std::fs::create_dir_all(path.parent().expect("a parent directory")).expect("mkdir");
        std::fs::write(&path, contents).expect("write the profile config");
    }

    #[test]
    fn a_toml_fixture_loads_into_the_expected_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "work",
            r#"
default_engine = "fast"
compact_engine = "cheap"
default_model = "gemma4:e4b-mlx"
theme = "light"

[keybindings]
submit = "enter"

[engines.fast]
provider = "ollama"
api_base_url = "http://localhost:11434"
temperature = 0.2

[engines.cheap]
provider = "openrouter"
model = "some/cheap-model"
api_base_url = "https://openrouter.ai"
context_length = 32000

[engines.thinking]
provider = "openrouter"
model = "some/thinking-model"
api_base_url = "https://openrouter.ai"
context_length = "265kb"
reasoning = "xhigh"

[agents.default]
engine = "fast"
description = "General-purpose agent for ordinary tasks."
allowed_tools = ["read", "glob"]
can_spawn = true

[agents.explorer]
timeout_secs = 120
persona = "You explore code."
"#,
        );

        let profile = ProfileConfig::load(temp.path(), "work").expect("the fixture loads");

        assert_eq!(profile.default_engine, "fast");
        assert_eq!(profile.compact_engine_name(), "cheap");
        assert_eq!(profile.theme, ThemeName::Light);
        assert_eq!(
            profile.keybindings.get("submit").map(String::as_str),
            Some("enter")
        );

        let fast = profile.engines.get("fast").expect("the fast engine");
        assert_eq!(fast.provider_connection, "ollama");
        assert_eq!(
            fast.model, "gemma4:e4b-mlx",
            "an engine naming no model inherits default_model"
        );
        assert_eq!(fast.temperature, Some(0.2));
        assert_eq!(
            fast.context_length, None,
            "an unset context_length in TOML stays None; it is resolved lazily \
             against the provider catalog and only then the built-in default"
        );

        let cheap = profile.engines.get("cheap").expect("the cheap engine");
        assert_eq!(cheap.model, "some/cheap-model", "an explicit model wins");
        assert_eq!(cheap.context_length, Some(32_000));

        let thinking = profile
            .engines
            .get("thinking")
            .expect("the thinking engine");
        assert_eq!(
            thinking.context_length,
            Some(265 * 1024),
            "a 'kb'-suffixed context_length expands using 1024-based units"
        );
        assert_eq!(
            thinking.reasoning.as_deref(),
            Some("xhigh"),
            "an explicit reasoning field seeds the engine's initial reasoning effort"
        );

        let default_agent = profile.agents.get("default").expect("the default agent");
        assert_eq!(default_agent.engine.as_deref(), Some("fast"));
        assert_eq!(
            default_agent.description.as_deref(),
            Some("General-purpose agent for ordinary tasks.")
        );
        assert!(default_agent.can_spawn);
        let explorer = profile.agents.get("explorer").expect("the explorer agent");
        assert_eq!(explorer.timeout_secs, Some(120));
        assert!(
            !explorer.can_spawn,
            "can_spawn defaults to false when the table omits it"
        );
    }

    #[test]
    fn named_provider_connections_and_environment_interpolation_load() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "multiple",
            r#"
default_engine = "ollama/local"

[provider_connections.ollama-local]
provider = "ollama"
api_base_url = "${OLLAMA_LOCAL_URL}"

[provider_connections.ollama-cloud]
provider = "ollama"
api_base_url = "https://ollama.com"
api_key = "${OLLAMA_CLOUD_API_KEY}"

[engines."ollama/local"]
provider_connection = "ollama-local"
model = "local-model"

[engines."ollama/cloud"]
provider_connection = "ollama-cloud"
model = "cloud-model"
"#,
        );
        let environment = HashMap::from([
            (
                "OLLAMA_LOCAL_URL".to_string(),
                "http://localhost:11434".to_string(),
            ),
            (
                "OLLAMA_CLOUD_API_KEY".to_string(),
                "cloud-secret".to_string(),
            ),
        ]);

        let profile = ProfileConfig::load_with_environment(temp.path(), "multiple", &environment)
            .expect("the multiple-provider profile loads");

        assert_eq!(profile.provider_connections.len(), 2);
        assert_eq!(
            profile.provider_connections["ollama-local"].provider,
            "ollama"
        );
        assert_eq!(
            profile.provider_connections["ollama-local"]
                .api_base_url
                .as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(
            profile.provider_connections["ollama-cloud"]
                .api_key
                .as_deref(),
            Some("cloud-secret")
        );
        assert_eq!(
            profile.engines["ollama/local"].provider_connection,
            "ollama-local"
        );
    }

    #[test]
    fn numeric_and_boolean_environment_interpolation_loads() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "typed-values",
            r#"
[engines.local]
provider = "ollama"
temperature = "${TEMPERATURE}"
context_length = "${CONTEXT_LENGTH}"

[agents.worker]
timeout_secs = "${TIMEOUT_SECONDS}"
can_spawn = "${CAN_SPAWN}"
"#,
        );
        let environment = HashMap::from([
            ("TEMPERATURE".to_string(), "0.2".to_string()),
            ("CONTEXT_LENGTH".to_string(), "128kb".to_string()),
            ("TIMEOUT_SECONDS".to_string(), "120".to_string()),
            ("CAN_SPAWN".to_string(), "true".to_string()),
        ]);

        let profile =
            ProfileConfig::load_with_environment(temp.path(), "typed-values", &environment)
                .expect("the typed-value profile loads");

        assert_eq!(profile.engines["local"].temperature, Some(0.2));
        assert_eq!(profile.engines["local"].context_length, Some(128 * 1024));
        assert_eq!(profile.agents["worker"].timeout_secs, Some(120));
        assert!(profile.agents["worker"].can_spawn);
    }

    #[test]
    fn missing_environment_interpolation_is_a_profile_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "missing-variable",
            r#"
[provider_connections.cloud]
provider = "ollama"
api_base_url = "https://ollama.com"
api_key = "${MISSING_API_KEY}"
"#,
        );

        let error =
            ProfileConfig::load_with_environment(temp.path(), "missing-variable", &HashMap::new())
                .expect_err("missing variables must fail profile loading");

        assert!(error.to_string().contains("MISSING_API_KEY"), "{error}");
    }

    #[test]
    fn a_missing_config_file_falls_back_to_the_default_profile_only_via_load_or_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profile = ProfileConfig::load_or_default(temp.path(), "never-written")
            .expect("a missing file is not an error for the implicit default profile");
        assert_eq!(profile.default_engine, DEFAULT_PROFILE_NAME);
        assert!(profile.engines.is_empty());
        assert_eq!(profile.theme, ThemeName::Dark);
        assert!(
            profile
                .agents
                .get(DEFAULT_PROFILE_NAME)
                .expect("a default agent type")
                .can_spawn
        );
    }

    #[test]
    fn the_code_default_profile_is_the_implicit_floor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profiles_root = temp.path().join(PROFILES_DIRECTORY_NAME);
        let profile = ProfileConfig::load_or_default_with_environment_at_workspace_root(
            &profiles_root,
            DEFAULT_PROFILE_NAME,
            &HashMap::new(),
            None,
        )
        .expect("the code default profile is used");

        assert_eq!(profile.default_engine, DEFAULT_PROFILE_NAME);
        assert!(profile.engines.is_empty());
        assert_eq!(profile.theme, ThemeName::Dark);
    }

    #[test]
    fn project_profile_replaces_the_home_profile_for_the_same_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home_profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
        let workspace_root = temp.path().join("project");
        write_profile_config(
            &home_profiles_root,
            DEFAULT_PROFILE_NAME,
            "theme = \"light\"\n",
        );
        let project_config = workspace_root
            .join(".ylva")
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME)
            .join(PROFILE_CONFIG_FILE_NAME);
        std::fs::create_dir_all(project_config.parent().expect("project config parent"))
            .expect("mkdir");
        std::fs::write(&project_config, "theme = \"dark\"\n").expect("write project config");

        let profile = ProfileConfig::load_with_environment_at_workspace_root(
            &home_profiles_root,
            DEFAULT_PROFILE_NAME,
            &HashMap::new(),
            Some(&workspace_root),
        )
        .expect("the project profile loads");

        assert_eq!(profile.theme, ThemeName::Dark);
    }

    #[test]
    fn profile_config_source_path_matches_profile_layer_precedence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home_profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
        let workspace_root = temp.path().join("project");
        write_profile_config(
            &home_profiles_root,
            DEFAULT_PROFILE_NAME,
            "theme = \"light\"\n",
        );
        let workspace_config = workspace_root
            .join(YLVA_DIR_NAME)
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME)
            .join(PROFILE_CONFIG_FILE_NAME);
        std::fs::create_dir_all(workspace_config.parent().expect("workspace config parent"))
            .expect("mkdir");
        std::fs::write(&workspace_config, "theme = \"dark\"\n").expect("write workspace config");

        assert_eq!(
            profile_config_source_path(
                &home_profiles_root,
                DEFAULT_PROFILE_NAME,
                Some(&workspace_root)
            )
            .expect("resolve workspace profile path"),
            Some(workspace_config.clone())
        );

        std::fs::remove_file(&workspace_config).expect("remove workspace config");
        assert_eq!(
            profile_config_source_path(
                &home_profiles_root,
                DEFAULT_PROFILE_NAME,
                Some(&workspace_root)
            )
            .expect("resolve home profile path"),
            Some(
                home_profiles_root
                    .join(DEFAULT_PROFILE_NAME)
                    .join(PROFILE_CONFIG_FILE_NAME)
            )
        );
    }

    #[test]
    fn an_explicitly_named_missing_profile_is_a_hard_error_via_load() {
        let temp = tempfile::tempdir().expect("tempdir");
        let error = ProfileConfig::load(temp.path(), "never-written").expect_err(
            "an explicitly requested profile that does not exist must not silently fall back",
        );
        assert!(
            matches!(&error, ConfigError::ProfileNotFound { profile_name, .. } if profile_name == "never-written"),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("never-written"), "{message}");
        assert!(message.contains("not found"), "{message}");
    }

    #[test]
    fn a_malformed_config_file_is_a_clear_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(temp.path(), "broken", "default_engine = [not toml");
        let error = ProfileConfig::load(temp.path(), "broken")
            .expect_err("malformed TOML must not be silently ignored");
        let message = error.to_string();
        assert!(message.contains("malformed profile config"), "{message}");
        assert!(message.contains("config.toml"), "{message}");
    }

    #[test]
    fn a_config_file_without_agents_still_gets_the_default_agent_type() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(temp.path(), "bare", "theme = \"dark\"\n");
        let profile = ProfileConfig::load(temp.path(), "bare").expect("the fixture loads");
        assert!(profile.agents.contains_key(DEFAULT_PROFILE_NAME));
    }

    #[test]
    fn a_config_file_with_specialized_agents_still_gets_a_spawn_capable_default_agent() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "specialized",
            r#"
[agents.local-discovery]
engine = "ollama/local"
"#,
        );

        let profile = ProfileConfig::load(temp.path(), "specialized").expect("the fixture loads");
        let default_agent = profile
            .agents
            .get(DEFAULT_PROFILE_NAME)
            .expect("the implicit default agent type");
        assert!(default_agent.can_spawn);
        assert_eq!(
            profile
                .agents
                .get("local-discovery")
                .and_then(|agent| agent.engine.as_deref()),
            Some("ollama/local")
        );
    }

    #[test]
    fn an_explicit_default_agent_config_is_not_replaced_by_implicit_defaults() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "custom-default",
            r#"
[agents.default]
can_spawn = false

[agents.local-discovery]
engine = "ollama/local"
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "custom-default").expect("the fixture loads");
        assert!(
            !profile
                .agents
                .get(DEFAULT_PROFILE_NAME)
                .expect("the configured default agent type")
                .can_spawn
        );
    }

    #[test]
    fn compact_engine_falls_back_to_the_default_engine() {
        let profile = ProfileConfig {
            default_engine: "main".to_string(),
            compact_engine: None,
            ..ProfileConfig::default()
        };
        assert_eq!(profile.compact_engine_name(), "main");
    }

    #[test]
    fn utility_engine_falls_back_to_default_and_honors_explicit_selection() {
        let mut profile = ProfileConfig {
            default_engine: "main".to_string(),
            compact_engine: None,
            utility_engine: None,
            ..ProfileConfig::default()
        };
        assert_eq!(profile.utility_engine_name(), "main");
        profile.utility_engine = Some("cheap".to_string());
        assert_eq!(profile.utility_engine_name(), "cheap");
    }

    #[test]
    fn profile_config_field_access() {
        let profile = ProfileConfig {
            default_engine: "default".to_string(),
            compact_engine: None,
            agents: HashMap::from([(
                "main".to_string(),
                AgentConfig {
                    engine: Some("fast".to_string()),
                    description: None,
                    allowed_tools: Some(vec!["bash".to_string()]),
                    timeout_secs: Some(120),
                    persona: Some("You are a helpful assistant.".to_string()),
                    can_spawn: false,
                },
            )]),
            ..ProfileConfig::default()
        };

        assert_eq!(profile.default_engine, "default");
        assert!(profile.compact_engine.is_none());

        let agent_config = profile.agents.get("main").unwrap();
        assert_eq!(agent_config.engine.as_deref(), Some("fast"));
        assert_eq!(
            agent_config.allowed_tools.as_deref(),
            Some(["bash".to_string()].as_slice())
        );
        assert_eq!(agent_config.timeout_secs, Some(120));
        assert_eq!(
            agent_config.persona.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert!(!agent_config.can_spawn);
    }

    #[test]
    fn agent_config_defaults_to_no_engine_override() {
        let agent_config = AgentConfig {
            engine: None,
            description: None,
            allowed_tools: None,
            timeout_secs: None,
            persona: None,
            can_spawn: true,
        };

        assert!(agent_config.engine.is_none());
        assert!(agent_config.allowed_tools.is_none());
        assert!(agent_config.timeout_secs.is_none());
        assert!(agent_config.persona.is_none());
        assert!(agent_config.can_spawn);
    }
}
