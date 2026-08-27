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

use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{DeltaChunk, Message, StreamEvent};

use schemars::JsonSchema;
use serde::Serialize;

pub mod http;
pub mod openai_provider;

pub use openai_provider::OpenAiProvider;

/// A single tool definition in the OpenAI chat-completions wire shape.
///
/// This is the `{"type":"function","function":{name, description, parameters}}`
/// envelope. `parameters` is a serde-serialized [`schemars::schema::RootSchema`]
/// produced by deriving `JsonSchema` on a typed arguments struct — never a
/// hand-written JSON schema (blueprint §4D).
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolSchema {
    /// Render the OpenAI function-tool envelope for the request body.
    pub fn to_request_value(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// Metadata describing a single model available from a provider.
#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub name: Option<String>,
    pub context_length: Option<usize>,
}

/// Runtime abstraction over a chat-completions-style streaming provider.
///
/// Native async trait (Rust 1.75+): no `async-trait` crate. The Provider
/// creates its own internal `mpsc::channel` and returns the receiver, so
/// retry logic and stream parsing stay hidden inside the implementation.
/// Returns a boxed future so the trait stays `dyn`-compatible (an `impl
/// Future` return position would make `Arc<dyn Provider>` unbuildable).
/// The implementation polls the `CancellationToken`; when it fires, the
/// in-flight stream is aborted and the receiver eventually yields
/// `StreamEvent::Cancelled`.
///
/// `tools` carries the schemas the model may call (blueprint §4A Step 3
/// refinement): the request body sends them so the model can emit
/// `delta.tool_calls`, surfaced as [`StreamEvent::Delta`] fragments that the
/// agent's streaming task assembles. Tool assembly never happens here.
pub trait Provider: Send + Sync {
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>>;

    /// Return the short name identifying this provider (e.g. `"ollama"`, `"openrouter"`, `"mock"`).
    fn name(&self) -> &str;

    /// Return the list of models available from this provider.
    ///
    /// The real HTTP implementation is deferred to the phase that introduces
    /// the `/models` slash command. Providers that have not yet implemented
    /// this method return an error.
    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>>;
}

/// One item of a deterministic [`MockProvider`] script.
///
/// The script is the single deterministic path both text-streaming and
/// Tool-use-loop tests share: a sequence of text tokens and raw
/// [`DeltaChunk`] tool-call fragments, emitted at a fixed interval. `chat()`
/// always terminates the script with `StreamEvent::Done`.
#[derive(Debug, Clone)]
pub enum MockEvent {
    Text(String),
    Delta(DeltaChunk),
}

/// Deterministic fake provider used by tests and demos.
///
/// `chat()` streams the script items one by one, sleeping `interval` between
/// them, then emits `StreamEvent::Done`. If the `CancellationToken` fires
/// mid-stream the current sleep is interrupted via `select!` and
/// `StreamEvent::Cancelled` is emitted promptly. The token sleeps make the mock
/// fully deterministic under `tokio::time::pause`.
#[derive(Debug)]
pub struct MockProvider {
    script: Vec<MockEvent>,
    interval: Duration,
}

impl MockProvider {
    /// A text-only script from `tokens` (["Phase 2"`MockProvider`][]).
    ///
    /// [Phase 2`MockProvider`]: crate::llm::MockProvider
    pub fn new(tokens: Vec<String>, interval: Duration) -> Self {
        let script = tokens.into_iter().map(MockEvent::Text).collect();
        MockProvider { script, interval }
    }

    /// A fully scripted provider (text + tool-call deltas) for deterministic
    /// tool-use loop tests. `chat()` emits `script` at `interval`, then `Done`.
    pub fn with_script(script: Vec<MockEvent>, interval: Duration) -> Self {
        MockProvider { script, interval }
    }
}

impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }

    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let script = self.script.clone();
        let interval = self.interval;
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let last_index = script.len().saturating_sub(1);
                for (index, event) in script.into_iter().enumerate() {
                    let stream_event = match event {
                        MockEvent::Text(text) => StreamEvent::Text(text),
                        MockEvent::Delta(delta) => StreamEvent::Delta(delta),
                    };
                    if tx.send(stream_event).await.is_err() {
                        return;
                    }
                    if index == last_index {
                        break;
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            let _ = tx.send(StreamEvent::Cancelled).await;
                            return;
                        }
                        _ = tokio::time::sleep(interval) => {}
                    }
                }
                let _ = tx.send(StreamEvent::Done).await;
            });
            Ok(rx)
        })
    }
}

/// Tool results arrive as `role`:"tool" messages whose `content` is a JSON
/// envelope of this shape. `ok` is authoritative: when it is `false`, report
/// the error and do not claim the operation succeeded.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[schemars(title = "ToolResponseEnvelope")]
pub struct ToolEnvelope {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ToolEnvelope {
    /// Serialize this envelope to a JSON string for a `role:"tool"` message's
    /// `content`. The struct only holds strings/bools, so serialization never
    /// fails.
    fn to_string_checked(&self) -> String {
        serde_json::to_string(self).expect("ToolEnvelope always serializes")
    }
}

/// Maps the domain `Message` history onto the OpenAI chat-completions wire
/// format. The domain model stays decoupled from the wire format (blueprint
/// §4A): this is the only place the two communicate.
///
/// Phase 3 maps `System`/`User`/`Assistant` (including `Assistant.tool_calls`),
/// and turns `ToolResult`/`ToolError` into `role:"tool"` messages carrying the
/// matching `tool_call_id`. Both outcomes use the single [`ToolEnvelope`] so
/// the outcome is structurally explicit, not inferred from an arbitrary reply
/// (see `TOOL_PROTOCOL_SYSTEM_PROMPT`).
pub fn serialize_messages(messages: &[Message]) -> serde_json::Value {
    let serialized: Vec<serde_json::Value> = messages
        .iter()
        .map(|message| match message {
            Message::System { content } => serde_json::json!({
                "role": "system",
                "content": content,
            }),
            Message::User { content, .. } => serde_json::json!({
                "role": "user",
                "content": content,
            }),
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let tool_calls_value: Vec<serde_json::Value> = tool_calls
                    .iter()
                    .map(|call| {
                        let arguments = serde_json::to_string(&call.arguments)
                            .unwrap_or_else(|_| "{}".to_string());
                        serde_json::json!({
                            "id": call.id.to_string(),
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": arguments,
                            }
                        })
                    })
                    .collect();
                let mut value = serde_json::json!({
                    "role": "assistant",
                    "content": content,
                });
                if !tool_calls_value.is_empty() {
                    value["tool_calls"] = serde_json::Value::Array(tool_calls_value);
                }
                value
            }
            Message::ToolResult { id, result, .. } => {
                let content = ToolEnvelope {
                    ok: true,
                    result: Some(result.clone()),
                    error: None,
                }
                .to_string_checked();
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id.to_string(),
                    "content": content,
                })
            }
            Message::ToolError { id, error, .. } => {
                let content = ToolEnvelope {
                    ok: false,
                    result: None,
                    error: Some(error.clone()),
                }
                .to_string_checked();
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id.to_string(),
                    "content": content,
                })
            }
        })
        .collect();
    serde_json::Value::Array(serialized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::UserMessageKind;

    async fn assert_received_text(rx: &mut mpsc::Receiver<StreamEvent>, expected: &str) {
        assert!(
            matches!(rx.recv().await, Some(StreamEvent::Text(text)) if text == expected),
            "expected Text({expected:?})"
        );
    }

    #[test]
    fn mock_provider_name_returns_mock() {
        let provider = MockProvider::new(vec![], Duration::from_millis(10));
        assert_eq!(provider.name(), "mock");
    }

    #[tokio::test]
    async fn mock_provider_list_models_returns_empty_vec() {
        let provider = MockProvider::new(vec![], Duration::from_millis(10));
        let models = provider.list_models().await.unwrap();
        assert!(models.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn mock_emits_tokens_then_done_at_interval() {
        let provider = MockProvider::new(
            vec!["Hello".into(), " ".into(), "World".into()],
            Duration::from_millis(10),
        );
        let mut rx = provider
            .chat(Vec::new(), Vec::new(), CancellationToken::new())
            .await
            .unwrap();

        assert_received_text(&mut rx, "Hello").await;
        tokio::time::advance(Duration::from_millis(10)).await;
        assert_received_text(&mut rx, " ").await;
        tokio::time::advance(Duration::from_millis(10)).await;
        assert_received_text(&mut rx, "World").await;
        assert!(matches!(rx.recv().await, Some(StreamEvent::Done)));
    }

    #[tokio::test(start_paused = true)]
    async fn mock_emits_cancelled_when_token_fired_mid_stream() {
        let provider = MockProvider::new(
            vec!["first".into(), "second".into(), "third".into()],
            Duration::from_millis(10),
        );
        let cancel = CancellationToken::new();
        let mut rx = provider
            .chat(Vec::new(), Vec::new(), cancel.clone())
            .await
            .unwrap();

        assert_received_text(&mut rx, "first").await;
        cancel.cancel();
        assert!(
            matches!(rx.recv().await, Some(StreamEvent::Cancelled)),
            "expected Cancelled after token fired mid-stream"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn mock_emits_first_token_without_delay() {
        let provider = MockProvider::new(vec!["instant".into()], Duration::from_millis(1000));
        let mut rx = provider
            .chat(Vec::new(), Vec::new(), CancellationToken::new())
            .await
            .unwrap();
        assert_received_text(&mut rx, "instant").await;
        assert!(matches!(rx.recv().await, Some(StreamEvent::Done)));
    }

    #[tokio::test(start_paused = true)]
    async fn mock_script_emits_delta_fragments_then_done() {
        let provider = MockProvider::with_script(
            vec![
                MockEvent::Delta(DeltaChunk {
                    tool_call_index: 0,
                    tool_call_id: Some("call_1".into()),
                    function_name: Some("read".into()),
                    arguments_fragment: Some("{\"path\":".into()),
                }),
                MockEvent::Delta(DeltaChunk {
                    tool_call_index: 0,
                    tool_call_id: None,
                    function_name: None,
                    arguments_fragment: Some("\"x.txt\"}".into()),
                }),
            ],
            Duration::from_millis(10),
        );
        let mut rx = provider
            .chat(Vec::new(), Vec::new(), CancellationToken::new())
            .await
            .unwrap();
        let first = rx.recv().await;
        assert!(
            matches!(first, Some(StreamEvent::Delta(d)) if d.arguments_fragment.as_deref() == Some("{\"path\":"))
        );
        let second = rx.recv().await;
        assert!(
            matches!(second, Some(StreamEvent::Delta(d)) if d.arguments_fragment.as_deref() == Some("\"x.txt\"}"))
        );
        assert!(matches!(rx.recv().await, Some(StreamEvent::Done)));
    }

    #[test]
    fn tool_schema_renders_function_envelope() {
        let schema = ToolSchema {
            name: "read".into(),
            description: "read a file".into(),
            parameters: serde_json::json!({ "type": "object" }),
        };
        assert_eq!(
            schema.to_request_value(),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "read a file",
                    "parameters": { "type": "object" },
                }
            })
        );
    }

    #[test]
    fn serialize_messages_maps_system_user_assistant() {
        use crate::agent::AssistantStatus;
        let messages = vec![
            Message::System {
                content: "sys".into(),
            },
            Message::User {
                content: "usr".into(),
                kind: UserMessageKind::Explicit,
            },
            Message::Assistant {
                content: "ast".into(),
                status: AssistantStatus::Complete,
                tool_calls: Vec::new(),
            },
        ];
        let value = serialize_messages(&messages);
        assert_eq!(
            value,
            serde_json::json!([
                { "role": "system", "content": "sys" },
                { "role": "user", "content": "usr" },
                { "role": "assistant", "content": "ast" },
            ])
        );
    }

    #[test]
    fn serialize_messages_maps_assistant_tool_calls_and_tool_messages() {
        use crate::agent::{AssistantStatus, ToolCallRequest};
        let id = uuid::Uuid::new_v4();
        let messages = vec![
            Message::Assistant {
                content: String::new(),
                status: AssistantStatus::Complete,
                tool_calls: vec![ToolCallRequest {
                    id,
                    name: "read".into(),
                    arguments: serde_json::json!({ "path": "a.txt" }),
                }],
            },
            Message::ToolResult {
                id,
                name: "read".into(),
                result: "file contents".into(),
            },
            Message::ToolError {
                id,
                name: "read".into(),
                error: "boom".into(),
                duration: std::time::Duration::ZERO,
            },
        ];
        let value = serialize_messages(&messages);
        assert_eq!(
            value,
            serde_json::json!([
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": id.to_string(),
                        "type": "function",
                        "function": {
                            "name": "read",
                            "arguments": "{\"path\":\"a.txt\"}",
                        }
                    }]
                },
                { "role": "tool", "tool_call_id": id.to_string(), "content": "{\"ok\":true,\"result\":\"file contents\"}" },
                {
                    "role": "tool",
                    "tool_call_id": id.to_string(),
                    "content": "{\"ok\":false,\"error\":\"boom\"}",
                },
            ])
        );
    }

    #[test]
    fn serialize_messages_empty_slice_yields_empty_array() {
        assert_eq!(serialize_messages(&[]), serde_json::json!([]));
    }
}
