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
use crate::agent::{AssistantStatus, Message, UserMessageKind};
use crate::store::SubagentRunRecord;

#[test]
fn replay_places_the_compaction_marker_at_the_boundary_and_keeps_the_transcript() {
    let messages = vec![
        crate::agent::Message::User {
            content: "first question".to_string(),
            kind: UserMessageKind::Explicit,
        },
        crate::agent::Message::Assistant {
            content: "first answer".to_string(),
            status: crate::agent::AssistantStatus::Complete,
            tool_calls: Vec::new(),
        },
        crate::agent::Message::User {
            content: "second question".to_string(),
            kind: UserMessageKind::Explicit,
        },
        crate::agent::Message::Assistant {
            content: "second answer".to_string(),
            status: crate::agent::AssistantStatus::Complete,
            tool_calls: Vec::new(),
        },
    ];

    let mut state = AppState::new();
    state.replay_messages(&messages, &[], Some(2));

    assert_eq!(
        state.chat_entries.len(),
        5,
        "four messages plus the marker: {:?}",
        state.chat_entries
    );
    assert!(
        matches!(
            &state.chat_entries[0],
            ChatEntry::UserMessage { text, .. } if text == "first question"
        ),
        "the summarized prefix stays browsable: {:?}",
        state.chat_entries[0]
    );
    assert!(matches!(
        &state.chat_entries[1],
        ChatEntry::AssistantMessage { text, .. } if text == "first answer"
    ));
    assert_eq!(
        state.chat_entries[2],
        ChatEntry::CompactionMarker {
            summarized_messages: 2
        },
        "the marker sits exactly where the verbatim tail begins"
    );
    assert!(
        matches!(
            &state.chat_entries[3],
            ChatEntry::UserMessage { text, .. } if text == "second question"
        ),
        "the tail after the boundary keeps rendering in order: {:?}",
        state.chat_entries[3]
    );
    assert!(matches!(
        &state.chat_entries[4],
        ChatEntry::AssistantMessage { text, .. } if text == "second answer"
    ));
    assert_eq!(
        state.input_history,
        vec!["first question".to_string(), "second question".to_string()],
        "the marker is not a message and never seeds input history"
    );

    // A session without a snapshot replays exactly as before.
    let mut plain = AppState::new();
    plain.replay_messages(&messages, &[], None);
    assert_eq!(plain.chat_entries.len(), 4);
}

#[test]
fn replay_projection_restores_full_fidelity_tool_bubble() {
    let tool_id = Uuid::new_v4();
    let content_section =
        crate::agent::tools::Section::Content(crate::agent::tools::ContentSection {
            label: Some("Output".to_string()),
            content: "file contents".to_string(),
            file_extension: Some("txt".to_string()),
            line_number_offset: Some(2),
        });
    let messages = vec![
        Message::User {
            content: "read the file".to_string(),
            kind: UserMessageKind::Explicit,
        },
        Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id: tool_id,
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "README.md"}),
                persisted_title_arguments: vec![(
                    "path".to_string(),
                    serde_json::json!("README.md"),
                )],
            }],
        },
        Message::ToolResult {
            value: crate::agent::ToolResult {
                id: tool_id,
                name: "read".to_string(),
                result: "file contents".to_string(),
                attachments: vec![],
                result_section_id: None,
                result_metadata: None,
                result_metadata_schema: None,
                summary: vec![("path".to_string(), "README.md".to_string())],
                content_sections: vec![content_section.clone()],
                duration: Duration::from_millis(37),
            },
        },
    ];

    let entries = project_messages(&messages);
    assert!(matches!(
        entries.first(),
        Some(ChatEntry::UserMessage { text, state: UserMessageState::Sent })
            if text == "read the file"
    ));
    assert!(matches!(
        entries.get(1),
        Some(ChatEntry::ToolBubble { id, state, collapsed: true, .. })
            if *id == tool_id && matches!(
                state.as_ref(),
                ToolCallState::Done {
                    arguments,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    title_arguments,
                } if arguments == &serde_json::json!({"path": "README.md"})
                    && result_section == &ResultSection::plain("file contents")
                    && content_sections == &vec![content_section]
                    && summary == &vec![("path".to_string(), "README.md".to_string())]
                    && *duration == Duration::from_millis(37)
                    && title_arguments == &vec![("path".to_string(), serde_json::json!("README.md"))]
            )
    ));
}

#[test]
fn replay_projection_hides_transport_only_tool_result() {
    let tool_id = Uuid::new_v4();
    let encoded = "iVBORwD/";
    let messages = vec![
        Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id: tool_id,
                name: "read".to_string(),
                arguments: serde_json::json!({
                    "path": "image.png",
                    "mode": "bytes",
                    "encoding": "base64",
                }),
                persisted_title_arguments: vec![],
            }],
        },
        Message::ToolResult {
            value: crate::agent::ToolResult {
                id: tool_id,
                name: "read".to_string(),
                result: encoded.to_string(),
                attachments: vec![],
                result_section_id: Some(result_section_ids::HIDDEN.to_string()),
                result_metadata: Some(serde_json::json!({"read_bytes": 6})),
                result_metadata_schema: None,
                summary: vec![],
                content_sections: vec![],
                duration: Duration::ZERO,
            },
        },
    ];

    let entries = project_messages(&messages);
    assert!(matches!(
        entries.first(),
        Some(ChatEntry::ToolBubble { state, .. })
            if matches!(
                state.as_ref(),
                ToolCallState::Done { result_section, .. }
                    if result_section.id == result_section_ids::HIDDEN
                        && result_section.content.is_empty()
            )
    ));
}

#[test]
fn replay_projection_replaces_spawn_result_with_finished_or_orphaned_subagent_state() {
    let finished_call_id = Uuid::new_v4();
    let orphaned_call_id = Uuid::new_v4();
    let finished_run_id = Uuid::new_v4();
    let orphaned_run_id = Uuid::new_v4();
    let spawn_call = |id| ToolCallRequest {
        id,
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        arguments: serde_json::json!({"task": "inspect the patch"}),
        persisted_title_arguments: vec![],
    };
    let messages = vec![Message::Assistant {
        content: String::new(),
        status: AssistantStatus::Complete,
        tool_calls: vec![spawn_call(finished_call_id), spawn_call(orphaned_call_id)],
    }];
    let subagent_runs = vec![
        SubagentRunRecord {
            run_id: finished_run_id,
            call_id: finished_call_id,
            parent_session: Uuid::new_v4(),
            agent_type: "research".to_string(),
            task: "inspect the patch".to_string(),
            label: "Inspect patch".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            completed_at: Some("2026-01-01T00:00:01.000Z".to_string()),
            status: Some("success".to_string()),
            result: Some("patch is safe".to_string()),
            sources: vec!["src/lib.rs".to_string()],
            origin: crate::subagent_manager::FinishResultOrigin::Agent,
            duration: Some(Duration::from_secs(1)),
        },
        SubagentRunRecord {
            run_id: orphaned_run_id,
            call_id: orphaned_call_id,
            parent_session: Uuid::new_v4(),
            agent_type: "review".to_string(),
            task: "inspect the patch".to_string(),
            label: "Inspect patch".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            completed_at: None,
            status: None,
            result: None,
            sources: vec![],
            origin: crate::subagent_manager::FinishResultOrigin::default(),
            duration: None,
        },
    ];

    let entries = project_messages_with_subagent_runs(&messages, &subagent_runs, None);
    let finished_entry = entries
        .iter()
        .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == finished_call_id))
        .expect("finished subagent bubble");
    assert!(matches!(
        finished_entry,
        ChatEntry::ToolBubble {
            state,
            ..
        } if matches!(
            state.as_ref(),
            ToolCallState::Done {
                summary,
                content_sections,
                duration,
                ..
            } if summary == &vec![("status".to_string(), "success".to_string())]
                && content_sections.len() == 2
                && *duration == Duration::from_secs(1)
        )
    ));

    let orphaned_entry = entries
        .iter()
        .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == orphaned_call_id))
        .expect("orphaned subagent bubble");
    assert!(matches!(
        orphaned_entry,
        ChatEntry::ToolBubble {
            state,
            ..
        } if matches!(
            state.as_ref(),
            ToolCallState::Error { error, .. }
                if error == "Subagent did not finish before the session ended."
        )
    ));
}

#[test]
fn replay_subagent_transcripts_load_once_on_drill_in() {
    let run_id = Uuid::new_v4();
    let call_id = Uuid::new_v4();
    let subagent_runs = vec![SubagentRunRecord {
        run_id,
        call_id,
        parent_session: Uuid::new_v4(),
        agent_type: "research".to_string(),
        task: "inspect the patch".to_string(),
        label: "Inspect patch".to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        completed_at: Some("2026-01-01T00:00:01.000Z".to_string()),
        status: Some("success".to_string()),
        result: Some("done".to_string()),
        sources: vec![],
        origin: crate::subagent_manager::FinishResultOrigin::Agent,
        duration: Some(Duration::from_secs(1)),
    }];
    let root_messages = vec![Message::Assistant {
        content: String::new(),
        status: AssistantStatus::Complete,
        tool_calls: vec![ToolCallRequest {
            id: call_id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            arguments: serde_json::json!({"task": "inspect the patch"}),
            persisted_title_arguments: vec![],
        }],
    }];
    let subagent_messages = vec![
        Message::User {
            content: "inspect the patch".to_string(),
            kind: UserMessageKind::Explicit,
        },
        Message::Assistant {
            content: "done".to_string(),
            status: AssistantStatus::Complete,
            tool_calls: vec![],
        },
    ];

    let mut state = AppState::new();
    state.replay_messages(&root_messages, &subagent_runs, None);
    assert!(state.subagent_needs_lazy_load(run_id));
    assert!(state
        .subagent_conversations
        .get(&run_id)
        .expect("replayed subagent")
        .messages
        .is_empty());

    state.load_subagent_messages(run_id, &subagent_messages, None);
    let loaded_messages = state
        .subagent_conversations
        .get(&run_id)
        .expect("replayed subagent")
        .messages
        .clone();
    assert!(!state.subagent_needs_lazy_load(run_id));
    assert_eq!(loaded_messages.len(), 2);

    state.load_subagent_messages(
        run_id,
        &[Message::Assistant {
            content: "should not replace the cached transcript".to_string(),
            status: AssistantStatus::Complete,
            tool_calls: vec![],
        }],
        Some(1),
    );
    assert_eq!(
        state
            .subagent_conversations
            .get(&run_id)
            .expect("replayed subagent")
            .messages,
        loaded_messages
    );
}

#[test]
fn active_subagent_counts_include_running_and_queued_bubbles_only() {
    let mut state = AppState::new();
    let push_running_subagent = |state: &mut AppState, status: &str| {
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![(
                    "status".to_string(),
                    serde_json::Value::String(status.to_string()),
                )],
            }),
            collapsed: true,
        });
    };

    push_running_subagent(&mut state, "running");
    push_running_subagent(&mut state, "queued");
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: Uuid::new_v4(),
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Done {
            arguments: serde_json::json!({}),
            result_section: ResultSection::plain("finished"),
            content_sections: vec![],
            summary: vec![],
            duration: Duration::ZERO,
            title_arguments: vec![],
        }),
        collapsed: true,
    });

    assert_eq!(
        state.active_subagent_counts(),
        ActiveSubagentCounts {
            running: 1,
            queued: 1,
        }
    );
}

#[test]
fn apply_tool_denied_preserves_arguments() {
    let mut state = AppState::new();
    let id = Uuid::new_v4();
    let args = serde_json::json!({"cmd": "rm -rf /"});
    state.chat_entries.push(ChatEntry::ToolBubble {
        id,
        name: "shell".to_string(),
        state: Box::new(ToolCallState::Pending {
            arguments: args.clone(),
            content_sections: vec![],

            title_arguments: vec![],
        }),
        collapsed: false,
    });
    state.apply_tool_denied(id, "");
    assert!(
        matches!(
            state.chat_entries.last(),
            Some(ChatEntry::ToolBubble {
                state,
                ..
            }) if matches!(state.as_ref(), ToolCallState::Denied { arguments, error, .. } if *arguments == args && error == "Denied by user")
        ),
        "apply_tool_denied must produce Denied with the original Pending arguments"
    );
}

#[test]
fn apply_tool_error_skips_already_denied_bubble() {
    let mut state = AppState::new();
    let id = Uuid::new_v4();
    let args = serde_json::json!({"cmd": "ls"});
    state.chat_entries.push(ChatEntry::ToolBubble {
        id,
        name: "shell".to_string(),
        state: Box::new(ToolCallState::Denied {
            arguments: args.clone(),
            error: "Denied by user".to_string(),
            content_sections: vec![],

            title_arguments: vec![],
        }),
        collapsed: false,
    });
    state.apply_tool_error(
        id,
        "Tool 'shell' was denied: Denied by user".to_string(),
        std::time::Duration::ZERO,
        vec![],
        None,
    );
    assert!(
        matches!(
            state.chat_entries.last(),
            Some(ChatEntry::ToolBubble {
                state,
                ..
            }) if matches!(state.as_ref(), ToolCallState::Denied { arguments, .. } if *arguments == args)
        ),
        "apply_tool_error must not overwrite a Denied bubble"
    );
}

#[test]
fn apply_tool_error_on_running_bubble_produces_error() {
    let mut state = AppState::new();
    let id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id,
        name: "shell".to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({"cmd": "ls"}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    state.apply_tool_error(
        id,
        "execution failed".to_string(),
        std::time::Duration::ZERO,
        vec![],
        None,
    );
    assert!(
        matches!(
            state.chat_entries.last(),
            Some(ChatEntry::ToolBubble {
                state,
                ..
            }) if matches!(state.as_ref(), ToolCallState::Error { error, .. } if error == "execution failed")
        ),
        "Running + ToolError must produce Error"
    );
}

#[test]
fn flush_partial_assistant_message_pushes_thinking_before_assistant_bubble() {
    let mut state = AppState::new();
    state.turn_buffer.partial_thinking_text = "pondering...".to_string();
    state.turn_buffer.partial_assistant_text = "the answer".to_string();
    state.flush_partial_assistant_message(false);

    assert!(state.turn_buffer.partial_thinking_text.is_empty());
    assert!(state.turn_buffer.partial_assistant_text.is_empty());
    assert_eq!(state.chat_entries.len(), 2);
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ThinkingMessage { text, collapsed, .. } if text == "pondering..." && !*collapsed
    ));
    assert!(matches!(
        &state.chat_entries[1],
        ChatEntry::AssistantMessage { text, interrupted } if text == "the answer" && !*interrupted
    ));
}

#[test]
fn flush_partial_assistant_message_computes_thinking_duration_from_started_at() {
    let mut state = AppState::new();
    state.turn_buffer.partial_thinking_text = "pondering...".to_string();
    state.turn_buffer.thinking_started_at =
        Some(std::time::Instant::now() - std::time::Duration::from_millis(50));
    state.flush_partial_assistant_message(false);

    assert!(state.turn_buffer.thinking_started_at.is_none());
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ThinkingMessage { duration, .. } if *duration >= std::time::Duration::from_millis(50)
    ));
}

#[test]
fn flush_partial_assistant_message_defaults_thinking_duration_when_not_started() {
    let mut state = AppState::new();
    state.turn_buffer.partial_thinking_text = "pondering...".to_string();
    state.flush_partial_assistant_message(false);

    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ThinkingMessage { duration, .. } if *duration == std::time::Duration::ZERO
    ));
}

#[test]
fn flush_partial_assistant_message_pushes_thinking_only_when_assistant_text_empty() {
    // A thinking-only turn: the model reasons, then calls a tool with no
    // visible prose. Must still flush the thinking bubble, without an
    // empty AssistantMessage.
    let mut state = AppState::new();
    state.turn_buffer.partial_thinking_text = "considering options...".to_string();
    state.flush_partial_assistant_message(false);

    assert!(state.turn_buffer.partial_thinking_text.is_empty());
    assert_eq!(state.chat_entries.len(), 1);
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ThinkingMessage { text, collapsed, .. } if text == "considering options..." && !*collapsed
    ));
}

#[test]
fn flush_partial_assistant_message_pushes_nothing_when_both_empty() {
    let mut state = AppState::new();
    state.flush_partial_assistant_message(false);
    assert!(state.chat_entries.is_empty());
}

#[test]
fn flush_partial_assistant_message_drops_whitespace_only_assistant_text() {
    // Some models (observed: deepseek/deepseek-v4-flash) stream a lone
    // `"\n\n"` content chunk immediately before a tool call, with no
    // other prose. That whitespace-only text must not surface as a
    // visually-empty `AssistantMessage` bubble.
    let mut state = AppState::new();
    state.turn_buffer.partial_assistant_text = "\n\n".to_string();
    state.flush_partial_assistant_message(false);

    assert!(state.chat_entries.is_empty(), "{:?}", state.chat_entries);
    assert!(state.turn_buffer.partial_assistant_text.is_empty());
}

#[test]
fn flush_subagent_partial_text_drops_whitespace_only_assistant_text() {
    let mut state = AppState::new();
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    let conversation = state
        .subagent_conversations
        .get_mut(&run_id)
        .expect("conversation");
    conversation.turn_buffer.partial_assistant_text = "\n\n".to_string();
    state.flush_subagent_partial_text(run_id);

    let conversation = state
        .subagent_conversations
        .get(&run_id)
        .expect("conversation");
    assert!(
        conversation.messages.is_empty(),
        "{:?}",
        conversation.messages
    );
}

#[test]
fn flush_partial_thinking_message_pushes_thinking_bubble_and_leaves_assistant_text_untouched() {
    let mut state = AppState::new();
    state.turn_buffer.partial_thinking_text = "pondering...".to_string();
    state.turn_buffer.partial_assistant_text = "so far so good".to_string();
    state.flush_partial_thinking_message();

    assert!(state.turn_buffer.partial_thinking_text.is_empty());
    assert_eq!(
        state.turn_buffer.partial_assistant_text, "so far so good",
        "must not touch the assistant buffer"
    );
    assert_eq!(state.chat_entries.len(), 1);
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ThinkingMessage { text, .. } if text == "pondering..."
    ));
}

#[test]
fn flush_partial_thinking_message_is_noop_when_no_pending_thinking_text() {
    let mut state = AppState::new();
    state.flush_partial_thinking_message();
    assert!(state.chat_entries.is_empty());
}

#[test]
fn flush_partial_response_message_pushes_assistant_bubble_and_leaves_thinking_text_untouched() {
    let mut state = AppState::new();
    state.turn_buffer.partial_assistant_text = "the answer".to_string();
    state.turn_buffer.partial_thinking_text = "still pondering...".to_string();
    state.flush_partial_response_message();

    assert!(state.turn_buffer.partial_assistant_text.is_empty());
    assert_eq!(
        state.turn_buffer.partial_thinking_text, "still pondering...",
        "must not touch the thinking buffer"
    );
    assert_eq!(state.chat_entries.len(), 1);
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::AssistantMessage { text, interrupted } if text == "the answer" && !*interrupted
    ));
}

#[test]
fn flush_partial_response_message_is_noop_when_no_pending_assistant_text() {
    let mut state = AppState::new();
    state.flush_partial_response_message();
    assert!(state.chat_entries.is_empty());
}

#[test]
fn toggle_thinking_collapsed_flips_state_at_index() {
    let mut state = AppState::new();
    state.chat_entries.push(ChatEntry::ThinkingMessage {
        text: "reasoning".to_string(),
        collapsed: true,
        duration: std::time::Duration::from_secs(3),
    });
    state.conversation.scroll_locked = true;

    state.toggle_thinking_collapsed(0);

    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ThinkingMessage { collapsed, .. } if !*collapsed
    ));
    assert!(!state.conversation.scroll_locked);
}

#[test]
fn toggle_partial_thinking_collapsed_flips_state_while_streaming() {
    let mut state = AppState::new();
    state.turn_buffer.partial_thinking_text = "pondering...".to_string();
    state.turn_buffer.partial_thinking_collapsed = false;
    state.conversation.scroll_locked = true;

    state.toggle_partial_thinking_collapsed();

    assert!(state.turn_buffer.partial_thinking_collapsed);
    assert!(!state.conversation.scroll_locked);

    state.toggle_partial_thinking_collapsed();

    assert!(!state.turn_buffer.partial_thinking_collapsed);
}

#[test]
fn toggle_partial_thinking_collapsed_clamps_scroll_offset_to_bubble_start_row() {
    let mut state = AppState::new();
    state.turn_buffer.partial_thinking_text = "pondering...".to_string();
    // Simulate a previous render pass recording the in-flight bubble's
    // title row under the streaming sentinel index.
    state.conversation.thinking_title_rows = vec![(5, 40, STREAMING_THINKING_ENTRY_INDEX)];
    state.conversation.scroll_offset = 30;
    state.conversation.scroll_locked = false;

    state.toggle_partial_thinking_collapsed();

    assert_eq!(state.conversation.scroll_offset, 5);
}

#[test]
fn toggle_thinking_collapsed_clamps_scroll_offset_to_bubble_start_row() {
    let mut state = AppState::new();
    state.chat_entries.push(ChatEntry::ThinkingMessage {
        text: "reasoning".to_string(),
        collapsed: false,
        duration: std::time::Duration::from_secs(3),
    });
    // Simulate a previous render pass recording this bubble's title row
    // and a viewport scrolled well below it (e.g. the long expanded body
    // pushed the view down).
    state.conversation.thinking_title_rows = vec![(5, 40, 0)];
    state.conversation.scroll_offset = 30;
    state.conversation.scroll_locked = false;

    state.toggle_thinking_collapsed(0);

    // The offset must not exceed the bubble's own start row, otherwise
    // the title scrolls out of the viewport once the body collapses.
    assert_eq!(state.conversation.scroll_offset, 5);
}

#[test]
fn toggle_tool_bubble_collapsed_clamps_scroll_offset_to_bubble_start_row() {
    let mut state = AppState::new();
    let id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id,
        name: "read".to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    // Simulate a previous render pass recording this bubble's title row
    // and a viewport scrolled well below it (e.g. the long expanded body
    // pushed the view down).
    state.conversation.bubble_title_rows = vec![(5, 40, id)];
    state.conversation.scroll_offset = 30;
    state.conversation.scroll_locked = false;

    state.toggle_tool_bubble_collapsed(id);

    // The offset must not exceed the bubble's own start row, otherwise
    // the title scrolls out of the viewport once the body collapses.
    assert_eq!(state.conversation.scroll_offset, 5);
}

#[test]
fn toggle_tool_bubble_collapsed_toggles_bubble_in_subagent_conversations() {
    let mut state = AppState::new();
    let id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    state
        .subagent_conversations
        .get_mut(&run_id)
        .unwrap()
        .messages
        .push(ChatEntry::ToolBubble {
            id,
            name: "read".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: true,
        });

    state.toggle_tool_bubble_collapsed(id);

    let ChatEntry::ToolBubble { collapsed, .. } =
        &state.subagent_conversations[&run_id].messages[0]
    else {
        panic!("expected ToolBubble");
    };
    assert!(!collapsed);
}

#[test]
fn toggle_thinking_collapsed_ignores_out_of_range_or_wrong_variant_index() {
    let mut state = AppState::new();
    state.chat_entries.push(ChatEntry::AssistantMessage {
        text: "hi".to_string(),
        interrupted: false,
    });
    // Neither index should panic or mutate anything.
    state.toggle_thinking_collapsed(0);
    state.toggle_thinking_collapsed(5);
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::AssistantMessage { .. }
    ));
}

#[test]
fn header_bubble_scroll_up_and_down_saturate_at_bounds() {
    let mut state = AppState::new();
    state.header_bubble_scroll_up(3);
    assert_eq!(
        state.subagent_view.header_bubble_scroll_offset, 0,
        "must not underflow below zero"
    );
    state.header_bubble_scroll_down(5);
    assert_eq!(state.subagent_view.header_bubble_scroll_offset, 5);
    state.header_bubble_scroll_up(2);
    assert_eq!(state.subagent_view.header_bubble_scroll_offset, 3);
}

#[test]
fn switch_to_subagent_view_resets_header_bubble_scroll_offset() {
    let mut state = AppState::new();
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    state.subagent_view.header_bubble_scroll_offset = 7;

    state.switch_to_subagent_view(run_id);

    assert_eq!(state.subagent_view.header_bubble_scroll_offset, 0);
}

#[test]
fn switch_to_subagent_view_re_pins_chat_scroll_to_bottom() {
    // `scroll_offset`/`scroll_locked` are shared between the root and
    // subagent views (single fields, not per-view). If the user had
    // scrolled up while viewing `Root` and then drills into a running
    // subagent, that stale non-bottom position must not carry over —
    // the subagent transcript should always open pinned to its latest
    // activity (regression test for the bug where drilling in left the
    // list scrolled wherever `Root` had been left).
    let mut state = AppState::new();
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    state.conversation.scroll_offset = 30;
    state.conversation.scroll_locked = false;

    state.switch_to_subagent_view(run_id);

    assert_eq!(state.conversation.scroll_offset, 0);
    assert!(
        state.conversation.scroll_locked,
        "subagent view must open auto-scrolled (pinned) to the bottom"
    );
}

#[test]
fn switch_to_root_view_restores_scroll_position_from_before_drill_in() {
    // Regression test: Esc (empty draft) must land the user back where they
    // drilled in from (e.g. a `spawn_subagent` bubble scrolled up to),
    // not always at the end of the root conversation.
    let mut state = AppState::new();
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    state.conversation.scroll_offset = 30;
    state.conversation.scroll_locked = false;

    state.switch_to_subagent_view(run_id);
    state.switch_to_root_view();

    assert_eq!(state.conversation.scroll_offset, 30);
    assert!(!state.conversation.scroll_locked);
    assert!(state.saved_root_scroll.is_none());
}

#[test]
fn switch_to_root_view_is_a_no_op_on_scroll_when_subagent_was_auto_switched() {
    // `note_interactive_pause` auto-switches into the subagent view
    // without going through `switch_to_subagent_view`, so it never
    // populates `saved_root_scroll`. The scroll fields it leaves behind
    // belong to whatever was on screen while viewing `Root` and must be
    // left untouched on auto-return.
    let mut state = AppState::new();
    state.conversation.scroll_offset = 12;
    state.conversation.scroll_locked = false;
    state.active_view = ConversationView::Subagent {
        run_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    };

    state.switch_to_root_view();

    assert_eq!(state.conversation.scroll_offset, 12);
    assert!(!state.conversation.scroll_locked);
}

#[test]
fn switch_to_subagent_view_captures_the_drilled_into_spawn_bubble_id() {
    let mut state = AppState::new();
    let bubble_id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: bubble_id,
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id,
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });

    state.switch_to_subagent_view(run_id);

    assert_eq!(state.saved_root_scroll_bubble, Some(bubble_id));
}

#[test]
fn switch_to_root_view_arms_pending_scroll_to_bubble_from_the_saved_bubble_id() {
    // Regression test: the raw offset saved by `switch_to_subagent_view`
    // drifts if the root conversation grows while the subagent view is
    // open (new messages/tool bubbles shift row indices). Resolving by
    // the drilled-into bubble's own id, once root is rendered again,
    // stays correct regardless.
    let mut state = AppState::new();
    let bubble_id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: bubble_id,
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id,
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    state.switch_to_subagent_view(run_id);
    // Simulate the root conversation growing while the subagent view is
    // open — must not affect which bubble is targeted.
    state.chat_entries.push(ChatEntry::AssistantMessage {
        text: "a concurrent root reply".to_string(),
        interrupted: false,
    });

    state.switch_to_root_view();

    assert_eq!(state.pending_scroll_to_bubble, Some(bubble_id));
    assert!(
        state.saved_root_scroll_bubble.is_none(),
        "must be consumed (taken), not just copied"
    );
}

#[test]
fn header_bubble_id_is_none_while_viewing_root() {
    let mut state = AppState::new();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: Uuid::new_v4(),
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });

    assert_eq!(state.header_bubble_id(), None);
}

#[test]
fn header_bubble_id_finds_the_root_spawn_subagent_bubble_while_viewing_subagent() {
    let mut state = AppState::new();
    let bubble_id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: bubble_id,
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id,
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    state.switch_to_subagent_view(run_id);

    assert_eq!(state.header_bubble_id(), Some(bubble_id));
}

#[test]
fn toggle_header_bubble_collapsed_flips_the_root_bubble_without_touching_chat_scroll_state() {
    let mut state = AppState::new();
    let bubble_id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: bubble_id,
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id,
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    state.switch_to_subagent_view(run_id);
    state.conversation.scroll_offset = 42;
    state.conversation.scroll_locked = true;

    state.toggle_header_bubble_collapsed();

    assert!(
        matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if *collapsed)
    );
    assert_eq!(
        state.conversation.scroll_offset, 42,
        "must not touch the subagent transcript's own scroll"
    );
    assert!(
        state.conversation.scroll_locked,
        "must not touch the subagent transcript's own scroll lock"
    );
}

#[test]
fn toggle_header_bubble_collapsed_is_a_no_op_while_viewing_root() {
    let mut state = AppState::new();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: Uuid::new_v4(),
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });

    state.toggle_header_bubble_collapsed();

    assert!(
        matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed)
    );
}

fn pending_tool_bubble(id: Uuid, name: &str) -> ChatEntry {
    ChatEntry::ToolBubble {
        id,
        name: name.to_string(),
        state: Box::new(ToolCallState::Pending {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
        }),
        collapsed: true,
    }
}

fn subagent_run_info(run_id: Uuid) -> SubagentRunInfo {
    SubagentRunInfo {
        run_id,
        bubble_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "task".to_string(),
        label: "Label".to_string(),
    }
}

#[test]
fn pending_approvals_are_offered_in_request_order_across_root_and_subagents() {
    let mut state = AppState::new();
    let first_run_id = Uuid::new_v4();
    let second_run_id = Uuid::new_v4();
    state.start_subagent_run(subagent_run_info(first_run_id));
    state.start_subagent_run(subagent_run_info(second_run_id));
    let root_tool_id = Uuid::new_v4();
    let first_subagent_tool_id = Uuid::new_v4();
    let second_subagent_tool_id = Uuid::new_v4();

    // Requested: second subagent, then root, then first subagent.
    for (tool_id, transcript_run_id) in [
        (second_subagent_tool_id, Some(second_run_id)),
        (root_tool_id, None),
        (first_subagent_tool_id, Some(first_run_id)),
    ] {
        match transcript_run_id {
            Some(run_id) => state
                .subagent_conversations
                .get_mut(&run_id)
                .expect("subagent transcript")
                .messages
                .push(pending_tool_bubble(tool_id, "bash")),
            None => state
                .chat_entries
                .push(pending_tool_bubble(tool_id, "bash")),
        }
        state.pending_approval_order.push(tool_id);
    }

    assert_eq!(
        state.pending_tool_approval().map(|(id, _)| id),
        Some(second_subagent_tool_id),
        "the earliest request is the head, regardless of transcript"
    );

    // Answering the head moves on to the next request.
    if let Some(ChatEntry::ToolBubble {
        state: tool_state, ..
    }) = state
        .subagent_conversations
        .get_mut(&second_run_id)
        .and_then(|conversation| conversation.messages.first_mut())
    {
        **tool_state = ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        };
    }
    state.prune_pending_approval_order();
    assert_eq!(
        state.pending_tool_approval().map(|(id, _)| id),
        Some(root_tool_id)
    );
    assert_eq!(state.pending_approval_order.len(), 2);
}

#[test]
fn two_running_subagents_each_keep_their_own_agent_mode() {
    let mut state = AppState::new();
    let first_run_id = Uuid::new_v4();
    let second_run_id = Uuid::new_v4();
    state.start_subagent_run(subagent_run_info(first_run_id));
    state.start_subagent_run(subagent_run_info(second_run_id));

    assert_eq!(
        state.agent_mode_for_subagent(first_run_id),
        AgentMode::Streaming
    );
    assert_eq!(
        state.agent_mode_for_subagent(second_run_id),
        AgentMode::Streaming
    );

    state.running_subagents.remove(&first_run_id);
    assert_eq!(state.agent_mode_for_subagent(first_run_id), AgentMode::Idle);
    assert_eq!(
        state.agent_mode_for_subagent(second_run_id),
        AgentMode::Streaming,
        "finishing one subagent leaves its sibling running"
    );
}
