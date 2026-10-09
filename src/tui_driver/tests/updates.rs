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

use super::super::updates::ui_update_to_domain_event;
use super::*;

#[tokio::test]
async fn replayed_child_restores_its_compaction_marker_on_first_drill_in() {
    assert_child_compaction_replay(false).await;
}

#[tokio::test]
async fn archived_child_restores_its_compaction_marker_and_reports_corrupt_replay() {
    assert_child_compaction_replay(true).await;
}

async fn assert_child_compaction_replay(archived: bool) {
    use crate::agent::{AssistantStatus, Message, UserMessageKind};
    use crate::tui_reducer_state::{ChildConversation, ChildRunInfo};

    let temp = tempfile::tempdir().expect("tempdir");
    let store = crate::store::Store::create(
        &temp.path().join("chats"),
        temp.path(),
        Uuid::new_v4(),
        "mock",
        "conversation",
        None,
    )
    .expect("parent store created");
    let run_id = Uuid::new_v4();
    let messages = vec![
        Message::User {
            content: "inspect the patch".to_string(),
            kind: UserMessageKind::Explicit,
        },
        Message::User {
            content: "hidden harness note".to_string(),
            kind: UserMessageKind::ImplicitToolNote,
        },
        Message::Assistant {
            content: "retained reply".to_string(),
            status: AssistantStatus::Complete,
            tool_calls: vec![],
        },
    ];
    let child_store = store
        .write_child_transcript_for_run_store(run_id, None, &messages)
        .await
        .expect("child transcript written");
    child_store.record_compaction(2, 1, "old summary");
    child_store.record_compaction(3, 1, "latest summary");
    child_store.flush().await;
    let child_path = child_store.path().to_path_buf();
    drop(child_store);
    tokio::time::sleep(Duration::from_millis(50)).await;
    if archived {
        let report = crate::Store::maintain_sessions(
            store.chats_root(),
            store.project_key(),
            &crate::store::SessionSelection::Ids(vec![run_id]),
            crate::store::MaintenanceAction::Compress,
            false,
        )
        .unwrap();
        assert!(report.failures.is_empty());
        assert!(!child_path.exists());
    }

    let mut state = AppState::new();
    state.chat_entries.push(ChatEntry::UserMessage {
        text: "root request".to_string(),
        state: UserMessageState::Sent,
    });
    let root_entries = state.chat_entries.clone();
    state.child_conversations.insert(
        run_id,
        ChildConversation::new_replay(ChildRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "research".to_string(),
            task: "inspect the patch".to_string(),
            label: "Inspect".to_string(),
        }),
    );
    assert!(state.child_conversations[&run_id].messages.is_empty());

    assert!(prepare_child_replay_effects(
        &mut state,
        vec![Effect::LoadChildTranscript { run_id }],
        Some(&store),
    )
    .is_empty());
    assert_eq!(
        state.child_conversations[&run_id].messages,
        vec![
            ChatEntry::UserMessage {
                text: "inspect the patch".to_string(),
                state: UserMessageState::Sent,
            },
            ChatEntry::CompactionMarker {
                summarized_messages: 2,
            },
            ChatEntry::AssistantMessage {
                text: "retained reply".to_string(),
                interrupted: false,
            },
        ],
        "the latest persisted cut uses transcript indices, not rendered entry indices"
    );
    assert_eq!(state.chat_entries, root_entries);
    assert!(!state.child_needs_lazy_load(run_id));
    let loaded_entries = state.child_conversations[&run_id].messages.clone();
    prepare_child_replay_effects(
        &mut state,
        vec![Effect::LoadChildTranscript { run_id }],
        Some(&store),
    );
    assert_eq!(state.child_conversations[&run_id].messages, loaded_entries);
    if archived {
        let info = state.child_conversations[&run_id].info.clone();
        state
            .child_conversations
            .insert(run_id, ChildConversation::new_replay(info));
        std::fs::write(child_path.with_extension("jsonl.gz"), b"corrupt").unwrap();
        prepare_child_replay_effects(
            &mut state,
            vec![Effect::LoadChildTranscript { run_id }],
            Some(&store),
        );
        assert!(state.child_needs_lazy_load(run_id));
        assert!(format!("{:?}", state.child_conversations[&run_id].messages)
            .contains("Could not load subagent transcript"));
    }
}

#[tokio::test]
async fn compaction_applied_update_inserts_the_transcript_marker() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    state.chat_entries.push(ChatEntry::UserMessage {
        text: "old turn".to_string(),
        state: UserMessageState::Sent,
    });
    state.chat_entries.push(ChatEntry::AssistantMessage {
        text: "old reply".to_string(),
        interrupted: false,
    });
    state.chat_entries.push(ChatEntry::UserMessage {
        text: "new turn".to_string(),
        state: UserMessageState::Sent,
    });

    let textarea_context = textarea_ctx_from(&textarea);
    let outcome = process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::CompactionApplied {
            run_id: None,
            entry_index: 2,
            summarized_messages: 2,
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;

    assert_eq!(outcome, ActionOutcome::Continue);
    assert_eq!(
        state.chat_entries[2],
        crate::tui_reducer_state::ChatEntry::CompactionMarker {
            summarized_messages: 2
        },
        "the marker lands where the tail begins: {:?}",
        state.chat_entries
    );
    assert!(matches!(
        &state.chat_entries[3],
        ChatEntry::UserMessage { text, .. } if text == "new turn"
    ));

    // A repeated emission of the same boundary must not stack markers.
    let repeat_context = textarea_ctx_from(&textarea);
    process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &repeat_context,
        &UiUpdate::CompactionApplied {
            run_id: None,
            entry_index: 2,
            summarized_messages: 2,
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(
        state
            .chat_entries
            .iter()
            .filter(|entry| matches!(entry, ChatEntry::CompactionMarker { .. }))
            .count(),
        1,
        "the same boundary is marked once"
    );
}

#[tokio::test]
async fn child_compaction_marker_does_not_enter_the_root_transcript() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let run_ids = [Uuid::new_v4(), Uuid::new_v4()];
    for run_id in run_ids {
        let mut conversation = crate::tui_reducer_state::ChildConversation::new(
            crate::tui_reducer_state::ChildRunInfo {
                run_id,
                bubble_id: Uuid::new_v4(),
                agent_type: "default".to_string(),
                task: "Inspect material".to_string(),
                label: "Inspect".to_string(),
            },
        );
        conversation.messages.push(ChatEntry::AssistantMessage {
            text: "retained reply".to_string(),
            interrupted: false,
        });
        state.child_conversations.insert(run_id, conversation);
    }
    let root_entries = state.chat_entries.clone();
    let textarea_context = textarea_ctx_from(&textarea);
    for active_view in [
        ConversationView::Root,
        ConversationView::Child {
            run_id: run_ids[0],
            agent_type: "default".to_string(),
            task: "Inspect material".to_string(),
            label: "Inspect".to_string(),
        },
    ] {
        state.active_view = active_view.clone();
        for run_id in run_ids {
            process_one_update(
                &mut state,
                &mut textarea,
                &cmd_tx,
                &textarea_context,
                &UiUpdate::CompactionApplied {
                    run_id: Some(run_id),
                    entry_index: 0,
                    summarized_messages: 15,
                },
                &mut MouseDrag::default(),
                &mut NoopClipboard,
                &mut PendingRunIds::default(),
            )
            .await;
        }
        assert_eq!(state.active_view, active_view);
    }

    assert_eq!(
        state.chat_entries, root_entries,
        "a forwarded child compaction must not add a root marker"
    );
    for run_id in run_ids {
        assert_eq!(
            state.child_conversations[&run_id].messages,
            vec![
                ChatEntry::CompactionMarker {
                    summarized_messages: 15,
                },
                ChatEntry::AssistantMessage {
                    text: "retained reply".to_string(),
                    interrupted: false,
                },
            ],
            "duplicate suppression is scoped to each child"
        );
    }

    process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::CompactionApplied {
            run_id: None,
            entry_index: 0,
            summarized_messages: 15,
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(
        state.chat_entries,
        vec![ChatEntry::CompactionMarker {
            summarized_messages: 15,
        }],
        "root compaction still routes to root while viewing a child"
    );
}

#[tokio::test]
async fn unknown_child_compaction_marker_does_not_enter_the_root_transcript() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let textarea_context = textarea_ctx_from(&textarea);
    process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::CompactionApplied {
            run_id: Some(Uuid::new_v4()),
            entry_index: 0,
            summarized_messages: 15,
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert!(state.chat_entries.is_empty());
    assert!(state.child_conversations.is_empty());
}

#[tokio::test]
async fn context_usage_update_stores_the_footer_readout() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    assert_eq!(state.context_usage, None, "no report before the first turn");

    let textarea_context = textarea_ctx_from(&textarea);
    let outcome = process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::ContextUsage {
            estimated_tokens: 64_200,
            context_length: 128_000,
            source: crate::tui_reducer_state::ContextUsageSource::SerializedRequest,
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;

    assert_eq!(outcome, ActionOutcome::Continue);
    assert_eq!(
        state.context_usage,
        Some(crate::tui_reducer_state::ContextUsage {
            estimated_tokens: 64_200,
            context_length: 128_000,
            source: crate::tui_reducer_state::ContextUsageSource::SerializedRequest,
        }),
        "the readout is stored as reported, not pre-formatted"
    );
    assert_eq!(
        crate::tui_reducer_state::format_context_usage(64_200, 128_000),
        "64.2K (50%)"
    );

    // A later report replaces the earlier one, in any mode.
    state.mode = TuiMode::Streaming;
    let streaming_context = textarea_ctx_from(&textarea);
    process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &streaming_context,
        &UiUpdate::ContextUsage {
            estimated_tokens: 1_000,
            context_length: 128_000,
            source: crate::tui_reducer_state::ContextUsageSource::SerializedRequest,
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(
        state.context_usage,
        Some(crate::tui_reducer_state::ContextUsage {
            estimated_tokens: 1_000,
            context_length: 128_000,
            source: crate::tui_reducer_state::ContextUsageSource::SerializedRequest,
        })
    );
    assert_eq!(state.mode, TuiMode::Streaming, "the mode is untouched");
}

#[test]
fn child_context_usage_update_projects_for_active_child_without_replacing_root() {
    let (mut state, _textarea, _cmd_tx, _cmd_rx) = setup();
    let run_id = uuid::Uuid::new_v4();
    state.start_child_run(crate::tui_reducer_state::ChildRunInfo {
        run_id,
        bubble_id: uuid::Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "Review the change".to_string(),
        label: "Review change".to_string(),
    });
    state.context_usage = Some(crate::tui_reducer_state::ContextUsage {
        estimated_tokens: 12_000,
        context_length: 128_000,
        source: crate::tui_reducer_state::ContextUsageSource::SerializedRequest,
    });
    state.switch_to_child_view(run_id);

    let event = ui_update_to_domain_event(&UiUpdate::ChildContextUsage {
        run_id,
        estimated_tokens: 4_000,
        context_length: 32_000,
        source: crate::tui_reducer_state::ContextUsageSource::ModelView,
    })
    .expect("child usage update maps to a domain event");
    presenter::update(
        &mut state,
        TuiEvent::Domain(event),
        &textarea_ctx_from(&TextArea::default()),
    );

    assert_eq!(
        state.context_usage,
        Some(crate::tui_reducer_state::ContextUsage {
            estimated_tokens: 12_000,
            context_length: 128_000,
            source: crate::tui_reducer_state::ContextUsageSource::SerializedRequest,
        }),
        "child usage must not replace the root readout"
    );
    assert_eq!(
        state.active_context_usage(),
        Some(crate::tui_reducer_state::ContextUsage {
            estimated_tokens: 4_000,
            context_length: 32_000,
            source: crate::tui_reducer_state::ContextUsageSource::ModelView,
        }),
        "the active child view selects its own readout"
    );
    assert_eq!(
        crate::tui_reducer_state::format_context_usage(4_000, 32_000),
        "4.0K (13%)"
    );
}

#[tokio::test]
async fn engine_notices_render_as_transcript_notices() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let textarea_context = textarea_ctx_from(&textarea);

    let outcome = process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::Notice {
            text: "Model mock/test-model does not advertise reasoning effort \"max\"; using the provider default.".to_string(),
            run_id: None,
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;

    assert_eq!(outcome, ActionOutcome::Continue);
    let Some(ChatEntry::Notice { text }) = state.chat_entries.last() else {
        panic!("expected a notice bubble, got {:?}", state.chat_entries);
    };
    assert!(
        text.contains("does not advertise reasoning effort"),
        "the notice must carry the engine's wording: {text}"
    );
}
