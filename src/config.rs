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

/// Per-agent configuration within a profile.
pub struct AgentConfig {
    /// Name of the engine to use for this agent type. When `None`, resolves to
    /// `ProfileConfig::default_engine`.
    pub engine: Option<String>,
    /// Subset of tool names the agent is permitted to invoke. When `None`, all
    /// registered tools are available. Tool filtering is implemented in Phase 6.
    pub allowed_tools: Option<Vec<String>>,
    /// Maximum wall-clock seconds the agent may run before being cancelled.
    /// `None` means no timeout is enforced.
    pub timeout_secs: Option<u64>,
    /// Optional persona instruction prepended to the system prompt.
    pub persona: Option<String>,
    /// When `true` the agent is permitted to spawn child agents via
    /// `AgentFactory::create` with `Role::Child`. Checked in Phase 6.
    pub can_spawn: bool,
}

/// Top-level runtime profile that groups engine selection and per-agent settings.
pub struct ProfileConfig {
    /// Name of the engine used for all agents unless overridden by
    /// `AgentConfig::engine`.
    pub default_engine: String,
    /// Name of the engine used for compact/summarisation tasks. When `None`,
    /// falls back to `default_engine`. Phase 7 wires this at runtime.
    pub compact_engine: Option<String>,
    /// Map of agent-type name to its configuration.
    pub agents: HashMap<String, AgentConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_config_field_access() {
        let profile = ProfileConfig {
            default_engine: "default".to_string(),
            compact_engine: None,
            agents: HashMap::from([(
                "main".to_string(),
                AgentConfig {
                    engine: Some("fast".to_string()),
                    allowed_tools: Some(vec!["bash".to_string()]),
                    timeout_secs: Some(120),
                    persona: Some("You are a helpful assistant.".to_string()),
                    can_spawn: false,
                },
            )]),
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
