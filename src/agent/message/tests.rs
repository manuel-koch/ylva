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

use super::*;
#[test]
fn constructs_message_variants() {
    let system = Message::System {
        content: "system".into(),
    };
    let user = Message::User {
        content: "user".into(),
        kind: UserMessageKind::Explicit,
    };
    let assistant = Message::Assistant {
        content: "assistant".into(),
        status: AssistantStatus::Complete,
        tool_calls: Vec::new(),
    };
    let tool_result = Message::ToolResult {
        value: ToolResult {
            id: Uuid::new_v4(),
            name: "tool".into(),
            result: "ok".into(),
            attachments: vec![],
            result_section_id: None,
            result_metadata: None,
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: std::time::Duration::ZERO,
        },
    };
    let tool_error = Message::ToolError {
        id: Uuid::new_v4(),
        name: "tool".into(),
        error: "err".into(),
        duration: std::time::Duration::ZERO,
        content_sections: vec![],
    };
    assert!(matches!(system, Message::System { .. }));
    assert!(matches!(user, Message::User { .. }));
    assert!(matches!(assistant, Message::Assistant { .. }));
    assert!(matches!(tool_result, Message::ToolResult { .. }));
    assert!(matches!(tool_error, Message::ToolError { .. }));
}

#[test]
fn tool_call_request_uses_uuid_id() {
    let id = Uuid::new_v4();
    let request = ToolCallRequest {
        id,
        name: "search".into(),
        arguments: serde_json::json!({ "query": "rust" }),
        persisted_title_arguments: vec![],
    };
    assert_eq!(request.id, id);
    assert_eq!(request.name, "search");
    assert_eq!(request.arguments, serde_json::json!({ "query": "rust" }));
}

#[test]
fn tool_call_request_preserves_tool_specific_title_arguments() {
    let id = Uuid::new_v4();
    let title_arguments = vec![(
        "path".to_string(),
        serde_json::Value::String("src/example.rs".to_string()),
    )];
    let request = ToolCallRequest {
        id,
        name: "write".to_string(),
        arguments: serde_json::json!({
            "path": "src/example.rs",
            "content": "fn main() {}",
        }),
        persisted_title_arguments: title_arguments.clone(),
    };

    assert_eq!(request.title_arguments(), title_arguments);
    let serialized = serde_json::to_value(&request).expect("request serializes");
    assert_eq!(
        serialized["title_arguments"],
        serde_json::json!(title_arguments)
    );
}

#[test]
fn tool_messages_round_trip_replay_metadata_and_legacy_defaults() {
    let id = Uuid::new_v4();
    let content_section =
        crate::agent::tools::Section::Content(crate::agent::tools::ContentSection {
            label: Some("Output".to_string()),
            content: "contents".to_string(),
            file_extension: Some("txt".to_string()),
            line_number_offset: Some(4),
        });
    let result = Message::ToolResult {
        value: ToolResult {
            id,
            name: "read".to_string(),
            result: "contents".to_string(),
            attachments: vec![ToolAttachment::File {
                media_type: "application/pdf".to_string(),
                filename: "report.pdf".to_string(),
            }],
            result_section_id: None,
            result_metadata: Some(serde_json::json!({"bytes": 8})),
            result_metadata_schema: Some(serde_json::json!({"type": "object"})),
            summary: vec![("path".to_string(), "README.md".to_string())],
            content_sections: vec![content_section.clone()],
            duration: std::time::Duration::from_millis(37),
        },
    };
    let error = Message::ToolError {
        id,
        name: "write".to_string(),
        error: "denied".to_string(),
        duration: std::time::Duration::from_millis(11),
        content_sections: vec![content_section],
    };

    let serialized_result =
        serde_json::to_value(&result).expect("tool result serializes to a value");
    assert_eq!(serialized_result["role"], "tool_result");
    assert_eq!(serialized_result["id"], serde_json::json!(id));
    assert_eq!(serialized_result["result"], "contents");
    assert_eq!(serialized_result["attachments"][0]["type"], "file");
    assert_eq!(
        serialized_result["attachments"][0]["filename"],
        "report.pdf"
    );
    assert!(
        serialized_result.get("value").is_none(),
        "the canonical payload must remain flattened in the persisted schema"
    );

    for message in [result.clone(), error.clone()] {
        let serialized = serde_json::to_string(&message).expect("message serializes");
        let restored = serde_json::from_str::<Message>(&serialized).expect("message deserializes");
        assert_eq!(restored, message);
    }

    let legacy_result = serde_json::json!({
        "role": "tool_result",
        "id": id,
        "name": "read",
        "result": "old contents",
        "result_metadata": null,
        "result_metadata_schema": null,
    });
    assert_eq!(
        serde_json::from_value::<Message>(legacy_result).expect("legacy result deserializes"),
        Message::ToolResult {
            value: ToolResult {
                id,
                name: "read".to_string(),
                result: "old contents".to_string(),
                attachments: vec![],
                result_section_id: None,
                result_metadata: None,
                result_metadata_schema: None,
                summary: vec![],
                content_sections: vec![],
                duration: std::time::Duration::ZERO,
            },
        }
    );

    let legacy_error = serde_json::json!({
        "role": "tool_error",
        "id": id,
        "name": "write",
        "error": "old error",
        "duration": { "secs": 1, "nanos": 2 },
    });
    assert_eq!(
        serde_json::from_value::<Message>(legacy_error).expect("legacy error deserializes"),
        Message::ToolError {
            id,
            name: "write".to_string(),
            error: "old error".to_string(),
            duration: std::time::Duration::new(1, 2),
            content_sections: vec![],
        }
    );
}
