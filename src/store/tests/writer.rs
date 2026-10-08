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

#[tokio::test]
async fn round_trip_writes_meta_header_then_events_in_order() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let session_id = Uuid::new_v4();
    let store = Store::create(
        &chats_root,
        temp.path(),
        session_id,
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("store created");

    store.record_message(&user_message("hello store"));
    store.record_message(&Message::Assistant {
        content: "hi there".to_string(),
        status: AssistantStatus::Complete,
        tool_calls: vec![],
    });
    store.flush().await;

    let events = read_events(store.path());
    assert_eq!(events.len(), 3, "meta + two messages: {events:?}");
    let StoreEvent::Meta(meta) = &events[0] else {
        panic!("first line must be the meta header, got {:?}", events[0]);
    };
    assert_eq!(meta.schema_version, SCHEMA_VERSION);
    assert_eq!(meta.session_id, session_id);
    assert_eq!(meta.project_key, project_key(temp.path()));
    assert_eq!(
        meta.title, "",
        "the store does not copy conversation text into the title"
    );
    assert!(matches!(
        &events[1],
        StoreEvent::Message { message: Message::User { content, .. }, .. } if content == "hello store"
    ));
    assert!(matches!(
        &events[2],
        StoreEvent::Message { message: Message::Assistant { content, .. }, .. } if content == "hi there"
    ));
}

#[tokio::test]
async fn a_user_message_is_durable_on_disk_before_any_reply_arrives() {
    // Every commit must be flushed to disk immediately, even though the
    // meta header's title is only finalized once the turn completes.
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let store = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("store created");

    store.record_message(&user_message("are you still there"));
    store.flush().await;

    assert!(
        store.title_prompt().is_none(),
        "a user message without a completed reply is not title-ready"
    );
    let events = read_events(store.path());
    assert_eq!(events.len(), 2, "meta + the user message: {events:?}");
    let StoreEvent::Meta(meta) = &events[0] else {
        panic!("first line must be the meta header, got {:?}", events[0]);
    };
    assert_eq!(meta.title, "", "title stays empty until the turn completes");
    assert!(matches!(
        &events[1],
        StoreEvent::Message { message: Message::User { content, .. }, .. }
            if content == "are you still there"
    ));
}

#[tokio::test]
async fn an_interrupted_assistant_reply_does_not_make_title_ready() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let store = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("store created");

    store.record_message(&user_message("unfinished question"));
    store.record_message(&Message::Assistant {
        content: "partial reply".to_string(),
        status: AssistantStatus::Interrupted,
        tool_calls: Vec::new(),
    });
    store.flush().await;

    assert!(
        store.title_prompt().is_none(),
        "interrupted replies must not trigger title generation"
    );
}

#[tokio::test]
async fn an_empty_tool_call_completion_does_not_hide_the_first_text_reply() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let store = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("store created");

    store.record_message(&user_message("count the files"));
    store.record_message(&Message::Assistant {
        content: String::new(),
        status: AssistantStatus::Complete,
        tool_calls: vec![ToolCallRequest {
            id: Uuid::new_v4(),
            name: "glob".to_string(),
            arguments: serde_json::json!({"pattern": "**/*"}),
            persisted_title_arguments: Vec::new(),
        }],
    });
    assert!(
        store.title_prompt().is_none(),
        "an assistant tool-call envelope is not the completed text reply"
    );

    let session_id = store.session_id();
    store.flush().await;
    drop(store);
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resumed = Store::resume(&chats_root, temp.path(), session_id).expect("resume succeeds");
    resumed.record_message(&assistant_message("There are 42 files."));
    let prompt = resumed
        .title_prompt()
        .expect("the first text reply should make the title ready");
    assert!(prompt.contains("<assistant_message>\nThere are 42 files."));
    assert!(!prompt.contains("<assistant_message>\n</assistant_message>"));
}

#[tokio::test]
async fn resuming_a_session_rebuilds_title_prompt_inputs() {
    // A prior run committed a user message and was killed before the
    // assistant replied: the title is still empty on disk. Resuming and
    // completing the turn should rebuild both prompt inputs from the
    // transcript rather than relying on in-memory state from the crashed
    // process.
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let session_id = Uuid::new_v4();
    let first_run = Store::create(
        &chats_root,
        temp.path(),
        session_id,
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("store created");
    first_run.record_message(&user_message("orphaned question"));
    first_run.flush().await;
    drop(first_run);
    // Give the writer task a chance to observe the dropped senders and
    // release the advisory lock before re-opening the same session.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resumed = Store::resume(&chats_root, temp.path(), session_id).expect("resume succeeds");
    resumed.record_message(&assistant_message("late reply"));
    resumed.flush().await;

    let prompt = resumed.title_prompt().expect("title prompt is ready");
    assert!(prompt.contains("<user_message>\norphaned question"));
    assert!(prompt.contains("<assistant_message>\nlate reply"));
    assert!(prompt.contains("Return only one bare title"));
    let events = read_events(resumed.path());
    let StoreEvent::Meta(meta) = &events[0] else {
        panic!("first line must be the meta header, got {:?}", events[0]);
    };
    assert_eq!(meta.title, "");
}

#[tokio::test]
async fn set_title_trims_and_bounds_only_the_metadata_line() {
    let long = "x".repeat(TITLE_MAX_CHARS * 2);
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let store = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("store created");
    store.record_message(&user_message("preserve this user message"));
    store.record_message(&assistant_message("preserve this assistant reply"));
    store.flush().await;
    let events_before_title = read_events(store.path());
    store
        .set_title(format!("  {long}\nignored "))
        .await
        .expect("title rewrite succeeds");
    let StoreEvent::Meta(meta) = &read_events(store.path())[0] else {
        panic!("first line must be the meta header")
    };
    assert_eq!(meta.title.chars().count(), TITLE_MAX_CHARS);
    assert!(meta.title.chars().all(|character| character == 'x'));
    let events_after_title = read_events(store.path());
    assert_eq!(
        events_after_title[1..],
        events_before_title[1..],
        "title rewrite must preserve every event after the metadata line"
    );
    let bytes_after_first_title = std::fs::read(store.path()).expect("session file readable");
    store
        .set_title("a different title".to_string())
        .await
        .expect("idempotent title rewrite succeeds");
    assert_eq!(
        std::fs::read(store.path()).expect("session file readable"),
        bytes_after_first_title,
        "a second title write must not alter the session file"
    );
}

#[tokio::test]
async fn record_model_changed_persists_every_switch_independent_of_meta() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    // Deliberately a different initial provider/model than the ones
    // switched to below, so the assertion can't pass by accident.
    let store = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "ollama",
        "llama3",
        None,
    )
    .expect("store created");

    store.record_model_changed("openrouter", "gpt-4o");
    store.record_model_changed("openrouter", "gpt-4o-mini");
    store.flush().await;

    let events = read_events(store.path());
    let StoreEvent::Meta(meta) = &events[0] else {
        panic!("first line must be the meta header")
    };
    assert_eq!(
        (meta.provider_connection.as_str(), meta.model.as_str()),
        ("ollama", "llama3"),
        "meta keeps the provider/model resolved at creation, never rewritten by later switches"
    );
    let changes: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            StoreEvent::ModelChanged {
                provider_connection,
                model,
                ..
            } => Some((provider_connection, model)),
            _ => None,
        })
        .collect();
    assert_eq!(
        changes,
        vec![
            (&"openrouter".to_string(), &"gpt-4o".to_string()),
            (&"openrouter".to_string(), &"gpt-4o-mini".to_string()),
        ],
        "every switch is recorded, in order, independent of `meta`: {events:?}"
    );
}

#[tokio::test]
async fn child_records_link_to_the_parent_session() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let parent_id = Uuid::new_v4();
    let store = Store::create(
        &chats_root,
        temp.path(),
        parent_id,
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("store created");
    let run_id = Uuid::new_v4();

    store.record_message(&user_message("delegate this"));
    store.record_child_start(
        run_id,
        Uuid::new_v4(),
        "research",
        "investigate the bug",
        "Investigate",
    );
    store.record_child_result(
        run_id,
        Uuid::new_v4(),
        "success",
        Some("found it".to_string()),
        vec!["src/agent.rs".to_string()],
        FinishResultOrigin::Agent,
    );
    store.flush().await;

    let events = read_events(store.path());
    let start = events
        .iter()
        .find(|event| matches!(event, StoreEvent::ChildStart { .. }))
        .expect("a child_start record");
    let StoreEvent::ChildStart {
        id,
        parent_session,
        agent_type,
        task,
        ..
    } = start
    else {
        unreachable!()
    };
    assert_eq!(*id, run_id);
    assert_eq!(*parent_session, parent_id);
    assert_eq!(agent_type, "research");
    assert_eq!(task, "investigate the bug");

    let result = events
        .iter()
        .find(|event| matches!(event, StoreEvent::ChildResult { .. }))
        .expect("a child_result record");
    let StoreEvent::ChildResult {
        id,
        parent_session,
        status,
        sources,
        origin,
        ..
    } = result
    else {
        unreachable!()
    };
    assert_eq!(*id, run_id);
    assert_eq!(*parent_session, parent_id);
    assert_eq!(status, "success");
    assert_eq!(sources, &["src/agent.rs".to_string()]);
    assert_eq!(*origin, FinishResultOrigin::Agent);
}
