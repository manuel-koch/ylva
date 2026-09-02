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

use eventsource_stream::Eventsource;
use futures::future::BoxFuture;
use futures::StreamExt;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{DeltaChunk, Message, StreamEvent};
use crate::llm::{serialize_messages, Provider, ToolSchema};

/// A streaming provider targeting the OpenAI chat-completions spec, serving
/// both OpenRouter and local Ollama via constructor configuration.
///
/// `base_url` is a **generic provider concept** interpreted solely by this
/// concrete implementation: it includes the backend origin and version prefix
/// (e.g. Ollama `http://localhost:11434/v1`, OpenRouter
/// `https://openrouter.ai/api/v1`) and the provider appends `/chat/completions`
/// internally. The `Provider` trait carries no `base_url` and no path
/// semantics — this leaves room for future provider-side queries (model
/// listing) under the same base URL.
///
/// Pre-stream failures (`429`/`5xx`/connect) are retried with bounded
/// exponential backoff honoring `Retry-After`; once streaming begins no retry
/// occurs and mid-stream errors surface as `StreamEvent::Error`.
#[derive(Debug)]
pub struct OpenAiProvider {
    name: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Create a provider for an OpenAI-compatible backend.
    ///
    /// `name` is the short identifier for this provider instance (e.g. `"ollama"`, `"openrouter"`).
    /// `base_url` must include the backend origin and version prefix.
    /// `api_key` is `None` for backends that need no auth (local Ollama).
    pub fn new(name: String, base_url: String, model: String, api_key: Option<String>) -> Self {
        OpenAiProvider {
            name,
            base_url,
            model,
            api_key,
            client: reqwest::Client::new(),
        }
    }

    /// Compose the chat-completions endpoint from `base_url`. The concrete
    /// provider owns this path interpretation; the trait and `base_url`'s
    /// meaning never leak the OpenAI path structure.
    fn completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>> {
        Box::pin(async { anyhow::bail!("list_models not yet implemented") })
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let tools: Vec<serde_json::Value> = if tools.is_empty() {
            Vec::new()
        } else {
            tools.iter().map(ToolSchema::to_request_value).collect()
        };
        Box::pin(async move {
            let mut request_body = serde_json::json!({
                "model": model,
                "messages": serialize_messages(&messages),
                "stream": true,
            });
            if !tools.is_empty() {
                request_body["tools"] = serde_json::Value::Array(tools);
                request_body["tool_choice"] = serde_json::json!("auto");
            }
            let completions_url = self.completions_url();
            let mut headers = HeaderMap::new();
            if let Some(key) = &api_key {
                headers.insert(AUTHORIZATION, format!("Bearer {key}").parse()?);
            }
            // Only the URL and model are logged — never the request headers
            // (which carry the `Authorization`/API-key value) or the body.
            tracing::debug!(url = %completions_url, model = %model, "chat request started");
            let response = super::http::post_with_retry(
                &client,
                &completions_url,
                &headers,
                &request_body,
                &cancel,
            )
            .await?;

            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                // Own the byte stream (and the `Response`) inside this task so
                // dropping it on cancel closes the underlying TCP socket.
                let stream = response.bytes_stream().eventsource();
                futures::pin_mut!(stream);
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            // Dropping `stream` (which owns the byte stream and
                            // the `Response`) kills the underlying TCP socket.
                            let _ = tx.send(StreamEvent::Cancelled).await;
                            break;
                        }
                        event = stream.next() => {
                            let Some(event) = event else {
                                // Stream ended without a [DONE] sentinel.
                                let _ = tx.send(StreamEvent::Done).await;
                                break;
                            };
                            match event {
                                Ok(event) => {
                                    match parse_sse_payload(&event.data) {
                                        Some(StreamEvent::Done) => {
                                            tracing::debug!("stream ended with [DONE]");
                                            let _ = tx.send(StreamEvent::Done).await;
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
            });
            Ok(rx)
        })
    }
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
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../../tests/fixtures/", $name))
        };
    }

    #[test]
    fn parse_done_sentinel() {
        assert!(matches!(
            parse_sse_payload("[DONE]"),
            Some(StreamEvent::Done)
        ));
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
