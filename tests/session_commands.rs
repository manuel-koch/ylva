use std::process::{Command, Stdio};
use ylva::{project_key, session_path, Store};

#[tokio::test]
async fn resumed_old_sessions_sort_by_last_use_and_are_not_selected_by_before() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let resumed_identity = uuid::Uuid::from_u128(10);
    let inactive_identity = uuid::Uuid::from_u128(11);
    for (identity, created_at, title) in [
        (resumed_identity, "2020-01-01T00:00:00Z", "resumed"),
        (inactive_identity, "2021-01-01T00:00:00Z", "inactive"),
    ] {
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
        let mut metadata = Store::read_session_meta(&path).unwrap().unwrap();
        metadata.created_at = created_at.into();
        metadata.title = title.into();
        drop(store);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::fs::write(
            path,
            format!(
                "{}\n",
                serde_json::to_string(&ylva::StoreEvent::Meta(metadata)).unwrap()
            ),
        )
        .unwrap();
    }
    let output = command(directory.path())
        .args(["session", "compress", &resumed_identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let resumed_at = chrono::Utc::now();
    let output = command(directory.path())
        .args([
            "--no-tui",
            "--conversation-id",
            &resumed_identity.to_string(),
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let output = command(directory.path())
        .args(["session", "compress", &resumed_identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let resumed_path = session_path(
        &chats_root,
        &project_key(directory.path()),
        resumed_identity,
    );
    let archive_path = resumed_path.with_extension("jsonl.gz");
    let archive_bytes = std::fs::read(&archive_path).unwrap();
    let output = command(directory.path())
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let listing = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = listing.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains(&inactive_identity.to_string()));
    assert!(lines[1].contains(&resumed_identity.to_string()));
    let last_used_at =
        chrono::DateTime::parse_from_rfc3339(lines[1].split_whitespace().next().unwrap()).unwrap();
    assert!(last_used_at.timestamp_millis() >= resumed_at.timestamp_millis());
    assert_eq!(
        Store::read_session_meta(&resumed_path)
            .unwrap()
            .unwrap()
            .created_at,
        "2020-01-01T00:00:00Z"
    );
    let before = (resumed_at - chrono::Duration::hours(1)).to_rfc3339();
    for operation in ["compress", "decompress", "delete"] {
        let preview = command(directory.path())
            .args(["session", operation, "--before", &before, "--dry-run"])
            .output()
            .unwrap();
        assert!(preview.status.success(), "{:?}", preview);
        let preview = String::from_utf8(preview.stdout).unwrap();
        assert!(preview.contains(&inactive_identity.to_string()));
        assert!(!preview.contains(&resumed_identity.to_string()));
        let output = command(directory.path())
            .args(["session", operation, "--before", &before])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(output.contains(&inactive_identity.to_string()));
        assert!(!output.contains(&resumed_identity.to_string()));
        assert_eq!(std::fs::read(&archive_path).unwrap(), archive_bytes);
    }
    assert!(!session_path(
        &chats_root,
        &project_key(directory.path()),
        inactive_identity
    )
    .exists());
}

#[tokio::test]
async fn listing_groups_archived_and_nested_children_with_their_parents() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    for (identity, parent, day, title) in [
        (1, None, 1, "root one"),
        (2, None, 2, "root two"),
        (3, Some(1), 4, "child\none"),
        (4, Some(3), 5, "nested"),
        (5, Some(2), 3, "child two"),
        (6, Some(1), 4, "sibling"),
        (7, Some(99), 6, "orphan"),
    ] {
        let store = Store::create(
            &chats_root,
            directory.path(),
            uuid::Uuid::from_u128(identity),
            "mock",
            "mock",
            None,
        )
        .unwrap();
        store.flush().await;
        let path = store.path().to_path_buf();
        let mut metadata = Store::read_session_meta(&path).unwrap().unwrap();
        metadata.created_at = "2020-01-01T00:00:00Z".into();
        metadata.title = title.into();
        metadata.parent_session = parent.map(uuid::Uuid::from_u128);
        let activity = ylva::StoreEvent::ModelChanged {
            version: metadata.version,
            changed_at: format!("2026-01-{day:02}T00:00:00Z"),
            provider_connection: "mock".into(),
            model: "mock".into(),
        };
        drop(store);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::fs::write(
            path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&ylva::StoreEvent::Meta(metadata)).unwrap(),
                serde_json::to_string(&activity).unwrap()
            ),
        )
        .unwrap();
    }
    let child_identity = uuid::Uuid::from_u128(3);
    let output = command(directory.path())
        .args(["session", "compress", &child_identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let output = command(directory.path())
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "2026-01-01T00:00:00Z {} root one\n2026-01-02T00:00:00Z {} root two\n",
            uuid::Uuid::from_u128(1),
            uuid::Uuid::from_u128(2)
        )
    );
    let output = command(directory.path())
        .args(["session", "list", "--include-subagents"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let expected: String = [
        (1, 1, "", "root one"),
        (3, 4, "+ ", "child one"),
        (4, 5, "  + ", "nested"),
        (6, 4, "+ ", "sibling"),
        (2, 2, "", "root two"),
        (5, 3, "+ ", "child two"),
    ]
    .iter()
    .map(|(identity, day, marker, title)| {
        format!(
            "2026-01-{day:02}T00:00:00Z {marker}{} {title}\n",
            uuid::Uuid::from_u128(*identity)
        )
    })
    .collect();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{expected}2026-01-06T00:00:00Z + {} orphan [parent unavailable: {}]\n",
            uuid::Uuid::from_u128(7),
            uuid::Uuid::from_u128(99)
        )
    );
    assert!(output.stderr.is_empty());
    let child_path = session_path(&chats_root, &project_key(directory.path()), child_identity);
    assert!(!child_path.exists());
    assert!(child_path.with_extension("jsonl.gz").exists());
}

fn command(directory: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ylva"));
    command
        .env_clear()
        .env("YLVA_HOME", directory)
        .current_dir(directory);
    command
}

#[test]
fn empty_listing_ignores_providers_profiles_and_open_stdin() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = command(directory.path())
        .args(["--profile", "missing", "session", "list"])
        .env("OLLAMA_BASE_URL", "http://127.0.0.1:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let output = child.wait_with_output().unwrap();
    drop(stdin);
    assert!(output.status.success(), "{:?}", output);
    assert!(output.stdout.is_empty());
    assert!(!directory.path().join("chats").exists());
}
#[tokio::test]
async fn listing_is_sorted_filtered_safe_and_diagnoses_bad_headers() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let store = Store::create(
        &chats_root,
        directory.path(),
        uuid::Uuid::new_v4(),
        "mock",
        "mock",
        None,
    )
    .unwrap();
    let child = store
        .start_new_session("mock", "mock", Some(store.session_id()), None)
        .unwrap();
    store.set_title("Title".to_string()).await.unwrap();
    let bad_path = session_path(
        &chats_root,
        &project_key(directory.path()),
        uuid::Uuid::new_v4(),
    );
    std::fs::write(&bad_path, "invalid\n").unwrap();
    let output = command(directory.path())
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains(&format!("{} Title", store.session_id())));
    assert!(!text.contains(&child.session_id().to_string()));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("no readable meta header"));
    let output = command(directory.path())
        .args(["session", "list", "--include-subagents"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 2);
}

#[test]
fn maintenance_requires_one_valid_selector() {
    let directory = tempfile::tempdir().unwrap();
    for arguments in [
        vec!["session", "delete"],
        vec!["session", "delete", "--dry-run"],
        vec!["session", "compress", "--all", "--before", "2026-01-01"],
        vec!["session", "decompress", "bad"],
        vec!["session", "delete", "--before", "2026-01-01T00:00:00"],
    ] {
        let output = command(directory.path()).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!directory.path().join("chats").exists());
    }
}

#[test]
fn session_list_logs_build_info_and_resolved_store_at_debug_level() {
    let directory = tempfile::tempdir().unwrap();
    let log_path = directory.path().join("session-list.log");
    let output = command(directory.path())
        .args(["--log-level", "debug", "--log-file"])
        .arg(&log_path)
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let logs = std::fs::read_to_string(&log_path).unwrap();
    assert!(logs.contains("build info"), "{logs}");
    assert!(logs.contains("resolved session store"), "{logs}");
    assert!(logs.contains(&project_key(directory.path())), "{logs}");
}

#[test]
fn log_file_with_leading_tilde_is_written_under_home() {
    let directory = tempfile::tempdir().unwrap();
    let home_directory = tempfile::tempdir().unwrap();
    let output = command(directory.path())
        .env("HOME", home_directory.path())
        .args([
            "--log-level",
            "debug",
            "--log-file",
            "~/session-list.log",
            "session",
            "list",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert!(home_directory.path().join("session-list.log").exists());
    assert!(!directory.path().join("~").exists());
}

#[test]
fn compiled_cli_logs_session_chains_in_text_and_json_without_prompt_content() {
    for format in ["text", "json"] {
        let directory = tempfile::tempdir().unwrap();
        let log_path = directory.path().join("session.log");
        let output = command(directory.path())
            .args([
                "--no-tui",
                "--prompt",
                "PRIVATE LOGGING PROMPT",
                "--log-level",
                "debug",
                "--log-format",
                format,
            ])
            .arg("--log-file")
            .arg(&log_path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let stderr = String::from_utf8(output.stderr).unwrap();
        let identity = stderr
            .lines()
            .find_map(|line| line.strip_prefix("Session: "))
            .unwrap();
        let logs = std::fs::read_to_string(log_path).unwrap();
        assert!(!logs.contains("PRIVATE LOGGING PROMPT"));
        let command_line = logs
            .lines()
            .find(|line| line.contains("agent command received"))
            .unwrap();
        let writer_line = logs
            .lines()
            .find(|line| line.contains("session event persisted"))
            .unwrap();
        for line in [command_line, writer_line] {
            assert!(line.contains(identity), "{line}");
            if format == "json" {
                let event: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(event["spans"][0]["session_id"], identity);
            }
        }
    }
}
#[tokio::test]
async fn listing_exact_output_uses_timestamp_instants_then_uuid_and_launch_scope() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    for identity in [uuid::Uuid::from_u128(2), uuid::Uuid::from_u128(1)] {
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
        let mut metadata = Store::read_session_meta(&path).unwrap().unwrap();
        metadata.created_at = "2026-01-01T00:00:00Z".into();
        metadata.title = "one\nsafe\u{1b}line".into();
        drop(store);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::fs::write(
            path,
            format!(
                "{}\n",
                serde_json::to_string(&ylva::StoreEvent::Meta(metadata)).unwrap()
            ),
        )
        .unwrap();
    }
    let output = command(directory.path())
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "2026-01-01T00:00:00Z {} one safe line\n2026-01-01T00:00:00Z {} one safe line\n",
            uuid::Uuid::from_u128(1),
            uuid::Uuid::from_u128(2)
        )
    );
    let another_directory = directory.path().join("another");
    std::fs::create_dir(&another_directory).unwrap();
    let output = command(directory.path())
        .current_dir(another_directory)
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[tokio::test]
async fn command_batches_preview_transform_report_noops_and_delete_without_yes() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
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
    let path = store.path().to_path_buf();
    store.flush().await;
    let bytes = std::fs::read(&path).unwrap();
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let preview = command(directory.path())
        .args(["session", "delete", &identity.to_string(), "--dry-run"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert_eq!(
        String::from_utf8(preview.stdout).unwrap(),
        format!("would delete {identity}\n")
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    for operation in ["compress", "compress", "decompress", "decompress"] {
        let output = command(directory.path())
            .args(["session", operation, "--all"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .contains(&identity.to_string()));
    }
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let output = command(directory.path())
        .args(["session", "delete", &identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("deleted {identity}\n")
    );
    assert!(!path.exists());
    assert!(path.with_extension("lock").exists());
}

#[tokio::test]
async fn deletion_and_previews_report_sessions_including_archived_descendants() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let root = Store::create(
        &chats_root,
        directory.path(),
        uuid::Uuid::new_v4(),
        "mock",
        "mock",
        None,
    )
    .unwrap();
    let child = root
        .start_new_session("mock", "mock", Some(root.session_id()), None)
        .unwrap();
    let root_identity = root.session_id();
    let child_identity = child.session_id();
    let root_path = root.path().to_path_buf();
    let child_path = child.path().to_path_buf();
    root.flush().await;
    child.flush().await;
    drop(root);
    drop(child);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let output = command(directory.path())
        .args(["session", "compress", &child_identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let archive_path = child_path.with_extension("jsonl.gz");
    let metadata_path = child_path.with_extension("meta.json");
    let output = command(directory.path())
        .args(["session", "delete", &root_identity.to_string(), "--dry-run"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let paths = [root_path, archive_path, metadata_path];
    let mut identities = [root_identity, child_identity];
    identities.sort();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        identities
            .iter()
            .map(|identity| format!("would delete {identity}\n"))
            .collect::<String>()
    );
    assert!(paths.iter().all(|path| path.exists()));
    let output = command(directory.path())
        .args(["session", "delete", &root_identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        identities
            .iter()
            .map(|identity| format!("deleted {identity}\n"))
            .collect::<String>()
    );
    assert!(paths.iter().all(|path| !path.exists()));
}
