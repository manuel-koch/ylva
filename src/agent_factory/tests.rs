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

use futures::future::BoxFuture;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::agent::StreamEvent;
use crate::llm::engine::{EngineConfig, EngineImpl};
use crate::llm::{MockProvider, ModelInfo, Provider, ToolSchema};
use crate::{AgentConfig, AssistantStatus, ProfileConfig};
use uuid::Uuid;

struct SelectableMockProvider {
    inner: MockProvider,
}

impl Provider for SelectableMockProvider {
    fn with_runtime_config(
        &self,
        _runtime_config: crate::llm::engine::EngineRuntimeConfigHandle,
    ) -> anyhow::Result<Arc<dyn Provider>> {
        Ok(Arc::new(Self {
            inner: self.inner.clone(),
        }))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner.chat(messages, tools, cancel)
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async {
            Ok([
                "main-model",
                "fast-model",
                "main-catalog-model",
                "fast-catalog-model",
            ]
            .into_iter()
            .map(|id| ModelInfo {
                reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
                id: id.to_string(),
                context_length: Some(32_000),
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            })
            .collect())
        })
    }

    fn set_model(&self, _model: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn test_engine_config() -> EngineConfig {
    EngineConfig {
        provider_connection: "mock".to_string(),
        model: "test-model".to_string(),
        temperature: None,
        context_length: None,
        api_base_url: None,
        api_key: None,
        reasoning: None,

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
    }
}

fn make_factory() -> (AgentFactory, ProfileConfig) {
    let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
    let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, test_engine_config()));
    let factory = AgentFactory::new(HashMap::from([("default".to_string(), engine)]));
    let profile = ProfileConfig {
        default_engine: "default".to_string(),
        compaction: crate::config::CompactionConfig::default(),
        agents: HashMap::from([(
            "default".to_string(),
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
            },
        )]),
        ..ProfileConfig::default()
    };
    (factory, profile)
}

/// A profile with two mock engines: `main` and `fast`.
fn two_engine_profile() -> ProfileConfig {
    let engine_config = |model: &str| EngineConfig {
        provider_connection: "mock".to_string(),
        model: model.to_string(),
        temperature: None,
        context_length: None,
        api_base_url: None,
        api_key: None,
        reasoning: None,

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
    };
    ProfileConfig {
        default_engine: "main".to_string(),
        engines: HashMap::from([
            ("main".to_string(), engine_config("main-model")),
            ("fast".to_string(), engine_config("fast-model")),
        ]),
        agents: HashMap::from([
            (
                "default".to_string(),
                AgentConfig {
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
                },
            ),
            (
                "quick".to_string(),
                AgentConfig {
                    engine: Some("fast".to_string()),
                    compaction: None,
                    description: None,
                    allowed_tools: None,
                    allowed_skills: None,
                    timeout_secs: None,
                    max_tool_iterations: None,
                    persona: None,
                    can_spawn: false,
                    tools: HashMap::new(),
                },
            ),
        ]),
        ..ProfileConfig::default()
    }
}

fn factory_for_two_engine_profile(profile: &ProfileConfig) -> AgentFactory {
    let provider = Arc::new(SelectableMockProvider {
        inner: MockProvider::new(vec![], Duration::ZERO),
    });
    let engines = profile
        .engines
        .iter()
        .map(|(name, config)| {
            (
                name.clone(),
                Arc::new(EngineImpl::new(provider.clone(), config.clone())) as Arc<dyn Engine>,
            )
        })
        .collect();
    AgentFactory::new(engines)
}

mod configuration;
mod roles;
mod selection;
mod skills;
mod subagents;
