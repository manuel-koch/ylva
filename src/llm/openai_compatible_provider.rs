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

use base64::Engine as _;
use eventsource_stream::Eventsource;
use futures::future::BoxFuture;
use futures::StreamExt;
use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{DeltaChunk, Message, StreamEvent};
use crate::http::RequestConcurrencyLimiter;
use crate::llm::engine::{EngineRuntimeConfig, EngineRuntimeConfigHandle, DEFAULT_CONTEXT_LENGTH};
use crate::llm::openai_compatible_provider::messages::{
    serialize_messages_with_policy, ContentPartFormat,
};
use crate::llm::token_usage::{TokenUsageDumpSource, TokenUsageRecord};
use crate::llm::usage_cost::{CostLedger, RequestCost};
use crate::llm::{Provider, ToolSchema};

pub(crate) mod messages;

/// Extracts the request cost a backend reports in one SSE payload. Reported
/// costs are not part of the OpenAI Chat Completions protocol, so each backend
/// that reports them supplies its own parser.
pub(crate) type UsageCostParser = fn(&str) -> Option<RequestCost>;

type RequestBodyCustomizer = fn(&mut serde_json::Value, &EngineRuntimeConfig, Option<uuid::Uuid>);
pub(crate) type RequestBodyRedactor = fn(&serde_json::Value) -> serde_json::Value;

fn no_request_body_customization(
    _request_body: &mut serde_json::Value,
    _runtime_config: &EngineRuntimeConfig,
    _session_id: Option<uuid::Uuid>,
) {
}

/// Per-request endpoint and credential resolved by a [`RequestAuthorizer`].
pub(crate) struct RequestAuthorization {
    pub base_url: String,
    pub bearer_token: String,
}

impl std::fmt::Debug for RequestAuthorization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestAuthorization")
            .field("base_url", &self.base_url)
            .field("bearer_token", &"<redacted>")
            .finish()
    }
}

/// Resolves the base URL and bearer token before every request, for backends
/// whose credential is short-lived and exchanged at runtime.
pub(crate) trait RequestAuthorizer: Send + Sync + std::fmt::Debug {
    fn authorize(&self) -> BoxFuture<'_, anyhow::Result<RequestAuthorization>>;

    /// Headers derived from the conversation, added on top of the static
    /// extra headers.
    fn chat_request_headers(&self, _messages: &[Message]) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
}

const REDACTED_BASE64_RESULT: &str = "<REDACTED_BASE64_RESULT>";
const REDACTED_MEDIA_DATA: &str = "<REDACTED_MEDIA_DATA>";

/// Redact media payloads from a Chat Completions request copy used only for
/// request dumps. The live request body is never mutated.
pub(crate) fn redact_chat_completion_request_body(body: &serde_json::Value) -> serde_json::Value {
    let mut redacted = body.clone();
    if let Some(messages) = redacted
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    {
        for message in messages {
            redact_chat_completion_message(message);
        }
    }
    if let Some(items) = redacted
        .get_mut("input")
        .and_then(serde_json::Value::as_array_mut)
    {
        for item in items {
            redact_responses_input_item(item);
        }
    }
    redacted
}

fn redact_responses_input_item(item: &mut serde_json::Value) {
    let Some(object) = item.as_object_mut() else {
        return;
    };
    if object.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output") {
        if let Some(output) = object.get_mut("output") {
            if let Some(mut envelope) = output
                .as_str()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            {
                redact_tool_envelope(&mut envelope);
                if let Ok(serialized) = serde_json::to_string(&envelope) {
                    *output = serde_json::Value::String(serialized);
                }
            }
        }
        return;
    }
    for content_part in object
        .get_mut("content")
        .and_then(serde_json::Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        match content_part.get("type").and_then(serde_json::Value::as_str) {
            Some("input_image") => {
                if let Some(image_url) = content_part.get_mut("image_url") {
                    redact_data_uri_in_place(image_url);
                }
            }
            Some("input_file") => {
                if let Some(file_data) = content_part.get_mut("file_data") {
                    redact_data_uri_in_place(file_data);
                }
            }
            _ => {}
        }
    }
}

fn redact_chat_completion_message(message: &mut serde_json::Value) {
    let Some(object) = message.as_object_mut() else {
        return;
    };
    let role = object
        .get("role")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    match role.as_deref() {
        Some("tool") => {
            let Some(content) = object.get_mut("content") else {
                return;
            };
            let Some(content_text) = content.as_str() else {
                return;
            };
            let Ok(mut envelope) = serde_json::from_str::<serde_json::Value>(content_text) else {
                return;
            };
            redact_tool_envelope(&mut envelope);
            if let Ok(serialized) = serde_json::to_string(&envelope) {
                *content = serde_json::Value::String(serialized);
            }
        }
        Some("user") => {
            let Some(content_parts) = object
                .get_mut("content")
                .and_then(serde_json::Value::as_array_mut)
            else {
                return;
            };
            for content_part in content_parts {
                redact_chat_completion_content_part(content_part);
            }
        }
        _ => {}
    }
}

fn redact_tool_envelope(envelope: &mut serde_json::Value) {
    let Some(object) = envelope.as_object_mut() else {
        return;
    };
    let result_is_base64 = object
        .get("result_metadata")
        .and_then(|metadata| metadata.get("encoding"))
        .and_then(serde_json::Value::as_str)
        == Some("base64");
    let result_is_probable_base64 = object
        .get("result")
        .and_then(serde_json::Value::as_str)
        .is_some_and(is_probable_base64_payload);
    if result_is_base64 || result_is_probable_base64 {
        if let Some(result) = object.get_mut("result") {
            if result.is_string() {
                *result = serde_json::Value::String(REDACTED_BASE64_RESULT.to_string());
            }
        }
    }
}

fn redact_chat_completion_content_part(content_part: &mut serde_json::Value) {
    let Some(object) = content_part.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(serde_json::Value::as_str) {
        Some("image_url") => {
            if let Some(image_url) = object.get_mut("image_url") {
                redact_image_url(image_url);
            }
        }
        Some("file") => {
            if let Some(file_data) = object
                .get_mut("file")
                .and_then(serde_json::Value::as_object_mut)
                .and_then(|file| file.get_mut("file_data"))
            {
                redact_data_uri_in_place(file_data);
            }
        }
        _ => {}
    }
}

fn redact_image_url(image_url: &mut serde_json::Value) {
    if let Some(url) = image_url
        .as_object_mut()
        .and_then(|object| object.get_mut("url"))
    {
        redact_data_uri_in_place(url);
    } else {
        redact_data_uri_in_place(image_url);
    }
}

fn redact_data_uri_in_place(value: &mut serde_json::Value) {
    if let Some(data_uri) = value.as_str() {
        if let Some(redacted_data_uri) = redact_data_uri(data_uri) {
            *value = serde_json::Value::String(redacted_data_uri);
        }
    }
}

fn is_probable_base64_payload(value: &str) -> bool {
    value.len() >= 8
        && value.len().is_multiple_of(4)
        && base64::engine::general_purpose::STANDARD
            .decode(value)
            .is_ok()
}

fn redact_data_uri(value: &str) -> Option<String> {
    let (prefix, payload) = value.split_once(";base64,")?;
    if !prefix.starts_with("data:") || payload.is_empty() {
        return None;
    }
    Some(format!("{prefix};base64,{REDACTED_MEDIA_DATA}"))
}

/// Shared streaming transport targeting the OpenAI chat-completions spec.
///
/// `base_url` is a **generic provider concept** interpreted solely by this
/// concrete implementation: it includes the backend origin and version prefix
/// used by the OpenAI-compatible chat endpoint (for example,
/// `https://openrouter.ai/api/v1`). The provider appends
/// `/chat/completions` internally. Backend-specific providers may wrap this
/// transport when their model catalog lives outside that path, as Ollama's
/// native `/api` endpoints do.
///
/// Pre-stream failures (`429`/`5xx`/connect) are retried with bounded
/// exponential backoff honoring `Retry-After`; once streaming begins no retry
/// occurs and provider error frames or incomplete streams surface as
/// `StreamEvent::Error`.
#[derive(Debug, Clone)]
pub struct OpenAiCompatibleProvider {
    name: String,
    base_url: String,
    /// Behind one lock so model and future request-level settings are
    /// snapshotted atomically for each request.
    runtime_config: EngineRuntimeConfigHandle,
    api_key: Option<String>,
    client: reqwest::Client,
    request_body_customizer: RequestBodyCustomizer,
    request_body_redactor: RequestBodyRedactor,
    content_part_format: ContentPartFormat,
    token_usage_dump_source: TokenUsageDumpSource,
    request_dump_source: crate::http::RequestDumpSource,
    request_limiter: RequestConcurrencyLimiter,
    provider_kind: &'static str,
    cost_ledger: CostLedger,
    /// `None` when the backend does not report request costs.
    usage_cost_parser: Option<UsageCostParser>,
    /// Overrides `base_url` and `api_key` on every request when set.
    request_authorizer: Option<std::sync::Arc<dyn RequestAuthorizer>>,
    /// Static headers added to every chat and model-listing request.
    extra_headers: Vec<(&'static str, &'static str)>,
    /// Interprets the backend's `/models` body and endpoint routing.
    model_catalog: std::sync::Arc<dyn ModelCatalog>,
}

/// Translates a backend's `GET /models` body into [`ModelInfo`]s and tells
/// which models need the Responses API. Implementations hold any state they
/// learn from the listing, shared by all clones of a provider.
pub(crate) trait ModelCatalog: Send + Sync + std::fmt::Debug {
    /// Translate a `/models` body; may record state for later routing.
    fn parse(&self, body: &serde_json::Value) -> Vec<crate::llm::ModelInfo>;

    /// Whether a listing has been parsed, when routing depends on one.
    fn is_loaded(&self) -> bool {
        true
    }

    /// Whether `model` is served only through `/responses`.
    fn uses_responses_api(&self, _model: &str) -> bool {
        false
    }
}

/// Catalog of generic OpenAI-compatible backends: entries with a string `id`,
/// plus best-effort `context_length`, a top-level `reasoning` object and
/// `architecture.input_modalities` / `input_modalities` when present.
#[derive(Debug, Default)]
pub(crate) struct DefaultModelCatalog;

impl ModelCatalog for DefaultModelCatalog {
    fn parse(&self, body: &serde_json::Value) -> Vec<crate::llm::ModelInfo> {
        model_list_entries(body)
            .filter_map(|entry| {
                let id = entry.get("id")?.as_str()?;
                let (reasoning_efforts, reasoning_default) =
                    crate::llm::parse_reasoning_object(entry.get("reasoning"));
                Some(crate::llm::ModelInfo {
                    id: id.to_string(),
                    context_length: entry
                        .get("context_length")
                        .and_then(serde_json::Value::as_u64)
                        .map(|length| length as usize),
                    reasoning_efforts,
                    reasoning_default,
                    input_modalities: entry
                        .pointer("/architecture/input_modalities")
                        .or_else(|| entry.get("input_modalities"))
                        .and_then(crate::llm::parse_input_modality_names),
                })
            })
            .collect()
    }
}

/// The entries of a `/models` body's `data` array.
pub(crate) fn model_list_entries(
    body: &serde_json::Value,
) -> impl Iterator<Item = &serde_json::Value> {
    body.get("data")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
}

fn join_endpoint(base_url: &str, endpoint: &str) -> String {
    format!("{}/{endpoint}", base_url.trim_end_matches('/'))
}

impl OpenAiCompatibleProvider {
    pub(crate) fn clone_with_runtime_config(
        &self,
        runtime_config: EngineRuntimeConfigHandle,
    ) -> Self {
        Self {
            runtime_config,
            ..self.clone()
        }
    }

    /// Create a provider for an OpenAI-compatible backend.
    ///
    /// `name` is the short identifier for this provider instance (for example,
    /// an Ollama connection name or `"openrouter"`).
    /// `base_url` must include the backend origin and version prefix used by
    /// the OpenAI-compatible chat endpoint.
    /// `api_key` is `None` for backends that need no auth (local Ollama).
    pub fn new(name: String, base_url: String, model: String, api_key: Option<String>) -> Self {
        Self::new_with_request_body_customizer(
            name,
            base_url,
            model,
            api_key,
            no_request_body_customization,
            redact_chat_completion_request_body,
        )
    }

    pub(crate) fn new_with_request_body_customizer(
        name: String,
        base_url: String,
        model: String,
        api_key: Option<String>,
        request_body_customizer: RequestBodyCustomizer,
        request_body_redactor: RequestBodyRedactor,
    ) -> Self {
        Self::new_with_request_body_customizer_and_format(
            name,
            base_url,
            model,
            api_key,
            request_body_customizer,
            ContentPartFormat::OpenAiCompatible,
            request_body_redactor,
        )
    }

    pub(crate) fn new_with_request_body_customizer_and_format(
        name: String,
        base_url: String,
        model: String,
        api_key: Option<String>,
        request_body_customizer: RequestBodyCustomizer,
        content_part_format: ContentPartFormat,
        request_body_redactor: RequestBodyRedactor,
    ) -> Self {
        let request_limiter = crate::llm::provider_request_limiter(&name);
        OpenAiCompatibleProvider {
            name,
            base_url,
            runtime_config: std::sync::Arc::new(std::sync::RwLock::new(EngineRuntimeConfig {
                context_length_override: None,
                model,
                temperature: None,
                context_length: DEFAULT_CONTEXT_LENGTH,
                reasoning_effort: None,
                input_modalities: None,
            })),
            api_key,
            client: crate::http::provider_client_builder(
                crate::http::CONNECT_TIMEOUT,
                crate::http::READ_IDLE_TIMEOUT,
            )
            .build()
            .expect("shared HTTP client construction must succeed"),
            request_body_customizer,
            request_body_redactor,
            content_part_format,
            token_usage_dump_source: TokenUsageDumpSource::default(),
            request_dump_source: crate::http::RequestDumpSource::default(),
            request_limiter,
            provider_kind: "openai_compatible",
            cost_ledger: CostLedger::process(),
            usage_cost_parser: None,
            request_authorizer: None,
            extra_headers: Vec::new(),
            model_catalog: std::sync::Arc::new(DefaultModelCatalog),
        }
    }

    /// Resolve the endpoint and bearer token per request and send
    /// `extra_headers` with every request.
    pub(crate) fn with_request_authorizer(
        mut self,
        request_authorizer: std::sync::Arc<dyn RequestAuthorizer>,
        extra_headers: Vec<(&'static str, &'static str)>,
    ) -> Self {
        self.request_authorizer = Some(request_authorizer);
        self.extra_headers = extra_headers;
        self
    }

    /// Send `extra_headers` with every chat and model-listing request.
    /// Header names must be lowercase.
    pub(crate) fn with_extra_headers(
        mut self,
        extra_headers: Vec<(&'static str, &'static str)>,
    ) -> Self {
        self.extra_headers = extra_headers;
        self
    }

    /// Interpret model listings, and route models the catalog marks as
    /// `/responses`-only through the Responses API.
    pub(crate) fn with_model_catalog(
        mut self,
        model_catalog: std::sync::Arc<dyn ModelCatalog>,
    ) -> Self {
        self.model_catalog = model_catalog;
        self
    }

    pub(crate) fn with_provider_kind(mut self, provider_kind: &'static str) -> Self {
        self.provider_kind = provider_kind;
        self
    }

    /// Record the costs this backend reports in its stream frames. Without a
    /// parser, reported costs are ignored.
    pub(crate) fn with_usage_cost_parser(mut self, usage_cost_parser: UsageCostParser) -> Self {
        self.usage_cost_parser = Some(usage_cost_parser);
        self
    }

    #[cfg(test)]
    fn with_cost_ledger(mut self, cost_ledger: CostLedger) -> Self {
        self.cost_ledger = cost_ledger;
        self
    }

    #[cfg(test)]
    fn with_request_dump_source(mut self, source: crate::http::RequestDumpSource) -> Self {
        self.request_dump_source = source;
        self
    }

    #[cfg(test)]
    fn with_token_usage_dump_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.token_usage_dump_source = TokenUsageDumpSource::from_template(path);
        self
    }

    /// Compose the model-listing endpoint from `base_url`. The concrete
    /// provider owns this path interpretation; the trait and `base_url`'s
    /// meaning never leak the OpenAI path structure.
    fn models_url(&self) -> String {
        join_endpoint(&self.base_url, "models")
    }

    /// Return a clone of the HTTP client for a provider-specific request.
    pub(crate) fn http_client(&self) -> reqwest::Client {
        self.client.clone()
    }

    /// Return the API key for a provider-specific request.
    pub(crate) fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// Snapshot all mutable request-level settings before starting a request.
    fn current_runtime_config(&self) -> anyhow::Result<EngineRuntimeConfig> {
        self.runtime_config
            .read()
            .map(|config| config.clone())
            .map_err(|_| anyhow::anyhow!("the engine runtime configuration lock is poisoned"))
    }

    pub(crate) fn set_runtime_reasoning_effort(
        &self,
        reasoning_effort: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut runtime_config = self
            .runtime_config
            .write()
            .map_err(|_| anyhow::anyhow!("the engine runtime configuration lock is poisoned"))?;
        runtime_config.reasoning_effort = reasoning_effort.map(String::from);
        Ok(())
    }
}

impl Provider for OpenAiCompatibleProvider {
    fn with_runtime_config(
        &self,
        runtime_config: EngineRuntimeConfigHandle,
    ) -> anyhow::Result<std::sync::Arc<dyn Provider>> {
        Ok(std::sync::Arc::new(
            self.clone_with_runtime_config(runtime_config),
        ))
    }

    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn runtime_config_handle(&self) -> Option<EngineRuntimeConfigHandle> {
        Some(std::sync::Arc::clone(&self.runtime_config))
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>> {
        let models_url = self.models_url();
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        let request_limiter = self.request_limiter.clone();
        let request_authorizer = self.request_authorizer.clone();
        let extra_headers = self.extra_headers.clone();
        let model_catalog = std::sync::Arc::clone(&self.model_catalog);
        Box::pin(async move {
            let _permit = request_limiter.acquire().await?;
            let (models_url, api_key) = match &request_authorizer {
                Some(request_authorizer) => {
                    let authorization = request_authorizer.authorize().await?;
                    (
                        join_endpoint(&authorization.base_url, "models"),
                        Some(authorization.bearer_token),
                    )
                }
                None => (models_url, api_key),
            };
            let body = crate::http::get_json_with_headers(
                &client,
                &models_url,
                api_key.as_deref(),
                &extra_headers,
                "model listing",
                &crate::http::RequestDumpSource::Environment,
            )
            .await?;
            Ok(model_catalog.parse(&body))
        })
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        let mut runtime_config = self
            .runtime_config
            .write()
            .map_err(|_| anyhow::anyhow!("the engine runtime configuration lock is poisoned"))?;
        runtime_config.model = model.to_string();
        Ok(())
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.chat_with_session_id(messages, tools, cancel, None)
    }

    fn chat_with_session_id<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.chat_with_session_id_and_estimate(
            messages,
            tools,
            cancel,
            session_id,
            Box::new(|_, _| Box::pin(async {})),
        )
    }

    fn chat_with_session_id_and_estimate<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.chat_with_session_id_and_diagnostics(
            messages,
            tools,
            cancel,
            session_id,
            on_estimate,
            None,
        )
    }

    fn chat_with_session_id_and_diagnostics<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        session_id: Option<uuid::Uuid>,
        on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
        on_failure: Option<crate::http::RequestDumpFailureHandler>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let runtime_config = match self.current_runtime_config() {
            Ok(runtime_config) => runtime_config,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let provider_connection = self.name.clone();
        let base_url = self.base_url.clone();
        let model_catalog = std::sync::Arc::clone(&self.model_catalog);
        let catalog_provider = (!model_catalog.is_loaded()).then(|| self.clone());
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let request_body_customizer = self.request_body_customizer;
        let request_body_redactor = self.request_body_redactor;
        let content_part_format = self.content_part_format;
        let provider_kind = self.provider_kind;
        let cost_ledger = self.cost_ledger.clone();
        let usage_cost_parser = self.usage_cost_parser;
        let request_limiter = self.request_limiter.clone();
        let request_authorizer = self.request_authorizer.clone();
        let extra_headers = self.extra_headers.clone();
        let token_usage_dump_source = self.token_usage_dump_source.clone();
        let request_dump_source = self.request_dump_source.clone();
        let tools: Vec<serde_json::Value> = if tools.is_empty() {
            Vec::new()
        } else {
            tools.iter().map(ToolSchema::to_request_value).collect()
        };
        Box::pin(async move {
            if let Some(catalog_provider) = catalog_provider.filter(|_| !model_catalog.is_loaded())
            {
                // The catalog tells which models need `/responses`; a
                // failure here falls back to chat completions. Re-checked so
                // concurrent first chats skip a listing another one finished.
                if let Err(error) = catalog_provider.list_models().await {
                    tracing::debug!(%error, "model catalog unavailable for endpoint selection");
                }
            }
            let use_responses = model_catalog.uses_responses_api(&runtime_config.model);
            let serialized_messages = serialize_messages_with_policy(
                &messages,
                runtime_config.input_modalities.as_deref(),
                content_part_format,
            );
            let request_body = if use_responses {
                crate::llm::openai_responses::responses_request_body(
                    &runtime_config.model,
                    &serialized_messages,
                    &tools,
                    runtime_config.temperature,
                    runtime_config.reasoning_effort.as_deref(),
                )
            } else {
                let mut request_body = serde_json::json!({
                    "model": runtime_config.model,
                    "messages": serialized_messages,
                    "stream": true,
                    "stream_options": { "include_usage": true },
                });
                if let Some(temperature) = runtime_config.temperature {
                    request_body["temperature"] = serde_json::json!(temperature);
                }
                request_body_customizer(&mut request_body, &runtime_config, session_id);
                if !tools.is_empty() {
                    request_body["tools"] = serde_json::Value::Array(tools);
                    request_body["tool_choice"] = serde_json::json!("auto");
                }
                request_body
            };
            let endpoint = if use_responses {
                "responses"
            } else {
                "chat/completions"
            };
            let (completions_url, api_key) = match &request_authorizer {
                Some(request_authorizer) => {
                    let authorization = request_authorizer.authorize().await?;
                    (
                        join_endpoint(&authorization.base_url, endpoint),
                        Some(authorization.bearer_token),
                    )
                }
                None => (join_endpoint(&base_url, endpoint), api_key),
            };
            let derived_headers = request_authorizer
                .as_ref()
                .map(|request_authorizer| request_authorizer.chat_request_headers(&messages))
                .unwrap_or_default();
            let mut headers = HeaderMap::new();
            for (name, value) in extra_headers.iter().chain(&derived_headers) {
                headers.insert(
                    reqwest::header::HeaderName::from_static(name),
                    reqwest::header::HeaderValue::from_static(value),
                );
            }
            if let Some(value) = &api_key {
                let authorization = format!("{} {}", "Bearer", value);
                headers.insert(AUTHORIZATION, authorization.parse()?);
            }
            let dump_context = crate::http::RequestDumpContext::new(&request_dump_source);
            let request_dump_failure_handler = on_failure;
            if let Some(handler) = &request_dump_failure_handler {
                let response_dump_context = dump_context.clone();
                handler.register(move |failure: &str| {
                    response_dump_context.dump_failure_response(failure);
                });
            }
            let request_id = uuid::Uuid::new_v4();
            let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let serialized_request_body = request_body.to_string();
            let estimated_input_tokens =
                crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&serialized_request_body);
            on_estimate(estimated_input_tokens, runtime_config.context_length).await;
            let connection_for_usage = provider_connection.clone();
            let model_for_usage = runtime_config.model.clone();
            let token_usage_record =
                move |reported_input_tokens: Option<(usize, &'static str)>| TokenUsageRecord {
                    request_id,
                    timestamp: timestamp.clone(),
                    session_id,
                    provider: provider_kind.to_string(),
                    provider_connection: connection_for_usage.clone(),
                    model: model_for_usage.clone(),
                    estimated_input_tokens,
                    estimate_source: "cl100k_base_serialized_chat_request",
                    reported_input_tokens: reported_input_tokens.map(|(tokens, _)| tokens),
                    reported_source: reported_input_tokens.map(|(_, source)| source),
                };
            let dump_id = dump_context.dump_id().to_string();
            let request_dump_path = dump_context.request_path();
            // Log the effective request snapshot, never request headers (which
            // carry the Authorization/API-key value) or the request body.
            tracing::debug!(
                provider_connection = %provider_connection,
                url = %completions_url,
                model = %runtime_config.model,
                temperature = ?runtime_config.temperature,
                effective_context_length = runtime_config.context_length,
                reasoning_effort = %crate::llm::format_reasoning_effort_for_log(
                    runtime_config.reasoning_effort.as_deref()
                ),
                input_modalities = %crate::llm::format_input_modalities_for_log(
                    runtime_config.input_modalities.as_deref()
                ),
                dump_id = %dump_id,
                "chat request started"
            );
            if let Some(path) = &request_dump_path {
                let redacted_headers = crate::http::redacted_headers_json(&headers);
                let redacted_body = request_body_redactor(&request_body);
                crate::http::dump_request(
                    &completions_url,
                    &redacted_headers,
                    &redacted_body,
                    path,
                );
            }
            let request_started_at = std::time::Instant::now();
            let (response, request_permit) = match crate::http::post_with_retry_and_limiter(
                &client,
                &completions_url,
                &headers,
                &request_body,
                &cancel,
                &request_limiter,
            )
            .await
            {
                Ok(response) => response,
                Err(error) => {
                    crate::llm::token_usage::append_token_usage_record(
                        &token_usage_dump_source,
                        &token_usage_record(None),
                    );
                    let response_dump_path =
                        dump_context.response_path_for_content_type(None, "jsonl");
                    crate::http::dump_response_transport_error(&response_dump_path, &error);
                    return Err(error);
                }
            };
            tracing::debug!(
                dump_id = %dump_id,
                elapsed_ms = request_started_at.elapsed().as_millis() as u64,
                "response headers received"
            );
            let response_content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok());
            let response_dump_path =
                dump_context.response_path_for_content_type(response_content_type, "jsonl");
            if let (Some(handler), Some(response_path)) =
                (&request_dump_failure_handler, response_dump_path.clone())
            {
                handler.register(move |failure: &str| {
                    crate::http::append_response_dump_line(
                        &format!("[failure] {failure}"),
                        &response_path,
                    );
                });
            }
            if let Some(path) = &response_dump_path {
                tracing::debug!(dump_id = %dump_id, path = %path.display(), "response dump path resolved");
            }

            let (tx, rx) = mpsc::channel(16);
            let stream_dump_id = dump_id.clone();
            let provider_connection_for_cost = provider_connection.clone();
            let model_for_cost = runtime_config.model.clone();
            tokio::spawn(async move {
                let _request_permit = request_permit;
                let mut reported_input_tokens = None;
                let mut cost_recorded = false;
                let mut first_token_received = false;
                let mut responses_decoder = use_responses
                    .then(crate::llm::openai_responses::ResponsesStreamDecoder::default);
                let terminal_marker = if use_responses {
                    "response.completed"
                } else {
                    "[DONE]"
                };
                // Own the byte stream (and the `Response`) inside this task so
                // dropping it on cancel closes the underlying TCP socket.
                let stream = response.bytes_stream().eventsource();
                futures::pin_mut!(stream);
                'stream: loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            if let Some(dump_path) = &response_dump_path {
                                crate::http::append_response_dump_line(
                                    "[transport] cancelled",
                                    dump_path,
                                );
                            }
                            // Dropping `stream` (which owns the byte stream and
                            // the `Response`) kills the underlying TCP socket.
                            let _ = tx.send(StreamEvent::Cancelled).await;
                            break;
                        }
                        event = stream.next() => {
                            let Some(event) = event else {
                                if let Some(dump_path) = &response_dump_path {
                                    crate::http::append_response_dump_line(
                                        &format!("[transport] stream ended without {terminal_marker}"),
                                        dump_path,
                                    );
                                }
                            tracing::debug!(
                                dump_id = %stream_dump_id,
                                elapsed_ms = request_started_at.elapsed().as_millis() as u64,
                                "stream ended without a terminal event"
                            );
                                let _ = tx
                                    .send(StreamEvent::Error(anyhow::anyhow!(
                                        "provider stream ended before {terminal_marker}"
                                    )))
                                    .await;
                                break;
                            };
                            match event {
                                Ok(event) => {
                                    if let Some(dump_path) = &response_dump_path {
                                        crate::http::append_response_dump_line(&event.data, dump_path);
                                    }
                                    if let Some(usage) = parse_reported_input_tokens(&event.data) {
                                        reported_input_tokens = Some(usage);
                                    }
                                    // Recorded as soon as the usage frame arrives, before the
                                    // terminal event reaches the consumer, so a redraw triggered
                                    // by completion already sees the cost.
                                    if let (false, Some(cost)) = (
                                        cost_recorded,
                                        usage_cost_parser.and_then(|parse| parse(&event.data)),
                                    ) {
                                        cost_recorded = true;
                                        tracing::debug!(
                                            provider_connection = %provider_connection_for_cost,
                                            model = %model_for_cost,
                                            amount = cost.amount,
                                            currency = %cost.currency,
                                            "request cost reported"
                                        );
                                        cost_ledger.record(cost);
                                    }
                                    let stream_events = match responses_decoder.as_mut() {
                                        Some(decoder) => decoder.decode(&event.data),
                                        None => parse_sse_payload(&event.data).into_iter().collect(),
                                    };
                                    for stream_event in stream_events {
                                        match stream_event {
                                            StreamEvent::Done => {
                                                tracing::debug!(
                                                    dump_id = %stream_dump_id,
                                                    elapsed_ms = request_started_at.elapsed().as_millis() as u64,
                                                    "stream ended with {terminal_marker}"
                                                );
                                                let _ = tx.send(StreamEvent::Done).await;
                                                break 'stream;
                                            }
                                            StreamEvent::Error(error) => {
                                                let _ = tx.send(StreamEvent::Error(error)).await;
                                                break 'stream;
                                            }
                                            stream_event => {
                                                // Only Text/Thinking/Delta remain here, so the
                                                // first such event is the first token.
                                                if !first_token_received {
                                                    first_token_received = true;
                                                    tracing::debug!(
                                                        dump_id = %stream_dump_id,
                                                        elapsed_ms = request_started_at.elapsed().as_millis() as u64,
                                                        "first token received"
                                                    );
                                                }
                                                if tx.send(stream_event).await.is_err() {
                                                    break 'stream;
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    if let Some(dump_path) = &response_dump_path {
                                        crate::http::append_response_dump_line(
                                            &format!("[transport] SSE error: {error}"),
                                            dump_path,
                                        );
                                    }
                                    let _ = tx
                                        .send(StreamEvent::Error(anyhow::anyhow!(
                                            "SSE transport error: {error}"
                                        )))
                                        .await;
                                    break;
                                }
                            }
                        }
                    }
                }
                crate::llm::token_usage::append_token_usage_record(
                    &token_usage_dump_source,
                    &token_usage_record(reported_input_tokens),
                );
            });
            Ok(rx)
        })
    }
}

/// Parse the OpenAI Chat Completions input-token usage fields used by
/// OpenAI-compatible providers. Usage-only SSE frames often have no choices,
/// so parsing is independent of the user-visible stream event parser.
fn parse_reported_input_tokens(data: &str) -> Option<(usize, &'static str)> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    for (pointer, source) in [
        (
            "/usage/prompt_tokens",
            "openai_compatible.usage.prompt_tokens",
        ),
        (
            "/usage/input_tokens",
            "openai_compatible.usage.input_tokens",
        ),
        (
            "/response/usage/input_tokens",
            "openai_compatible.response.usage.input_tokens",
        ),
    ] {
        if let Some(tokens) = value
            .pointer(pointer)
            .and_then(serde_json::Value::as_u64)
            .and_then(|tokens| usize::try_from(tokens).ok())
        {
            return Some((tokens, source));
        }
    }
    None
}

/// Parse one SSE `data:` payload into an optional [`StreamEvent`].
///
/// Returns [`None`] for frames that must be silently skipped — comment lines
/// (already dropped by the SSE parser, but blank `data:` frames can still
/// arrive) and deltas with no user-facing `content`, no thinking/reasoning
/// fragment, and no `tool_calls` (the `role`-only first chunk). These must
/// never be emitted as `StreamEvent::Text`. A `delta.tool_calls` chunk
/// becomes a [`StreamEvent::Delta`] (raw fragment; assembly is the agent's
/// task). A chunk with both `content` and `tool_calls` prefers `content`
/// (the text is user-facing). Malformed JSON yields [`StreamEvent::Error`].
///
/// Extended-thinking / reasoning-token fragments become
/// [`StreamEvent::Thinking`] — this is a model capability, not
/// provider-specific, so three shapes are recognised: Anthropic-style
/// `content_block_delta` deltas relayed with `delta.type ==
/// "thinking_delta"` (fragment in `delta.thinking`), OpenAI-compatible
/// `delta.reasoning_content` string fragments (e.g. DeepSeek, OpenRouter
/// passthrough), and bare `delta.reasoning` string fragments (e.g. Ollama's
/// and vLLM's OpenAI-compatible chat-completions endpoints).
pub(crate) fn parse_sse_payload(data: &str) -> Option<StreamEvent> {
    if data.is_empty() {
        return None;
    }
    if data == "[DONE]" {
        return Some(StreamEvent::Done);
    }
    let value: serde_json::Value = match serde_json::from_str(data) {
        Ok(value) => value,
        Err(error) => {
            return Some(StreamEvent::Error(anyhow::anyhow!(
                "malformed SSE payload: {error}"
            )));
        }
    };
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .or_else(|| error.as_str().map(str::to_owned))
            .unwrap_or_else(|| error.to_string());
        let code = error.get("code").and_then(serde_json::Value::as_i64);
        let error_type = error
            .pointer("/metadata/error_type")
            .and_then(serde_json::Value::as_str);
        let message = match error_type {
            Some(error_type) => format!("{message} ({error_type})"),
            None => message,
        };
        return Some(StreamEvent::Error(match code {
            Some(code) => anyhow::anyhow!("provider returned HTTP {code}: {message}"),
            None => anyhow::anyhow!("provider stream error: {message}"),
        }));
    }
    let delta = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("delta"));

    let is_anthropic_thinking_delta =
        delta.and_then(|d| d.get("type")).and_then(|t| t.as_str()) == Some("thinking_delta");
    if is_anthropic_thinking_delta {
        if let Some(serde_json::Value::String(thinking)) = delta.and_then(|d| d.get("thinking")) {
            if !thinking.is_empty() {
                return Some(StreamEvent::Thinking(thinking.clone()));
            }
        }
    }

    // `content` wins over `reasoning_content`/`reasoning` when a delta
    // somehow carries both, matching the tests' documented convention.
    if let Some(serde_json::Value::String(content)) = delta.and_then(|d| d.get("content")) {
        if !content.is_empty() {
            return Some(StreamEvent::Text(content.clone()));
        }
    }

    if let Some(serde_json::Value::String(reasoning)) =
        delta.and_then(|d| d.get("reasoning_content"))
    {
        if !reasoning.is_empty() {
            return Some(StreamEvent::Thinking(reasoning.clone()));
        }
    }

    // Bare `reasoning` (no `_content` suffix) — Ollama's and vLLM's
    // OpenAI-compatible endpoints use this field name for the same purpose.
    if let Some(serde_json::Value::String(reasoning)) = delta.and_then(|d| d.get("reasoning")) {
        if !reasoning.is_empty() {
            return Some(StreamEvent::Thinking(reasoning.clone()));
        }
    }

    if let Some(call) = delta
        .and_then(|d| d.get("tool_calls"))
        .and_then(|calls| calls.as_array())
        .and_then(|calls| calls.first())
    {
        let tool_call_index = call.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
        let tool_call_id = call.get("id").and_then(|id| id.as_str()).map(String::from);
        let function_name = call
            .pointer("/function/name")
            .and_then(|name| name.as_str())
            .map(String::from);
        let arguments_fragment = call
            .pointer("/function/arguments")
            .and_then(|arguments| arguments.as_str())
            .map(String::from);
        if tool_call_id.is_some() || function_name.is_some() || arguments_fragment.is_some() {
            return Some(StreamEvent::Delta(DeltaChunk {
                tool_call_index,
                tool_call_id,
                function_name,
                arguments_fragment,
            }));
        }
    }

    None
}

#[cfg(test)]
mod tests;
