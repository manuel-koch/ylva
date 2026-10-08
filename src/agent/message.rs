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

use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantStatus {
    Complete,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallRequest {
    pub id: Uuid,
    pub name: String,
    pub arguments: Value,
    /// Tool-specific title arguments captured when the call is created.
    ///
    /// An empty list means that the call has no title arguments.
    #[serde(rename = "title_arguments", default)]
    pub persisted_title_arguments: Vec<(String, Value)>,
}

impl ToolCallRequest {
    /// Return the captured tool-specific title arguments.
    pub fn title_arguments(&self) -> Vec<(String, Value)> {
        self.persisted_title_arguments.clone()
    }
}

/// Provider-neutral media returned by a successful tool call.
///
/// The descriptor deliberately does not own the encoded bytes. It refers to
/// the containing successful [`ToolResult::result`] value, which remains the
/// canonical Base64 payload for persistence, replay, and text-only fallback.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolAttachment {
    Image {
        media_type: String,
    },
    File {
        media_type: String,
        /// A basename safe to send to a provider as the source filename.
        filename: String,
    },
}

/// Persisted data for one completed tool call.
///
/// This is the payload of [`Message::ToolResult`] so callers can pass the
/// complete result through the context without expanding it into another
/// positional argument list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolResult {
    pub id: Uuid,
    pub name: String,
    pub result: String,
    /// Media descriptors that reference this result's Base64 `result` value.
    /// The descriptors never store a second copy of that payload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ToolAttachment>,
    /// Optional UI result-section identifier. `None` means the standard
    /// plain-result section; specialized tools such as `clarify` use a
    /// different identifier so replay can preserve the live rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_section_id: Option<String>,
    /// Structured metadata emitted by the tool for this specific result.
    #[serde(default)]
    pub result_metadata: Option<serde_json::Value>,
    /// JSON Schema for the fields present in `result_metadata`.
    #[serde(default)]
    pub result_metadata_schema: Option<serde_json::Value>,
    /// Key/value metadata displayed in the completed tool title.
    #[serde(default)]
    pub summary: Vec<(String, String)>,
    /// Structured content rendered in the completed tool bubble.
    #[serde(default)]
    pub content_sections: Vec<crate::agent::tools::Section>,
    /// Elapsed execution time displayed in the completed tool title.
    #[serde(default)]
    pub duration: std::time::Duration,
}

/// Distinguishes user messages that were typed explicitly by the user from
/// those injected by the agent harness as model hints.
///
/// `Explicit` messages are rendered in the TUI conversation list and the
/// headless CLI output. `ImplicitToolNote` messages are sent to the model
/// as regular `role: "user"` turns but are **never rendered** in any UI —
/// they exist solely to give the model richer context (e.g. a denial reason).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserMessageKind {
    #[default]
    Explicit,
    ImplicitToolNote,
}

/// Persisted as one JSONL line by `crate::store` — hence the `serde`
/// derives. The discriminant is serialized as a `role` field
/// (`#[serde(tag = "role")]`) and **no** `deny_unknown_fields` is used, so a
/// session file written by an older or newer build still deserializes.
///
/// The tool-result variant intentionally stays inline so the persisted
/// `#[serde(flatten)]` shape and the single-writer context API remain direct.
#[expect(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
        #[serde(default)]
        kind: UserMessageKind,
    },
    Assistant {
        content: String,
        status: AssistantStatus,
        #[serde(default)]
        tool_calls: Vec<ToolCallRequest>,
    },
    ToolResult {
        #[serde(flatten)]
        value: ToolResult,
    },
    ToolError {
        id: Uuid,
        name: String,
        error: String,
        #[serde(default)]
        duration: std::time::Duration,
        /// Structured content produced before the tool failed.
        #[serde(default)]
        content_sections: Vec<crate::agent::tools::Section>,
    },
}

#[cfg(test)]
mod tests;
