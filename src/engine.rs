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

use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{Message, StreamEvent};
use crate::llm::{Provider, ToolSchema};

/// Default context-length used when no explicit value is configured.
pub const DEFAULT_CONTEXT_LENGTH: usize = 128_000;

/// Configuration for an Engine.
///
/// **Note:** `model`, `temperature`, `context_length`, `api_base_url`, and `api_key`
/// are advisory in Phase 5 — `EngineImpl::chat()` delegates directly to the wrapped
/// `Provider`, which already bakes these settings in at construction time. Phase 7
/// wires these fields into provider construction from TOML config, making `EngineConfig`
/// the true SSOT for sampling parameters.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub provider: String,
    pub model: String,
    /// `None` means the provider uses the model's own default; `Some(v)` overrides it.
    pub temperature: Option<f32>,
    pub context_length: usize,
    pub api_base_url: Option<String>,
    pub api_key: Option<String>,
}

/// Runtime abstraction over a chat-completions-style streaming backend.
///
/// Decouples `AgentLoop` from a concrete `Provider` so that Phase 6 child-agent
/// construction can be done through `AgentFactory::create` rather than through
/// direct `AgentLoop::new` calls.
pub trait Engine: Send + Sync {
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>>;

    fn config(&self) -> &EngineConfig;
}

/// Default `Engine` implementation — thin wrapper around an `Arc<dyn Provider>`.
pub struct EngineImpl {
    provider: Arc<dyn Provider>,
    config: EngineConfig,
}

impl EngineImpl {
    pub fn new(provider: Arc<dyn Provider>, config: EngineConfig) -> Self {
        EngineImpl { provider, config }
    }
}

impl Engine for EngineImpl {
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.provider.chat(messages, tools, cancel)
    }

    fn config(&self) -> &EngineConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::llm::MockProvider;

    fn make_engine_config() -> EngineConfig {
        EngineConfig {
            provider: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: DEFAULT_CONTEXT_LENGTH,
            api_base_url: None,
            api_key: None,
        }
    }

    #[test]
    fn engine_impl_config_returns_injected_config() {
        let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
        let config = make_engine_config();
        let engine = EngineImpl::new(provider, config.clone());

        assert_eq!(engine.config().provider, config.provider);
        assert_eq!(engine.config().model, config.model);
        assert_eq!(engine.config().context_length, DEFAULT_CONTEXT_LENGTH);
    }

    #[tokio::test]
    async fn engine_impl_chat_delegates_to_mock_provider() {
        let provider = Arc::new(MockProvider::new(
            vec!["hello".to_string()],
            Duration::from_millis(0),
        ));
        let engine = EngineImpl::new(provider, make_engine_config());
        let cancel = CancellationToken::new();

        let result = engine.chat(vec![], vec![], cancel).await;
        assert!(
            result.is_ok(),
            "chat should delegate to MockProvider successfully"
        );
    }
}
