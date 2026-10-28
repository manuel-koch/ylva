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
fn harness_notice_projection_preserves_the_canonical_transcript() {
    let mut context = Context::new();
    context.push_async_harness_notice("full subagent result".to_string());
    let canonical = context.messages().to_vec();
    context.project_last_harness_notice("bounded subagent result".to_string());
    assert_eq!(context.messages(), canonical);
    assert_eq!(
        context.model_messages().last(),
        Some(&Message::User {
            content: format_prompt_block(
                PromptBlockKind::HarnessNotice,
                "Harness notice; not from the user.\nbounded subagent result",
            ),
            kind: UserMessageKind::ImplicitToolNote,
        })
    );
    assert!(context.projection_dirty);
}

#[test]
fn model_projections_restore_exactly_on_resume_and_clone_with_rebuilt_systems() {
    crate::test_support::run_locked(async {
        let directory = tempfile::tempdir().expect("directory");
        let chats = directory.path().join("chats");
        let session_id = Uuid::new_v4();
        let store = Store::create(&chats, directory.path(), session_id, "mock", "model", None)
            .expect("store");
        let mut context = Context::new();
        context.set_store(store.clone());
        context
            .push_implicit_user("[harness notice — not from the user]\nlegacy notice".to_string());
        context.push_user("keep this exact task".to_string());
        let id = Uuid::new_v4();
        context.push_assistant_tool_call(
            String::new(),
            AssistantStatus::Complete,
            vec![ToolCallRequest {
                id,
                name: "read".to_string(),
                arguments: Value::Null,
                persisted_title_arguments: Vec::new(),
            }],
        );
        context.push_tool_error(
            id,
            "read".to_string(),
            "long error ".repeat(4_000),
            Some(&"partial output ".repeat(4_000)),
            false,
            Duration::ZERO,
            Vec::new(),
        );
        let report = context.recover_model_view(2_000, 4_000, &[]);
        assert!(report.fits_ceiling);
        context.persist_model_projection();
        let boundary = context.model_view_start();
        context.limit_last_model_message(128);
        context.persist_model_projection();
        assert_eq!(context.model_view_start(), boundary);
        store.flush().await;
        let expected = context.model_messages()[context.leading_system_len()..].to_vec();
        let canonical = context.messages()[context.leading_system_len()..].to_vec();
        let state = Store::read_session_state(store.path()).expect("state");
        let mut stale = state.model_projection.expect("projection");
        assert_eq!(stale.projection_revision, 2);
        stale.projection_revision = 1;
        stale.transcript_start += 100;
        store.record_model_projection(stale);
        store.flush().await;
        drop(context);
        drop(store);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let resumed = Store::resume(&chats, directory.path(), session_id).expect("resume");
        let mut replay = Context::new();
        replay.push_system("additional rebuilt instructions".to_string());
        replay.set_store(resumed.clone());
        assert_eq!(
            &replay.model_messages()[replay.leading_system_len()..],
            expected
        );
        assert_eq!(&replay.messages()[replay.leading_system_len()..], canonical);
        assert!(matches!(
            &replay.messages()[replay.leading_system_len()],
            Message::User { content, .. }
                if content == "[harness notice — not from the user]\nlegacy notice"
        ));
        drop(replay);
        drop(resumed);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let cloned =
            Store::clone_session(&chats, directory.path(), session_id, None).expect("clone");
        let mut clone_context = Context::new();
        clone_context.set_store(cloned);
        assert_eq!(
            &clone_context.model_messages()[clone_context.leading_system_len()..],
            expected
        );
        assert_eq!(
            &clone_context.messages()[clone_context.leading_system_len()..],
            canonical
        );
    });
}

#[test]
fn invalid_projections_and_late_summaries_cannot_resurrect_or_break_tool_groups() {
    let mut context = Context::new();
    context.push_user("continue the exact task".to_string());
    let id = Uuid::new_v4();
    context.push_assistant_tool_call(
        String::new(),
        AssistantStatus::Complete,
        vec![ToolCallRequest {
            id,
            name: "read".to_string(),
            arguments: Value::Null,
            persisted_title_arguments: Vec::new(),
        }],
    );
    context.push_tool_error(
        id,
        "read".to_string(),
        "failure ".repeat(4_000),
        None,
        false,
        Duration::ZERO,
        Vec::new(),
    );
    let original = context.model_messages();
    context.restore_model_projection(compaction::ModelProjectionSnapshot {
        projection_revision: 9,
        transcript_start: 0,
        message_overrides: vec![(
            2,
            Message::ToolError {
                id: Uuid::new_v4(),
                name: "read".to_string(),
                error: "wrong correlation".to_string(),
                duration: Duration::ZERO,
                content_sections: Vec::new(),
            },
        )],
        summary_override: None,
        handoff: None,
        anchor: None,
    });
    assert_eq!(context.model_messages(), original);
    assert_eq!(context.projection_revision, 9);
    context.restore_model_projection(compaction::ModelProjectionSnapshot {
        projection_revision: 10,
        transcript_start: 0,
        message_overrides: Vec::new(),
        summary_override: None,
        handoff: Some(Message::System {
            content: "invalid recovery".to_string(),
        }),
        anchor: None,
    });
    assert_eq!(context.model_messages(), original);
    let boundary = context.leading_system_len();
    assert!(context.recover_model_view(1_000, 2_000, &[]).fits_ceiling);
    let recovered = context.model_messages();
    assert!(!context.set_compaction(compaction::CompactionSnapshot {
        boundary,
        summary: "late, obsolete summary".to_string(),
    }));
    assert_eq!(context.model_messages(), recovered);
    assert!(orphaned_tool_call_ids(&recovered).is_empty());
}

#[tokio::test]
async fn resuming_restores_the_latest_compaction_without_rewriting_the_session() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let chats_root = temporary_directory.path().join("chats");
    let session_id = Uuid::new_v4();
    let store = crate::store::Store::create(
        &chats_root,
        temporary_directory.path(),
        session_id,
        "ollama",
        "llama3",
        None,
    )
    .expect("store created");
    store.record_message(&Message::User {
        content: "first turn".to_string(),
        kind: UserMessageKind::Explicit,
    });
    store.record_message(&Message::Assistant {
        content: "first answer".to_string(),
        status: AssistantStatus::Complete,
        tool_calls: Vec::new(),
    });
    store.record_message(&Message::User {
        content: "second turn".to_string(),
        kind: UserMessageKind::Explicit,
    });
    // Absolute transcript index: the two rebuilt system prompts occupy
    // `[..2]` in the live context and again after a resume.
    store.record_compaction(4, 2, "first turn, summarized");
    store.flush().await;
    let path = store.path().to_path_buf();
    let before = std::fs::read(&path).expect("session file readable");

    let mut context = Context::new();
    context.set_store(store.clone());
    store.flush().await;
    let after = std::fs::read(&path).expect("session file readable");
    assert_eq!(
        before, after,
        "hydration plus snapshot restore must not append a second event"
    );

    assert_eq!(context.messages().len(), 5, "2 system + 3 stored messages");
    let snapshot = context.compaction().expect("snapshot restored");
    assert_eq!(snapshot.boundary, 4);
    assert_eq!(snapshot.summary, "first turn, summarized");

    let model_messages = context.model_messages();
    let leading_system_len = context.leading_system_len();
    assert_eq!(
        model_messages.len(),
        leading_system_len + 2,
        "system block + restored summary + tail containing the latest task"
    );
    assert!(
        matches!(
            &model_messages[leading_system_len],
            Message::User { content, .. }
                if content.starts_with(compaction::COMPACTION_SUMMARY_MARKER)
        ),
        "the restored summary keeps the provider-safe projection: {model_messages:?}"
    );
    assert_eq!(
        &model_messages[leading_system_len + 1..],
        &context.messages()[4..],
        "the tail after the restored boundary is verbatim"
    );
    assert!(
        !model_messages.iter().any(|message| matches!(
            message,
            Message::User { content, .. } if content == "first turn"
        )),
        "the summarized prefix stays out of the model view"
    );
    assert!(
        context.messages().iter().any(|message| matches!(
            message,
            Message::User { content, .. } if content == "first turn"
        )),
        "the transcript keeps every original message"
    );
}

#[tokio::test]
async fn resuming_a_snapshot_never_splits_a_tool_call_from_its_result() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let chats_root = temporary_directory.path().join("chats");
    let session_id = Uuid::new_v4();
    let store = crate::store::Store::create(
        &chats_root,
        temporary_directory.path(),
        session_id,
        "ollama",
        "llama3",
        None,
    )
    .expect("store created");
    let call_id = Uuid::new_v4();
    store.record_message(&Message::User {
        content: "do the work".to_string(),
        kind: UserMessageKind::Explicit,
    });
    store.record_message(&Message::Assistant {
        content: String::new(),
        status: AssistantStatus::Complete,
        tool_calls: vec![ToolCallRequest {
            id: call_id,
            name: "read".to_string(),
            arguments: Value::Null,
            persisted_title_arguments: Vec::new(),
        }],
    });
    store.record_message(&Message::ToolResult {
        value: ToolResult {
            id: call_id,
            name: "read".to_string(),
            result: "file contents".to_string(),
            attachments: vec![],
            result_section_id: None,
            result_metadata: None,
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: Duration::ZERO,
        },
    });
    store.record_message(&Message::User {
        content: "second turn".to_string(),
        kind: UserMessageKind::Explicit,
    });
    // Deliberately splits the round: the assistant tool call lands in the
    // summarized prefix while its result stays in the tail.
    store.record_compaction(4, 2, "the tool round, summarized");
    store.flush().await;

    let mut context = Context::new();
    context.set_store(store.clone());
    store.flush().await;

    let snapshot = context.compaction().expect("snapshot restored");
    assert_eq!(
        snapshot.boundary, 5,
        "the restored boundary snaps forward past the tool call's result"
    );
    let model_messages = context.model_messages();
    assert_eq!(
        orphaned_tool_call_ids(&model_messages),
        Vec::<Uuid>::new(),
        "the model view never carries a call without its result: {model_messages:?}"
    );
    assert_eq!(
        orphaned_tool_call_ids(context.messages()),
        Vec::<Uuid>::new(),
        "nor does the transcript"
    );
    assert_eq!(
        model_messages.last(),
        context.messages().last(),
        "the verbatim tail keeps the final turn"
    );
}
