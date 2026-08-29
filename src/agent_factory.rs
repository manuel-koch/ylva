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
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::tools::{default_registry, ToolRegistryConfig, DEFAULT_CLI_TOOLS};
use crate::agent::{AgentLoop, Role, UiUpdate};
use crate::config::ProfileConfig;
use crate::engine::Engine;
use crate::sandbox::Sandbox;

/// Constructs [`AgentLoop`] instances from a named set of [`Engine`]s and a
/// [`ProfileConfig`].
///
/// Phase 5 supports only `Role::Root`; `Role::Child` returns an error until
/// Phase 6 implements it.
pub struct AgentFactory {
    engines: HashMap<String, Arc<dyn Engine>>,
    /// When `true`, the `edit` tool is auto-approved without user confirmation.
    /// Intended for automated / e2e testing where no human is present to approve.
    auto_approve_edit: bool,
    /// When `true`, the `write` tool is auto-approved without user confirmation.
    /// Intended for automated / e2e testing where no human is present to approve.
    auto_approve_write: bool,
    /// Explicit list of tool names to register. Defaults to [`DEFAULT_CLI_TOOLS`].
    allowed_tools: Vec<String>,
}

impl AgentFactory {
    pub fn new(engines: HashMap<String, Arc<dyn Engine>>) -> Self {
        AgentFactory {
            engines,
            auto_approve_edit: false,
            auto_approve_write: false,
            allowed_tools: DEFAULT_CLI_TOOLS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Enable automatic approval of `write` tool calls (no user prompt).
    pub fn with_auto_approve_write(mut self) -> Self {
        self.auto_approve_write = true;
        self
    }

    /// Enable automatic approval of `edit` tool calls (no user prompt).
    pub fn with_auto_approve_edit(mut self) -> Self {
        self.auto_approve_edit = true;
        self
    }

    /// Override the allowed tool set. Pass [`DEFAULT_CLI_TOOLS`], [`DEFAULT_TUI_TOOLS`],
    /// or a custom subset for per-agent restrictions.
    pub fn with_allowed_tools(mut self, tools: &[&str]) -> Self {
        self.allowed_tools = tools.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Creates an [`AgentLoop`] and its paired UI-update receiver.
    ///
    /// Engine resolution order:
    /// 1. `profile.agents[agent_type].engine` (when present).
    /// 2. `profile.default_engine` (fallback).
    ///
    /// Returns `Err` when:
    /// - the resolved engine name is not registered in `self.engines`.
    /// - `role` is `Role::Child` (Phase 6 will implement this path).
    pub fn create(
        &self,
        profile: &ProfileConfig,
        agent_type: &str,
        role: Role,
    ) -> anyhow::Result<(AgentLoop, mpsc::Receiver<UiUpdate>)> {
        if matches!(role, Role::Child) {
            anyhow::bail!("Role::Child is not yet implemented");
        }

        let engine_name = profile
            .agents
            .get(agent_type)
            .and_then(|agent_config| agent_config.engine.as_deref())
            .unwrap_or(profile.default_engine.as_str());

        let engine = self
            .engines
            .get(engine_name)
            .ok_or_else(|| anyhow::anyhow!("unknown engine: {engine_name}"))?;

        let sandbox = std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new()));
        let registry = Arc::new(default_registry(
            sandbox,
            ToolRegistryConfig {
                auto_approve_edit: self.auto_approve_edit,
                auto_approve_write: self.auto_approve_write,
                allowed_tools: self.allowed_tools.clone(),
            },
        ));
        let (agent_loop, ui_rx) = AgentLoop::new(Arc::clone(engine), registry);
        Ok((agent_loop, ui_rx))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::engine::{EngineConfig, EngineImpl, DEFAULT_CONTEXT_LENGTH};
    use crate::llm::MockProvider;
    use crate::{AgentConfig, ProfileConfig};

    fn test_engine_config() -> EngineConfig {
        EngineConfig {
            provider: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: DEFAULT_CONTEXT_LENGTH,
            api_base_url: None,
            api_key: None,
        }
    }

    fn make_factory() -> (AgentFactory, ProfileConfig) {
        let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, test_engine_config()));
        let factory = AgentFactory::new(HashMap::from([("default".to_string(), engine)]));
        let profile = ProfileConfig {
            default_engine: "default".to_string(),
            compact_engine: None,
            agents: HashMap::from([(
                "default".to_string(),
                AgentConfig {
                    engine: None,
                    allowed_tools: None,
                    timeout_secs: None,
                    persona: None,
                    can_spawn: true,
                },
            )]),
        };
        (factory, profile)
    }

    #[test]
    fn create_returns_ok_for_root_role_with_known_engine() {
        let (factory, profile) = make_factory();
        let result = factory.create(&profile, "default", Role::Root);
        assert!(
            result.is_ok(),
            "expected Ok for Role::Root with known engine"
        );
        let (_agent_loop, ui_rx) = result.unwrap();
        // The channel should still be open (sender not dropped).
        assert!(!ui_rx.is_closed(), "UiUpdate receiver should be open");
    }

    #[test]
    fn create_returns_err_for_unknown_engine() {
        let (factory, _) = make_factory();
        let profile = ProfileConfig {
            default_engine: "nonexistent".to_string(),
            compact_engine: None,
            agents: HashMap::new(),
        };
        let result = factory.create(&profile, "default", Role::Root);
        assert!(result.is_err(), "expected Err for unknown engine name");
        let error_message = result.err().unwrap().to_string();
        assert!(
            error_message.contains("nonexistent"),
            "error should mention the unknown engine name"
        );
    }

    #[test]
    fn create_returns_err_for_child_role() {
        let (factory, profile) = make_factory();
        let result = factory.create(&profile, "default", Role::Child);
        assert!(result.is_err(), "expected Err for Role::Child");
        let error_message = result.err().unwrap().to_string();
        assert!(
            error_message.contains("Role::Child"),
            "error should mention Role::Child"
        );
    }

    #[test]
    fn create_uses_agent_config_engine_when_present() {
        let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, test_engine_config()));
        let factory = AgentFactory::new(HashMap::from([("custom".to_string(), engine)]));
        let profile = ProfileConfig {
            default_engine: "nonexistent".to_string(),
            compact_engine: None,
            agents: HashMap::from([(
                "myagent".to_string(),
                AgentConfig {
                    engine: Some("custom".to_string()),
                    allowed_tools: None,
                    timeout_secs: None,
                    persona: None,
                    can_spawn: false,
                },
            )]),
        };
        let result = factory.create(&profile, "myagent", Role::Root);
        assert!(
            result.is_ok(),
            "should resolve engine from AgentConfig.engine override"
        );
    }
}
