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
use crate::http::{RequestConcurrencyLimiter, OUTBOUND_USER_AGENT};
use crate::llm::engine::{EngineRuntimeConfig, EngineRuntimeConfigHandle, DEFAULT_CONTEXT_LENGTH};
use crate::llm::openai_compatible_messages::{serialize_messages_with_policy, ContentPartFormat};
use crate::llm::token_usage::{TokenUsageDumpSource, TokenUsageRecord};
use crate::llm::{InputModality, Provider, ToolSchema};

type RequestBodyCustomizer = fn(&mut serde_json::Value, &EngineRuntimeConfig);
pub(crate) type RequestBodyRedactor = fn(&serde_json::Value) -> serde_json::Value;

fn no_request_body_customization(
    _request_body: &mut serde_json::Value,
    _runtime_config: &EngineRuntimeConfig,
) {
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
    redacted
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
                model,
                temperature: None,
                context_length: DEFAULT_CONTEXT_LENGTH,
                reasoning_effort: None,
                input_modalities: None,
            })),
            api_key,
            client: reqwest::Client::builder()
                .user_agent(OUTBOUND_USER_AGENT)
                .build()
                .expect("shared HTTP client construction must succeed"),
            request_body_customizer,
            request_body_redactor,
            content_part_format,
            token_usage_dump_source: TokenUsageDumpSource::default(),
            request_dump_source: crate::http::RequestDumpSource::default(),
            request_limiter,
            provider_kind: "openai_compatible",
        }
    }

    pub(crate) fn with_provider_kind(mut self, provider_kind: &'static str) -> Self {
        self.provider_kind = provider_kind;
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

    /// Compose the chat-completions endpoint from `base_url`. The concrete
    /// provider owns this path interpretation; the trait and `base_url`'s
    /// meaning never leak the OpenAI path structure.
    fn completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Compose the model-listing endpoint from `base_url`, the same way
    /// [`Self::completions_url`] composes the chat endpoint.
    fn models_url(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
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
        Box::pin(async move {
            let _permit = request_limiter.acquire().await?;
            let body =
                crate::http::get_json(&client, &models_url, api_key.as_deref(), "model listing")
                    .await?;
            Ok(parse_model_list(&body))
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
        let completions_url = self.completions_url();
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let request_body_customizer = self.request_body_customizer;
        let request_body_redactor = self.request_body_redactor;
        let content_part_format = self.content_part_format;
        let provider_kind = self.provider_kind;
        let request_limiter = self.request_limiter.clone();
        let token_usage_dump_source = self.token_usage_dump_source.clone();
        let request_dump_source = self.request_dump_source.clone();
        let tools: Vec<serde_json::Value> = if tools.is_empty() {
            Vec::new()
        } else {
            tools.iter().map(ToolSchema::to_request_value).collect()
        };
        Box::pin(async move {
            let mut request_body = serde_json::json!({
                "model": runtime_config.model,
                "messages": serialize_messages_with_policy(
                    &messages,
                    runtime_config.input_modalities.as_deref(),
                    content_part_format,
                ),
                "stream": true,
                "stream_options": { "include_usage": true },
            });
            if let Some(temperature) = runtime_config.temperature {
                request_body["temperature"] = serde_json::json!(temperature);
            }
            request_body_customizer(&mut request_body, &runtime_config);
            if !tools.is_empty() {
                request_body["tools"] = serde_json::Value::Array(tools);
                request_body["tool_choice"] = serde_json::json!("auto");
            }
            let mut headers = HeaderMap::new();
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
            tokio::spawn(async move {
                let _request_permit = request_permit;
                let mut reported_input_tokens = None;
                // Own the byte stream (and the `Response`) inside this task so
                // dropping it on cancel closes the underlying TCP socket.
                let stream = response.bytes_stream().eventsource();
                futures::pin_mut!(stream);
                loop {
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
                                        "[transport] stream ended without [DONE]",
                                        dump_path,
                                    );
                                }
                                let _ = tx
                                    .send(StreamEvent::Error(anyhow::anyhow!(
                                        "provider stream ended before [DONE]"
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
                                    match parse_sse_payload(&event.data) {
                                        Some(StreamEvent::Done) => {
                                            tracing::debug!("stream ended with [DONE]");
                                            let _ = tx.send(StreamEvent::Done).await;
                                            break;
                                        }
                                        Some(StreamEvent::Error(error)) => {
                                            let _ = tx.send(StreamEvent::Error(error)).await;
                                            break;
                                        }
                                        Some(stream_event) => {
                                            if tx.send(stream_event).await.is_err() {
                                                break;
                                            }
                                        }
                                        None => {}
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

/// Translate an OpenAI-compatible `GET /models` body into [`ModelInfo`]s.
///
/// OpenRouter carries `context_length` in each `data` entry.
/// Entries without an `id` are the only ones dropped — every other field is
/// best-effort so an unknown OpenAI-compatible backend still yields a usable
/// list.
fn parse_model_list(body: &serde_json::Value) -> Vec<crate::llm::ModelInfo> {
    let entries = match body.get("data").and_then(serde_json::Value::as_array) {
        Some(entries) => entries,
        None => return Vec::new(),
    };
    entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(serde_json::Value::as_str)?;
            Some(crate::llm::ModelInfo {
                id: id.to_string(),
                context_length: entry
                    .get("context_length")
                    .and_then(serde_json::Value::as_u64)
                    .map(|length| length as usize),
                reasoning_efforts: parse_reasoning_efforts(entry),
                reasoning_default: parse_reasoning_default(entry),
                input_modalities: parse_input_modalities(entry),
            })
        })
        .collect()
}

fn parse_input_modalities(entry: &serde_json::Value) -> Option<Vec<InputModality>> {
    let values = entry
        .pointer("/architecture/input_modalities")
        .or_else(|| entry.get("input_modalities"))?
        .as_array()?;
    Some(
        values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter_map(|modality| match modality {
                "image" => Some(InputModality::Image),
                "file" => Some(InputModality::File),
                _ => None,
            })
            .collect(),
    )
}

fn parse_reasoning_efforts(entry: &serde_json::Value) -> Option<Vec<String>> {
    let reasoning = entry.get("reasoning")?;
    Some(
        reasoning
            .get("supported_efforts")
            .and_then(serde_json::Value::as_array)
            .map(|efforts| {
                efforts
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|effort| !effort.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Read an entry's provider-advertised default effort
/// (`reasoning.default_effort`, OpenRouter's shape) — `None` when absent or
/// not a non-empty string.
///
/// The same `reasoning` object also carries `mandatory` and
/// `default_enabled`, which Ylva intentionally does not consume (see
/// `docs/implementation-notes.md`).
fn parse_reasoning_default(entry: &serde_json::Value) -> Option<String> {
    entry
        .pointer("/reasoning/default_effort")?
        .as_str()
        .filter(|effort| !effort.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../../tests/fixtures/", $name))
        };
    }

    #[test]
    fn parse_model_list_preserves_context_length() {
        let body = serde_json::json!({
            "data": [
                {
                    "id": "openai/gpt-oss-120b",
                    "name": "OpenAI: GPT OSS 120B",
                    "context_length": 131072
                },
                {"name": "missing-id"}
            ]
        });

        assert_eq!(
            parse_model_list(&body),
            vec![crate::llm::ModelInfo {
                id: "openai/gpt-oss-120b".to_string(),
                context_length: Some(131072),
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            }]
        );
    }

    #[test]
    fn parse_model_list_extracts_reasoning_effort_scenarios() {
        let body = serde_json::json!({
            "data": [
                {
                    "id": "provider/with-efforts",
                    "reasoning": {
                        "supported_efforts": ["low", "high"]
                    }
                },
                {
                    "id": "provider/without-selector",
                    "reasoning": {}
                },
                {"id": "provider/without-reasoning"}
            ]
        });

        let models = parse_model_list(&body);
        assert_eq!(
            models
                .iter()
                .map(|model| model.reasoning_efforts.clone())
                .collect::<Vec<_>>(),
            vec![
                Some(vec!["low".to_string(), "high".to_string()]),
                Some(Vec::new()),
                None,
            ]
        );
    }

    #[test]
    fn parse_model_list_extracts_the_provider_advertised_default_effort() {
        let body = serde_json::json!({
            "data": [
                {
                    "id": "provider/with-default",
                    "reasoning": {
                        "supported_efforts": ["low", "high"],
                        "default_effort": "high"
                    }
                },
                {
                    "id": "provider/without-default",
                    "reasoning": {"supported_efforts": ["low"]}
                },
                {
                    "id": "provider/non-string-default",
                    "reasoning": {"default_effort": 3}
                },
                {
                    "id": "provider/empty-default",
                    "reasoning": {"default_effort": ""}
                },
                {"id": "provider/no-reasoning"}
            ]
        });

        let models = parse_model_list(&body);
        assert_eq!(
            models
                .iter()
                .map(|model| model.reasoning_default.clone())
                .collect::<Vec<_>>(),
            vec![Some("high".to_string()), None, None, None, None,],
            "only a non-empty string default_effort becomes a reasoning_default"
        );
        assert_eq!(
            models[0].reasoning_efforts,
            Some(vec!["low".to_string(), "high".to_string()]),
            "supported_efforts parsing must be unchanged"
        );
    }

    #[test]
    fn parse_model_list_extracts_supported_input_modalities_conservatively() {
        let body = serde_json::json!({
            "data": [
                {
                    "id": "vision-and-files",
                    "architecture": {"input_modalities": ["text", "image", "file"]}
                },
                {
                    "id": "known-text-only",
                    "architecture": {"input_modalities": ["text", "audio"]}
                },
                {"id": "unknown"}
            ]
        });

        let models = parse_model_list(&body);
        assert_eq!(
            models
                .iter()
                .map(|model| model.input_modalities.clone())
                .collect::<Vec<_>>(),
            vec![
                Some(vec![InputModality::Image, InputModality::File]),
                Some(vec![]),
                None,
            ]
        );
    }

    #[test]
    fn request_body_redactor_redacts_only_chat_completion_media_payloads() {
        let encoded = "QklOQVJZX1NFQ1JFVA==";
        let ordinary_media_marker = "TEXT_MEDIA_SENTINEL";
        let body = serde_json::json!({
            "model": "vision-model",
            "messages": [
                {
                    "role": "tool",
                    "tool_call_id": "call-1",
                    "content": serde_json::json!({
                        "ok": true,
                        "result": encoded,
                        "result_metadata": {"encoding": "base64", "media_type": "image/png"}
                    }).to_string()
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "inspect"},
                        {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{encoded}")}},
                        {"type": "image_url", "image_url": format!("data:image/jpeg;base64,{encoded}")},
                        {"type": "file", "file": {"file_data": format!("data:application/pdf;base64,{encoded}"), "filename": "report.pdf"}},
                        {"type": "text", "url": format!("data:image/png;base64,{ordinary_media_marker}"), "file_data": "ordinary-field"}
                    ]
                }
            ]
        });
        let original = body.clone();
        let redacted = redact_chat_completion_request_body(&body);

        assert_eq!(body, original);
        let redacted_text = redacted.to_string();
        assert!(!redacted_text.contains(encoded));
        assert!(redacted_text.contains("<REDACTED_BASE64_RESULT>"));
        assert!(redacted_text.contains("data:image/png;base64,<REDACTED_MEDIA_DATA>"));
        assert!(redacted_text.contains("data:image/jpeg;base64,<REDACTED_MEDIA_DATA>"));
        assert!(redacted_text.contains("data:application/pdf;base64,<REDACTED_MEDIA_DATA>"));
        assert!(redacted_text.contains(ordinary_media_marker));
        assert!(redacted_text.contains("ordinary-field"));
        assert!(redacted_text.contains("report.pdf"));
    }

    #[test]
    fn provider_logs_effective_runtime_snapshot_without_sensitive_request_data() {
        let log = crate::test_support::capture_logs(async {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind local test server");
            let address = listener.local_addr().expect("local test server address");
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept provider request");
                let response_body = "data: [DONE]\n\n";
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/event-stream\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\
                     \r\n\
                     {}",
                    response_body.len(),
                    response_body
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write streaming response");
            });

            let provider = OpenAiCompatibleProvider::new(
                "test-connection".to_string(),
                format!("http://{address}"),
                "runtime-model".to_string(),
                Some("test-api-key".to_string()),
            );
            {
                let mut runtime_config = provider
                    .runtime_config
                    .write()
                    .expect("Engine runtime config lock");
                runtime_config.temperature = Some(0.25);
                runtime_config.context_length = 64_000;
                runtime_config.reasoning_effort = Some("high".to_string());
                runtime_config.input_modalities = Some(vec![InputModality::Image]);
            }

            let mut receiver = provider
                .chat(
                    Vec::new(),
                    vec![ToolSchema {
                        name: "request-body-secret".to_string(),
                        description: "request-body-secret".to_string(),
                        parameters: serde_json::json!({}),
                    }],
                    CancellationToken::new(),
                )
                .await
                .expect("mock server response");
            while let Some(event) = receiver.recv().await {
                if matches!(event, StreamEvent::Error(_)) {
                    break;
                }
            }
            server.await.expect("test server task");
        });

        assert!(log.contains("chat request started"));
        assert!(log.contains("provider_connection=test-connection"));
        assert!(log.contains("model=runtime-model"));
        assert!(log.contains("temperature=Some(0.25)"));
        assert!(log.contains("effective_context_length=64000"));
        assert!(
            log.contains("reasoning_effort=high"),
            "the request log must carry the thinking mode: {log}"
        );
        assert!(
            log.contains("input_modalities=image"),
            "the request log must carry the modalities: {log}"
        );
        assert!(log.contains("dump_id="));
        assert!(!log.contains("test-api-key"));
        assert!(!log.contains("request-body-secret"));
    }

    #[test]
    fn emits_serialized_request_estimate_before_provider_response() {
        crate::test_support::run_locked(async {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind local test server");
            let address = listener.local_addr().expect("local test server address");
            let (request_sender, request_receiver) = tokio::sync::oneshot::channel();
            let (response_sender, response_receiver) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 2048];
                let request_body = loop {
                    let bytes_read = stream.read(&mut buffer).await.expect("read request");
                    request.extend_from_slice(&buffer[..bytes_read]);
                    let Some(header_end) = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|position| position + 4)
                    else {
                        continue;
                    };
                    let header_text = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = header_text
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().expect("content length"))
                        })
                        .expect("content length header");
                    if request.len() >= header_end + content_length {
                        break serde_json::from_slice::<serde_json::Value>(
                            &request[header_end..header_end + content_length],
                        )
                        .expect("JSON request body");
                    }
                };
                let _ = request_sender.send(request_body);
                let _ = response_receiver.await;
                let response_body = "data: [DONE]\n\n";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write streaming response");
            });

            let provider = Arc::new(OpenAiCompatibleProvider::new(
                "estimate-test".to_string(),
                format!("http://{address}"),
                "test-model".to_string(),
                None,
            ));
            let (estimate_sender, estimate_receiver) = tokio::sync::oneshot::channel();
            let provider_request = tokio::spawn(async move {
                provider
                    .chat_with_session_id_and_estimate(
                        vec![Message::User {
                            content: "estimate before response".to_string(),
                            kind: crate::agent::UserMessageKind::Explicit,
                        }],
                        Vec::new(),
                        CancellationToken::new(),
                        None,
                        Box::new(move |estimated_tokens, context_length| {
                            Box::pin(async move {
                                let _ = estimate_sender.send((estimated_tokens, context_length));
                            })
                        }),
                    )
                    .await
                    .expect("provider stream")
            });
            let request_body = request_receiver.await.expect("server received request");
            let (estimated_tokens, context_length) =
                tokio::time::timeout(std::time::Duration::from_secs(1), estimate_receiver)
                    .await
                    .expect("estimate arrives before a response")
                    .expect("estimate callback");
            assert_eq!(context_length, DEFAULT_CONTEXT_LENGTH);
            assert_eq!(
                estimated_tokens,
                crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&request_body.to_string())
            );

            let _ = response_sender.send(());
            let mut events = provider_request.await.expect("provider task");
            assert!(matches!(events.recv().await, Some(StreamEvent::Done)));
            server.await.expect("test server task");
        });
    }

    #[test]
    fn caller_timeout_is_appended_to_the_matching_response_dump() {
        crate::test_support::run_locked(async {
            let temp_dir = tempfile::tempdir().expect("dump directory");
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind local test server");
            let address = listener.local_addr().expect("local test server address");
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).await.expect("read request");
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100\r\n\r\n",
                    )
                    .await
                    .expect("write response headers");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            });

            let provider = OpenAiCompatibleProvider::new(
                "timeout-dump-test".to_string(),
                format!("http://{address}"),
                "test-model".to_string(),
                None,
            )
            .with_request_dump_source(crate::http::RequestDumpSource::from_template(
                temp_dir.path().join("summary.jsonl"),
            ));
            let failure_handler = crate::http::RequestDumpFailureHandler::default();
            let mut response = provider
                .chat_with_session_id_and_diagnostics(
                    vec![Message::User {
                        content: "summarize".to_string(),
                        kind: crate::agent::UserMessageKind::ImplicitToolNote,
                    }],
                    Vec::new(),
                    CancellationToken::new(),
                    None,
                    Box::new(|_, _| Box::pin(async {})),
                    Some(failure_handler.clone()),
                )
                .await
                .expect("provider request starts");

            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(20), response.recv())
                    .await
                    .is_err()
            );
            failure_handler.report("compaction summary timed out after 60 seconds");

            let request_path = std::fs::read_dir(temp_dir.path())
                .expect("dump directory readable")
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| path.to_string_lossy().contains("_request."))
                .expect("request dump exists");
            let request_stem = request_path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("request filename")
                .replace("_request.jsonl", "");
            let response_path = temp_dir
                .path()
                .join(format!("{request_stem}_response.jsonl"));
            let response_dump = std::fs::read_to_string(response_path).expect("response dump");
            assert!(response_dump.contains("compaction summary timed out after 60 seconds"));

            server.abort();
        });
    }

    #[test]
    fn parse_done_sentinel() {
        assert!(matches!(
            parse_sse_payload("[DONE]"),
            Some(StreamEvent::Done)
        ));
    }

    #[test]
    fn parse_provider_error_frame_as_a_stream_error() {
        let event = parse_sse_payload(
            r#"{"error":{"code":502,"message":"Upstream unavailable","metadata":{"error_type":"provider_unavailable"}}}"#,
        );
        assert!(matches!(event, Some(StreamEvent::Error(error))
            if error.to_string() == "provider returned HTTP 502: Upstream unavailable (provider_unavailable)"));
    }

    #[test]
    fn parses_reported_prompt_and_input_usage_from_usage_only_chunks() {
        assert_eq!(
            parse_reported_input_tokens(r#"{"choices":[],"usage":{"prompt_tokens":17}}"#),
            Some((17, "openai_compatible.usage.prompt_tokens"))
        );
        assert_eq!(
            parse_reported_input_tokens(r#"{"usage":{"input_tokens":23}}"#),
            Some((23, "openai_compatible.usage.input_tokens"))
        );
        assert_eq!(parse_reported_input_tokens(r#"{"choices":[]}"#), None);
        assert_eq!(parse_reported_input_tokens("not json"), None);
    }

    #[test]
    fn each_request_gets_its_own_estimate_and_optional_reported_usage_record() {
        crate::test_support::run_locked(async {
            let temporary_directory = tempfile::tempdir().expect("temporary directory");
            let dump_path = temporary_directory.path().join("token-usage.jsonl");
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind local test server");
            let address = listener.local_addr().expect("local test server address");
            let server = tokio::spawn(async move {
                let mut request_bodies = Vec::new();
                for request_index in 0..2 {
                    let (mut stream, _) = listener.accept().await.expect("accept request");
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 2048];
                    loop {
                        let bytes_read = stream.read(&mut buffer).await.expect("read request");
                        if bytes_read == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..bytes_read]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            let header_end = request
                                .windows(4)
                                .position(|window| window == b"\r\n\r\n")
                                .expect("request headers")
                                + 4;
                            let header_text = String::from_utf8_lossy(&request[..header_end]);
                            let content_length = header_text
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length").then(|| {
                                        value.trim().parse::<usize>().expect("content length")
                                    })
                                })
                                .unwrap_or_default();
                            if request.len() >= header_end + content_length {
                                request_bodies.push(
                                    serde_json::from_slice::<serde_json::Value>(
                                        &request[header_end..header_end + content_length],
                                    )
                                    .expect("JSON request body"),
                                );
                                break;
                            }
                        }
                    }
                    let frames = if request_index == 0 {
                        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":31}}\n\ndata: [DONE]\n\n"
                    } else {
                        "data: {\"choices\":[],\"usage\":null}\n\ndata: [DONE]\n\n"
                    };
                    let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    frames.len(),
                    frames
                );
                    stream
                        .write_all(response.as_bytes())
                        .await
                        .expect("write streaming response");
                }
                request_bodies
            });

            let provider = OpenAiCompatibleProvider::new(
                "test-connection".to_string(),
                format!("http://{address}"),
                "test-model".to_string(),
                None,
            )
            .with_token_usage_dump_path(&dump_path);
            let session_id = uuid::Uuid::new_v4();
            for request_content in ["first request sentinel", "second request sentinel"] {
                let mut events = provider
                    .chat_with_session_id(
                        vec![Message::User {
                            content: request_content.to_string(),
                            kind: crate::agent::UserMessageKind::Explicit,
                        }],
                        Vec::new(),
                        CancellationToken::new(),
                        Some(session_id),
                    )
                    .await
                    .expect("provider stream");
                while let Some(event) = events.recv().await {
                    if matches!(event, StreamEvent::Error(_)) {
                        break;
                    }
                }
            }
            let request_bodies = server.await.expect("server task");
            assert_eq!(request_bodies.len(), 2);
            assert!(request_bodies
                .iter()
                .all(|body| { body["stream_options"]["include_usage"] == true }));

            let records = std::fs::read_to_string(dump_path).expect("token usage dump");
            let records = records
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON record"))
                .collect::<Vec<_>>();
            assert_eq!(records.len(), 2, "one record per model request");
            assert_ne!(records[0]["request_id"], records[1]["request_id"]);
            for record in &records {
                assert_eq!(record["session_id"], session_id.to_string());
                let timestamp = record["timestamp"].as_str().expect("timestamp");
                chrono::DateTime::parse_from_rfc3339(timestamp).expect("RFC 3339 timestamp");
            }
            assert_eq!(records[0]["reported_input_tokens"], 31);
            assert_eq!(
                records[0]["reported_source"],
                "openai_compatible.usage.prompt_tokens"
            );
            assert!(records[1]["reported_input_tokens"].is_null());
            assert!(records[1]["reported_source"].is_null());
            assert!(records.iter().all(|record| {
                record["estimated_input_tokens"]
                    .as_u64()
                    .unwrap_or_default()
                    > 0
                    && record["estimate_source"] == "cl100k_base_serialized_chat_request"
            }));
            for (record, body) in records.iter().zip(&request_bodies) {
                let expected_estimate =
                    crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&body.to_string());
                assert_eq!(record["estimated_input_tokens"], expected_estimate);
            }
            let dump_text = serde_json::to_string(&records).expect("records serialize");
            assert!(!dump_text.contains("first request sentinel"));
            assert!(!dump_text.contains("second request sentinel"));
        });
    }

    #[test]
    fn interrupted_stream_is_recorded_without_inventing_usage() {
        crate::test_support::run_locked(async {
            let temporary_directory = tempfile::tempdir().expect("temporary directory");
            let dump_path = temporary_directory.path().join("token-usage.jsonl");
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind local test server");
            let address = listener.local_addr().expect("local test server address");
            let (headers_sent, headers_received) = tokio::sync::oneshot::channel();
            let (close_stream, wait_to_close) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).await.expect("read request");
                let first_frame = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
                let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:x}\r\n{}\r\n",
                first_frame.len(),
                first_frame
            );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response headers and first chunk");
                let _ = headers_sent.send(());
                let _ = wait_to_close.await;
            });

            let provider = OpenAiCompatibleProvider::new(
                "test-connection".to_string(),
                format!("http://{address}"),
                "test-model".to_string(),
                None,
            )
            .with_token_usage_dump_path(&dump_path);
            let cancellation = CancellationToken::new();
            let mut events = provider
                .chat(
                    vec![Message::User {
                        content: "interrupted request sentinel".to_string(),
                        kind: crate::agent::UserMessageKind::Explicit,
                    }],
                    Vec::new(),
                    cancellation.clone(),
                )
                .await
                .expect("provider stream");
            headers_received.await.expect("server sent response");
            assert!(
                matches!(events.recv().await, Some(StreamEvent::Text(text)) if text == "partial")
            );
            cancellation.cancel();
            assert!(matches!(events.recv().await, Some(StreamEvent::Cancelled)));
            let _ = close_stream.send(());
            server.await.expect("server task");

            let contents = std::fs::read_to_string(dump_path).expect("token usage dump");
            let record: serde_json::Value =
                serde_json::from_str(contents.trim()).expect("JSON record");
            assert!(
                record["estimated_input_tokens"]
                    .as_u64()
                    .unwrap_or_default()
                    > 0
            );
            assert!(record["reported_input_tokens"].is_null());
            assert!(record["reported_source"].is_null());
            assert!(!contents.contains("interrupted request sentinel"));
        });
    }

    #[test]
    fn parse_empty_data_is_skipped() {
        assert!(parse_sse_payload("").is_none());
    }

    #[test]
    fn parse_content_delta_emits_text() {
        let event = parse_sse_payload(r#"{"choices":[{"delta":{"content":"hello"}}]}"#);
        assert!(matches!(event, Some(StreamEvent::Text(text)) if text == "hello"));
    }

    #[test]
    fn parse_delta_without_content_is_skipped() {
        // `role`-only first chunk.
        assert!(parse_sse_payload(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#).is_none());
    }

    #[test]
    fn parse_anthropic_style_thinking_delta_emits_thinking() {
        let event = parse_sse_payload(
            r#"{"choices":[{"delta":{"type":"thinking_delta","thinking":"pondering..."}}]}"#,
        );
        assert!(matches!(event, Some(StreamEvent::Thinking(text)) if text == "pondering..."));
    }

    #[test]
    fn parse_empty_anthropic_thinking_delta_is_skipped() {
        assert!(parse_sse_payload(
            r#"{"choices":[{"delta":{"type":"thinking_delta","thinking":""}}]}"#
        )
        .is_none());
    }

    #[test]
    fn parse_reasoning_content_delta_emits_thinking() {
        let event =
            parse_sse_payload(r#"{"choices":[{"delta":{"reasoning_content":"because..."}}]}"#);
        assert!(matches!(event, Some(StreamEvent::Thinking(text)) if text == "because..."));
    }

    #[test]
    fn parse_empty_reasoning_content_delta_is_skipped() {
        assert!(parse_sse_payload(r#"{"choices":[{"delta":{"reasoning_content":""}}]}"#).is_none());
    }

    #[test]
    fn parse_bare_reasoning_delta_emits_thinking() {
        // Ollama's and vLLM's OpenAI-compatible endpoints use bare `reasoning`
        // (no `_content` suffix) for the same purpose as `reasoning_content`.
        let event = parse_sse_payload(r#"{"choices":[{"delta":{"reasoning":"thinking..."}}]}"#);
        assert!(matches!(event, Some(StreamEvent::Thinking(text)) if text == "thinking..."));
    }

    #[test]
    fn parse_empty_bare_reasoning_delta_is_skipped() {
        assert!(parse_sse_payload(r#"{"choices":[{"delta":{"reasoning":""}}]}"#).is_none());
    }

    #[test]
    fn parse_content_prefers_over_reasoning_content() {
        // If a delta somehow carries both, user-facing content wins.
        let event = parse_sse_payload(
            r#"{"choices":[{"delta":{"content":"hi","reasoning_content":"because..."}}]}"#,
        );
        assert!(matches!(event, Some(StreamEvent::Text(text)) if text == "hi"));
    }

    #[test]
    fn parse_tool_call_chunk_emits_delta() {
        let event = parse_sse_payload(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]}}]}"#,
        );
        match event {
            Some(StreamEvent::Delta(delta)) => {
                assert_eq!(delta.tool_call_index, 0);
                assert_eq!(delta.tool_call_id.as_deref(), Some("call_1"));
                assert_eq!(delta.function_name.as_deref(), Some("read"));
                assert_eq!(delta.arguments_fragment.as_deref(), Some(""));
            }
            other => panic!("expected Delta, got {other:?}"),
        }
    }

    #[test]
    fn parse_arguments_fragment_delta_has_no_name_or_id() {
        let payload = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{ "index": 0, "function": { "arguments": "{\"path\"" } }]
                }
            }]
        })
        .to_string();
        let event = parse_sse_payload(&payload);
        match event {
            Some(StreamEvent::Delta(delta)) => {
                assert_eq!(delta.tool_call_index, 0);
                assert_eq!(delta.tool_call_id, None);
                assert_eq!(delta.function_name, None);
                assert_eq!(delta.arguments_fragment.as_deref(), Some("{\"path\""));
            }
            other => panic!("expected Delta, got {other:?}"),
        }
    }

    #[test]
    fn parse_empty_content_is_skipped() {
        assert!(parse_sse_payload(r#"{"choices":[{"delta":{"content":""}}]}"#).is_none());
    }

    #[test]
    fn parse_malformed_json_is_error() {
        assert!(matches!(
            parse_sse_payload("{not json"),
            Some(StreamEvent::Error(_))
        ));
    }

    #[test]
    fn plain_stream_fixture_produces_text_in_order() {
        let data = fixture!("plain_stream.sse");
        let texts: Vec<String> = data
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(parse_sse_payload)
            .filter_map(|event| match event {
                StreamEvent::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Hello", " ", "there", " "]);
    }

    #[test]
    fn comment_stream_fixture_tolerates_comment_lines() {
        let data = fixture!("comment_stream.sse");
        let mut texts = Vec::new();
        let mut done = false;
        for line in data.lines() {
            if let Some(payload) = line.strip_prefix("data: ") {
                match parse_sse_payload(payload) {
                    Some(StreamEvent::Text(text)) => texts.push(text),
                    Some(StreamEvent::Done) => done = true,
                    Some(StreamEvent::Error(_)) => panic!("comment fixture must not error"),
                    Some(StreamEvent::Cancelled) => {}
                    Some(StreamEvent::Delta(_)) => {}
                    Some(StreamEvent::Thinking(_)) => {}
                    None => {}
                }
            }
        }
        assert!(done, "comment fixture must still reach [DONE]");
        assert_eq!(texts, vec!["One", "Two"]);
    }

    #[test]
    fn reasoning_fixture_skips_reasoning_only_delta() {
        let data = fixture!("reasoning_stream.sse");
        let mut texts: Vec<String> = Vec::new();
        let mut done = false;
        for line in data.lines() {
            if let Some(payload) = line.strip_prefix("data: ") {
                match parse_sse_payload(payload) {
                    Some(StreamEvent::Text(text)) => texts.push(text),
                    Some(StreamEvent::Done) => done = true,
                    Some(StreamEvent::Error(_)) => panic!("reasoning fixture must not error"),
                    Some(StreamEvent::Cancelled) => {}
                    Some(StreamEvent::Delta(_)) => {}
                    Some(StreamEvent::Thinking(_)) => {}
                    None => {}
                }
            }
        }
        assert!(done);
        // The reasoning-only delta carries no content and is skipped; only the
        // final `content` delta surfaces as user-visible text.
        assert_eq!(texts, vec!["final"]);
    }
}
