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
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{Message, StreamEvent};
use crate::llm::engine::{EngineRuntimeConfig, EngineRuntimeConfigHandle};
use crate::llm::{ModelInfo, Provider, ToolSchema};

use super::OpenAiCompatibleProvider;

/// OpenRouter provider.
///
/// OpenRouter exposes both chat completions and its model catalog through the
/// OpenAI-compatible `/api/v1` path. The provider accepts the OpenRouter
/// origin and derives that path before constructing the shared transport.
#[derive(Debug)]
pub struct OpenRouterProvider {
    inner: OpenAiCompatibleProvider,
}

impl OpenRouterProvider {
    /// Create an OpenRouter provider.
    ///
    /// `base_url` should normally be `https://openrouter.ai`.
    pub fn new(name: String, base_url: String, model: String, api_key: Option<String>) -> Self {
        let openai_base_url = format!("{base_url}/api/v1");
        Self {
            inner: OpenAiCompatibleProvider::new_with_request_body_customizer(
                name,
                openai_base_url,
                model,
                api_key,
                Self::customize_request_body,
                super::openai_compatible_provider::redact_chat_completion_request_body,
            )
            .with_provider_kind("openrouter"),
        }
    }

    fn customize_request_body(
        request_body: &mut serde_json::Value,
        runtime_config: &EngineRuntimeConfig,
    ) {
        if let Some(reasoning_effort) = runtime_config.reasoning_effort.as_deref() {
            request_body["reasoning"] = serde_json::json!({
                "effort": reasoning_effort,
            });
        }
    }
}

impl Provider for OpenRouterProvider {
    fn with_runtime_config(
        &self,
        runtime_config: EngineRuntimeConfigHandle,
    ) -> anyhow::Result<std::sync::Arc<dyn Provider>> {
        Ok(std::sync::Arc::new(Self {
            inner: self.inner.clone_with_runtime_config(runtime_config),
        }))
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn runtime_config_handle(&self) -> Option<EngineRuntimeConfigHandle> {
        self.inner.runtime_config_handle()
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        self.inner.list_models()
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        self.inner.set_model(model)
    }

    fn set_reasoning_effort(&self, reasoning_effort: Option<&str>) -> anyhow::Result<()> {
        self.inner.set_runtime_reasoning_effort(reasoning_effort)
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner.chat(messages, tools, cancel)
    }

    fn chat_with_session_id<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner
            .chat_with_session_id(messages, tools, cancel, session_id)
    }

    fn chat_with_session_id_and_estimate<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<
            dyn FnOnce(usize, usize) -> futures::future::BoxFuture<'static, ()> + Send,
        >,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner.chat_with_session_id_and_estimate(
            messages,
            tools,
            cancel,
            session_id,
            on_estimate,
        )
    }

    fn chat_with_session_id_and_diagnostics<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<
            dyn FnOnce(usize, usize) -> futures::future::BoxFuture<'static, ()> + Send,
        >,
        on_failure: Option<crate::http::RequestDumpFailureHandler>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner.chat_with_session_id_and_diagnostics(
            messages,
            tools,
            cancel,
            session_id,
            on_estimate,
            on_failure,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegates_identity_runtime_settings_and_chat() {
        let provider = OpenRouterProvider::new(
            "openrouter".to_string(),
            "https://openrouter.ai".to_string(),
            "some/model".to_string(),
            Some("test-key".to_string()),
        );

        assert_eq!(provider.name(), "openrouter");
        let runtime_config = provider
            .runtime_config_handle()
            .expect("OpenRouter exposes Engine runtime settings");
        provider
            .set_model("other/model")
            .expect("OpenRouter supports model switching");
        assert_eq!(
            runtime_config
                .read()
                .expect("Engine runtime configuration lock")
                .model,
            "other/model"
        );

        let _chat_future = provider.chat(Vec::new(), Vec::new(), CancellationToken::new());
    }

    #[test]
    fn customizes_reasoning_request_body() {
        let mut request_body = serde_json::json!({});
        let runtime_config = crate::llm::engine::EngineRuntimeConfig {
            model: "some/model".to_string(),
            temperature: None,
            context_length: crate::llm::engine::DEFAULT_CONTEXT_LENGTH,
            reasoning_effort: Some("high".to_string()),
            input_modalities: None,
        };

        OpenRouterProvider::customize_request_body(&mut request_body, &runtime_config);

        assert_eq!(
            request_body,
            serde_json::json!({"reasoning": {"effort": "high"}})
        );
    }
}
