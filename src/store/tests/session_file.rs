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
async fn create_takes_an_exclusive_lock_that_conflicting_opens_report() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let session_id = Uuid::new_v4();
    let first = Store::create(
        &chats_root,
        temp.path(),
        session_id,
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("first open succeeds");
    first.record_message(&user_message("first"));
    first.flush().await;

    let conflict = Store::resume(&chats_root, temp.path(), session_id);
    assert!(
        matches!(conflict, Err(StoreError::SessionLocked { session_id: locked }) if locked == session_id),
        "second open must fail fast with a lock conflict, got {conflict:?}"
    );
}

#[tokio::test]
async fn resume_of_unknown_session_id_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let missing = Uuid::new_v4();

    let result = Store::resume(&chats_root, temp.path(), missing);

    assert!(
        matches!(result, Err(StoreError::SessionNotFound { session_id, .. }) if session_id == missing),
        "resuming an unknown id must not create a new session, got {result:?}"
    );
    assert!(!session_path(&chats_root, &project_key(temp.path()), missing).exists());
}

#[tokio::test]
async fn resume_appends_to_the_existing_file_without_overwriting_it() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let session_id = Uuid::new_v4();
    {
        let store = Store::create(
            &chats_root,
            temp.path(),
            session_id,
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("store created");
        store.record_message(&user_message("first turn"));
        store.flush().await;
    }
    // Give the writer task a chance to observe the dropped senders and
    // release the advisory lock before re-opening the same session.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resumed = Store::resume(&chats_root, temp.path(), session_id).expect("resume succeeds");
    resumed.record_message(&user_message("second turn"));
    resumed.flush().await;

    let events = read_events(resumed.path());
    assert_eq!(events.len(), 3, "meta + both turns: {events:?}");
    assert!(matches!(&events[0], StoreEvent::Meta(_)));
    assert!(matches!(
        &events[1],
        StoreEvent::Message { message: Message::User { content, .. }, .. } if content == "first turn"
    ));
    assert!(matches!(
        &events[2],
        StoreEvent::Message { message: Message::User { content, .. }, .. } if content == "second turn"
    ));
}

#[tokio::test]
async fn resume_truncates_a_corrupt_trailing_line_before_appending() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let session_id = Uuid::new_v4();
    {
        let store = Store::create(
            &chats_root,
            temp.path(),
            session_id,
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("store created");
        store.record_message(&user_message("first turn"));
        store.flush().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let path = session_path(&chats_root, &project_key(temp.path()), session_id);
    let mut corrupted = std::fs::read_to_string(&path).expect("read");
    corrupted.push_str("{\"type\":\"message\",\"v\":1,\"mess");
    std::fs::write(&path, &corrupted).expect("write corrupt tail");

    let resumed =
        Store::resume(&chats_root, temp.path(), session_id).expect("resume repairs the tail");
    resumed.record_message(&user_message("after repair"));
    resumed.flush().await;

    let events = read_events(&path);
    assert_eq!(events.len(), 3, "corrupt tail dropped: {events:?}");
    assert!(matches!(
        &events[2],
        StoreEvent::Message { message: Message::User { content, .. }, .. } if content == "after repair"
    ));
}

#[tokio::test]
async fn a_session_without_events_still_gets_its_meta_header() {
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
    let path = store.path().to_path_buf();
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = read_events(&path);
    assert!(matches!(events.as_slice(), [StoreEvent::Meta(_)]));
}

#[cfg(unix)]
#[tokio::test]
async fn created_session_files_and_directories_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mode_of = |path: &Path| {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
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
    store.record_message(&user_message("secret"));
    store.flush().await;

    let session_path = store.path().to_path_buf();
    let project_directory = session_path.parent().expect("project directory");
    assert_eq!(mode_of(&session_path), 0o600, "session file");
    assert_eq!(
        mode_of(&project_directory.join(format!("{session_id}.lock"))),
        0o600,
        "identity lock file"
    );
    assert_eq!(
        mode_of(&project_directory.join(".project.lock")),
        0o600,
        "project lock file"
    );
    assert_eq!(mode_of(project_directory), 0o700, "project directory");
    assert_eq!(mode_of(&chats_root), 0o700, "chats root");
}
