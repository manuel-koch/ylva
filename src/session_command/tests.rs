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
use crate::cli::SessionSelector;

fn session_summary(identity: u128, parent: Option<u128>, day: u32) -> SessionSummary {
    SessionSummary {
        session_id: uuid::Uuid::from_u128(identity),
        parent_session: parent.map(uuid::Uuid::from_u128),
        created_at: "2020-01-01T00:00:00Z".into(),
        last_used_at: format!("2026-01-{day:02}T00:00:00Z"),
        title: format!("session {identity}"),
        provider_connection: "mock".into(),
        model: "mock".into(),
        path: std::path::PathBuf::from(format!("{identity}.jsonl")),
    }
}

#[test]
fn session_tree_groups_interleaved_subagents_and_marks_missing_parents() {
    let sessions = [
        session_summary(1, None, 1),
        session_summary(2, None, 2),
        session_summary(3, Some(2), 3),
        session_summary(4, Some(1), 4),
        session_summary(5, Some(4), 5),
        session_summary(6, Some(99), 6),
    ];
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();
    write_session_tree(&[], &mut output, &mut diagnostics).unwrap();
    assert!(output.is_empty());
    write_session_tree(&sessions, &mut output, &mut diagnostics).unwrap();
    let expected: String = [
        (1, 1, ""),
        (4, 4, "+ "),
        (5, 5, "  + "),
        (2, 2, ""),
        (3, 3, "+ "),
    ]
    .iter()
    .map(|(identity, day, marker)| {
        format!(
            "2026-01-{day:02}T00:00:00Z {marker}{} session {identity}\n",
            uuid::Uuid::from_u128(*identity)
        )
    })
    .collect();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        format!(
            "{expected}2026-01-06T00:00:00Z + {} session 6 [parent unavailable: {}]\n",
            uuid::Uuid::from_u128(6),
            uuid::Uuid::from_u128(99)
        )
    );
    assert!(diagnostics.is_empty());
}

#[test]
fn session_tree_reports_parent_cycles_without_hiding_or_repeating_sessions() {
    let sessions = [
        session_summary(1, Some(2), 1),
        session_summary(2, Some(1), 2),
        session_summary(3, Some(3), 3),
    ];
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();
    write_session_tree(&sessions, &mut output, &mut diagnostics).unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        format!(
            "2026-01-01T00:00:00Z + {} session 1 [unresolved parent: {}]\n2026-01-02T00:00:00Z   + {} session 2\n2026-01-03T00:00:00Z + {} session 3 [unresolved parent: {}]\n",
            uuid::Uuid::from_u128(1),
            uuid::Uuid::from_u128(2),
            uuid::Uuid::from_u128(2),
            uuid::Uuid::from_u128(3),
            uuid::Uuid::from_u128(3)
        )
    );
    let diagnostics = String::from_utf8(diagnostics).unwrap();
    assert_eq!(diagnostics.lines().count(), 2);
    assert!(diagnostics
        .lines()
        .all(|line| line.starts_with("warning: ") && line.contains("no available hierarchy root")));
}

#[test]
fn dates_require_exact_calendar_dates_or_offset_timestamps() {
    assert_eq!(
        parse_date("2026-01-02").unwrap().to_rfc3339(),
        "2026-01-02T00:00:00+00:00"
    );
    assert_eq!(
        parse_date("2026-01-02T03:00:00+03:00").unwrap(),
        parse_date("2026-01-02").unwrap()
    );
    for invalid in ["2026-02-30", "2026-1-2", "2026-01-02T03:00:00", "tomorrow"] {
        assert!(parse_date(invalid).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn archive_commands_report_session_actions_without_paths() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let key = project_key(directory.path());
    let identity = uuid::Uuid::new_v4();
    let store = Store::create(
        &chats_root,
        directory.path(),
        identity,
        "mock",
        "mock",
        None,
    )
    .unwrap();
    store.flush().await;
    let path = store.path().to_path_buf();
    let original = std::fs::read(&path).unwrap();
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    for (compress, dry_run, expected, archived) in [
        (true, true, "would compress", false),
        (true, false, "compressed", true),
        (true, true, "already compressed", true),
        (true, false, "already compressed", true),
        (false, true, "would uncompress", true),
        (false, false, "decompressed", false),
        (false, true, "already uncompressed", false),
        (false, false, "already uncompressed", false),
    ] {
        let selector = SessionSelector {
            session_ids: vec![identity],
            before: None,
            all: false,
            dry_run,
        };
        let command = if compress {
            SessionCommand::Compress(selector)
        } else {
            SessionCommand::Decompress(selector)
        };
        let mut output = Vec::new();
        let mut diagnostics = Vec::new();
        assert_eq!(
            run_for_project(&command, &chats_root, &key, &mut output, &mut diagnostics).unwrap(),
            0
        );
        let expected = format!("{expected} {identity}\n");
        assert_eq!(String::from_utf8(output).unwrap(), expected);
        assert!(diagnostics.is_empty());
        assert_eq!(path.exists(), !archived);
        assert_eq!(path.with_extension("jsonl.gz").exists(), archived);
        if !archived {
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }
}

#[tokio::test]
async fn pure_orchestration_lists_and_maintains_without_environment_changes() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let key = project_key(directory.path());
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();
    let list = SessionCommand::List {
        include_subagents: false,
    };
    assert_eq!(
        run_for_project(&list, &chats_root, &key, &mut output, &mut diagnostics).unwrap(),
        0
    );
    assert!(output.is_empty());
    let store = Store::create(
        &chats_root,
        directory.path(),
        uuid::Uuid::new_v4(),
        "mock",
        "mock",
        None,
    )
    .unwrap();
    let identity = store.session_id();
    let selector = || SessionSelector {
        session_ids: vec![identity],
        before: None,
        all: false,
        dry_run: false,
    };
    assert_eq!(
        run_for_project(
            &SessionCommand::Compress(selector()),
            &chats_root,
            &key,
            &mut output,
            &mut diagnostics
        )
        .unwrap(),
        1
    );
    assert!(String::from_utf8_lossy(&diagnostics).contains("open in another"));
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    output.clear();
    diagnostics.clear();
    assert_eq!(
        run_for_project(&list, &chats_root, &key, &mut output, &mut diagnostics).unwrap(),
        0
    );
    assert!(String::from_utf8_lossy(&output).contains(&identity.to_string()));
    for command in [
        SessionCommand::Compress(selector()),
        SessionCommand::Decompress(selector()),
    ] {
        assert_eq!(
            run_for_project(&command, &chats_root, &key, &mut output, &mut diagnostics).unwrap(),
            0
        );
    }
    let mut preview = selector();
    preview.dry_run = true;
    output.clear();
    assert_eq!(
        run_for_project(
            &SessionCommand::Delete(preview),
            &chats_root,
            &key,
            &mut output,
            &mut diagnostics
        )
        .unwrap(),
        0
    );
    assert_eq!(
        String::from_utf8_lossy(&output),
        format!("would delete {identity}\n")
    );
    let dated = SessionSelector {
        session_ids: vec![],
        before: Some("2000-01-01".into()),
        all: false,
        dry_run: false,
    };
    assert_eq!(
        run_for_project(
            &SessionCommand::Delete(dated),
            &chats_root,
            &key,
            &mut output,
            &mut diagnostics
        )
        .unwrap(),
        0
    );
    output.clear();
    assert_eq!(
        run_for_project(
            &SessionCommand::Delete(selector()),
            &chats_root,
            &key,
            &mut output,
            &mut diagnostics
        )
        .unwrap(),
        0
    );
    assert_eq!(
        String::from_utf8_lossy(&output),
        format!("deleted {identity}\n")
    );
    let all = SessionSelector {
        session_ids: vec![],
        before: None,
        all: true,
        dry_run: false,
    };
    assert_eq!(
        run_for_project(
            &SessionCommand::Compress(all),
            &chats_root,
            &key,
            &mut output,
            &mut diagnostics
        )
        .unwrap(),
        0
    );
}
