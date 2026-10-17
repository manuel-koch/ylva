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
fn subagent_events_roundtrip_with_new_tags_and_reject_old_tags() {
    let run_id = Uuid::new_v4();
    let parent_session = Uuid::new_v4();
    let call_id = Uuid::new_v4();
    let events = [
        (
            StoreEvent::SubagentStart {
                version: EVENT_VERSION,
                created_at: now_timestamp(),
                id: run_id,
                parent_session,
                call_id,
                agent_type: "research".to_string(),
                task: "Investigate the issue".to_string(),
                label: "Investigate the issue".to_string(),
            },
            "subagent_start",
            "child_start",
        ),
        (
            StoreEvent::SubagentResult {
                version: EVENT_VERSION,
                completed_at: now_timestamp(),
                id: run_id,
                parent_session,
                call_id,
                status: "success".to_string(),
                result: Some("Found the cause".to_string()),
                sources: vec!["src/agent.rs".to_string()],
                origin: crate::subagent_manager::FinishResultOrigin::Agent,
            },
            "subagent_result",
            "child_result",
        ),
    ];
    for (event, tag, old_tag) in events {
        let mut serialized = serde_json::to_value(&event).expect("event serializes");
        assert_eq!(serialized["type"], tag);
        assert_eq!(
            serde_json::from_value::<StoreEvent>(serialized.clone()).expect("event reads back"),
            event
        );
        serialized["type"] = serde_json::json!(old_tag);
        assert!(
            serde_json::from_value::<StoreEvent>(serialized).is_err(),
            "old delegation tags are intentionally unsupported"
        );
    }
}

#[test]
fn a_compaction_event_written_before_the_field_still_reads() {
    // An event from before the marker existed: no `transcript_boundary`
    // key at all. The reader tolerates it and reports no marker position.
    let legacy = r#"{"type":"compaction","version":1,"compacted_at":"2026-01-01T00:00:00.000Z","boundary":4,"summary":"old summary"}"#;
    let event: StoreEvent = serde_json::from_str(legacy).expect("the legacy line parses");
    let StoreEvent::Compaction {
        boundary,
        transcript_boundary,
        ..
    } = event
    else {
        panic!("a compaction line");
    };
    assert_eq!(boundary, 4);
    assert_eq!(transcript_boundary, None);
}

#[test]
fn unknown_fields_and_unknown_event_types_stay_readable() {
    let with_extra_field = r#"{"type":"message","v":1,"message":{"role":"user","content":"hi","kind":"explicit"},"future_field":42}"#;
    let parsed: StoreEvent =
        serde_json::from_str(with_extra_field).expect("unknown fields are ignored");
    assert!(matches!(parsed, StoreEvent::Message { .. }));

    let unknown_type = r#"{"type":"something_new","v":9}"#;
    assert!(
        serde_json::from_str::<StoreEvent>(unknown_type).is_err(),
        "an unknown event type fails only its own line, never the file"
    );
}

#[tokio::test]
async fn a_top_level_sessions_meta_header_omits_parent_session_entirely() {
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

    let raw_first_line = std::fs::read_to_string(store.path())
        .expect("session file readable")
        .lines()
        .next()
        .expect("at least one line")
        .to_string();
    assert!(
        !raw_first_line.contains("parent_session"),
        "a top-level session's meta line must not serialize `parent_session` \
         at all (not even as null): {raw_first_line}"
    );

    let StoreEvent::Meta(meta) = &read_events(store.path())[0] else {
        panic!("first line must be the meta header")
    };
    assert_eq!(meta.parent_session, None);
}

#[tokio::test]
async fn meta_records_the_root_agent_type_and_omits_it_when_unset() {
    let temp = tempfile::tempdir().expect("tempdir");
    let chats_root = temp.path().join("chats");

    let selected = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "ollama",
        "llama3",
        Some("researcher"),
    )
    .expect("store created");
    selected.flush().await;
    let StoreEvent::Meta(selected_meta) = &read_events(selected.path())[0] else {
        panic!("first line must be the meta header")
    };
    assert_eq!(
        selected_meta.agent_type.as_deref(),
        Some("researcher"),
        "the resolved root agent type is part of the creation header"
    );

    // A session written before the field existed: no `agent_type` key at
    // all, so readers fall back to `default` instead of failing.
    let legacy = Store::create(
        &chats_root,
        temp.path(),
        Uuid::new_v4(),
        "ollama",
        "llama3",
        None,
    )
    .expect("store created");
    legacy.flush().await;
    let raw_first_line = std::fs::read_to_string(legacy.path())
        .expect("session file readable")
        .lines()
        .next()
        .expect("at least one line")
        .to_string();
    assert!(
        !raw_first_line.contains("agent_type"),
        "an unset agent type must not serialize at all: {raw_first_line}"
    );
    assert_eq!(
        Store::read_latest_agent_type(legacy.path()).expect("legacy file is readable"),
        None,
        "a header without the field reads back as unset"
    );
}
