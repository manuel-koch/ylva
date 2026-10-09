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
use crate::agent::{AssistantStatus, Message, ToolCallRequest};

mod archive;
mod event;
mod maintenance;
mod replay;
mod session_file;
mod writer;

#[tokio::test]
async fn title_rewrite_retains_the_writer_lock_and_append_destination_for_every_handle() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let chats_root = temporary_directory.path().join("chats");
    let session_id = Uuid::new_v4();
    let store = Store::create(
        &chats_root,
        temporary_directory.path(),
        session_id,
        "ollama",
        "model",
        None,
    )
    .expect("store created");
    let other_handle = store.clone();
    store.record_message(&user_message("question"));
    other_handle.record_message(&assistant_message("answer"));
    store.flush().await;
    let original_bytes = std::fs::read(store.path()).expect("session readable");
    let original_tail = original_bytes
        .splitn(2, |byte| *byte == b'\n')
        .nth(1)
        .expect("metadata newline");

    other_handle
        .set_title("Shared title".to_string())
        .await
        .expect("title persisted");
    assert_eq!(store.title(), "Shared title");
    assert!(store.title_prompt().is_none());
    assert!(matches!(
        Store::resume(&chats_root, temporary_directory.path(), session_id),
        Err(StoreError::SessionLocked { session_id: locked }) if locked == session_id
    ));
    let rewritten_bytes = std::fs::read(store.path()).expect("rewritten session readable");
    assert_eq!(
        rewritten_bytes
            .splitn(2, |byte| *byte == b'\n')
            .nth(1)
            .expect("metadata newline"),
        original_tail
    );

    drop(store);
    other_handle.record_message(&user_message("after rewrite"));
    other_handle.flush().await;
    assert_eq!(
        Store::read_messages(other_handle.path()).expect("messages readable"),
        vec![
            user_message("question"),
            assistant_message("answer"),
            user_message("after rewrite"),
        ]
    );
}

#[tokio::test]
async fn dropping_the_last_handle_drains_queued_events_before_releasing_the_session() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let chats_root = temporary_directory.path().join("chats");
    let session_id = Uuid::new_v4();
    let store = Store::create(
        &chats_root,
        temporary_directory.path(),
        session_id,
        "ollama",
        "model",
        None,
    )
    .expect("store created");
    let other_handle = store.clone();
    store.record_message(&user_message("first"));
    drop(store);
    other_handle.record_message(&user_message("second"));
    let session_path = other_handle.path().to_path_buf();
    drop(other_handle);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resumed = Store::resume(&chats_root, temporary_directory.path(), session_id)
        .expect("writer drained and released its lock");
    assert_eq!(
        Store::read_messages(&session_path).expect("messages readable"),
        vec![user_message("first"), user_message("second")]
    );
    resumed.record_message(&user_message("third"));
    resumed.flush().await;
    assert_eq!(
        Store::read_messages(&session_path).expect("messages readable"),
        vec![
            user_message("first"),
            user_message("second"),
            user_message("third")
        ]
    );
}

fn read_events(path: &Path) -> Vec<StoreEvent> {
    std::fs::read_to_string(path)
        .expect("session file readable")
        .lines()
        .map(|line| serde_json::from_str::<StoreEvent>(line).expect("parseable store event"))
        .collect()
}

fn user_message(text: &str) -> Message {
    Message::User {
        content: text.to_string(),
        kind: UserMessageKind::Explicit,
    }
}

fn assistant_message(text: &str) -> Message {
    Message::Assistant {
        content: text.to_string(),
        status: AssistantStatus::Complete,
        tool_calls: Vec::new(),
    }
}

#[tokio::test]
async fn context_forwards_every_committed_message_exactly_once() {
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

    let mut context = crate::agent::Context::new();
    context.set_store(store.clone());
    context.push_user("write a haiku".to_string());
    context.push_assistant("a haiku".to_string(), AssistantStatus::Complete);
    store.flush().await;

    let events = read_events(store.path());
    let messages: Vec<&Message> = events
        .iter()
        .filter_map(|event| match event {
            StoreEvent::Message { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        messages.len(),
        2,
        "the two system prompts installed by Context::new are not persisted, and each \
         committed message is forwarded exactly once: {messages:?}"
    );
    assert!(matches!(
        messages[0],
        Message::User { content, .. } if content == "write a haiku"
    ));
    assert!(matches!(
        messages[1],
        Message::Assistant { content, status: AssistantStatus::Complete, .. }
            if content == "a haiku"
    ));
}

#[tokio::test]
async fn every_committed_message_carries_a_millisecond_precision_timestamp() {
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

    let mut context = crate::agent::Context::new();
    context.set_store(store.clone());
    context.push_user("write a haiku".to_string());
    context.push_assistant("a haiku".to_string(), AssistantStatus::Complete);
    store.flush().await;

    let events = read_events(store.path());
    let timestamps: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            StoreEvent::Message { timestamp, .. } => Some(timestamp.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(timestamps.len(), 2, "one timestamp per committed message");
    for timestamp in timestamps {
        assert!(
            chrono::DateTime::parse_from_rfc3339(timestamp).is_ok(),
            "expected an RFC 3339 timestamp, got {timestamp:?}"
        );
        assert!(
            timestamp.contains('.'),
            "expected millisecond precision (a fractional-seconds part), got {timestamp:?}"
        );
    }
}

#[test]
fn project_key_is_stable_for_one_directory_and_differs_across_directories() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .expect("subscriber mutex poisoned");
    let temp = tempfile::tempdir().expect("tempdir");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    std::fs::create_dir_all(&first).expect("mkdir");
    std::fs::create_dir_all(&second).expect("mkdir");

    assert_eq!(project_key(&first), project_key(&first));
    assert_ne!(project_key(&first), project_key(&second));
    assert_eq!(project_key(&first).len(), 64, "sha-256 hex digest");
}

#[cfg(unix)]
#[test]
fn project_key_is_identical_for_two_symlinks_to_the_same_directory() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .expect("subscriber mutex poisoned");
    let temp = tempfile::tempdir().expect("tempdir");
    let real = temp.path().join("real");
    let other = temp.path().join("other");
    std::fs::create_dir_all(&real).expect("mkdir");
    std::fs::create_dir_all(&other).expect("mkdir");
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    assert_eq!(
        project_key(&real),
        project_key(&link),
        "a symlink and its target are one physical directory"
    );
    assert_ne!(project_key(&real), project_key(&other));
}

#[tokio::test]
async fn child_transcript_is_written_to_its_own_session_file() {
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
    let context_messages = vec![
        Message::System {
            content: "SOUL".to_string(),
        },
        Message::System {
            content: "tools schema".to_string(),
        },
        Message::System {
            content: "child briefing".to_string(),
        },
        user_message("summarize the module"),
        assistant_message("done"),
    ];

    let child_session_id = store
        .write_child_transcript(Some(("ollama", "llama3")), &context_messages)
        .await
        .expect("child transcript written");

    let child_path = session_path(&chats_root, store.project_key(), child_session_id);
    let events = read_events(&child_path);
    let StoreEvent::Meta(meta) = &events[0] else {
        panic!("child transcript needs its own meta header")
    };
    assert_eq!(
        meta.title, "",
        "child transcript titles are generated by the driver, not copied by the store"
    );
    assert_eq!(meta.provider_connection, "ollama");
    assert_eq!(meta.model, "llama3");
    assert_eq!(
        meta.parent_session,
        Some(store.session_id()),
        "the child's own meta header names its parent, mirroring the \
         parent-side child_start/child_result records"
    );
    let StoreEvent::ModelChanged {
        provider_connection,
        model,
        ..
    } = &events[1]
    else {
        panic!("second line must record the child's initial provider/model: {events:?}")
    };
    assert_eq!(provider_connection, "ollama");
    assert_eq!(model, "llama3");
    let persisted_messages: Vec<Message> = events
        .into_iter()
        .filter_map(|event| match event {
            StoreEvent::Message { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(persisted_messages, context_messages);
}

#[tokio::test]
async fn child_transcript_does_not_duplicate_task_already_in_context() {
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
    let run_id = Uuid::new_v4();
    let task = "summarize the module";
    let context_messages = vec![
        Message::System {
            content: "SOUL".to_string(),
        },
        Message::System {
            content: "tools schema".to_string(),
        },
        Message::System {
            content: "child briefing".to_string(),
        },
        user_message(task),
        assistant_message("done"),
    ];

    let child_session_id = store
        .write_child_transcript_for_run(run_id, Some(("ollama", "llama3")), &context_messages)
        .await
        .expect("child transcript written");

    let child_path = session_path(&chats_root, store.project_key(), child_session_id);
    let persisted_messages: Vec<Message> = read_events(&child_path)
        .into_iter()
        .filter_map(|event| match event {
            StoreEvent::Message { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        persisted_messages, context_messages,
        "the child transcript must preserve the Context message order without \
         prepending a duplicate task"
    );
}

#[tokio::test]
async fn a_clear_rollovers_meta_header_has_no_parent_session_either() {
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
    store.record_message(&user_message("hi"));
    store.flush().await;

    // `/clear` rolls over to a fresh session but is not a child run.
    let rolled_over = store
        .start_new_session("openrouter", "gpt-4o", None, None)
        .expect("rollover session created");
    rolled_over.record_message(&user_message("hi again"));
    rolled_over.flush().await;

    let StoreEvent::Meta(meta) = &read_events(rolled_over.path())[0] else {
        panic!("first line must be the meta header")
    };
    assert_eq!(meta.parent_session, None);
}

#[tokio::test]
async fn cloning_replays_the_source_into_a_new_session_and_leaves_the_source_untouched() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let source_id = Uuid::new_v4();
    let source = Store::create(
        &chats_root,
        temp.path(),
        source_id,
        "openrouter",
        "gpt-4o",
        Some("explorer"),
    )
    .expect("source created");
    source.record_message(&user_message("first turn"));
    source.record_message(&assistant_message("first reply"));
    source.record_model_changed("ollama", "llama3.2");
    let run_id = Uuid::new_v4();
    let call_id = Uuid::new_v4();
    source.record_child_start(run_id, call_id, "research", "investigate", "Investigate");
    source.record_child_result(
        run_id,
        call_id,
        "success",
        Some("found it".to_string()),
        vec!["src/store.rs".to_string()],
        FinishResultOrigin::Agent,
    );
    source
        .set_title("Copied title".to_string())
        .await
        .expect("title set");
    source.flush().await;
    let source_path = source.path().to_path_buf();
    let source_bytes_before = std::fs::read(&source_path).expect("source readable");
    drop(source);
    // Dropping the store only closes the writer task asynchronously; the
    // clone's exclusive nonblock lock must wait for that file handle.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let clone = Store::clone_session(&chats_root, temp.path(), source_id, Some("default"))
        .expect("clone created");
    clone.flush().await;
    let clone_id = clone.session_id();
    let clone_path = clone.path().to_path_buf();

    assert_ne!(clone_id, source_id, "the clone gets a fresh session id");
    assert_eq!(
        std::fs::read(&source_path).expect("source readable"),
        source_bytes_before,
        "the source file stays byte-for-byte unchanged"
    );

    let clone_meta = Store::read_session_meta(&clone_path)
        .expect("clone meta readable")
        .expect("clone meta present");
    assert_eq!(clone_meta.session_id, clone_id);
    assert_eq!(
        clone_meta.cloned_from,
        Some(source_id),
        "the clone names its source"
    );
    assert_eq!(
        clone_meta.parent_session, None,
        "a clone is a top-level session, not a child of the source"
    );
    assert_eq!(clone_meta.provider_connection, "openrouter");
    assert_eq!(clone_meta.model, "gpt-4o");
    assert_eq!(
        clone_meta.title, "Copied title",
        "the source title is carried over"
    );
    assert_eq!(
        clone_meta.agent_type.as_deref(),
        Some("default"),
        "an explicit --agent applies to the clone"
    );

    let state = Store::read_session_state(&clone_path).expect("clone state readable");
    assert_eq!(
        state.messages,
        vec![user_message("first turn"), assistant_message("first reply")],
        "the clone replays the full transcript"
    );

    let clone_events = read_events(&clone_path);
    assert!(
        !clone_events
            .iter()
            .any(|event| matches!(event, StoreEvent::Meta(meta) if meta.session_id == source_id)),
        "the source header is not copied: {clone_events:?}"
    );
    let child_start = clone_events
        .iter()
        .find_map(|event| match event {
            StoreEvent::ChildStart {
                id,
                parent_session,
                call_id: copied_call_id,
                ..
            } => Some((*id, *parent_session, *copied_call_id)),
            _ => None,
        })
        .expect("child_start copied");
    assert_eq!(child_start.0, run_id, "the child run id is preserved");
    assert_eq!(
        child_start.1, clone_id,
        "the lifecycle record is rebased onto the clone"
    );
    assert_eq!(child_start.2, call_id, "the spawn call id is preserved");
    let child_result_parent = clone_events
        .iter()
        .find_map(|event| match event {
            StoreEvent::ChildResult {
                id, parent_session, ..
            } => Some((*id, *parent_session)),
            _ => None,
        })
        .expect("child_result copied");
    assert_eq!(child_result_parent, (run_id, clone_id));

    // New writes land only on the clone.
    clone.record_message(&user_message("second turn"));
    clone.flush().await;
    assert_eq!(
        Store::read_messages(&clone_path)
            .expect("clone messages")
            .len(),
        3
    );
    assert_eq!(
        Store::read_messages(&source_path)
            .expect("source messages")
            .len(),
        2,
        "the source transcript gains nothing from the clone run"
    );
}

#[tokio::test]
async fn cloning_a_child_transcript_is_rejected_without_creating_a_session() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let source_id = Uuid::new_v4();
    let source = Store::create(
        &chats_root,
        temp.path(),
        source_id,
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("source created");
    let child_id = source
        .write_child_transcript(None, &[user_message("child work")])
        .await
        .expect("child transcript written");
    drop(source);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let result = Store::clone_session(&chats_root, temp.path(), child_id, None);

    assert!(
        matches!(
            result,
            Err(StoreError::SessionIsChild { session_id, .. }) if session_id == child_id
        ),
        "a child transcript must not be cloned, got {result:?}"
    );
    let project_directory = project_directory(&chats_root, &project_key(temp.path()));
    let session_files = std::fs::read_dir(&project_directory)
        .expect("project directory readable")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .count();
    assert_eq!(
        session_files, 2,
        "the source and its child transcript are the only session files"
    );
}

#[tokio::test]
async fn cloning_a_locked_source_fails_without_creating_a_session() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let source_id = Uuid::new_v4();
    let source = Store::create(
        &chats_root,
        temp.path(),
        source_id,
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("source created");
    source.record_message(&user_message("live session"));
    source.flush().await;

    let result = Store::clone_session(&chats_root, temp.path(), source_id, None);

    assert!(
        matches!(
            result,
            Err(StoreError::SessionLocked { session_id }) if session_id == source_id
        ),
        "a source held open by another handle must fail the clone, got {result:?}"
    );
    let project_directory = project_directory(&chats_root, &project_key(temp.path()));
    let session_files = std::fs::read_dir(&project_directory)
        .expect("project directory readable")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .count();
    assert_eq!(session_files, 1, "no clone file was created");
}

#[tokio::test]
async fn cloning_an_unknown_source_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let missing = Uuid::new_v4();

    let result = Store::clone_session(&chats_root, temp.path(), missing, None);

    assert!(
        matches!(
            result,
            Err(StoreError::SessionNotFound { session_id, .. }) if session_id == missing
        ),
        "cloning an unknown id must not start a fresh session, got {result:?}"
    );
    assert!(!session_path(&chats_root, &project_key(temp.path()), missing).exists());
}
