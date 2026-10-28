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
use crate::agent::tests::*;
use crate::agent::*;
use crate::llm::EngineConfig;

#[test]
fn request_retry_and_batch_bookkeeping_remain_context_owned() {
    let mut context = Context::new();
    context.record_batch_admission(4, 1);
    context.record_skipped_call();
    assert_eq!(context.batch_admitted_calls(), 2);
    assert_eq!(context.batch_skipped_calls(), 2);
    assert!(!context.take_context_rejection_retry());
    context.retry_after_context_rejection();
    assert!(context.take_context_rejection_retry());
    assert!(!context.take_context_rejection_retry());
    let request_id = Uuid::new_v4();
    context.set_compaction_diagnostics(Some(request_id), "summary_failure");
    context.set_recovery_trigger(true);
    assert_eq!(context.compaction_request_id(), Some(request_id));
    assert_eq!(context.compaction_trigger, "provider_context_rejection");
    context.set_recovery_trigger(false);
    assert_eq!(context.compaction_trigger, "request_boundary");
}
#[test]
fn session_begins_with_seeded_system_messages() {
    let context = Context::new();
    assert!(TOOL_PROTOCOL_SYSTEM_PROMPT.contains(&format!(
        "at most {MAX_TOOL_CALLS_PER_RESPONSE} distinct tool calls"
    )));
    assert_eq!(
        context.messages(),
        &[
            Message::System {
                content: crate::config::soul::DEFAULT_SOUL.to_string(),
            },
            Message::System {
                content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
            },
        ]
    );
}

#[test]
fn session_can_begin_with_a_caller_supplied_system_message() {
    let context = Context::with_system_prompt("project persona");

    assert_eq!(
        context.messages()[0],
        Message::System {
            content: "project persona".to_string(),
        }
    );
    assert!(matches!(context.messages()[1], Message::System { .. }));
}

#[test]
fn push_messages_appends_in_order() {
    let mut context = Context::new();
    context.push_user("first user turn".to_string());
    context.push_assistant("first response".to_string(), AssistantStatus::Complete);
    context.push_user("second user turn".to_string());
    assert_eq!(context.messages().len(), 5);
    assert_eq!(
        context.messages()[2],
        Message::User {
            content: "first user turn".to_string(),
            kind: UserMessageKind::Explicit,
        }
    );
    assert_eq!(
        context.messages()[3],
        Message::Assistant {
            content: "first response".to_string(),
            status: AssistantStatus::Complete,
            tool_calls: Vec::new(),
        }
    );
    assert_eq!(
        context.messages()[4],
        Message::User {
            content: "second user turn".to_string(),
            kind: UserMessageKind::Explicit,
        }
    );
}

#[test]
fn push_assistant_marks_interrupted_status() {
    let mut context = Context::new();
    context.push_assistant("partial".to_string(), AssistantStatus::Interrupted);
    assert_eq!(
        context.messages()[2],
        Message::Assistant {
            content: "partial".to_string(),
            status: AssistantStatus::Interrupted,
            tool_calls: Vec::new(),
        }
    );
}

#[test]
fn push_assistant_tool_call_stores_calls() {
    let mut context = Context::new();
    let id = Uuid::new_v4();
    context.push_assistant_tool_call(
        String::new(),
        AssistantStatus::Complete,
        vec![ToolCallRequest {
            id,
            name: "read".into(),
            arguments: serde_json::json!({ "path": "a.txt" }),
            persisted_title_arguments: vec![],
        }],
    );
    assert_eq!(
        context.messages()[2],
        Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id,
                name: "read".into(),
                arguments: serde_json::json!({ "path": "a.txt" }),
                persisted_title_arguments: vec![],
            }],
        }
    );
}

#[test]
fn push_tool_result_and_error_append_messages() {
    let mut context = Context::new();
    let id = Uuid::new_v4();
    context.push_tool_result(ToolResult {
        id,
        name: "read".into(),
        result: "contents".into(),
        attachments: vec![],
        result_section_id: None,
        result_metadata: None,
        result_metadata_schema: None,
        summary: vec![],
        content_sections: vec![],
        duration: std::time::Duration::ZERO,
    });
    context.push_tool_error(
        id,
        "write".into(),
        "denied".into(),
        None,
        false,
        std::time::Duration::ZERO,
        vec![],
    );
    assert_eq!(
        context.messages()[2],
        Message::ToolResult {
            value: ToolResult {
                id,
                name: "read".into(),
                result: "contents".into(),
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
    assert_eq!(
        context.messages()[3],
        Message::ToolError {
            id,
            name: "write".into(),
            error: "denied".into(),
            duration: std::time::Duration::ZERO,
            content_sections: vec![],
        }
    );
}

#[test]
fn hydrate_appends_after_system_prompts_and_synthesizes_orphaned_tool_error() {
    let id = Uuid::new_v4();
    let mut context = Context::new();
    context.hydrate(vec![
        Message::User {
            content: "resume this".to_string(),
            kind: UserMessageKind::Explicit,
        },
        Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id,
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "README.md"}),
                persisted_title_arguments: vec![],
            }],
        },
    ]);

    assert!(matches!(context.messages()[0], Message::System { .. }));
    assert!(matches!(context.messages()[1], Message::System { .. }));
    assert!(matches!(
        context.messages()[2],
        Message::User { ref content, .. } if content == "resume this"
    ));
    assert!(matches!(
        context.messages().last(),
        Some(Message::ToolError { id: error_id, name, error, duration, content_sections })
            if *error_id == id
                && name == "read"
                && error == "Tool call did not complete before the session ended."
                && *duration == std::time::Duration::ZERO
                && content_sections.is_empty()
    ));

    let wire_payload =
        crate::llm::openai_compatible_provider::messages::serialize_messages(context.messages());
    assert!(wire_payload
        .as_array()
        .expect("wire payload is an array")
        .iter()
        .any(|message| {
            let content = message["content"]
                .as_str()
                .and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok());
            message["role"] == "tool"
                && message["tool_call_id"] == id.to_string()
                && content.is_some_and(|content| content["ok"] == false)
        }));
}

#[tokio::test]
async fn attaching_a_store_hydrates_without_recommitting_messages() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let store = crate::store::Store::create(
        &temporary_directory.path().join("chats"),
        temporary_directory.path(),
        Uuid::new_v4(),
        "mock",
        "test-model",
        None,
    )
    .expect("store created");
    store.record_message(&Message::User {
        content: "persisted once".to_string(),
        kind: UserMessageKind::Explicit,
    });
    store.flush().await;

    let mut context = Context::new();
    context.set_store(store.clone());
    store.flush().await;

    let messages = crate::store::Store::read_messages(store.path()).expect("messages readable");
    assert_eq!(messages.len(), 1);
    assert_eq!(context.messages().len(), 3);
    assert!(matches!(
        &context.messages()[2],
        Message::User { content, .. } if content == "persisted once"
    ));
}

#[test]
fn messages_returns_immutable_read_back_slice() {
    let mut context = Context::new();
    context.push_user("hello".to_string());
    let read_back = context.messages();
    assert_eq!(read_back.len(), 3);
    assert!(matches!(read_back[0], Message::System { .. }));
    assert!(matches!(read_back[1], Message::System { .. }));
    assert_eq!(
        read_back[2],
        Message::User {
            content: "hello".into(),
            kind: UserMessageKind::Explicit,
        }
    );
}

mod projection;
mod recovery;
mod tokens;

#[test]
fn a_context_without_a_store_commits_messages_in_memory_only() {
    let mut context = crate::agent::Context::new();
    context.push_user("no persistence configured".to_string());
    assert!(context.messages().iter().any(|message| matches!(
        message,
        Message::User { content, .. } if content == "no persistence configured"
    )));
}
