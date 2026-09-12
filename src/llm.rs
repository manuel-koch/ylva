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

use schemars::JsonSchema;
use serde::Serialize;

pub mod engine;
pub mod http;
pub mod mock_provider;
pub mod ollama_provider;
pub mod openai_compatible_provider;
pub mod openrouter_provider;
pub mod provider_registry;

pub use engine::{
    Engine, EngineConfig, EngineDisplayInfo, EngineImpl, EngineRouter, EngineRuntimeConfig,
    EngineRuntimeConfigHandle, ProviderModelOption, DEFAULT_CONTEXT_LENGTH,
};
pub use mock_provider::{MockEvent, MockProvider};
pub use ollama_provider::OllamaProvider;
pub use openai_compatible_provider::OpenAiCompatibleProvider;
pub use openrouter_provider::OpenRouterProvider;
pub use provider_registry::{ProviderEnv, ProviderRegistry, ProviderSelection};

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub context_length: Option<usize>,
    /// Reasoning support advertised by the provider.
    ///
    /// - `None` means that reasoning support is not advertised.
    /// - `Some([])` means that reasoning is supported but the
    ///   provider exposes no selectable effort alternatives.
    /// - `Some(values)` means that reasoning is supported and
    ///   `values` contains the selectable effort values.
    pub reasoning_efforts: Option<Vec<String>>,
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

    /// Return the short name identifying this provider connection or the
    /// built-in implementation (e.g. `"ollama-cloud"`, `"openrouter"`,
    /// `"mock"`).
    fn name(&self) -> &str;

    /// Return the list of models available from this provider.
    ///
    /// Providers that cannot enumerate models return an error. An
    /// [`EngineRouter`](crate::llm::engine::EngineRouter) omits that connection
    /// when another configured connection succeeds and aggregates the
    /// failures when none do. A provider may use more than one backend
    /// endpoint to build the catalog; [`OllamaProvider`](crate::llm::OllamaProvider),
    /// for example, combines `/api/tags` with `/api/show` metadata.
    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>>;

    /// Return the shared runtime settings when this provider supports them.
    ///
    /// [`EngineImpl`](crate::llm::engine::EngineImpl) uses the handle to expose a
    /// coherent [`EngineConfig`](crate::llm::engine::EngineConfig) snapshot without
    /// maintaining a second mutable copy of provider settings. Providers that
    /// do not expose mutable runtime settings may keep the default.
    fn runtime_config_handle(&self) -> Option<EngineRuntimeConfigHandle> {
        None
    }

    /// Switch the model used by every subsequent `chat()` call.
    ///
    /// Providers share one `Arc<dyn Provider>` across all agents, so the
    /// switch takes effect through interior mutability rather than by
    /// rebuilding the provider. Providers that cannot switch (the mock)
    /// keep the default, which fails loudly.
    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "provider '{}' does not support switching to model '{model}'",
            self.name()
        )
    }

    /// Select a reasoning effort for subsequent requests.
    ///
    /// `None` clears an explicit effort and lets the provider use its own
    /// default behavior. Providers that do not support selectable reasoning
    /// efforts reject a non-empty value.
    fn set_reasoning_effort(&self, reasoning_effort: Option<&str>) -> anyhow::Result<()> {
        if let Some(reasoning_effort) = reasoning_effort {
            anyhow::bail!(
                "provider '{}' does not support reasoning effort '{reasoning_effort}'",
                self.name()
            );
        }
        Ok(())
    }
}

/// Tool results arrive as `role`:"tool" messages whose `content` is a JSON
/// envelope of this shape. `ok` is authoritative: when it is `false`, report
/// the error and do not claim the operation succeeded. When `result_metadata`
/// is present it carries structured facts about the result (e.g. byte counts,
/// match counts); `result_metadata_schema` describes each field that actually
/// appears — types and descriptions — so the shape is always self-documenting.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[schemars(title = "ToolResponseEnvelope")]
pub struct ToolEnvelope {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_metadata_schema: Option<serde_json::Value>,
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
            Message::ToolResult {
                value:
                    crate::agent::ToolResult {
                        id,
                        result,
                        result_metadata,
                        result_metadata_schema,
                        ..
                    },
            } => {
                let content = ToolEnvelope {
                    ok: true,
                    result: Some(result.clone()),
                    error: None,
                    result_metadata: result_metadata.clone(),
                    result_metadata_schema: result_metadata_schema.clone(),
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
                    result_metadata: None,
                    result_metadata_schema: None,
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
        use crate::agent::{AssistantStatus, ToolCallRequest, ToolResult};
        let id = uuid::Uuid::new_v4();
        let messages = vec![
            Message::Assistant {
                content: String::new(),
                status: AssistantStatus::Complete,
                tool_calls: vec![ToolCallRequest {
                    id,
                    name: "read".into(),
                    arguments: serde_json::json!({ "path": "a.txt" }),
                    persisted_title_arguments: vec![],
                }],
            },
            Message::ToolResult {
                value: ToolResult {
                    id,
                    name: "read".into(),
                    result: "file contents".into(),
                    result_section_id: None,
                    result_metadata: None,
                    result_metadata_schema: None,
                    summary: vec![],
                    content_sections: vec![],
                    duration: std::time::Duration::ZERO,
                },
            },
            Message::ToolError {
                id,
                name: "read".into(),
                error: "boom".into(),
                duration: std::time::Duration::ZERO,
                content_sections: vec![],
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
