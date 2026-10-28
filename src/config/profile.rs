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
    deserialize_reporting_unknown_keys,
    layered::{current_workspace_root, resolve_file_at_paths, ConfigLayer, ResolvedFile},
    load_profile_environment_at_workspace_root, AgentConfig, ConfigError, ConfigWarning,
    FavoriteModelsWriter, FavoriteSourceLayer, ProviderConnectionConfig, WebFetchConfig,
    WebSearchConfig, DEFAULT_PROFILE_NAME, PROFILES_DIRECTORY_NAME, PROFILE_CONFIG_FILE_NAME,
};

/// Top-level Ylva profile that groups engine selection and per-agent settings.
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
    /// Cap on subagents running at the same time across the whole agent
    /// tree (root plus all nested subagents). An effective value of `None`
    /// means unlimited; a configured value `<= 0` resolves to `None`. Queued
    /// spawns beyond the cap wait in FIFO order. Read through
    /// [`ProfileConfig::max_concurrent_subagents_limit`].
    #[serde(
        default = "default_max_concurrent_subagents",
        deserialize_with = "deserialize_max_concurrent_subagents"
    )]
    pub max_concurrent_subagents: Option<usize>,
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
    /// Unknown keys found while loading this profile (its `config.toml` and
    /// agent-type files). Never read from a file; callers surface them.
    #[serde(skip)]
    pub config_warnings: Vec<ConfigWarning>,
}

/// Settings for background conversation-summary compaction.
#[derive(Debug, Clone, Deserialize)]
pub struct CompactionConfig {
    /// Profile-level summarizer engine, unless an agent type overrides it.
    /// When absent, each agent follows its active conversation Engine runtime.
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
    /// the profile's `[compaction].engine`, then this agent type's configured
    /// conversation engine (its `engine` or the profile's `default_engine`).
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
#[serde(deny_unknown_fields)]
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
            if rule.is_read_tool_only() && tool_name != "read" {
                return Err(ConfigError::InvalidSandboxRuleKind {
                    path: path.to_path_buf(),
                    tool: tool_name.clone(),
                    index,
                    expected: "`skill_sidecar` is valid only for read".into(),
                });
            }
            if rule.has_target() && !rule.is_network_tool_only() {
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
                if rule.is_path_tool_only() || rule.is_network_tool_only() {
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
                if rule.is_bash_tool_only() || rule.is_path_tool_only() {
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
            } else if rule.is_bash_tool_only() || rule.is_network_tool_only() {
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
            rule.validate()
                .map_err(|source| ConfigError::InvalidSandboxRule {
                    path: path.to_path_buf(),
                    tool: tool_name.clone(),
                    index,
                    source,
                })?;
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

/// Validate every provider connection's `favorite_models`/`allowed_models`
/// and every nonempty configured model against its connection's policy.
///
/// Covers explicit `[engines.*]` models (including those inherited from
/// `default_model`) and the implicit engines the factory derives from
/// `default_model` for connections no explicit engine references. An implicit
/// engine without a model stays discovery-only and is not checked. No provider
/// catalog is consulted.
fn validate_provider_connection_models(
    profile: &ProfileConfig,
    path: &Path,
) -> Result<(), ConfigError> {
    let invalid = |connection: &str, detail: String| ConfigError::InvalidProviderConnectionModels {
        path: path.to_path_buf(),
        connection: connection.to_string(),
        detail,
    };
    let mut connection_names: Vec<_> = profile.provider_connections.keys().collect();
    connection_names.sort();
    for name in connection_names {
        let connection = &profile.provider_connections[name];
        let policy = connection
            .validate_preferences()
            .map_err(|detail| invalid(name, detail))?;
        let referenced_by_engine = profile
            .engines
            .values()
            .any(|engine| &engine.provider_connection == name);
        if !referenced_by_engine {
            if let Some(default_model) = profile.default_model.as_deref() {
                policy.ensure_allowed(name, default_model).map_err(|_| {
                    invalid(
                        name,
                        format!(
                            "default_model '{default_model}' used by its implicit engine is not \
                             permitted by allowed_models"
                        ),
                    )
                })?;
            }
        }
    }
    let mut engine_names: Vec<_> = profile.engines.keys().collect();
    engine_names.sort();
    for engine_name in engine_names {
        let engine = &profile.engines[engine_name];
        let Some(connection) = profile
            .provider_connections
            .get(&engine.provider_connection)
        else {
            continue;
        };
        if engine.model.is_empty() {
            continue;
        }
        let policy = connection
            .model_policy()
            .map_err(|error| invalid(&engine.provider_connection, error.to_string()))?;
        if !policy.allows(&engine.model) {
            return Err(invalid(
                &engine.provider_connection,
                format!(
                    "model '{}' of engine '{engine_name}' is not permitted by allowed_models",
                    engine.model
                ),
            ));
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

const DEFAULT_MAX_CONCURRENT_SUBAGENTS: usize = 4;

fn default_max_concurrent_subagents() -> Option<usize> {
    Some(DEFAULT_MAX_CONCURRENT_SUBAGENTS)
}

/// Maps a configured concurrency setting to its effective cap: a value `<= 0`
/// means unlimited and yields `None`; a positive value yields `Some`.
pub fn concurrency_limit_from_setting(value: i64) -> Option<usize> {
    usize::try_from(value).ok().filter(|limit| *limit > 0)
}

fn deserialize_max_concurrent_subagents<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ConcurrencyValue {
        Number(i64),
        Text(String),
    }

    let value = match ConcurrencyValue::deserialize(deserializer)? {
        ConcurrencyValue::Number(value) => value,
        ConcurrencyValue::Text(value) => value.trim().parse::<i64>().map_err(|error| {
            serde::de::Error::custom(format!(
                "invalid max_concurrent_subagents '{value}': {error}"
            ))
        })?,
    };
    Ok(concurrency_limit_from_setting(value))
}

fn default_agent_config() -> AgentConfig {
    AgentConfig {
        engine: None,
        compaction: None,
        description: None,
        allowed_tools: None,
        allowed_skills: None,
        timeout_secs: None,
        max_tool_iterations: None,
        persona: None,
        can_spawn: true,
        tools: HashMap::new(),
    }
}

impl Default for ProfileConfig {
    /// The in-code default profile: one `default` agent type that may spawn
    /// subagents, no configured engines (the caller supplies its
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
            max_concurrent_subagents: default_max_concurrent_subagents(),
            keybindings: HashMap::new(),
            agents: HashMap::from([(DEFAULT_PROFILE_NAME.to_string(), default_agent_config())]),
            tools: HashMap::new(),
            web_search: WebSearchConfig::default(),
            web_fetch: WebFetchConfig::default(),
            config_warnings: Vec::new(),
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
        Self::load_file_with_source(profiles_root, profile_name, environment, workspace_root)
            .map(|(profile, _source)| profile)
    }

    /// Load a profile like [`ProfileConfig::load_with_environment_at_workspace_root`]
    /// (or, when `fall_back_to_default` is set, like
    /// [`ProfileConfig::load_or_default_with_environment_at_workspace_root`]) and
    /// also return a [`FavoriteModelsWriter`] captured from the very document
    /// that was parsed. The writer is `None` for built-in defaults, which have
    /// no file to save to.
    pub fn load_with_favorite_writer(
        profiles_root: &Path,
        profile_name: &str,
        environment: &HashMap<String, String>,
        workspace_root: Option<&Path>,
        fall_back_to_default: bool,
    ) -> Result<(ProfileConfig, Option<FavoriteModelsWriter>), ConfigError> {
        match Self::load_file_with_source(profiles_root, profile_name, environment, workspace_root)
        {
            Ok((profile, source)) => {
                let writer = FavoriteSourceLayer::from_config_layer(source.layer).map(|layer| {
                    FavoriteModelsWriter::from_loaded_source(source.path, source.contents, layer)
                });
                Ok((profile, writer))
            }
            Err(ConfigError::ProfileNotFound { path, .. }) if fall_back_to_default => {
                tracing::debug!(path = %path.display(), "no profile config; using defaults");
                Self::load_agent_types_into_profile(
                    ProfileConfig::default(),
                    profiles_root,
                    profile_name,
                    workspace_root,
                )
                .map(|profile| (profile, None))
            }
            Err(other) => Err(other),
        }
    }

    fn load_file_with_source(
        profiles_root: &Path,
        profile_name: &str,
        environment: &HashMap<String, String>,
        workspace_root: Option<&Path>,
    ) -> Result<(ProfileConfig, ResolvedFile), ConfigError> {
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
        let mut warnings = Vec::new();
        let inline_agents = extract_agent_config_overrides(&document, &path, &mut warnings)?;
        document
            .as_table_mut()
            .expect("a parsed TOML document is always a table")
            .remove("agents");
        let mut profile: ProfileConfig =
            deserialize_reporting_unknown_keys(document, &path, "", &mut warnings).map_err(
                |source| ConfigError::Malformed {
                    path: path.clone(),
                    source,
                },
            )?;
        profile.apply_default_model();
        profile.agents = load_agent_types(
            profiles_root,
            profile_name,
            workspace_root,
            layer,
            &path,
            inline_agents,
            &mut warnings,
        )?;
        profile.config_warnings = warnings;
        validate_provider_connection_models(&profile, &path)?;
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
        Ok((
            profile,
            ResolvedFile {
                contents,
                layer,
                path,
            },
        ))
    }

    fn load_agent_types_into_profile(
        mut profile: ProfileConfig,
        profiles_root: &Path,
        profile_name: &str,
        workspace_root: Option<&Path>,
    ) -> Result<ProfileConfig, ConfigError> {
        let mut warnings = Vec::new();
        profile.agents = load_agent_types(
            profiles_root,
            profile_name,
            workspace_root,
            ConfigLayer::BuiltIn,
            Path::new("<built-in>"),
            HashMap::new(),
            &mut warnings,
        )?;
        profile.config_warnings = warnings;
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

    /// One consolidated, human-readable text listing every configuration
    /// warning found while loading, or `None` when there are none.
    pub fn config_warnings_notice(&self) -> Option<String> {
        if self.config_warnings.is_empty() {
            return None;
        }
        let lines: Vec<String> = self
            .config_warnings
            .iter()
            .map(|warning| format!("- {warning}"))
            .collect();
        Some(format!("Configuration warnings:\n{}", lines.join("\n")))
    }

    /// Effective cap on concurrently running subagents across the whole
    /// agent tree; `None` means unlimited.
    pub fn max_concurrent_subagents_limit(&self) -> Option<usize> {
        self.max_concurrent_subagents.filter(|limit| *limit > 0)
    }

    /// Apply the signed CLI override for the profile-level subagent concurrency
    /// cap. Non-positive values mean unlimited, matching profile deserialization.
    pub fn apply_max_concurrent_subagents_override(&mut self, value: i64) {
        self.max_concurrent_subagents = concurrency_limit_from_setting(value);
    }

    /// Profile-level compaction engine without agent-type resolution:
    /// `[compaction].engine` when set, else `default_engine`.
    /// Agent runs use [`Self::compaction_engine_name_for`] instead.
    pub fn compaction_engine_name(&self) -> &str {
        self.compaction
            .engine
            .as_deref()
            .unwrap_or(self.default_engine.as_str())
    }

    /// Explicit compaction-engine selection for one agent type, if configured.
    ///
    /// The per-agent value wins over the profile value. `None` means
    /// compaction follows the agent's active conversation Engine runtime.
    pub fn explicit_compaction_engine_name_for(&self, agent_type: &str) -> Option<&str> {
        self.agents
            .get(agent_type)
            .and_then(|agent_config| agent_config.compaction.as_ref())
            .and_then(|compaction| compaction.engine.as_deref())
            .or(self.compaction.engine.as_deref())
    }

    /// Initial compaction engine name for validation and startup checks: its
    /// explicit compaction override, then the profile's explicit compaction
    /// engine, then its configured conversation engine. Engine runtime binding
    /// for an implicit selection follows the conversation engine's active
    /// Engine runtime.
    pub fn compaction_engine_name_for(&self, agent_type: &str) -> &str {
        self.explicit_compaction_engine_name_for(agent_type)
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
    warnings: &mut Vec<ConfigWarning>,
) -> Result<HashMap<String, AgentConfigOverrides>, ConfigError> {
    let Some(agent_table) = document.get("agents") else {
        return Ok(HashMap::new());
    };
    deserialize_reporting_unknown_keys(agent_table.clone(), path, "agents", warnings).map_err(
        |source| ConfigError::Malformed {
            path: path.to_path_buf(),
            source,
        },
    )
}

fn load_agent_types(
    profiles_root: &Path,
    profile_name: &str,
    workspace_root: Option<&Path>,
    config_layer: ConfigLayer,
    config_path: &Path,
    inline_agents: HashMap<String, AgentConfigOverrides>,
    warnings: &mut Vec<ConfigWarning>,
) -> Result<HashMap<String, AgentConfig>, ConfigError> {
    let global_agents_root = profiles_root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(AGENTS_DIRECTORY_NAME);
    let profile_agents_root = profiles_root.join(profile_name).join(AGENTS_DIRECTORY_NAME);
    let project_agents_root =
        workspace_root.map(|root| root.join(YLVA_DIR_NAME).join(AGENTS_DIRECTORY_NAME));

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
            warnings.extend(file.definition.warnings);
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
            allowed_skills: None,
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
    if overrides.allowed_skills.is_some() {
        config.allowed_skills = overrides.allowed_skills;
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

pub(crate) fn resolve_profile_config_file(
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

/// Longest accepted profile name, in characters.
/// Directory holding agent-type files in the global, profile and project scopes.
pub(crate) const AGENTS_DIRECTORY_NAME: &str = "agents";

pub const MAX_PROFILE_NAME_LENGTH: usize = 64;

/// Whether `name` may name a profile directory.
///
/// A name starts with an ASCII letter or digit and continues with ASCII
/// letters, digits, `_`, or `-`, up to [`MAX_PROFILE_NAME_LENGTH`] characters.
/// Because separators and dots are rejected, a valid name is always a single
/// path component under the profiles root.
pub fn is_valid_profile_name(name: &str) -> bool {
    if name.len() > MAX_PROFILE_NAME_LENGTH {
        return false;
    }
    let mut characters = name.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_alphanumeric())
        && characters.all(|character| {
            character == '_' || character == '-' || character.is_ascii_alphanumeric()
        })
}

#[cfg(test)]
mod tests;
