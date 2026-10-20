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

//! OpenAI-compatible Chat Completions message projection.

use crate::agent::Message;

use crate::llm::{InputModality, ToolEnvelope};

/// Provider-specific content-part formatting for the shared Chat Completions
/// transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContentPartFormat {
    /// Standard object-form OpenAI-compatible content parts.
    OpenAiCompatible,
    /// Ollama's explicitly selected compatibility form for image parts.
    OllamaCompatibility,
}

/// Maps the domain message history onto the OpenAI-compatible Chat Completions
/// wire format. The synthetic user message created for supported attachments
/// is wire-only and is never added to the domain history.
#[cfg(test)]
pub(crate) fn serialize_messages(messages: &[Message]) -> serde_json::Value {
    serialize_messages_with_policy(messages, None, ContentPartFormat::OpenAiCompatible)
}

/// Serialize messages using the active model's advertised capabilities and
/// the provider's content-part policy.
pub(crate) fn serialize_messages_with_policy(
    messages: &[Message],
    input_modalities: Option<&[InputModality]>,
    content_part_format: ContentPartFormat,
) -> serde_json::Value {
    let mut serialized = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        if !is_tool_outcome(&messages[index]) {
            serialized.push(serialize_non_tool_message(&messages[index]));
            index += 1;
            continue;
        }

        let group_start = index;
        while index < messages.len() && is_tool_outcome(&messages[index]) {
            index += 1;
        }

        let mut attachments = Vec::new();
        for message in &messages[group_start..index] {
            match message {
                Message::ToolResult { value } => {
                    let projected_attachments = supported_attachments(value, input_modalities);
                    let marker = (!projected_attachments.is_empty())
                        .then(|| attachment_marker(&projected_attachments));
                    serialized.push(serialize_tool_result(value, marker.as_deref()));
                    attachments.extend(projected_attachments.into_iter().map(
                        |(attachment_index, attachment)| WireAttachment {
                            tool_call_id: value.id,
                            tool_name: &value.name,
                            attachment_index,
                            attachment,
                            result: value.result.as_str(),
                        },
                    ));
                }
                Message::ToolError { .. } => {
                    serialized.push(serialize_tool_error(message));
                }
                _ => unreachable!("tool outcome group contains only tool outcomes"),
            }
        }

        if !attachments.is_empty() {
            serialized.push(serialize_attachment_follow_up(
                &attachments,
                content_part_format,
            ));
        }
    }
    serde_json::Value::Array(serialized)
}

fn is_tool_outcome(message: &Message) -> bool {
    matches!(
        message,
        Message::ToolResult { .. } | Message::ToolError { .. }
    )
}

fn serialize_non_tool_message(message: &Message) -> serde_json::Value {
    match message {
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
                    let arguments =
                        serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string());
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
        Message::ToolResult { .. } | Message::ToolError { .. } => {
            unreachable!("tool outcomes are serialized as a contiguous group")
        }
    }
}

fn serialize_tool_result(
    value: &crate::agent::ToolResult,
    result_marker: Option<&str>,
) -> serde_json::Value {
    let content = ToolEnvelope {
        ok: true,
        result: Some(result_marker.unwrap_or(&value.result).to_string()),
        error: None,
        result_metadata: value.result_metadata.clone(),
        result_metadata_schema: value.result_metadata_schema.clone(),
    }
    .to_string_checked();
    serde_json::json!({
        "role": "tool",
        "tool_call_id": value.id.to_string(),
        "content": content,
    })
}

fn serialize_tool_error(message: &Message) -> serde_json::Value {
    let Message::ToolError { id, error, .. } = message else {
        unreachable!("only tool errors reach this helper")
    };
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

fn supported_attachments<'a>(
    value: &'a crate::agent::ToolResult,
    input_modalities: Option<&[InputModality]>,
) -> Vec<(usize, &'a crate::agent::ToolAttachment)> {
    let Some(input_modalities) = input_modalities else {
        return vec![];
    };
    if value.attachments.is_empty() {
        return vec![];
    }
    let all_supported = value.attachments.iter().all(|attachment| {
        input_modalities.iter().any(|modality| {
            matches!(
                (attachment, modality),
                (
                    crate::agent::ToolAttachment::Image { .. },
                    InputModality::Image
                ) | (
                    crate::agent::ToolAttachment::File { .. },
                    InputModality::File
                )
            )
        })
    });
    if all_supported {
        value.attachments.iter().enumerate().collect()
    } else {
        vec![]
    }
}

struct WireAttachment<'a> {
    tool_call_id: uuid::Uuid,
    tool_name: &'a str,
    attachment_index: usize,
    attachment: &'a crate::agent::ToolAttachment,
    result: &'a str,
}

fn attachment_marker(attachments: &[(usize, &crate::agent::ToolAttachment)]) -> String {
    let has_image = attachments
        .iter()
        .any(|(_, attachment)| matches!(attachment, crate::agent::ToolAttachment::Image { .. }));
    let has_file = attachments
        .iter()
        .any(|(_, attachment)| matches!(attachment, crate::agent::ToolAttachment::File { .. }));
    match (has_image, has_file) {
        (true, true) => "[image and file attached]".to_string(),
        (true, false) => "[image attached]".to_string(),
        (false, true) => "[file attached]".to_string(),
        (false, false) => "[media attached]".to_string(),
    }
}

fn serialize_attachment_follow_up(
    attachments: &[WireAttachment<'_>],
    content_part_format: ContentPartFormat,
) -> serde_json::Value {
    let attachment_mapping = attachments
        .iter()
        .enumerate()
        .map(|(wire_index, attachment)| {
            format!(
                "{}. tool_call_id={} tool={} attachment={} {}",
                wire_index + 1,
                attachment.tool_call_id,
                attachment.tool_name,
                attachment.attachment_index + 1,
                attachment_description(attachment.attachment),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!("The following media parts correspond to the preceding tool results in this order. Keep each media part associated with its listed tool_call_id:\n{attachment_mapping}");
    let mut content = vec![serde_json::json!({"type": "text", "text": text})];
    content.extend(attachments.iter().map(|attachment| {
        attachment_content_part(
            attachment.attachment,
            attachment.result,
            content_part_format,
        )
    }));
    serde_json::json!({
        "role": "user",
        "content": content,
    })
}

fn attachment_description(attachment: &crate::agent::ToolAttachment) -> String {
    match attachment {
        crate::agent::ToolAttachment::Image { media_type } => {
            format!("image media_type={media_type}")
        }
        crate::agent::ToolAttachment::File {
            media_type,
            filename,
        } => format!("file media_type={media_type} filename={filename}"),
    }
}

fn attachment_content_part(
    attachment: &crate::agent::ToolAttachment,
    result: &str,
    content_part_format: ContentPartFormat,
) -> serde_json::Value {
    match attachment {
        crate::agent::ToolAttachment::Image { media_type } => {
            let data_uri = format!("data:{media_type};base64,{result}");
            match content_part_format {
                ContentPartFormat::OpenAiCompatible => serde_json::json!({
                    "type": "image_url",
                    "image_url": {"url": data_uri},
                }),
                ContentPartFormat::OllamaCompatibility => serde_json::json!({
                    "type": "image_url",
                    "image_url": data_uri,
                }),
            }
        }
        crate::agent::ToolAttachment::File {
            media_type,
            filename,
        } => serde_json::json!({
            "type": "file",
            "file": {
                "file_data": format!("data:{media_type};base64,{result}"),
                "filename": filename,
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::UserMessageKind;

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
                    attachments: vec![],
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

    fn attachment_result(
        id: uuid::Uuid,
        result: &str,
        attachment: crate::agent::ToolAttachment,
    ) -> Message {
        Message::ToolResult {
            value: crate::agent::ToolResult {
                id,
                name: "read".to_string(),
                result: result.to_string(),
                attachments: vec![attachment],
                result_section_id: None,
                result_metadata: Some(serde_json::json!({
                    "read_bytes": 4,
                    "encoding": "base64",
                })),
                result_metadata_schema: None,
                summary: vec![],
                content_sections: vec![],
                duration: std::time::Duration::ZERO,
            },
        }
    }

    #[test]
    fn capable_image_projection_sends_marker_and_one_standard_image_part() {
        let id = uuid::Uuid::new_v4();
        let encoded = "PNG_SENTINEL";
        let messages = vec![attachment_result(
            id,
            encoded,
            crate::agent::ToolAttachment::Image {
                media_type: "image/png".to_string(),
            },
        )];

        let value = serialize_messages_with_policy(
            &messages,
            Some(&[InputModality::Image]),
            ContentPartFormat::OpenAiCompatible,
        );
        assert_eq!(value.as_array().expect("messages").len(), 2);
        let tool_content = value[0]["content"].as_str().expect("tool content string");
        assert!(tool_content.contains("[image attached]"));
        assert!(!tool_content.contains(encoded));
        assert_eq!(value[1]["role"], "user");
        assert_eq!(value[1]["content"][0]["type"], "text");
        assert_eq!(value[1]["content"][1]["type"], "image_url");
        assert_eq!(
            value[1]["content"][1]["image_url"]["url"],
            format!("data:image/png;base64,{encoded}")
        );
        assert_eq!(value.to_string().matches(encoded).count(), 1);
    }

    #[test]
    fn capable_file_projection_uses_file_part_and_never_image_url() {
        let id = uuid::Uuid::new_v4();
        let encoded = "PDF_SENTINEL";
        let value = serialize_messages_with_policy(
            &[attachment_result(
                id,
                encoded,
                crate::agent::ToolAttachment::File {
                    media_type: "application/pdf".to_string(),
                    filename: "report.pdf".to_string(),
                },
            )],
            Some(&[InputModality::File]),
            ContentPartFormat::OpenAiCompatible,
        );

        assert!(value[0]["content"]
            .as_str()
            .expect("tool content")
            .contains("[file attached]"));
        assert_eq!(value[1]["content"][1]["type"], "file");
        assert_eq!(value[1]["content"][1]["file"]["filename"], "report.pdf");
        assert_eq!(
            value[1]["content"][1]["file"]["file_data"],
            format!("data:application/pdf;base64,{encoded}")
        );
        assert!(value[1]["content"][1].get("image_url").is_none());
        assert_eq!(value.to_string().matches(encoded).count(), 1);
    }

    #[test]
    fn unknown_or_incompatible_capabilities_keep_the_complete_tool_text() {
        let id = uuid::Uuid::new_v4();
        let encoded = "PNG_FALLBACK_SENTINEL";
        let messages = vec![attachment_result(
            id,
            encoded,
            crate::agent::ToolAttachment::Image {
                media_type: "image/png".to_string(),
            },
        )];

        for modalities in [None, Some(Vec::new())] {
            let value = serialize_messages_with_policy(
                &messages,
                modalities.as_deref(),
                ContentPartFormat::OpenAiCompatible,
            );
            assert_eq!(value.as_array().expect("messages").len(), 1);
            assert!(value[0]["content"]
                .as_str()
                .expect("tool content")
                .contains(encoded));
            assert!(value.to_string().matches(encoded).count() >= 1);
        }
    }

    #[test]
    fn contiguous_tool_outcomes_emit_all_tool_messages_before_one_media_follow_up() {
        let image_id = uuid::Uuid::new_v4();
        let file_id = uuid::Uuid::new_v4();
        let messages = vec![
            attachment_result(
                image_id,
                "IMAGE_SENTINEL",
                crate::agent::ToolAttachment::Image {
                    media_type: "image/png".to_string(),
                },
            ),
            Message::ToolError {
                id: uuid::Uuid::new_v4(),
                name: "read".to_string(),
                error: "failed".to_string(),
                duration: std::time::Duration::ZERO,
                content_sections: vec![],
            },
            attachment_result(
                file_id,
                "FILE_SENTINEL",
                crate::agent::ToolAttachment::File {
                    media_type: "application/pdf".to_string(),
                    filename: "report.pdf".to_string(),
                },
            ),
        ];
        let value = serialize_messages_with_policy(
            &messages,
            Some(&[InputModality::Image, InputModality::File]),
            ContentPartFormat::OpenAiCompatible,
        );
        let values = value.as_array().expect("messages");
        assert_eq!(values.len(), 4);
        assert!(values[..3].iter().all(|message| message["role"] == "tool"));
        assert_eq!(values[3]["role"], "user");
        assert_eq!(values[3]["content"][0]["type"], "text");
        let mapping = values[3]["content"][0]["text"]
            .as_str()
            .expect("attachment mapping");
        assert!(mapping.contains(&image_id.to_string()));
        assert!(mapping.contains(&file_id.to_string()));
        assert_eq!(values[3]["content"][1]["type"], "image_url");
        assert_eq!(values[3]["content"][2]["type"], "file");
    }

    #[test]
    fn media_follow_up_maps_two_image_reads_to_their_tool_call_ids() {
        let first_id = uuid::Uuid::new_v4();
        let second_id = uuid::Uuid::new_v4();
        let messages = vec![
            attachment_result(
                first_id,
                "FIRST_IMAGE_SENTINEL",
                crate::agent::ToolAttachment::Image {
                    media_type: "image/png".to_string(),
                },
            ),
            attachment_result(
                second_id,
                "SECOND_IMAGE_SENTINEL",
                crate::agent::ToolAttachment::Image {
                    media_type: "image/png".to_string(),
                },
            ),
        ];

        let value = serialize_messages_with_policy(
            &messages,
            Some(&[InputModality::Image]),
            ContentPartFormat::OpenAiCompatible,
        );
        let content = value[2]["content"].as_array().expect("user content");
        let mapping = content[0]["text"].as_str().expect("attachment mapping");
        let first_position = mapping
            .find(&first_id.to_string())
            .expect("first tool call id");
        let second_position = mapping
            .find(&second_id.to_string())
            .expect("second tool call id");
        assert!(first_position < second_position);
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[2]["type"], "image_url");
        assert_eq!(value.to_string().matches("FIRST_IMAGE_SENTINEL").count(), 1);
        assert_eq!(
            value.to_string().matches("SECOND_IMAGE_SENTINEL").count(),
            1
        );
    }

    #[test]
    fn ollama_image_projection_uses_the_explicit_string_compatibility_shape() {
        let value = serialize_messages_with_policy(
            &[attachment_result(
                uuid::Uuid::new_v4(),
                "OLLAMA_SENTINEL",
                crate::agent::ToolAttachment::Image {
                    media_type: "image/png".to_string(),
                },
            )],
            Some(&[InputModality::Image]),
            ContentPartFormat::OllamaCompatibility,
        );
        assert!(value[1]["content"][1]["image_url"].is_string());
        assert_eq!(value[1]["content"][1]["type"], "image_url");
    }

    #[test]
    fn persisted_attachment_round_trip_reconstructs_the_same_media_projection() {
        let message = attachment_result(
            uuid::Uuid::new_v4(),
            "REPLAY_IMAGE_SENTINEL",
            crate::agent::ToolAttachment::Image {
                media_type: "image/png".to_string(),
            },
        );
        let serialized = serde_json::to_string(&message).expect("message serializes");
        let restored = serde_json::from_str::<Message>(&serialized).expect("message restores");
        assert_eq!(
            serialize_messages_with_policy(
                std::slice::from_ref(&restored),
                Some(&[InputModality::Image]),
                ContentPartFormat::OpenAiCompatible,
            ),
            serialize_messages_with_policy(
                std::slice::from_ref(&message),
                Some(&[InputModality::Image]),
                ContentPartFormat::OpenAiCompatible,
            )
        );
    }
}
