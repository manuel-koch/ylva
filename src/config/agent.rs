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

use serde::Deserialize;

use super::profile::ToolSandboxConfig;

/// Per-agent configuration within a profile.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// Name of the engine to use for this agent type. When `None`, resolves to
    /// `ProfileConfig::default_engine`.
    #[serde(default)]
    pub engine: Option<String>,
    /// Optional model-facing summary of the tasks this agent type is suited
    /// for. The root agent receives this in the `spawn_subagent` tool
    /// description when choosing a child type.
    #[serde(default)]
    pub description: Option<String>,
    /// Subset of tool names the agent is permitted to invoke. When `None`, all
    /// registered tools are available. Tool filtering is implemented in Phase 6.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// Per-tool sandbox rule lists scoped to this agent type, keyed by tool
    /// name (`[agents.<type>.tools.<name>]`/agent-type-file `[tools.<name>]`
    /// tables). When a tool name has an entry here it takes precedence over
    /// the profile-level `ProfileConfig::tools` entry for the same tool, for
    /// this agent type only; when absent, resolution falls back to the
    /// profile-level entry and then the built-in default floor. See
    /// `docs/configuration.md`'s "Sandbox" section.
    #[serde(default)]
    pub tools: HashMap<String, ToolSandboxConfig>,
    /// Maximum wall-clock seconds the agent may run before being cancelled.
    /// `None` means no timeout is enforced.
    #[serde(default, deserialize_with = "deserialize_optional_u64_or_string")]
    pub timeout_secs: Option<u64>,
    /// Optional persona instruction prepended to the system prompt.
    #[serde(default)]
    pub persona: Option<String>,
    /// File-authored agent-type system-prompt body. Inline
    /// `config.toml` `[agents.<type>]` tables do not set this field; they
    /// provide profile-level overrides such as `persona`, `engine`, and
    /// `allowed_tools`.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// When `true` the agent is permitted to spawn child agents via
    /// `AgentFactory::create` with `Role::Child`. Checked in Phase 6.
    #[serde(default, deserialize_with = "deserialize_bool_or_string")]
    pub can_spawn: bool,
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
            serde::de::Error::custom(format!("invalid timeout_secs '{value}': {error}"))
        }),
        None => Ok(None),
    }
}

fn deserialize_bool_or_string<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BooleanValue {
        Native(bool),
        Text(String),
    }

    match BooleanValue::deserialize(deserializer)? {
        BooleanValue::Native(value) => Ok(value),
        BooleanValue::Text(value) => value.trim().parse::<bool>().map_err(|error| {
            serde::de::Error::custom(format!("invalid boolean value '{value}': {error}"))
        }),
    }
}
