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

//! OpenAI Responses API wire format for the shared OpenAI-compatible
//! transport: request projection from the Chat Completions projection and the
//! stateful stream decoder.

use std::collections::{HashMap, HashSet};

use crate::agent::{DeltaChunk, StreamEvent};

/// Project the Chat Completions message array onto Responses `input` items.
///
/// Starting from the Chat Completions projection keeps one source of truth for
/// attachment and tool-outcome handling.
pub(crate) fn responses_input(chat_messages: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    for message in chat_messages.as_array().into_iter().flatten() {
        let role = message.get("role").and_then(serde_json::Value::as_str);
        match role {
            Some("tool") => items.push(serde_json::json!({
                "type": "function_call_output",
                "call_id": message.get("tool_call_id").cloned().unwrap_or_default(),
                "output": message.get("content").cloned().unwrap_or_default(),
            })),
            Some("assistant") => {
                let has_text = message
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|content| !content.is_empty());
                if has_text {
                    items.push(serde_json::json!({
                        "role": "assistant",
                        "content": message["content"],
                    }));
                }
                for call in message
                    .get("tool_calls")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    items.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": call.get("id").cloned().unwrap_or_default(),
                        "name": call.pointer("/function/name").cloned().unwrap_or_default(),
                        "arguments": call
                            .pointer("/function/arguments")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!("{}")),
                    }));
                }
            }
            Some(role) => items.push(serde_json::json!({
                "role": role,
                "content": responses_content(message.get("content")),
            })),
            None => {}
        }
    }
    items
}

fn responses_content(content: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(parts) = content.and_then(serde_json::Value::as_array) else {
        return content.cloned().unwrap_or_default();
    };
    serde_json::Value::Array(parts.iter().map(responses_content_part).collect())
}

fn responses_content_part(part: &serde_json::Value) -> serde_json::Value {
    match part.get("type").and_then(serde_json::Value::as_str) {
        Some("text") => serde_json::json!({"type": "input_text", "text": part["text"]}),
        Some("image_url") => {
            let url = part
                .pointer("/image_url/url")
                .or_else(|| part.get("image_url"))
                .cloned()
                .unwrap_or_default();
            serde_json::json!({"type": "input_image", "image_url": url})
        }
        Some("file") => serde_json::json!({
            "type": "input_file",
            "filename": part.pointer("/file/filename").cloned().unwrap_or_default(),
            "file_data": part.pointer("/file/file_data").cloned().unwrap_or_default(),
        }),
        _ => part.clone(),
    }
}

/// Flatten Chat Completions tool envelopes into Responses function tools.
pub(crate) fn responses_tools(tools: &[serde_json::Value]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            let function = tool.get("function").unwrap_or(tool);
            serde_json::json!({
                "type": "function",
                "name": function.get("name").cloned().unwrap_or_default(),
                "description": function.get("description").cloned().unwrap_or_default(),
                "parameters": function.get("parameters").cloned().unwrap_or_default(),
            })
        })
        .collect()
}

/// Build the streaming Responses request body.
///
/// Conversation state is never stored server-side (`store: false`), so every
/// request carries the full history.
pub(crate) fn responses_request_body(
    model: &str,
    chat_messages: &serde_json::Value,
    tools: &[serde_json::Value],
    temperature: Option<f32>,
    reasoning_effort: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "input": responses_input(chat_messages),
        "stream": true,
        "store": false,
    });
    if let Some(temperature) = temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(effort) = reasoning_effort {
        body["reasoning"] = serde_json::json!({"effort": effort, "summary": "auto"});
    }
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(responses_tools(tools));
        body["tool_choice"] = serde_json::json!("auto");
    }
    body
}

/// Translate Responses stream events into [`StreamEvent`]s.
///
/// Function calls are keyed by the event's `output_index`; the decoder numbers
/// them in order of appearance, as the agent's tool-call assembly expects.
#[derive(Debug, Default)]
pub(crate) struct ResponsesStreamDecoder {
    tool_call_indexes: HashMap<u64, usize>,
    indexes_with_arguments: HashSet<usize>,
}

impl ResponsesStreamDecoder {
    /// Decode one SSE `data:` payload. The stream ends with
    /// [`StreamEvent::Done`] or [`StreamEvent::Error`].
    pub(crate) fn decode(&mut self, data: &str) -> Vec<StreamEvent> {
        if data.is_empty() {
            return Vec::new();
        }
        let value: serde_json::Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(error) => {
                return vec![StreamEvent::Error(anyhow::anyhow!(
                    "malformed SSE payload: {error}"
                ))];
            }
        };
        let text = |pointer: &str| {
            value
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.is_empty())
                .map(String::from)
        };
        let output_index = value
            .get("output_index")
            .and_then(serde_json::Value::as_u64);
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("response.output_text.delta") => {
                text("/delta").map(StreamEvent::Text).into_iter().collect()
            }
            Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
                text("/delta")
                    .map(StreamEvent::Thinking)
                    .into_iter()
                    .collect()
            }
            Some("response.output_item.added") => {
                if value
                    .pointer("/item/type")
                    .and_then(serde_json::Value::as_str)
                    != Some("function_call")
                {
                    return Vec::new();
                }
                let Some(output_index) = output_index else {
                    return Vec::new();
                };
                let tool_call_index = self.tool_call_indexes.len();
                self.tool_call_indexes.insert(output_index, tool_call_index);
                vec![StreamEvent::Delta(DeltaChunk {
                    tool_call_index,
                    tool_call_id: text("/item/call_id"),
                    function_name: text("/item/name"),
                    arguments_fragment: None,
                })]
            }
            Some("response.function_call_arguments.delta") => {
                let Some(tool_call_index) =
                    output_index.and_then(|index| self.tool_call_indexes.get(&index).copied())
                else {
                    return Vec::new();
                };
                self.indexes_with_arguments.insert(tool_call_index);
                vec![StreamEvent::Delta(DeltaChunk {
                    tool_call_index,
                    tool_call_id: None,
                    function_name: None,
                    arguments_fragment: text("/delta"),
                })]
            }
            Some("response.output_item.done") => {
                // Backends that skip the argument deltas still deliver the
                // whole argument string on the finished item.
                let Some(tool_call_index) =
                    output_index.and_then(|index| self.tool_call_indexes.get(&index).copied())
                else {
                    return Vec::new();
                };
                if self.indexes_with_arguments.contains(&tool_call_index) {
                    return Vec::new();
                }
                text("/item/arguments")
                    .map(|arguments| {
                        StreamEvent::Delta(DeltaChunk {
                            tool_call_index,
                            tool_call_id: None,
                            function_name: None,
                            arguments_fragment: Some(arguments),
                        })
                    })
                    .into_iter()
                    .collect()
            }
            Some("response.completed" | "response.incomplete") => vec![StreamEvent::Done],
            Some("response.failed") => vec![StreamEvent::Error(anyhow::anyhow!(
                "provider response failed: {}",
                text("/response/error/message").unwrap_or_else(|| "unknown error".to_string())
            ))],
            Some("error") => vec![StreamEvent::Error(anyhow::anyhow!(
                "provider stream error: {}",
                text("/message").unwrap_or_else(|| value.to_string())
            ))],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
