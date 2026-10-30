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
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{Message, StreamEvent};
use crate::llm::copilot_provider::auth::{
    copilot_request_headers, CopilotAuth, COPILOT_MIN_REQUEST_INTERVAL,
};
use crate::llm::copilot_provider::catalog::CopilotModelCatalog;
use crate::llm::copilot_provider::github_protocol::{
    COPILOT_FALLBACK_API_BASE, COPILOT_TOKEN_EXCHANGE_URL,
};
use crate::llm::engine::{EngineRuntimeConfig, EngineRuntimeConfigHandle};
use crate::llm::{ModelInfo, Provider, RequestCost, ToolSchema};

use super::OpenAiCompatibleProvider;

/// Currency code for GitHub AI Credits, GitHub's own billing unit; costs are
/// kept in AIC and never converted to USD. See
/// <https://docs.github.com/en/copilot/concepts/billing-and-usage/individuals/billing>.
const AI_CREDITS_CURRENCY: &str = "AIC";
const NANO_AI_UNITS_PER_CREDIT: f64 = 1_000_000_000.0;

/// Parse the request cost Copilot reports as `usage.copilot_usage.total_nano_aiu`
/// (chat) or `copilot_usage.total_nano_aiu` (Responses completion)
/// in a stream frame and convert it to AI credits.
fn parse_reported_cost(data: &str) -> Option<RequestCost> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let nano_units = [
        "/usage/copilot_usage/total_nano_aiu",
        "/copilot_usage/total_nano_aiu",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer))
    .and_then(serde_json::Value::as_f64)
    .filter(|amount| amount.is_finite() && *amount >= 0.0)?;
    Some(RequestCost::new(
        nano_units / NANO_AI_UNITS_PER_CREDIT,
        AI_CREDITS_CURRENCY,
    ))
}

/// Copilot provider.
///
/// Wraps the shared OpenAI-compatible transport. The base URL and bearer
/// token are resolved per request from a [`CopilotAuth`], which exchanges the
/// long-lived GitHub token for a short-lived session token. The request cost
/// is read from the `copilot_usage` field Copilot reports, in AI credits.
#[derive(Debug)]
pub struct CopilotProvider {
    inner: OpenAiCompatibleProvider,
}

impl CopilotProvider {
    /// Create a Copilot provider from the long-lived `ghu_` GitHub token.
    ///
    /// Fails for PAT-looking or empty tokens.
    pub fn new(name: String, model: String, github_token: String) -> anyhow::Result<Self> {
        Self::with_endpoints(
            name,
            model,
            github_token,
            COPILOT_TOKEN_EXCHANGE_URL.to_string(),
            COPILOT_FALLBACK_API_BASE.to_string(),
            COPILOT_MIN_REQUEST_INTERVAL,
        )
    }

    /// Like [`Self::new`], with injectable token-exchange and fallback API
    /// base URLs and minimum request interval (used by tests).
    pub fn with_endpoints(
        name: String,
        model: String,
        github_token: String,
        token_exchange_url: String,
        fallback_api_base: String,
        min_request_interval: Duration,
    ) -> anyhow::Result<Self> {
        let transport = OpenAiCompatibleProvider::new_with_request_body_customizer(
            name,
            fallback_api_base.clone(),
            model,
            None,
            Self::customize_request_body,
            super::openai_compatible_provider::redact_chat_completion_request_body,
        )
        .with_provider_kind("copilot")
        .with_model_catalog(Arc::new(CopilotModelCatalog::default()))
        .with_usage_cost_parser(parse_reported_cost);
        let auth = CopilotAuth::with_urls(
            github_token,
            transport.http_client(),
            token_exchange_url,
            fallback_api_base,
            min_request_interval,
        )?;
        Ok(Self {
            inner: transport.with_request_authorizer(Arc::new(auth), copilot_request_headers()),
        })
    }
}

impl CopilotProvider {
    /// Copilot follows the OpenAI `reasoning_effort` field.
    fn customize_request_body(
        request_body: &mut serde_json::Value,
        runtime_config: &EngineRuntimeConfig,
        _session_id: Option<uuid::Uuid>,
    ) {
        if let Some(reasoning_effort) = runtime_config.reasoning_effort.as_deref() {
            request_body["reasoning_effort"] = serde_json::json!(reasoning_effort);
        }
    }
}

impl Provider for CopilotProvider {
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

pub mod auth;
mod catalog;
pub mod device_flow;
pub mod github_protocol;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reported_cost_in_ai_credits() {
        let cost =
            parse_reported_cost(r#"{"usage":{"copilot_usage":{"total_nano_aiu":2500000000}}}"#)
                .expect("cost");
        assert_eq!(cost, RequestCost::new(2.5, "AIC"));
        assert_eq!(
            parse_reported_cost(r#"{"usage":{"prompt_tokens":1}}"#),
            None
        );
        assert_eq!(
            parse_reported_cost(r#"{"usage":{"copilot_usage":{"total_nano_aiu":"1"}}}"#),
            None
        );
        assert_eq!(parse_reported_cost("not json"), None);
    }

    #[test]
    fn parses_reported_cost_from_responses_completion() {
        let cost = parse_reported_cost(
            r#"{"type":"response.completed","copilot_usage":{"total_nano_aiu":1000000000},"response":{}}"#,
        );
        assert_eq!(cost, Some(RequestCost::new(1.0, "AIC")));
    }

    #[test]
    fn rejects_personal_access_tokens() {
        let error = CopilotProvider::new(
            "copilot".to_string(),
            "gpt-4o".to_string(),
            "ghp_x".to_string(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("PAT unsupported"));
    }

    #[test]
    fn exposes_connection_name_and_model_updates() {
        let provider = CopilotProvider::new(
            "work-copilot".to_string(),
            "gpt-4o".to_string(),
            "ghu_x".to_string(),
        )
        .expect("provider");
        assert_eq!(provider.name(), "work-copilot");
        provider.set_model("claude-sonnet-4").expect("set model");
        let handle = provider.runtime_config_handle().expect("handle");
        assert_eq!(handle.read().unwrap().model, "claude-sonnet-4");
    }
}
