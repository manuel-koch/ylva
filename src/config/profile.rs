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
use crate::sandbox::network_target_pattern::NetworkTargetPattern;
use crate::ylva_home::YLVA_DIR_NAME;

use super::{
    agent_type_file::{
        discover_agent_type_files, AgentConfigOverrides, AgentTypeFile, AgentTypeScope,
    },
    layered::{current_workspace_root, resolve_file_at_paths, ConfigLayer, ResolvedFile},
    load_profile_environment_at_workspace_root, AgentConfig, ConfigError, ProviderConnectionConfig,
    WebFetchConfig, WebSearchConfig, DEFAULT_PROFILE_NAME, PROFILES_DIRECTORY_NAME,
    PROFILE_CONFIG_FILE_NAME,
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
    /// Compaction engine selection and summary-request timeout.
    #[serde(default)]
    pub compaction: CompactionConfig,
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
    /// Maximum number of explicit messages from prior sessions loaded into
    /// the TUI input history.
    #[serde(
        default = "default_input_history_limit",
        deserialize_with = "deserialize_usize_or_string"
    )]
    pub input_history_limit: usize,
    /// Reserved key-to-action map. Accepted and validated as a table of
    /// strings so a profile carrying one still loads, but Phase 7 ships a
    /// fixed key map (`docs/keybindings.md`) and does not apply these
    /// entries yet.
    #[serde(default)]
    pub keybindings: HashMap<String, String>,
    /// Map of agent-type name to its configuration.
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,
    /// Map of tool name (e.g. `read`, `write`) to its `[tools.<name>]`
    /// sandbox rule list. When a tool name has no entry here, its
    /// `SandboxContext` falls back to the built-in default floor
    /// (`outside_workspace`/`dot_path` → `UserApprovalRequired`, see
    /// `docs/configuration.md`'s "Sandbox" section and
    /// [`crate::sandbox::SandboxContext`]); when it does, that entry's rule
    /// list fully replaces the default floor for that tool.
    #[serde(default)]
    pub tools: HashMap<String, ToolSandboxConfig>,
    /// `[web_search]` section: which search backend serves the `web_search`
    /// tool, its internal HTTP timeout, and its default result limit. The
    /// `backend` value is validated when the profile loads.
    #[serde(default)]
    pub web_search: WebSearchConfig,
    /// `[web_fetch]` section: which fetch backend serves the `web_fetch`
    /// tool and its internal HTTP timeout. The `backend` value is validated
    /// when the profile loads.
    #[serde(default)]
    pub web_fetch: WebFetchConfig,
}

/// Settings for background conversation-summary compaction.
#[derive(Debug, Clone, Deserialize)]
pub struct CompactionConfig {
    /// Engine used for summaries. When absent, uses `default_engine`.
    #[serde(default)]
    pub engine: Option<String>,
    /// Wall-clock seconds allowed for one summary request.
    #[serde(default = "default_compaction_timeout_secs")]
    pub timeout_sec: u64,
}

/// Optional compaction overrides for one agent type.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentCompactionConfig {
    /// Engine used to summarize this agent's context. When absent, inherits
    /// the profile's `[compaction].engine` or `default_engine`.
    #[serde(default)]
    pub engine: Option<String>,
    /// Wall-clock summary timeout. When absent, inherits the profile value.
    #[serde(default, deserialize_with = "deserialize_optional_u64_or_string")]
    pub timeout_sec: Option<u64>,
}

fn deserialize_optional_u64_or_string<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum TimeoutValue {
        Number(u64),
        Text(String),
    }

    match Option::<TimeoutValue>::deserialize(deserializer)? {
        Some(TimeoutValue::Number(value)) => Ok(Some(value)),
        Some(TimeoutValue::Text(value)) => value.trim().parse::<u64>().map(Some).map_err(|error| {
            serde::de::Error::custom(format!("invalid compaction timeout_sec '{value}': {error}"))
        }),
        None => Ok(None),
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            engine: None,
            timeout_sec: default_compaction_timeout_secs(),
        }
    }
}

fn default_compaction_timeout_secs() -> u64 {
    crate::agent::compaction::DEFAULT_COMPACTION_TIMEOUT_SECS
}

/// A single `[tools.<name>]` TOML table: currently just the ordered
/// `rules` list evaluated by [`crate::sandbox::SandboxContext`] for that
/// tool (last match wins). See [`crate::sandbox::SandboxRuleConfig`].
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolSandboxConfig {
    /// Ordered sandbox rules for this tool; last match wins.
    #[serde(default)]
    pub rules: Vec<crate::sandbox::SandboxRuleConfig>,
}

/// Validate that every `[tools.<name>]`/`[agents.<type>.tools.<name>]` rule
/// list uses the rule kind appropriate for its tool: `bash` accepts only
/// [`crate::sandbox::SandboxRuleConfig::CommandGlob`] and the four
/// `bash`-only [`crate::sandbox::NamedSandboxRule`] variants
/// (`backgrounded`/`variable_assignment_prefix`/`unquoted_glob`/
/// `unquoted_brace_expansion`); the network tools `web_search` and
/// `web_fetch` accept only the network-tool
/// [`crate::sandbox::NamedSandboxRule::NetworkRequest`] rule
/// (`network_request`, optionally scoped by its `target` pattern) — they have
/// neither a path nor a command argument, so path and `command` rules would
/// be silently ignored there; every other tool accepts only
/// [`crate::sandbox::SandboxRuleConfig::PathGlob`] and the three path
/// [`crate::sandbox::NamedSandboxRule`] variants (`outside_workspace`/
/// `dot_path`/`cross_filesystem`). A `target` parameter is likewise valid
/// only on a `network_request` rule — it scopes that rule to request targets
/// whose `scheme://host[:port]` key matches its target pattern, so on every
/// other rule kind it
/// would be a silent no-op. A mismatch is a hard startup error naming
/// the tool, the rule's list index, and the expected kind, since a
/// silently-ignored rule kind mismatch would be a safety-critical footgun. A
/// `command` rule whose `*`/`**` wildcard isn't the last token is likewise a
/// hard error (see
/// [`crate::sandbox::SandboxRuleConfig::has_misplaced_command_glob_wildcard`]):
/// left unchecked, it would silently degrade to a literal (never-matching)
/// token instead of a wildcard.
fn validate_tool_sandbox_rules(
    tools: &HashMap<String, ToolSandboxConfig>,
    path: &Path,
) -> Result<(), ConfigError> {
    for (tool_name, tool_config) in tools {
        let is_bash = tool_name == crate::agent::tools::bash::BashTool::NAME;
        let is_network_tool = tool_name == crate::agent::tools::web_search::WebSearchTool::NAME
            || tool_name == crate::agent::tools::web_fetch::WebFetchTool::NAME;
        for (index, rule) in tool_config.rules.iter().enumerate() {
            if rule.has_target() && !rule.is_network_only() {
                return Err(ConfigError::MisplacedSandboxRuleTarget {
                    path: path.to_path_buf(),
                    tool: tool_name.clone(),
                    index,
                });
            }
            if let Some(target) = rule.target() {
                NetworkTargetPattern::parse(target).map_err(|error| {
                    ConfigError::InvalidSandboxRuleTarget {
                        path: path.to_path_buf(),
                        tool: tool_name.clone(),
                        index,
                        detail: error.to_string(),
                    }
                })?;
            }
            if is_bash {
                if rule.is_path_only() || rule.is_network_only() {
                    return Err(ConfigError::InvalidSandboxRuleKind {
                        path: path.to_path_buf(),
                        tool: tool_name.clone(),
                        index,
                        expected: "a `command` argv-glob rule or a named command rule \
                                   (`backgrounded`/`variable_assignment_prefix`/\
                                   `unquoted_glob`/`unquoted_brace_expansion`)"
                            .to_string(),
                    });
                }
            } else if is_network_tool {
                if rule.is_bash_only() || rule.is_path_only() {
                    return Err(ConfigError::InvalidSandboxRuleKind {
                        path: path.to_path_buf(),
                        tool: tool_name.clone(),
                        index,
                        expected: "a named `network_request` rule (the only rule kind \
                                   [tools.web_search] and [tools.web_fetch] accept; its \
                                   optional `target` scopes it to request targets)"
                            .to_string(),
                    });
                }
            } else if rule.is_bash_only() || rule.is_network_only() {
                return Err(ConfigError::InvalidSandboxRuleKind {
                    path: path.to_path_buf(),
                    tool: tool_name.clone(),
                    index,
                    expected: "a `pattern` path-glob rule or a named path rule \
                               (`outside_workspace`/`dot_path`/`cross_filesystem`)"
                        .to_string(),
                });
            }
            if rule.has_misplaced_command_glob_wildcard() {
                return Err(ConfigError::MisplacedCommandGlobWildcard {
                    path: path.to_path_buf(),
                    tool: tool_name.clone(),
                    index,
                });
            }
        }
    }
    Ok(())
}

/// Validate every agent type's configured `max_tool_iterations`. A limit of
/// `0` would make the iteration guard trip on the very first tool-use round,
/// so it is a hard profile-load error naming the agent type instead.
fn validate_agent_max_tool_iterations(
    agents: &HashMap<String, AgentConfig>,
    path: &Path,
) -> Result<(), ConfigError> {
    for (agent_type, agent_config) in agents {
        if agent_config.max_tool_iterations == Some(0) {
            return Err(ConfigError::InvalidMaxToolIterations {
                path: path.to_path_buf(),
                agent_type: agent_type.clone(),
            });
        }
    }
    Ok(())
}

/// Validate every `[engines.<name>]` compaction setting.
///
/// The three ratios must be fractions in `(0, 1]` and ordered
/// `compact_target_ratio < soft_compaction_threshold <
/// hard_compaction_threshold`: a target at or above the soft trigger would
/// make compaction unable to reach its own goal, and soft at or above hard
/// would make the background path unreachable (the hard drop would always
/// win). A violation is a hard startup error naming the engine and the rule.
fn validate_compaction_settings(
    engines: &HashMap<String, crate::llm::EngineConfig>,
    path: &Path,
) -> Result<(), ConfigError> {
    fn is_ratio(value: f64) -> bool {
        value.is_finite() && value > 0.0 && value <= 1.0
    }
    for (engine_name, engine_config) in engines {
        let target = engine_config.compact_target_ratio;
        let soft = engine_config.soft_compaction_threshold;
        let hard = engine_config.hard_compaction_threshold;
        let detail = if !is_ratio(target) {
            format!("compact_target_ratio must be > 0 and <= 1, got {target}")
        } else if !is_ratio(soft) {
            format!("soft_compaction_threshold must be > 0 and <= 1, got {soft}")
        } else if !is_ratio(hard) {
            format!("hard_compaction_threshold must be > 0 and <= 1, got {hard}")
        } else if target >= soft {
            format!(
                "compact_target_ratio ({target}) must be below \
                 soft_compaction_threshold ({soft})"
            )
        } else if soft >= hard {
            format!(
                "soft_compaction_threshold ({soft}) must be below \
                 hard_compaction_threshold ({hard})"
            )
        } else {
            continue;
        };
        return Err(ConfigError::InvalidCompactionSetting {
            path: path.to_path_buf(),
            engine: engine_name.clone(),
            detail,
        });
    }
    Ok(())
}

fn validate_compaction_timeout(
    compaction: &CompactionConfig,
    default_engine: &str,
    path: &Path,
) -> Result<(), ConfigError> {
    if compaction.timeout_sec == 0 {
        return Err(ConfigError::InvalidCompactionSetting {
            path: path.to_path_buf(),
            engine: compaction
                .engine
                .as_deref()
                .unwrap_or(default_engine)
                .to_string(),
            detail: "compaction timeout must be greater than 0".to_string(),
        });
    }
    Ok(())
}

fn validate_agent_compaction_settings(
    agents: &HashMap<String, AgentConfig>,
    profile_compaction: &CompactionConfig,
    path: &Path,
) -> Result<(), ConfigError> {
    for (agent_type, agent_config) in agents {
        let Some(compaction) = &agent_config.compaction else {
            continue;
        };
        if compaction.timeout_sec == Some(0) {
            return Err(ConfigError::InvalidCompactionSetting {
                path: path.to_path_buf(),
                engine: compaction
                    .engine
                    .as_deref()
                    .or(profile_compaction.engine.as_deref())
                    .unwrap_or("default_engine")
                    .to_string(),
                detail: format!(
                    "[agents.{agent_type}.compaction].timeout_sec must be greater than 0"
                ),
            });
        }
    }
    Ok(())
}

fn default_engine_name() -> String {
    DEFAULT_PROFILE_NAME.to_string()
}

const DEFAULT_INPUT_HISTORY_LIMIT: usize = 50;

fn default_input_history_limit() -> usize {
    DEFAULT_INPUT_HISTORY_LIMIT
}

fn deserialize_usize_or_string<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum HistoryLimitValue {
        Number(usize),
        Text(String),
    }

    match HistoryLimitValue::deserialize(deserializer)? {
        HistoryLimitValue::Number(value) => Ok(value),
        HistoryLimitValue::Text(value) => value.trim().parse::<usize>().map_err(|error| {
            serde::de::Error::custom(format!("invalid input_history_limit '{value}': {error}"))
        }),
    }
}

fn default_agent_config() -> AgentConfig {
    AgentConfig {
        engine: None,
        compaction: None,
        description: None,
        allowed_tools: None,
        timeout_secs: None,
        max_tool_iterations: None,
        persona: None,
        can_spawn: true,
        tools: HashMap::new(),
    }
}

impl Default for ProfileConfig {
    /// The in-code default profile: one `default` agent type that may spawn
    /// children, no configured engines (the caller supplies its
    /// environment-derived engine), and the dark theme.
    fn default() -> Self {
        ProfileConfig {
            default_engine: default_engine_name(),
            compaction: CompactionConfig::default(),
            utility_engine: None,
            provider_connections: HashMap::new(),
            engines: HashMap::new(),
            default_model: None,
            theme: ThemeName::default(),
            input_history_limit: default_input_history_limit(),
            keybindings: HashMap::new(),
            agents: HashMap::from([(DEFAULT_PROFILE_NAME.to_string(), default_agent_config())]),
            tools: HashMap::new(),
            web_search: WebSearchConfig::default(),
            web_fetch: WebFetchConfig::default(),
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
        let workspace_root = current_workspace_root();
        let environment = load_profile_environment_at_workspace_root(
            Some(&environment_path),
            workspace_root.as_deref(),
            profile_name,
        )?;
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
        let workspace_root = current_workspace_root();
        let environment = load_profile_environment_at_workspace_root(
            Some(&environment_path),
            workspace_root.as_deref(),
            profile_name,
        )?;
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
                Self::load_agent_types_into_profile(
                    ProfileConfig::default(),
                    profiles_root,
                    profile_name,
                    workspace_root,
                )
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
        let inline_agents = extract_agent_config_overrides(&document, &path)?;
        document
            .as_table_mut()
            .expect("a parsed TOML document is always a table")
            .remove("agents");
        let mut profile: ProfileConfig =
            document
                .try_into()
                .map_err(|source| ConfigError::Malformed {
                    path: path.clone(),
                    source,
                })?;
        profile.apply_default_model();
        profile.agents = load_agent_types(
            profiles_root,
            profile_name,
            workspace_root,
            layer,
            &path,
            inline_agents,
        )?;
        validate_tool_sandbox_rules(&profile.tools, &path)?;
        profile.web_search.validate(&path)?;
        profile.web_fetch.validate(&path)?;
        for agent in profile.agents.values() {
            validate_tool_sandbox_rules(&agent.tools, &path)?;
        }
        validate_agent_max_tool_iterations(&profile.agents, &path)?;
        validate_compaction_settings(&profile.engines, &path)?;
        validate_compaction_timeout(&profile.compaction, &profile.default_engine, &path)?;
        validate_agent_compaction_settings(&profile.agents, &profile.compaction, &path)?;
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

    fn load_agent_types_into_profile(
        mut profile: ProfileConfig,
        profiles_root: &Path,
        profile_name: &str,
        workspace_root: Option<&Path>,
    ) -> Result<ProfileConfig, ConfigError> {
        profile.agents = load_agent_types(
            profiles_root,
            profile_name,
            workspace_root,
            ConfigLayer::BuiltIn,
            Path::new("<built-in>"),
            HashMap::new(),
        )?;
        let built_in_path = Path::new("<built-in>");
        validate_tool_sandbox_rules(&profile.tools, built_in_path)?;
        for agent in profile.agents.values() {
            validate_tool_sandbox_rules(&agent.tools, built_in_path)?;
        }
        validate_agent_max_tool_iterations(&profile.agents, built_in_path)?;
        validate_compaction_settings(&profile.engines, built_in_path)?;
        validate_compaction_timeout(&profile.compaction, &profile.default_engine, built_in_path)?;
        validate_agent_compaction_settings(&profile.agents, &profile.compaction, built_in_path)?;
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

    /// Engine name used for compaction: `[compaction].engine` when set, else
    /// `default_engine` (see `docs/configuration.md`).
    pub fn compaction_engine_name(&self) -> &str {
        self.compaction
            .engine
            .as_deref()
            .unwrap_or(self.default_engine.as_str())
    }

    /// Compaction engine for one agent type, with its optional override.
    pub fn compaction_engine_name_for(&self, agent_type: &str) -> &str {
        self.agents
            .get(agent_type)
            .and_then(|agent_config| agent_config.compaction.as_ref())
            .and_then(|compaction| compaction.engine.as_deref())
            .or(self.compaction.engine.as_deref())
            .unwrap_or_else(|| self.engine_name_for(agent_type))
    }

    /// Summary timeout for one agent type, with its optional override.
    pub fn compaction_timeout_secs_for(&self, agent_type: &str) -> u64 {
        self.agents
            .get(agent_type)
            .and_then(|agent_config| agent_config.compaction.as_ref())
            .and_then(|compaction| compaction.timeout_sec)
            .unwrap_or(self.compaction.timeout_sec)
    }

    /// Engine name used for utility tasks such as session-title generation:
    /// `utility_engine` when set, else `default_engine`.
    pub fn utility_engine_name(&self) -> &str {
        self.utility_engine
            .as_deref()
            .unwrap_or(self.default_engine.as_str())
    }

    /// Engine name serving `agent_type`: the agent type's configured
    /// `engine` when set, else `default_engine`.
    pub fn engine_name_for(&self, agent_type: &str) -> &str {
        self.agents
            .get(agent_type)
            .and_then(|agent_config| agent_config.engine.as_deref())
            .unwrap_or(self.default_engine.as_str())
    }

    /// Effective tool-use iteration limit for `agent_type`: the configured
    /// `max_tool_iterations` when set and non-zero, else
    /// [`crate::agent::DEFAULT_MAX_TOOL_USE_ITERATIONS`]. Profile validation rejects
    /// a configured `0`; the `filter` here keeps a programmatically built
    /// profile from ever producing a limit no loop can run under.
    pub fn max_tool_iterations_for(&self, agent_type: &str) -> usize {
        self.agents
            .get(agent_type)
            .and_then(|agent_config| agent_config.max_tool_iterations)
            .filter(|limit| *limit > 0)
            .unwrap_or(crate::agent::DEFAULT_MAX_TOOL_USE_ITERATIONS)
    }
}

fn extract_agent_config_overrides(
    document: &toml::Value,
    path: &Path,
) -> Result<HashMap<String, AgentConfigOverrides>, ConfigError> {
    let Some(agent_table) = document.get("agents") else {
        return Ok(HashMap::new());
    };
    agent_table
        .clone()
        .try_into()
        .map_err(|source| ConfigError::Malformed {
            path: path.to_path_buf(),
            source,
        })
}

fn load_agent_types(
    profiles_root: &Path,
    profile_name: &str,
    workspace_root: Option<&Path>,
    config_layer: ConfigLayer,
    config_path: &Path,
    inline_agents: HashMap<String, AgentConfigOverrides>,
) -> Result<HashMap<String, AgentConfig>, ConfigError> {
    let global_agents_root = profiles_root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("agents");
    let profile_agents_root = profiles_root.join(profile_name).join("agents");
    let project_agents_root = workspace_root.map(|root| root.join(YLVA_DIR_NAME).join("agents"));

    let mut resolved_files = HashMap::<String, AgentTypeFile>::new();
    for (directory, scope) in [
        (&global_agents_root, AgentTypeScope::Global),
        (&profile_agents_root, AgentTypeScope::Profile),
    ] {
        for (agent_type, file) in discover_agent_type_files(directory, scope)? {
            resolved_files.insert(agent_type, file);
        }
    }
    if let Some(project_agents_root) = project_agents_root {
        for (agent_type, file) in
            discover_agent_type_files(&project_agents_root, AgentTypeScope::Project)?
        {
            resolved_files.insert(agent_type, file);
        }
    }

    let active_file_scope = match config_layer {
        ConfigLayer::Workspace => Some(AgentTypeScope::Project),
        ConfigLayer::Home => Some(AgentTypeScope::Profile),
        ConfigLayer::BuiltIn => None,
    };
    for agent_type in inline_agents.keys() {
        if let Some(file) = resolved_files.get(agent_type) {
            if Some(file.scope) == active_file_scope {
                return Err(ConfigError::AmbiguousAgentTypeDefinition {
                    agent_type: agent_type.clone(),
                    scope: file.scope.label().to_string(),
                    toml_path: config_path.to_path_buf(),
                    file_path: file.path.clone(),
                });
            }
        }
    }

    let mut agents = resolved_files
        .into_iter()
        .map(|(agent_type, file)| {
            debug_assert_eq!(agent_type, file.agent_type);
            (agent_type, file.definition.config)
        })
        .collect::<HashMap<_, _>>();
    agents
        .entry(DEFAULT_PROFILE_NAME.to_string())
        .or_insert_with(default_agent_config);

    for (agent_type, overrides) in inline_agents {
        let config = agents.entry(agent_type).or_insert_with(|| AgentConfig {
            engine: None,
            compaction: None,
            description: None,
            allowed_tools: None,
            timeout_secs: None,
            max_tool_iterations: None,
            persona: None,
            can_spawn: false,
            tools: HashMap::new(),
        });
        apply_agent_config_overrides(config, overrides);
    }
    Ok(agents)
}

fn apply_agent_config_overrides(config: &mut AgentConfig, overrides: AgentConfigOverrides) {
    if overrides.engine.is_some() {
        config.engine = overrides.engine;
    }
    if let Some(compaction_overrides) = overrides.compaction {
        let agent_compaction = config
            .compaction
            .get_or_insert_with(AgentCompactionConfig::default);
        if compaction_overrides.engine.is_some() {
            agent_compaction.engine = compaction_overrides.engine;
        }
        if compaction_overrides.timeout_sec.is_some() {
            agent_compaction.timeout_sec = compaction_overrides.timeout_sec;
        }
    }
    if overrides.description.is_some() {
        config.description = overrides.description;
    }
    if overrides.allowed_tools.is_some() {
        config.allowed_tools = overrides.allowed_tools;
    }
    if overrides.timeout_secs.is_some() {
        config.timeout_secs = overrides.timeout_secs;
    }
    if overrides.max_tool_iterations.is_some() {
        config.max_tool_iterations = overrides.max_tool_iterations;
    }
    if overrides.persona.is_some() {
        config.persona = overrides.persona;
    }
    if let Some(can_spawn) = overrides.can_spawn {
        config.can_spawn = can_spawn;
    }
    if overrides.tools.is_some() {
        config.tools = overrides.tools.unwrap_or_default();
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

    use crate::config::{profile_env_path, PROFILE_ENV_FILE_NAME};
    use crate::highlight::ThemeName;

    use super::*;
    use crate::agent::compaction::{
        DEFAULT_COMPACTION_TIMEOUT_SECS, DEFAULT_HARD_COMPACTION_THRESHOLD,
        DEFAULT_SOFT_COMPACTION_THRESHOLD,
    };

    /// Write `contents` as `<root>/<profile_name>/config.toml`.
    fn write_profile_config(root: &Path, profile_name: &str, contents: &str) {
        let path = super::super::profile_config_path(root, profile_name);
        std::fs::create_dir_all(path.parent().expect("a parent directory")).expect("mkdir");
        std::fs::write(&path, contents).expect("write the profile config");
    }

    fn write_agent_type_file(root: &Path, relative_path: &str, contents: &str) {
        let path = root.join(relative_path);
        std::fs::create_dir_all(path.parent().expect("an agent file parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write the agent-type file");
    }

    #[test]
    fn a_toml_fixture_loads_into_the_expected_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "work",
            r#"
default_engine = "fast"
default_model = "gemma4:e4b-mlx"
theme = "light"
input_history_limit = 25

[compaction]
engine = "cheap"
timeout_sec = 180

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
        assert_eq!(profile.compaction_engine_name(), "cheap");
        assert_eq!(profile.compaction.timeout_sec, 180);
        assert_eq!(profile.theme, ThemeName::Light);
        assert_eq!(profile.input_history_limit, 25);
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
    fn tool_sandbox_rules_table_loads_named_and_glob_rules() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "with-tools",
            r#"
[tools.read]
rules = [
  { rule = "dot_path", verdict = "user_approval_required" },
]

[tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { pattern = "**/build/**", verdict = "auto_approved" },
]
"#,
        );

        let profile = ProfileConfig::load(temp.path(), "with-tools").expect("the fixture loads");

        let read_rules = &profile.tools.get("read").expect("read table").rules;
        assert_eq!(read_rules.len(), 1);
        assert!(matches!(
            read_rules[0],
            crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::DotPath,
                ..
            }
        ));

        let write_rules = &profile.tools.get("write").expect("write table").rules;
        assert_eq!(write_rules.len(), 2);
        assert!(matches!(
            write_rules[0],
            crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::OutsideWorkspace,
                ..
            }
        ));
        assert!(
            matches!(&write_rules[1], crate::sandbox::SandboxRuleConfig::PathGlob { pattern, .. } if pattern == "**/build/**")
        );

        assert!(
            !profile.tools.contains_key("edit"),
            "a tool with no [tools.<name>] table has no entry, falling back to the default floor"
        );
    }

    #[test]
    fn tool_bash_table_loads_command_glob_and_bash_named_rules() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "with-bash-tools",
            r#"
[tools.bash]
rules = [
  { rule = "backgrounded", verdict = "auto_denied" },
  { command = ["git", "reflog", "*"], verdict = "auto_approved" },
]
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "with-bash-tools").expect("the fixture loads");
        let bash_rules = &profile.tools.get("bash").expect("bash table").rules;
        assert_eq!(bash_rules.len(), 2);
        assert!(matches!(
            bash_rules[0],
            crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::Backgrounded,
                ..
            }
        ));
        assert!(matches!(
            &bash_rules[1],
            crate::sandbox::SandboxRuleConfig::CommandGlob { command, .. }
                if command == &vec!["git".to_string(), "reflog".to_string(), "*".to_string()]
        ));
    }

    #[test]
    fn command_glob_rule_under_a_non_bash_tool_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-command-glob",
            r#"
[tools.read]
rules = [
  { command = ["git", "status"], verdict = "auto_approved" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-command-glob")
            .expect_err("a CommandGlob rule under [tools.read] must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "read"
        ));
    }

    #[test]
    fn path_glob_rule_under_tool_bash_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-path-glob",
            r#"
[tools.bash]
rules = [
  { pattern = "**/build/**", verdict = "auto_approved" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-path-glob")
            .expect_err("a PathGlob rule under [tools.bash] must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "bash"
        ));
    }

    #[test]
    fn command_glob_rule_with_a_non_trailing_single_wildcard_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-command-wildcard-position",
            r#"
[tools.bash]
rules = [
  { command = ["git", "*", "status"], verdict = "auto_approved" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-command-wildcard-position")
            .expect_err("a `*` that isn't the last command token must be rejected");
        assert!(matches!(
            error,
            ConfigError::MisplacedCommandGlobWildcard { tool, index: 0, .. } if tool == "bash"
        ));
    }

    #[test]
    fn command_glob_rule_with_a_non_trailing_multi_wildcard_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-command-double-wildcard-position",
            r#"
[tools.bash]
rules = [
  { command = ["git", "**", "status"], verdict = "auto_approved" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-command-double-wildcard-position")
            .expect_err("a `**` that isn't the last command token must be rejected");
        assert!(matches!(
            error,
            ConfigError::MisplacedCommandGlobWildcard { tool, index: 0, .. } if tool == "bash"
        ));
    }

    #[test]
    fn command_glob_rule_with_a_trailing_wildcard_loads_successfully() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "good-command-wildcard-position",
            r#"
[tools.bash]
rules = [
  { command = ["git", "status", "*"], verdict = "auto_approved" },
]
"#,
        );

        ProfileConfig::load(temp.path(), "good-command-wildcard-position")
            .expect("a trailing `*` as the last command token must be accepted");
    }

    #[test]
    fn outside_workspace_named_rule_under_tool_bash_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-named-rule",
            r#"
[tools.bash]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-named-rule")
            .expect_err("outside_workspace under [tools.bash] must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "bash"
        ));
    }

    #[test]
    fn backgrounded_named_rule_under_a_path_tool_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-named-rule-path-tool",
            r#"
[tools.write]
rules = [
  { rule = "backgrounded", verdict = "auto_denied" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-named-rule-path-tool")
            .expect_err("backgrounded under [tools.write] must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "write"
        ));
    }

    #[test]
    fn a_profile_with_no_tool_tables_has_an_empty_tools_map() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(temp.path(), "no-tools", "default_engine = \"fast\"\n");
        let profile = ProfileConfig::load(temp.path(), "no-tools").expect("the fixture loads");
        assert!(
            profile.tools.is_empty(),
            "no [tools.*] tables means no configured overrides for any tool"
        );
    }

    #[test]
    fn inline_agent_type_tools_table_parses_independent_of_profile_level_tools() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "agent-scoped-tools",
            r#"
[tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
]

[agents.custom]
allowed_tools = ["read", "write"]

[agents.custom.tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { pattern = "**/build/**", verdict = "auto_approved" },
]
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "agent-scoped-tools").expect("the fixture loads");

        let profile_write_rules = &profile.tools.get("write").expect("write table").rules;
        assert_eq!(profile_write_rules.len(), 1);

        let custom_agent = profile.agents.get("custom").expect("the custom agent");
        let agent_write_rules = &custom_agent
            .tools
            .get("write")
            .expect("agent-scoped write table")
            .rules;
        assert_eq!(
            agent_write_rules.len(),
            2,
            "the agent-scoped table is independent of, and need not match, the \
             profile-level table for the same tool"
        );
    }

    #[test]
    fn tool_sandbox_rules_table_loads_cross_filesystem_rule() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "with-cross-fs",
            r#"
[tools.read]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { rule = "dot_path", verdict = "auto_denied" },
  { rule = "cross_filesystem", verdict = "user_approval_required" },
]
"#,
        );

        let profile = ProfileConfig::load(temp.path(), "with-cross-fs").expect("the fixture loads");
        let read_rules = &profile.tools.get("read").expect("read table").rules;
        assert_eq!(read_rules.len(), 3);
        assert!(
            matches!(
                read_rules[2],
                crate::sandbox::SandboxRuleConfig::Named {
                    rule: crate::sandbox::NamedSandboxRule::CrossFilesystem,
                    ..
                }
            ),
            "cross_filesystem must parse as a named rule under [tools.read]: {:?}",
            read_rules[2]
        );
    }

    #[test]
    fn cross_filesystem_named_rule_under_tool_bash_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-cross-fs-rule",
            r#"
[tools.bash]
rules = [
  { rule = "cross_filesystem", verdict = "auto_denied" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-cross-fs-rule")
            .expect_err("cross_filesystem under [tools.bash] must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "bash"
        ));
    }

    #[test]
    fn agent_scoped_cross_filesystem_rule_parses_for_read() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "agent-scoped-cross-fs",
            r#"
[tools.read]
rules = [
  { rule = "dot_path", verdict = "auto_denied" },
]

[agents.custom]
allowed_tools = ["read"]

[agents.custom.tools.read]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { rule = "cross_filesystem", verdict = "user_approval_required" },
]
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "agent-scoped-cross-fs").expect("the fixture loads");

        let profile_read_rules = &profile.tools.get("read").expect("read table").rules;
        assert_eq!(profile_read_rules.len(), 1);

        let custom_agent = profile.agents.get("custom").expect("the custom agent");
        let agent_read_rules = &custom_agent
            .tools
            .get("read")
            .expect("agent-scoped read table")
            .rules;
        assert_eq!(agent_read_rules.len(), 2);
        assert!(
            matches!(
                agent_read_rules[1],
                crate::sandbox::SandboxRuleConfig::Named {
                    rule: crate::sandbox::NamedSandboxRule::CrossFilesystem,
                    ..
                }
            ),
            "the agent-scoped table carries its own cross_filesystem rule: {:?}",
            agent_read_rules[1]
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
input_history_limit = "${INPUT_HISTORY_LIMIT}"

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
            ("INPUT_HISTORY_LIMIT".to_string(), "17".to_string()),
            ("TIMEOUT_SECONDS".to_string(), "120".to_string()),
            ("CAN_SPAWN".to_string(), "true".to_string()),
        ]);

        let profile =
            ProfileConfig::load_with_environment(temp.path(), "typed-values", &environment)
                .expect("the typed-value profile loads");

        assert_eq!(profile.engines["local"].temperature, Some(0.2));
        assert_eq!(profile.engines["local"].context_length, Some(128 * 1024));
        assert_eq!(profile.input_history_limit, 17);
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
        assert_eq!(profile.input_history_limit, 50);
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
    fn global_agent_type_file_is_loaded_for_a_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
        write_profile_config(&profiles_root, "work", "theme = \"dark\"\n");
        write_agent_type_file(
            &home,
            "agents/research.md",
            "+++\ncan_spawn = false\n+++\nInvestigate evidence.\n",
        );

        let profile = ProfileConfig::load(&profiles_root, "work").expect("profile loads");
        let research = profile.agents.get("research").expect("global agent type");
        assert!(!research.can_spawn);
        assert_eq!(research.persona.as_deref(), Some("Investigate evidence."));
    }

    #[test]
    fn malformed_markdown_agent_type_file_is_a_named_load_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
        write_profile_config(&profiles_root, "work", "theme = \"dark\"\n");
        write_agent_type_file(
            &home,
            "agents/research.md",
            "This file is missing frontmatter.\n",
        );

        let error =
            ProfileConfig::load(&profiles_root, "work").expect_err("malformed agent file fails");
        let message = error.to_string();
        assert!(message.contains("malformed agent-type file"), "{message}");
        assert!(message.contains("research.md"), "{message}");
    }

    #[test]
    fn profile_agent_type_file_shadows_global_only_for_that_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
        write_profile_config(&profiles_root, "work", "theme = \"dark\"\n");
        write_profile_config(&profiles_root, "other", "theme = \"dark\"\n");
        write_agent_type_file(
            &home,
            "agents/research.toml",
            "persona = \"global\"\ncan_spawn = true\n",
        );
        write_agent_type_file(
            &home,
            "profiles/work/agents/research.toml",
            "persona = \"profile\"\ncan_spawn = false\n",
        );

        let work = ProfileConfig::load(&profiles_root, "work").expect("work profile loads");
        let other = ProfileConfig::load(&profiles_root, "other").expect("other profile loads");
        assert_eq!(work.agents["research"].persona.as_deref(), Some("profile"));
        assert!(!work.agents["research"].can_spawn);
        assert_eq!(other.agents["research"].persona.as_deref(), Some("global"));
        assert!(other.agents["research"].can_spawn);
    }

    #[test]
    fn project_agent_type_file_shadows_profile_and_global_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
        let workspace_root = temp.path().join("project");
        write_profile_config(&profiles_root, DEFAULT_PROFILE_NAME, "theme = \"dark\"\n");
        let project_config = workspace_root
            .join(YLVA_DIR_NAME)
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME)
            .join(PROFILE_CONFIG_FILE_NAME);
        std::fs::create_dir_all(project_config.parent().expect("project config parent"))
            .expect("mkdir");
        std::fs::write(&project_config, "theme = \"dark\"\n").expect("write project config");
        write_agent_type_file(&home, "agents/research.toml", "persona = \"global\"\n");
        write_agent_type_file(
            &home,
            "profiles/default/agents/research.toml",
            "persona = \"profile\"\n",
        );
        write_agent_type_file(
            &workspace_root,
            ".ylva/agents/research.toml",
            "persona = \"project\"\n",
        );

        let profile = ProfileConfig::load_with_environment_at_workspace_root(
            &profiles_root,
            DEFAULT_PROFILE_NAME,
            &HashMap::new(),
            Some(&workspace_root),
        )
        .expect("project profile loads");
        assert_eq!(
            profile.agents["research"].persona.as_deref(),
            Some("project")
        );
    }

    #[test]
    fn project_environment_is_used_for_project_profile_interpolation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
        let workspace_root = temp.path().join("project");
        write_profile_config(&profiles_root, DEFAULT_PROFILE_NAME, "theme = \"light\"\n");

        let project_config = workspace_root
            .join(YLVA_DIR_NAME)
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME)
            .join(PROFILE_CONFIG_FILE_NAME);
        std::fs::create_dir_all(project_config.parent().expect("project config parent"))
            .expect("mkdir");
        std::fs::write(
            &project_config,
            r#"
[provider_connections.project]
provider = "ollama"
api_base_url = "${YLVA_PHASE10_2_PROJECT_URL}"
"#,
        )
        .expect("write project config");

        let project_environment = workspace_root
            .join(YLVA_DIR_NAME)
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME)
            .join(PROFILE_ENV_FILE_NAME);
        std::fs::create_dir_all(project_environment.parent().expect("project env parent"))
            .expect("mkdir");
        std::fs::write(
            &project_environment,
            "YLVA_PHASE10_2_PROJECT_URL=http://project.example\n",
        )
        .expect("write project environment");

        let environment = load_profile_environment_at_workspace_root(
            Some(&profile_env_path(&profiles_root, DEFAULT_PROFILE_NAME)),
            Some(&workspace_root),
            DEFAULT_PROFILE_NAME,
        )
        .expect("load project environment");
        let profile = ProfileConfig::load_with_environment_at_workspace_root(
            &profiles_root,
            DEFAULT_PROFILE_NAME,
            &environment,
            Some(&workspace_root),
        )
        .expect("project profile loads");

        assert_eq!(
            profile.provider_connections["project"]
                .api_base_url
                .as_deref(),
            Some("http://project.example")
        );
    }

    #[test]
    fn inline_agent_fields_override_a_file_definition() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
        write_profile_config(
            &profiles_root,
            "work",
            r#"
[agents.research]
persona = "Be concise."
timeout_secs = 30

[agents.research.compaction]
engine = "summary"
timeout_sec = 240
"#,
        );
        write_agent_type_file(
            &temp.path().join("home"),
            "agents/research.md",
            "+++\ncan_spawn = true\n+++\nInvestigate evidence.\n",
        );

        let profile = ProfileConfig::load(&profiles_root, "work").expect("profile loads");
        let research = profile.agents.get("research").expect("research agent type");
        assert_eq!(
            research.persona.as_deref(),
            Some("Be concise."),
            "the inline persona replaces the file-sourced persona"
        );
        assert_eq!(research.timeout_secs, Some(30));
        assert_eq!(
            research
                .compaction
                .as_ref()
                .and_then(|settings| settings.engine.as_deref()),
            Some("summary")
        );
        assert_eq!(
            research
                .compaction
                .as_ref()
                .and_then(|settings| settings.timeout_sec),
            Some(240)
        );
        assert!(research.can_spawn);
    }

    #[test]
    fn max_tool_iterations_defaults_to_the_shared_limit_when_unconfigured() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
        write_profile_config(
            &profiles_root,
            "work",
            "[agents.explorer]\npersona = \"You explore code.\"\n",
        );

        let profile = ProfileConfig::load(&profiles_root, "work").expect("profile loads");
        let explorer = profile.agents.get("explorer").expect("explorer agent type");
        assert_eq!(explorer.max_tool_iterations, None);
        assert_eq!(
            profile.max_tool_iterations_for("explorer"),
            crate::agent::DEFAULT_MAX_TOOL_USE_ITERATIONS
        );
        assert_eq!(
            profile.max_tool_iterations_for("default"),
            crate::agent::DEFAULT_MAX_TOOL_USE_ITERATIONS
        );
        assert_eq!(
            profile.max_tool_iterations_for("never-configured"),
            crate::agent::DEFAULT_MAX_TOOL_USE_ITERATIONS
        );
    }

    #[test]
    fn inline_max_tool_iterations_overrides_a_file_authored_definition() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
        write_profile_config(
            &profiles_root,
            "work",
            "[agents.research]\nmax_tool_iterations = 7\n",
        );
        write_agent_type_file(
            &home,
            "agents/research.md",
            "+++\ncan_spawn = true\nmax_tool_iterations = 12\n+++\nInvestigate evidence.\n",
        );

        let profile = ProfileConfig::load(&profiles_root, "work").expect("profile loads");
        let research = profile.agents.get("research").expect("research agent type");
        assert_eq!(
            research.max_tool_iterations,
            Some(7),
            "the inline override replaces the file-authored limit"
        );
        assert_eq!(profile.max_tool_iterations_for("research"), 7);

        write_profile_config(
            &profiles_root,
            "solo",
            "[agents.solo]\nmax_tool_iterations = 20\n",
        );
        let solo_profile = ProfileConfig::load(&profiles_root, "solo").expect("profile loads");
        assert_eq!(solo_profile.max_tool_iterations_for("solo"), 20);
    }

    #[test]
    fn a_zero_max_tool_iterations_is_rejected_during_profile_validation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
        write_profile_config(
            &profiles_root,
            "work",
            "[agents.explorer]\nmax_tool_iterations = 0\n",
        );

        let error = ProfileConfig::load(&profiles_root, "work")
            .expect_err("a zero iteration limit must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidMaxToolIterations { .. }
        ));
        let message = error.to_string();
        assert!(message.contains("max_tool_iterations = 0"), "{message}");
        assert!(message.contains("explorer"), "{message}");
    }

    #[test]
    fn a_zero_max_tool_iterations_in_an_agent_type_file_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
        write_profile_config(&profiles_root, "work", "\n");
        write_agent_type_file(
            &home,
            "agents/research.md",
            "+++\nmax_tool_iterations = 0\n+++\nInvestigate evidence.\n",
        );

        let error = ProfileConfig::load(&profiles_root, "work")
            .expect_err("a file-authored zero iteration limit must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidMaxToolIterations { .. }
        ));
        assert!(error.to_string().contains("research"), "{error}");
    }

    #[test]
    fn compaction_thresholds_default_and_parse_per_engine() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profiles_root = temp.path().join(PROFILES_DIRECTORY_NAME);
        write_profile_config(
            &profiles_root,
            "work",
            "\n[engines.fast]\nprovider = \"mock\"\nmodel = \"test-model\"\n",
        );

        let profile = ProfileConfig::load(&profiles_root, "work").expect("the fixture loads");
        assert_eq!(
            profile.compaction.timeout_sec,
            DEFAULT_COMPACTION_TIMEOUT_SECS
        );
        let fast = profile.engines.get("fast").expect("the fast engine");
        assert_eq!(fast.compact_target_ratio, 0.5, "target defaults to half");
        assert_eq!(
            fast.soft_compaction_threshold,
            DEFAULT_SOFT_COMPACTION_THRESHOLD
        );
        assert_eq!(
            fast.hard_compaction_threshold,
            DEFAULT_HARD_COMPACTION_THRESHOLD
        );

        write_profile_config(
            &profiles_root,
            "work",
            "\n[engines.fast]\nprovider = \"mock\"\nmodel = \"test-model\"\n\
             compact_target_ratio = 0.4\n\
             soft_compaction_threshold = 0.8\n\
             hard_compaction_threshold = 0.95\n",
        );
        let profile = ProfileConfig::load(&profiles_root, "work").expect("the fixture loads");
        let fast = profile.engines.get("fast").expect("the fast engine");
        assert_eq!(fast.compact_target_ratio, 0.4);
        assert_eq!(fast.soft_compaction_threshold, 0.8);
        assert_eq!(fast.hard_compaction_threshold, 0.95);
    }

    #[test]
    fn compaction_thresholds_must_be_ordered_target_below_soft_below_hard() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profiles_root = temp.path().join(PROFILES_DIRECTORY_NAME);

        for (knobs, expected_fragment) in [
            (
                "compact_target_ratio = 1.5\n",
                "compact_target_ratio must be > 0 and <= 1",
            ),
            (
                "soft_compaction_threshold = 0.0\n",
                "soft_compaction_threshold must be > 0 and <= 1",
            ),
            (
                "hard_compaction_threshold = 1.5\n",
                "hard_compaction_threshold must be > 0 and <= 1",
            ),
            (
                "compact_target_ratio = 0.8\nsoft_compaction_threshold = 0.7\n",
                "must be below soft_compaction_threshold",
            ),
            (
                "soft_compaction_threshold = 0.9\nhard_compaction_threshold = 0.9\n",
                "must be below hard_compaction_threshold",
            ),
        ] {
            write_profile_config(
                &profiles_root,
                "work",
                &format!("\n[engines.fast]\nprovider = \"mock\"\nmodel = \"test-model\"\n{knobs}"),
            );

            let error = ProfileConfig::load(&profiles_root, "work")
                .expect_err("an unordered compaction knob must be rejected");
            assert!(
                matches!(error, ConfigError::InvalidCompactionSetting { .. }),
                "{error}"
            );
            let message = error.to_string();
            assert!(message.contains(expected_fragment), "{message}");
            assert!(message.contains("fast"), "{message}");
        }
    }

    #[test]
    fn compaction_timeout_must_be_positive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profiles_root = temp.path().join(PROFILES_DIRECTORY_NAME);
        write_profile_config(
            &profiles_root,
            "work",
            "default_engine = \"chat\"\n[compaction]\nengine = \"summarizer\"\ntimeout_sec = 0\n",
        );

        let error = ProfileConfig::load(&profiles_root, "work")
            .expect_err("zero timeout cannot define a useful summary deadline");
        assert!(error.to_string().contains("summarizer"), "{error}");
        assert!(error.to_string().contains("greater than 0"), "{error}");
    }

    #[test]
    fn agent_compaction_settings_parse_and_override_independently() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "agent-compaction",
            r#"
default_engine = "chat"

[compaction]
engine = "profile-summary"
timeout_sec = 90

[agents.my-agent]
engine = "agent-chat"

[agents.my-agent.compaction]
engine = "agent-summary"
timeout_sec = "240"

[agents.timeout-only.compaction]
timeout_sec = 180
"#,
        );

        let profile = ProfileConfig::load(temp.path(), "agent-compaction")
            .expect("per-agent compaction settings load");
        let agent_compaction = profile.agents["my-agent"]
            .compaction
            .as_ref()
            .expect("agent compaction settings");
        assert_eq!(agent_compaction.engine.as_deref(), Some("agent-summary"));
        assert_eq!(agent_compaction.timeout_sec, Some(240));
        let timeout_only_compaction = profile.agents["timeout-only"]
            .compaction
            .as_ref()
            .expect("timeout-only override");
        assert_eq!(timeout_only_compaction.engine, None);
        assert_eq!(timeout_only_compaction.timeout_sec, Some(180));
        assert_eq!(
            profile.compaction.engine.as_deref(),
            Some("profile-summary")
        );
        assert_eq!(profile.compaction.timeout_sec, 90);
    }

    #[test]
    fn zero_agent_compaction_timeout_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "invalid-agent-compaction",
            "default_engine = \"chat\"\n[agents.my-agent.compaction]\ntimeout_sec = 0\n",
        );

        let error = ProfileConfig::load(temp.path(), "invalid-agent-compaction")
            .expect_err("zero per-agent compaction timeout is invalid");
        assert!(error.to_string().contains("my-agent"), "{error}");
        assert!(error.to_string().contains("greater than 0"), "{error}");
    }

    #[test]
    fn same_scope_project_agent_file_and_inline_table_are_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
        let workspace_root = temp.path().join("project");
        let project_config = workspace_root
            .join(YLVA_DIR_NAME)
            .join(PROFILES_DIRECTORY_NAME)
            .join(DEFAULT_PROFILE_NAME)
            .join(PROFILE_CONFIG_FILE_NAME);
        std::fs::create_dir_all(project_config.parent().expect("project config parent"))
            .expect("mkdir");
        std::fs::write(&project_config, "[agents.research]\npersona = \"inline\"\n")
            .expect("write project config");
        write_agent_type_file(
            &workspace_root,
            ".ylva/agents/research.md",
            "+++\n+++\nfile body\n",
        );

        let error = ProfileConfig::load_with_environment_at_workspace_root(
            &profiles_root,
            DEFAULT_PROFILE_NAME,
            &HashMap::new(),
            Some(&workspace_root),
        )
        .expect_err("same-scope project definitions must conflict");
        let message = error.to_string();
        assert!(message.contains("research"), "{message}");
        assert!(message.contains("project"), "{message}");
        assert!(message.contains("config.toml"), "{message}");
        assert!(message.contains("research.md"), "{message}");
    }

    #[test]
    fn same_scope_profile_agent_file_and_inline_table_are_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
        write_profile_config(
            &profiles_root,
            "work",
            "[agents.research]\npersona = \"inline\"\n",
        );
        write_agent_type_file(
            &home,
            "profiles/work/agents/research.md",
            "+++\n+++\nfile body\n",
        );

        let error = ProfileConfig::load(&profiles_root, "work")
            .expect_err("same-scope profile definitions must conflict");
        let message = error.to_string();
        assert!(message.contains("research"), "{message}");
        assert!(message.contains("profile"), "{message}");
        assert!(message.contains("config.toml"), "{message}");
        assert!(message.contains("research.md"), "{message}");
    }

    #[test]
    fn inline_profile_override_of_a_global_agent_type_is_allowed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
        write_profile_config(
            &profiles_root,
            "work",
            "[agents.research]\npersona = \"Use concise findings.\"\n",
        );
        write_agent_type_file(&home, "agents/research.md", "+++\n+++\nglobal body\n");

        let profile = ProfileConfig::load(&profiles_root, "work")
            .expect("cross-scope inline adaptation is valid");
        assert_eq!(
            profile.agents["research"].persona.as_deref(),
            Some("Use concise findings."),
            "the inline persona replaces the file-sourced persona"
        );
    }

    #[test]
    fn missing_config_fallback_still_loads_project_agent_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
        let workspace_root = temp.path().join("project");
        write_agent_type_file(
            &workspace_root,
            ".ylva/agents/research.toml",
            "persona = \"project body\"\n",
        );

        let profile = ProfileConfig::load_or_default_with_environment_at_workspace_root(
            &profiles_root,
            DEFAULT_PROFILE_NAME,
            &HashMap::new(),
            Some(&workspace_root),
        )
        .expect("missing default config falls back");
        assert_eq!(
            profile.agents["research"].persona.as_deref(),
            Some("project body")
        );
        assert!(
            profile.agents[DEFAULT_PROFILE_NAME].can_spawn,
            "the code-owned default remains available"
        );
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
    fn compaction_engine_falls_back_to_the_default_engine() {
        let profile = ProfileConfig {
            default_engine: "main".to_string(),
            compaction: CompactionConfig::default(),
            ..ProfileConfig::default()
        };
        assert_eq!(profile.compaction_engine_name(), "main");
    }

    #[test]
    fn utility_engine_falls_back_to_default_and_honors_explicit_selection() {
        let mut profile = ProfileConfig {
            default_engine: "main".to_string(),
            compaction: CompactionConfig::default(),
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
            compaction: CompactionConfig::default(),
            agents: HashMap::from([(
                "main".to_string(),
                AgentConfig {
                    engine: Some("fast".to_string()),
                    compaction: None,
                    description: None,
                    allowed_tools: Some(vec!["bash".to_string()]),
                    timeout_secs: Some(120),
                    max_tool_iterations: None,
                    persona: Some("You are a helpful assistant.".to_string()),
                    can_spawn: false,
                    tools: HashMap::new(),
                },
            )]),
            ..ProfileConfig::default()
        };

        assert_eq!(profile.default_engine, "default");
        assert!(profile.compaction.engine.is_none());

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
            compaction: None,
            description: None,
            allowed_tools: None,
            timeout_secs: None,
            max_tool_iterations: None,
            persona: None,
            can_spawn: true,
            tools: HashMap::new(),
        };

        assert!(agent_config.engine.is_none());
        assert!(agent_config.allowed_tools.is_none());
        assert!(agent_config.timeout_secs.is_none());
        assert!(agent_config.persona.is_none());
        assert!(agent_config.can_spawn);
    }

    // ── `network_request` rule-kind validation ────────────────────────────────

    #[test]
    fn network_request_rule_loads_under_tool_web_search() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "network-web-search",
            r#"
[tools.web_search]
rules = [
  { rule = "network_request", verdict = "auto_approved" },
]
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "network-web-search").expect("the fixture loads");
        let rules = &profile
            .tools
            .get("web_search")
            .expect("web_search table")
            .rules;
        assert_eq!(rules.len(), 1);
        assert!(matches!(
            rules[0],
            crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
                ..
            }
        ));
    }

    #[test]
    fn network_request_rule_loads_under_agent_scoped_web_search() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "agent-scoped-network",
            r#"
[agents.custom]
allowed_tools = ["web_search"]

[agents.custom.tools.web_search]
rules = [
  { rule = "network_request", verdict = "auto_denied", reason = "off" },
]
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "agent-scoped-network").expect("the fixture loads");
        let custom = profile.agents.get("custom").expect("the custom agent");
        let rules = &custom
            .tools
            .get("web_search")
            .expect("web_search table")
            .rules;
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn path_glob_rule_under_tool_web_search_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-path-rule-for-web-search",
            r#"
[tools.web_search]
rules = [
  { pattern = "**", verdict = "auto_approved" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-path-rule-for-web-search")
            .expect_err("a path rule under [tools.web_search] must be rejected");
        let message = error.to_string();
        assert!(matches!(
            error,
            ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "web_search"
        ));
        assert!(
            message.contains("network_request"),
            "the message must name the accepted rule kind: {message}"
        );
    }

    #[test]
    fn command_glob_rule_under_tool_web_search_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-command-rule-for-web-search",
            r#"
[tools.web_search]
rules = [
  { command = ["git", "status"], verdict = "auto_approved" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-command-rule-for-web-search")
            .expect_err("a command rule under [tools.web_search] must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "web_search"
        ));
    }

    #[test]
    fn network_request_rule_under_a_path_tool_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-network-rule-for-read",
            r#"
[tools.read]
rules = [
  { rule = "network_request", verdict = "auto_approved" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-network-rule-for-read")
            .expect_err("a network_request rule under [tools.read] must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "read"
        ));
    }

    #[test]
    fn network_request_rule_under_tool_bash_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-network-rule-for-bash",
            r#"
[tools.bash]
rules = [
  { rule = "network_request", verdict = "auto_approved" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-network-rule-for-bash")
            .expect_err("a network_request rule under [tools.bash] must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "bash"
        ));
    }

    #[test]
    fn target_on_a_non_network_rule_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "target-on-path-rule",
            r#"
[tools.read]
rules = [
  { rule = "outside_workspace", target = "https://docs.rs", verdict = "auto_approved" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "target-on-path-rule")
            .expect_err("target on a path rule must be rejected");
        let message = error.to_string();
        assert!(matches!(
            error,
            ConfigError::MisplacedSandboxRuleTarget { tool, index: 0, .. } if tool == "read"
        ));
        assert!(
            message.contains("network_request"),
            "the message must name the only rule kind accepting target: {message}"
        );
    }

    #[test]
    fn target_on_a_command_rule_under_tool_bash_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "target-on-command-rule",
            r#"
[tools.bash]
rules = [
  { rule = "backgrounded", target = "https://docs.rs", verdict = "auto_denied" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "target-on-command-rule")
            .expect_err("target on a command rule must be rejected");
        assert!(matches!(
            error,
            ConfigError::MisplacedSandboxRuleTarget { tool, index: 0, .. } if tool == "bash"
        ));
    }

    #[test]
    fn network_request_rules_with_and_without_target_load_under_tool_web_fetch() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "web-fetch-targets",
            r#"
[tools.web_fetch]
rules = [
  { rule = "network_request", verdict = "user_approval_required" },
  { rule = "network_request", target = "https://docs.rs", verdict = "auto_approved" },
]
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "web-fetch-targets").expect("the fixture loads");
        let rules = &profile
            .tools
            .get("web_fetch")
            .expect("web_fetch table")
            .rules;
        assert_eq!(rules.len(), 2);
        assert!(matches!(
            rules[0],
            crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
                target: None,
                ..
            }
        ));
        assert!(matches!(
            rules[1],
            crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
                ref target,
                ..
            } if target.as_deref() == Some("https://docs.rs")
        ));
    }

    #[test]
    fn schemeless_and_wildcard_targets_load_under_tool_web_fetch() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "web-fetch-target-vocabulary",
            r#"
[tools.web_fetch]
rules = [
  { rule = "network_request", target = "github.com", verdict = "auto_approved" },
  { rule = "network_request", target = "https://*.github.com", verdict = "auto_approved" },
  { rule = "network_request", target = "https://**.github.com", verdict = "auto_approved" },
  { rule = "network_request", target = "https://github.com:*", verdict = "auto_approved" },
]
"#,
        );

        let profile = ProfileConfig::load(temp.path(), "web-fetch-target-vocabulary")
            .expect("the full target vocabulary loads");
        let rules = &profile
            .tools
            .get("web_fetch")
            .expect("web_fetch table")
            .rules;
        assert_eq!(rules.len(), 4);
    }

    #[test]
    fn invalid_target_is_a_hard_error() {
        for (name, target) in [
            ("catch-all-target", "*"),
            ("mixed-wildcard-target", "https://a**b.example.com"),
            ("path-target", "https://github.com/owner"),
            ("bad-port-target", "https://github.com:https"),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            write_profile_config(
                temp.path(),
                name,
                &format!(
                    r#"
[tools.web_fetch]
rules = [
  {{ rule = "network_request", target = "{target}", verdict = "auto_approved" }},
]
"#
                ),
            );

            let error = ProfileConfig::load(temp.path(), name)
                .expect_err("an invalid target must be rejected at startup");
            assert!(
                matches!(
                    &error,
                    ConfigError::InvalidSandboxRuleTarget { tool, index: 0, detail, .. }
                        if tool == "web_fetch" && !detail.is_empty()
                ),
                "expected InvalidSandboxRuleTarget for '{target}', got {error:?}"
            );
        }
    }

    #[test]
    fn path_rule_under_tool_web_fetch_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-path-rule-for-web-fetch",
            r#"
[tools.web_fetch]
rules = [
  { pattern = "**", verdict = "auto_approved" },
]
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-path-rule-for-web-fetch")
            .expect_err("a path rule under [tools.web_fetch] must be rejected");
        assert!(matches!(
            error,
            ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "web_fetch"
        ));
    }

    // ── `[web_fetch]` section ─────────────────────────────────────────────────

    #[test]
    fn web_fetch_section_loads_explicit_values() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "explicit-web-fetch",
            r#"
[web_fetch]
backend = "native"
timeout_secs = 5
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "explicit-web-fetch").expect("the fixture loads");
        assert_eq!(profile.web_fetch.backend, "native");
        assert_eq!(profile.web_fetch.timeout_secs, 5);
    }

    #[test]
    fn absent_web_fetch_section_defaults_to_native() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(temp.path(), "no-web-fetch", "default_engine = \"fast\"\n");

        let profile = ProfileConfig::load(temp.path(), "no-web-fetch").expect("the fixture loads");
        assert_eq!(
            profile.web_fetch,
            crate::config::WebFetchConfig::default(),
            "an absent [web_fetch] section behaves like the written-out default"
        );
    }

    #[test]
    fn unknown_web_fetch_backend_is_a_hard_error_naming_the_supported_names() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-web-fetch-backend",
            r#"
[web_fetch]
backend = "camofox"
"#,
        );

        let error = ProfileConfig::load(temp.path(), "bad-web-fetch-backend")
            .expect_err("an unknown backend must be rejected");
        let message = error.to_string();
        assert!(matches!(
            error,
            ConfigError::InvalidWebFetchBackend { ref backend, .. } if backend == "camofox"
        ));
        assert!(
            message.contains("native"),
            "names accepted values: {message}"
        );
    }

    // ── `[web_search]` section ────────────────────────────────────────────────

    #[test]
    fn web_search_section_loads_explicit_values() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "explicit-web-search",
            r#"
[web_search]
backend = "duckduckgo"
timeout_secs = 5
result_limit = 4
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "explicit-web-search").expect("the fixture loads");
        assert_eq!(profile.web_search.backend, "duckduckgo");
        assert_eq!(profile.web_search.timeout_secs, 5);
        assert_eq!(profile.web_search.result_limit, 4);
    }

    #[test]
    fn absent_web_search_section_defaults_to_duckduckgo() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(temp.path(), "no-web-search", "default_engine = \"fast\"\n");

        let profile = ProfileConfig::load(temp.path(), "no-web-search").expect("the fixture loads");
        assert_eq!(
            profile.web_search,
            crate::config::WebSearchConfig::default(),
            "an absent [web_search] section behaves like the written-out default"
        );
        assert_eq!(profile.web_search.backend, "duckduckgo");
        assert_eq!(profile.web_search.timeout_secs, 30);
        assert_eq!(
            profile.web_search.result_limit,
            crate::agent::tools::backends::web_search::WEB_SEARCH_DEFAULT_RESULT_LIMIT
        );
    }

    #[test]
    fn unknown_web_search_backend_is_rejected_naming_the_config_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "bad-web-search-backend",
            "[web_search]\nbackend = \"startpage\"\n",
        );

        let error = ProfileConfig::load(temp.path(), "bad-web-search-backend")
            .expect_err("an unsupported backend must fail profile loading");
        let message = error.to_string();
        assert!(matches!(
            error,
            ConfigError::InvalidWebSearchBackend { backend, .. } if backend == "startpage"
        ));
        assert!(message.contains("startpage"), "{message}");
        assert!(
            message.contains("duckduckgo, searxng, brave"),
            "names every accepted value: {message}"
        );
        assert!(
            message.contains("config.toml"),
            "the error must name the config path: {message}"
        );
    }

    #[test]
    fn searxng_web_search_section_loads_its_instance_url() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "searxng-web-search",
            r#"
[web_search]
backend = "searxng"

[web_search.backends.searxng]
base_url = "https://searx.example.org"
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "searxng-web-search").expect("the fixture loads");
        assert_eq!(profile.web_search.backend, "searxng");
        assert_eq!(
            profile.web_search.backends.searxng,
            Some(crate::config::SearXngBackendConfig {
                base_url: Some("https://searx.example.org".to_string())
            })
        );
    }

    #[test]
    fn searxng_web_search_backend_without_an_instance_url_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "searxng-without-instance",
            "[web_search]\nbackend = \"searxng\"\n",
        );

        let error = ProfileConfig::load(temp.path(), "searxng-without-instance")
            .expect_err("a missing instance table must fail profile loading");
        let message = error.to_string();
        assert!(matches!(
            error,
            ConfigError::InvalidWebSearchBackendSetting { backend, .. } if backend == "searxng"
        ));
        assert!(
            message.contains("backends.searxng"),
            "names the config key: {message}"
        );
        assert!(
            message.contains("config.toml"),
            "the error must name the config path: {message}"
        );
    }

    #[test]
    fn brave_web_search_section_loads_its_api_key() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "brave-web-search",
            r#"
[web_search]
backend = "brave"

[web_search.backends.brave]
api_key = "test-subscription-token"
"#,
        );

        let profile =
            ProfileConfig::load(temp.path(), "brave-web-search").expect("the fixture loads");
        assert_eq!(profile.web_search.backend, "brave");
        assert_eq!(
            profile.web_search.backends.brave,
            Some(crate::config::BraveBackendConfig {
                api_key: Some("test-subscription-token".to_string())
            })
        );
        let rendered = format!("{profile:?}");
        assert!(
            !rendered.contains("test-subscription-token"),
            "the token must never reach a Debug output: {rendered}"
        );
    }

    #[test]
    fn brave_web_search_backend_without_the_table_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            "brave-without-table",
            "[web_search]\nbackend = \"brave\"\n",
        );

        let error = ProfileConfig::load(temp.path(), "brave-without-table")
            .expect_err("a missing brave table must fail profile loading");
        let message = error.to_string();
        assert!(matches!(
            error,
            ConfigError::InvalidWebSearchBackendSetting { backend, .. } if backend == "brave"
        ));
        assert!(
            message.contains("backends.brave"),
            "names the config key: {message}"
        );
        assert!(
            message.contains("config.toml"),
            "the error must name the config path: {message}"
        );
    }
}
