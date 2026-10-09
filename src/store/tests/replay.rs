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
async fn last_use_takes_the_latest_top_level_timestamp_by_instant_not_line_order() {
    let (_directory, _chats_root, _identity, path, _bytes) = super::archive::fixture().await;
    let mut metadata = Store::read_session_meta(&path).unwrap().unwrap();
    metadata.created_at = "2020-01-01T00:00:00Z".into();
    let header = serde_json::to_string(&StoreEvent::Meta(metadata.clone())).unwrap();
    let derive = |events: &[serde_json::Value]| {
        let mut content = format!("{header}\n");
        for event in events {
            content.push_str(&serde_json::to_string(event).unwrap());
            content.push('\n');
        }
        crate::store::replay::last_used_at(content.as_bytes(), &metadata, &path)
    };
    for field in [
        "timestamp",
        "created_at",
        "changed_at",
        "completed_at",
        "compacted_at",
    ] {
        let mut event = serde_json::json!({"type": "future_event"});
        event[field] = serde_json::json!("2026-01-09T08:30:00Z");
        assert_eq!(derive(&[event]).unwrap(), "2026-01-09T08:30:00Z", "{field}");
    }
    assert_eq!(
        derive(&[
            serde_json::json!({"timestamp": "2026-01-09T10:00:00+02:00"}),
            serde_json::json!({"created_at": "2026-01-09T08:30:00Z"}),
            serde_json::json!({"changed_at": "2026-01-09T08:00:00Z"}),
            serde_json::json!({"completed_at": "2026-01-09T11:00:00+02:00"}),
            serde_json::json!({"compacted_at": "2026-01-09T08:59:59Z"}),
        ])
        .unwrap(),
        "2026-01-09T11:00:00+02:00"
    );
    for event in [
        serde_json::json!({"type": "message", "message": {"content": "2099-01-01T00:00:00Z", "timestamp": "2099-01-01T00:00:00Z"}}),
        serde_json::json!({"type": "message", "timestamp": ""}),
        serde_json::json!({"type": "model_projection", "projection": {}}),
        serde_json::json!({"timestamp": "2019-12-31T23:00:00-01:00"}),
    ] {
        assert_eq!(derive(&[event]).unwrap(), metadata.created_at);
    }
    for invalid in [
        serde_json::json!({"timestamp": "invalid"}),
        serde_json::json!({"changed_at": 42}),
    ] {
        assert!(derive(&[invalid]).is_err());
    }
}

#[tokio::test]
async fn invalid_activity_timestamps_are_diagnosed_and_block_age_based_deletion() {
    let (directory, chats_root, _, path, _) = super::archive::fixture().await;
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"type\":\"future_event\",\"timestamp\":\"invalid\"}\n");
    std::fs::write(&path, &bytes).unwrap();
    let key = project_key(directory.path());
    let (summaries, diagnostics) =
        Store::list_sessions_with_diagnostics(&chats_root, &key).unwrap();
    assert!(summaries.is_empty());
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].contains("invalid activity timestamp in 'timestamp'"));
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &crate::store::SessionSelection::Before(chrono::Utc::now()),
        crate::store::MaintenanceAction::Delete,
        false,
    )
    .unwrap();
    assert_eq!(report.failures.len(), 2);
    assert!(report.paths.is_empty());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[tokio::test]
async fn read_only_listing_never_mutates_a_file_with_a_corrupt_tail() {
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
        store.record_message(&user_message("only turn"));
        store.flush().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let path = session_path(&chats_root, &project_key(temp.path()), session_id);
    let mut corrupted = std::fs::read_to_string(&path).expect("read");
    corrupted.push_str("{\"type\":\"mes");
    std::fs::write(&path, &corrupted).expect("write corrupt tail");
    let before = std::fs::read_to_string(&path).expect("read");

    let summaries =
        Store::list_sessions(&chats_root, &project_key(temp.path())).expect("listing succeeds");
    let messages = Store::read_messages(&path).expect("messages readable");

    assert_eq!(summaries.len(), 1);
    assert_eq!(messages.len(), 1, "corrupt tail skipped in memory");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        before,
        "a pure read must never mutate the session file"
    );
}

#[tokio::test]
async fn list_sessions_returns_newest_first_and_skips_headerless_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let key = project_key(temp.path());

    let older = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("older session");
    older.record_message(&user_message("older question"));
    older.record_message(&assistant_message("older reply"));
    older.flush().await;
    let newer = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "ollama",
        "llama3",
        None,
    )
    .expect("newer session");
    newer.record_message(&user_message("newer question"));
    newer.record_message(&assistant_message("newer reply"));
    newer.flush().await;
    // Force a deterministic ordering independent of the one-second
    // timestamp resolution.
    rewrite_meta_created_at(older.path(), "2020-01-01T00:00:00Z");
    rewrite_meta_created_at(newer.path(), "2030-01-01T00:00:00Z");
    std::fs::write(
        project_directory(&chats_root, &key).join("garbage.jsonl"),
        "not json at all\n",
    )
    .expect("write garbage file");

    let summaries = Store::list_sessions(&chats_root, &key).expect("listing succeeds");

    assert_eq!(summaries.len(), 2, "unreadable file skipped: {summaries:?}");
    assert_eq!(summaries[0].session_id, newer.session_id());
    assert_eq!(summaries[0].title, "");
    assert_eq!(summaries[0].provider_connection, "ollama");
    assert_eq!(summaries[0].model, "llama3");
    assert_eq!(summaries[1].session_id, older.session_id());
}

#[tokio::test]
async fn past_explicit_user_messages_are_deduplicated_and_skip_current_and_children() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");

    let older = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("older session");
    older.record_message(&user_message("older prompt"));
    older.record_message(&user_message("duplicate prompt"));
    older.record_message(&user_message("older tail"));
    older.record_message(&Message::User {
        content: "implicit note".to_string(),
        kind: UserMessageKind::ImplicitToolNote,
    });
    older.flush().await;

    let newer = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("newer session");
    newer.record_message(&user_message("newer head"));
    newer.record_message(&user_message("duplicate prompt"));
    newer.record_message(&user_message("newer tail"));
    newer.flush().await;

    let child = older
        .write_child_transcript_for_run_store(Uuid::new_v4(), None, &[user_message("child task")])
        .await
        .expect("child session");
    child.flush().await;

    let current = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "openrouter",
        "gpt-4o",
        None,
    )
    .expect("current session");
    current.record_message(&user_message("current prompt"));
    current.flush().await;

    rewrite_meta_created_at(older.path(), "2020-01-01T00:00:00Z");
    rewrite_meta_created_at(newer.path(), "2030-01-01T00:00:00Z");
    rewrite_meta_created_at(current.path(), "2040-01-01T00:00:00Z");

    assert_eq!(
        current
            .read_past_explicit_user_messages(50)
            .expect("history reads"),
        vec![
            "older prompt".to_string(),
            "older tail".to_string(),
            "newer head".to_string(),
            "duplicate prompt".to_string(),
            "newer tail".to_string()
        ]
    );
    assert_eq!(
        current
            .read_past_explicit_user_messages(1)
            .expect("limited history reads"),
        vec!["newer tail".to_string()]
    );
}

#[tokio::test]
async fn list_sessions_of_unknown_project_key_is_empty() {
    let temp = tempfile::tempdir().expect("tempdir");
    let summaries = Store::list_sessions(&temp.path().join("chats"), "deadbeef")
        .expect("listing an unknown project is not an error");
    assert!(summaries.is_empty());
}

#[tokio::test]
async fn read_child_runs_pairs_finished_and_orphaned_runs() {
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
    .expect("parent store created");
    let finished_run_id = Uuid::new_v4();
    let finished_call_id = Uuid::new_v4();
    let orphaned_run_id = Uuid::new_v4();
    let orphaned_call_id = Uuid::new_v4();

    store.record_child_start(
        finished_run_id,
        finished_call_id,
        "research",
        "find the regression",
        "Find regression",
    );
    store.record_child_result(
        finished_run_id,
        finished_call_id,
        "success",
        Some("found it".to_string()),
        vec!["src/agent.rs".to_string()],
        FinishResultOrigin::Agent,
    );
    store.record_child_start(
        orphaned_run_id,
        orphaned_call_id,
        "review",
        "review the patch",
        "Review patch",
    );
    store.flush().await;

    let runs = Store::read_child_runs(store.path()).expect("child records readable");
    assert_eq!(runs.len(), 2);

    let finished = runs
        .iter()
        .find(|run| run.run_id == finished_run_id)
        .expect("finished run present");
    assert_eq!(finished.call_id, finished_call_id);
    assert_eq!(finished.label, "Find regression");
    assert_eq!(finished.status.as_deref(), Some("success"));
    assert_eq!(finished.result.as_deref(), Some("found it"));
    assert_eq!(finished.sources, vec!["src/agent.rs".to_string()]);
    assert!(finished.completed_at.is_some());
    assert!(finished.duration.is_some());

    let orphaned = runs
        .iter()
        .find(|run| run.run_id == orphaned_run_id)
        .expect("orphaned run present");
    assert_eq!(orphaned.call_id, orphaned_call_id);
    assert_eq!(orphaned.label, "Review patch");
    assert!(orphaned.completed_at.is_none());
    assert!(orphaned.status.is_none());
    assert!(orphaned.result.is_none());
    assert!(orphaned.duration.is_none());
}

#[tokio::test]
async fn read_latest_agent_type_follows_the_last_agent_type_changed_record() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let store = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "ollama",
        "llama3",
        Some("default"),
    )
    .expect("store created");

    assert_eq!(
        Store::read_latest_agent_type(store.path()).expect("readable"),
        Some("default".to_string()),
        "before any override the header's type is the active one"
    );

    store.record_agent_type_changed("researcher");
    store.flush().await;
    assert_eq!(
        Store::read_latest_agent_type(store.path()).expect("readable"),
        Some("researcher".to_string()),
        "an override supersedes the header without rewriting it"
    );

    store.record_agent_type_changed("default");
    store.flush().await;
    let events = read_events(store.path());
    let StoreEvent::Meta(meta) = &events[0] else {
        panic!("first line must be the meta header")
    };
    assert_eq!(
        meta.agent_type.as_deref(),
        Some("default"),
        "the header still names the type resolved at creation"
    );
    assert_eq!(
        Store::read_latest_agent_type(store.path()).expect("readable"),
        Some("default".to_string()),
        "the last override wins over every earlier one"
    );
}

#[tokio::test]
async fn compaction_events_carry_the_transcript_relative_boundary_for_replay() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let store = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "ollama",
        "llama3",
        None,
    )
    .expect("store created");
    store.record_message(&user_message("first turn"));
    // The live boundary counts the two rebuilt system prompts; the
    // persisted transcript does not contain them.
    store.record_compaction(5, 2, "summarized prefix");
    store.flush().await;

    let events = read_events(store.path());
    let compaction = events
        .iter()
        .find_map(|event| match event {
            StoreEvent::Compaction {
                boundary,
                transcript_boundary,
                ..
            } => Some((*boundary, *transcript_boundary)),
            _ => None,
        })
        .expect("the compaction event");
    assert_eq!(compaction, (5, Some(3)));

    let state = Store::read_session_state(store.path()).expect("state readable");
    assert_eq!(
        state.compaction.map(|snapshot| snapshot.boundary),
        Some(5),
        "the model-view boundary is unchanged for the context restore"
    );
    assert_eq!(
        state.compaction_transcript_boundary,
        Some(3),
        "replay-only readers get the cut without the system prompts"
    );
    store.record_compaction(4, 0, "newer prefix after fewer rebuilt systems");
    store.record_compaction(9, 6, "stale prefix with more rebuilt systems");
    store.flush().await;
    let state = Store::read_session_state(store.path()).expect("new state readable");
    assert_eq!(state.compaction_transcript_boundary, Some(4));
    let snapshot = state.compaction.expect("latest snapshot");
    assert_eq!(snapshot.boundary, 4);
    assert_eq!(snapshot.summary, "newer prefix after fewer rebuilt systems");
}

#[tokio::test]
async fn compaction_events_round_trip_and_the_latest_snapshot_wins() {
    use crate::agent::compaction::CompactionSnapshot;

    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let store = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "ollama",
        "llama3",
        None,
    )
    .expect("store created");
    store.record_message(&user_message("first turn"));
    store.record_compaction(3, 0, "first summary");
    store.record_compaction(7, 0, "second summary");
    store.flush().await;

    let events = read_events(store.path());
    let compactions: Vec<(u32, usize, String)> = events
        .iter()
        .filter_map(|event| match event {
            StoreEvent::Compaction {
                version,
                boundary,
                summary,
                ..
            } => Some((*version, *boundary, summary.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        compactions,
        vec![
            (EVENT_VERSION, 3, "first summary".to_string()),
            (EVENT_VERSION, 7, "second summary".to_string()),
        ],
        "each snapshot is appended, in order, with the event version stamped on it"
    );

    let state = Store::read_session_state(store.path()).expect("state readable");
    assert_eq!(
        state.events.len(),
        events.len(),
        "the full event stream comes back for replay and cloning"
    );
    assert_eq!(state.messages, vec![user_message("first turn")]);
    assert_eq!(
        state.compaction,
        Some(CompactionSnapshot {
            boundary: 7,
            summary: "second summary".to_string(),
        }),
        "the newest snapshot supersedes the older one"
    );
}

#[tokio::test]
async fn a_session_written_before_compaction_existed_reads_back_without_a_snapshot() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let store = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "ollama",
        "llama3",
        None,
    )
    .expect("store created");
    store.record_message(&user_message("legacy turn"));
    store.flush().await;

    let state = Store::read_session_state(store.path()).expect("state readable");
    assert_eq!(state.messages, vec![user_message("legacy turn")]);
    assert!(
        state.compaction.is_none(),
        "a legacy session has no snapshot to restore: {state:?}"
    );
    assert!(!state
        .events
        .iter()
        .any(|event| matches!(event, StoreEvent::Compaction { .. })));
}

#[tokio::test]
async fn reading_a_session_state_never_touches_the_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");
    let store = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "ollama",
        "llama3",
        None,
    )
    .expect("store created");
    store.record_message(&user_message("turn"));
    store.record_compaction(1, 0, "summarized");
    store.flush().await;
    let path = store.path().to_path_buf();
    let session_id = store.session_id();
    let before = std::fs::read(&path).expect("session file readable");
    // Release the writer lock so the resume below is the process that
    // opens the file, exactly as a later `--conversation-id` run would.
    drop(store);

    // The dropped handle's writer task releases the advisory lock
    // asynchronously, so retry briefly — exactly what a later process
    // would do after the previous one exited.
    let resumed = {
        let mut attempts = 0;
        loop {
            match Store::resume(&chats_root, temp.path(), session_id) {
                Ok(resumed) => break resumed,
                Err(StoreError::SessionLocked { .. }) if attempts < 100 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                Err(error) => panic!("resume succeeds: {error}"),
            }
        }
    };
    resumed.flush().await;
    let state = Store::read_session_state(&path).expect("state readable");
    assert_eq!(state.compaction.map(|snapshot| snapshot.boundary), Some(1));
    drop(resumed);

    let after = std::fs::read(&path).expect("session file readable");
    assert_eq!(
        before, after,
        "a resume plus a state read must not append, rewrite or repair anything"
    );
}

/// Rewrite the `created_at` of a session file's `meta` line so ordering
/// assertions do not depend on wall-clock resolution.
fn rewrite_meta_created_at(path: &Path, created_at: &str) {
    let content = std::fs::read_to_string(path).expect("read");
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let mut event: serde_json::Value = serde_json::from_str(&lines[0]).expect("meta line is JSON");
    event["created_at"] = serde_json::json!(created_at);
    lines[0] = serde_json::to_string(&event).expect("serialize");
    std::fs::write(path, format!("{}\n", lines.join("\n"))).expect("write");
}
