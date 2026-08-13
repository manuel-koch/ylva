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

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eventsource_stream::Eventsource;
use futures::future::BoxFuture;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{DeltaChunk, Message, StreamEvent};
use crate::llm::{serialize_messages, Provider, ToolSchema};

/// Maximum number of pre-stream retries (after the initial attempt).
const MAX_RETRIES: usize = 3;
/// Base backoff for the first retry, in seconds.
const RETRY_BASE_SECONDS: u64 = 1;
/// Upper bound on a single backoff delay, in seconds.
const RETRY_MAX_SECONDS: u64 = 30;

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
    base_url: String,
    model: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Create a provider for an OpenAI-compatible backend.
    ///
    /// `base_url` must include the backend origin and version prefix.
    /// `api_key` is `None` for backends that need no auth (local Ollama).
    pub fn new(base_url: String, model: String, api_key: Option<String>) -> Self {
        OpenAiProvider {
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
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let completions_url = self.completions_url();
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
            let response =
                send_with_retry(&client, &completions_url, &api_key, &request_body, &cancel)
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
/// arrive) and deltas with no user-facing `content` and no `tool_calls`
/// (reasoning deltas, the `role`-only first chunk). These must never be
/// emitted as `StreamEvent::Text`. A `delta.tool_calls` chunk becomes a
/// [`StreamEvent::Delta`] (raw fragment; assembly is the agent's task). A
/// chunk with both `content` and `tool_calls` prefers `content` (the text is
/// user-facing). Malformed JSON yields [`StreamEvent::Error`].
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

    if let Some(serde_json::Value::String(content)) = delta.and_then(|d| d.get("content")) {
        if !content.is_empty() {
            return Some(StreamEvent::Text(content.clone()));
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

/// Perform the initial `POST` with bounded exponential backoff retry for
/// pre-stream failures (`429`/`5xx`/connect), honoring `Retry-After` when
/// present. Returns the first successful response or a terminal [`anyhow::Error`].
async fn send_with_retry(
    client: &reqwest::Client,
    completions_url: &str,
    api_key: &Option<String>,
    body: &serde_json::Value,
    cancel: &CancellationToken,
) -> anyhow::Result<reqwest::Response> {
    let mut attempts = 0usize;
    loop {
        let mut request = client.post(completions_url).json(body);
        if let Some(key) = api_key {
            request = request.bearer_auth(key);
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                if attempts >= MAX_RETRIES {
                    return Err(anyhow::anyhow!(
                        "request to {completions_url} failed after {MAX_RETRIES} retries: {error}"
                    ));
                }
                attempts += 1;
                if sleep_cancel_aware(cancel, retry_delay(attempts, None))
                    .await
                    .is_none()
                {
                    return Err(anyhow::anyhow!("cancelled while waiting to retry"));
                }
                continue;
            }
        };

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        if !should_retry(status) {
            let detail = response_body_detail(response).await;
            return Err(anyhow::anyhow!(
                "provider returned HTTP {}{} for {completions_url}",
                status.as_u16(),
                detail,
            ));
        }

        if attempts >= MAX_RETRIES {
            let detail = response_body_detail(response).await;
            return Err(anyhow::anyhow!(
                "provider returned HTTP {}{} for {completions_url} after {MAX_RETRIES} retries",
                status.as_u16(),
                detail,
            ));
        }

        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        attempts += 1;
        if sleep_cancel_aware(cancel, retry_delay(attempts, retry_after))
            .await
            .is_none()
        {
            return Err(anyhow::anyhow!("cancelled while waiting to retry"));
        }
    }
}

/// Whether an HTTP status is worth a pre-stream retry: rate limit or server
/// error (the provider never retries once streaming has begun).
fn should_retry(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// Read a bounded snippet of the response body to attach to a pre-stream error
/// message, so backend-specific explanations (e.g. Ollama's `model not found`)
/// surface instead of a bare status code. Returns an empty string when the body
/// cannot be read or is blank.
async fn response_body_detail(response: reqwest::Response) -> String {
    match response.bytes().await {
        Ok(body) => {
            let text = String::from_utf8_lossy(&body);
            let text = text.trim();
            if text.is_empty() {
                return String::new();
            }
            let snippet: String = text.chars().take(512).collect();
            format!(": {snippet}")
        }
        Err(_) => String::new(),
    }
}

/// Compute the next pre-stream backoff delay. `Retry-After` (in seconds) wins
/// when present; otherwise exponential backoff (`base * 2^(attempt-1)`) capped
/// at [`RETRY_MAX_SECONDS`], with a small jitter.
fn retry_delay(attempt: usize, retry_after: Option<u64>) -> Duration {
    if let Some(seconds) = retry_after {
        return Duration::from_secs(seconds.min(RETRY_MAX_SECONDS));
    }
    let exponent = attempt.saturating_sub(1).min(63);
    let base = RETRY_BASE_SECONDS.saturating_mul(1u64 << exponent);
    let capped = base.min(RETRY_MAX_SECONDS);
    let jitter = jitter_fraction();
    Duration::from_millis((capped as f64 * jitter * 1000.0) as u64)
}

/// A tiny deterministic-per-call jitter in `[0, 1)`, seeded from the wall clock
/// so successive attempts back off differently without a dedicated RNG crate.
fn jitter_fraction() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut state = (nanos & 0xFFFF_FFFF) as u64 | 1;
    state ^= state >> 12;
    state ^= state << 25;
    state ^= state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D) as f64 / u64::MAX as f64
}

/// Sleep for `delay` unless `cancel` fires first; returns `Some` only when the
/// full delay elapsed, so a stop aborts a retry wait promptly.
async fn sleep_cancel_aware(cancel: &CancellationToken, delay: Duration) -> Option<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        _ = tokio::time::sleep(delay) => Some(()),
    }
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
        // Reasoning delta with neither content nor tool_calls.
        assert!(parse_sse_payload(r#"{"choices":[{"delta":{"reasoning":"thinking"}}]}"#).is_none());
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
                    None => {}
                }
            }
        }
        assert!(done);
        // The reasoning-only delta carries no content and is skipped; only the
        // final `content` delta surfaces as user-visible text.
        assert_eq!(texts, vec!["final"]);
    }

    #[test]
    fn retry_delay_honors_retry_after() {
        assert_eq!(retry_delay(1, Some(2)), Duration::from_secs(2));
        assert_eq!(
            retry_delay(5, Some(9999)),
            Duration::from_secs(RETRY_MAX_SECONDS)
        );
    }

    #[test]
    fn retry_delay_backs_off_bounded_by_max() {
        assert!(retry_delay(1, None) < Duration::from_secs(2));
        let first = retry_delay(2, None);
        assert!(first >= Duration::from_millis(1));
        let late = retry_delay(40, None);
        assert!(late <= Duration::from_secs(RETRY_MAX_SECONDS));
    }

    #[test]
    fn should_retry_marks_ratelimit_and_server_errors() {
        use reqwest::StatusCode;
        assert!(should_retry(StatusCode::TOO_MANY_REQUESTS));
        assert!(should_retry(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(should_retry(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!should_retry(StatusCode::OK));
        assert!(!should_retry(StatusCode::BAD_REQUEST));
        assert!(!should_retry(StatusCode::UNAUTHORIZED));
    }
}
