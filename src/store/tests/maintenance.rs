use super::*;
use crate::store::{archive, session_file, MaintenanceAction, SessionSelection};

#[tokio::test]
async fn before_is_revalidated_after_a_selected_session_is_used_again() {
    let (directory, chats_root, identity, path, _) = super::archive::fixture().await;
    let key = project_key(directory.path());
    let mut metadata = Store::read_session_meta(&path).unwrap().unwrap();
    metadata.created_at = "2020-01-01T00:00:00Z".into();
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&StoreEvent::Meta(metadata)).unwrap()
        ),
    )
    .unwrap();
    let before = chrono::Utc::now() - chrono::Duration::hours(1);
    let summary = Store::list_sessions(&chats_root, &key).unwrap().remove(0);
    assert!(chrono::DateTime::parse_from_rfc3339(&summary.last_used_at).unwrap() < before);
    let resumed = Store::resume(&chats_root, directory.path(), identity).unwrap();
    resumed.record_model_changed("mock", "mock");
    resumed.flush().await;
    drop(resumed);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let bytes = std::fs::read(&path).unwrap();
    for action in [
        MaintenanceAction::Delete,
        MaintenanceAction::Compress,
        MaintenanceAction::Decompress,
    ] {
        for dry_run in [false, true] {
            let mut report = crate::store::MaintenanceReport::default();
            crate::store::maintenance::process_group(
                &chats_root,
                &key,
                &std::collections::BTreeSet::from([identity]),
                &SessionSelection::Before(before),
                action,
                dry_run,
                &mut report,
            )
            .unwrap();
            assert!(report.paths.is_empty());
            assert!(report.failures.is_empty());
            assert_eq!(
                report.notices,
                vec![format!(
                    "skipped {identity}: last use is not before {}",
                    before.to_rfc3339()
                )]
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert!(!archive::archive_path(&path).exists());
        }
    }
}

fn snapshot(directory: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
            (path, bytes, modified)
        })
        .collect();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

#[tokio::test]
async fn dry_run_reports_exact_duplicate_files_without_creating_locks() {
    let (directory, chats_root, identity, path, bytes) = super::archive::fixture().await;
    archive::compress_locked(&path).unwrap();
    std::fs::write(&path, bytes).unwrap();
    std::fs::remove_file(path.with_extension("lock")).unwrap();
    std::fs::remove_file(path.parent().unwrap().join(".project.lock")).unwrap();
    let original = snapshot(path.parent().unwrap());
    let original_modified = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .modified()
        .unwrap();
    let report = Store::maintain_sessions(
        &chats_root,
        &project_key(directory.path()),
        &SessionSelection::Ids(vec![identity]),
        MaintenanceAction::Delete,
        true,
    )
    .unwrap();
    assert!(report.failures.is_empty());
    assert_eq!(report.paths, archive::paths(&path));
    assert_eq!(report.notices, vec![format!("would delete {identity}")]);
    assert_eq!(snapshot(path.parent().unwrap()), original);
    assert_eq!(
        std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .modified()
            .unwrap(),
        original_modified
    );
}

#[tokio::test]
async fn unknown_ids_prevalidate_entire_batch_and_active_targets_fail_independently() {
    let (directory, chats_root, identity, path, bytes) = super::archive::fixture().await;
    let key = project_key(directory.path());
    let unknown = Uuid::new_v4();
    assert!(Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::Ids(vec![identity, unknown]),
        MaintenanceAction::Delete,
        false
    )
    .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let active = Store::create(
        &chats_root,
        directory.path(),
        Uuid::new_v4(),
        "mock",
        "mock",
        None,
    )
    .unwrap();
    active.set_title("active".into()).await.unwrap();
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::All,
        MaintenanceAction::Compress,
        false,
    )
    .unwrap();
    assert_eq!(report.failures.len(), 1);
    assert!(report.failures[0].contains(&active.session_id().to_string()));
    assert!(!path.exists());
    assert!(active.path().exists());
    for action in [
        MaintenanceAction::Delete,
        MaintenanceAction::Compress,
        MaintenanceAction::Decompress,
    ] {
        let report = Store::maintain_sessions(
            &chats_root,
            &key,
            &SessionSelection::Ids(vec![active.session_id()]),
            action,
            true,
        )
        .unwrap();
        assert_eq!(report.failures.len(), 1);
        assert!(report.paths.is_empty());
    }
    assert!(
        Store::clone_session(&chats_root, directory.path(), active.session_id(), None).is_err()
    );
}

#[tokio::test]
async fn deletion_retains_clone_shared_children_and_never_cascades_clones() {
    let (directory, chats_root, identity, path, _bytes) = super::archive::fixture().await;
    let root = Store::resume(&chats_root, directory.path(), identity).unwrap();
    let run_id = Uuid::new_v4();
    root.record_child_start(run_id, Uuid::new_v4(), "default", "task", "label");
    root.write_child_transcript_for_run(
        run_id,
        Some(("mock", "mock")),
        &[user_message("child task")],
    )
    .await
    .unwrap();
    root.flush().await;
    drop(root);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let clone = Store::clone_session(&chats_root, directory.path(), identity, None).unwrap();
    let clone_id = clone.session_id();
    let clone_path = clone.path().to_path_buf();
    clone.flush().await;
    drop(clone);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let key = project_key(directory.path());
    let child_path = session_path(&chats_root, &key, run_id);
    let refused = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::Ids(vec![run_id]),
        MaintenanceAction::Delete,
        false,
    )
    .unwrap();
    assert_eq!(refused.failures.len(), 1);
    assert!(child_path.exists());
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::Ids(vec![identity]),
        MaintenanceAction::Delete,
        false,
    )
    .unwrap();
    assert!(report.failures.is_empty());
    assert!(report
        .notices
        .iter()
        .any(|notice| notice.contains("retained shared")));
    assert!(!path.exists());
    assert!(clone_path.exists());
    assert!(child_path.exists());
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::Ids(vec![clone_id]),
        MaintenanceAction::Delete,
        false,
    )
    .unwrap();
    assert!(report.failures.is_empty());
    assert!(!child_path.exists());
    assert!(!clone_path.exists());
}

#[tokio::test]
async fn locked_root_blocks_its_whole_descendant_group() {
    let (directory, chats_root, identity, path, _bytes) = super::archive::fixture().await;
    let root = Store::resume(&chats_root, directory.path(), identity).unwrap();
    let child = root
        .start_new_session("mock", "mock", Some(identity), None)
        .unwrap();
    let child_path = child.path().to_path_buf();
    child.flush().await;
    drop(child);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let key = project_key(directory.path());
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::All,
        MaintenanceAction::Delete,
        false,
    )
    .unwrap();
    assert_eq!(report.failures.len(), 1);
    assert!(path.exists());
    assert!(child_path.exists());
    drop(root);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::All,
        MaintenanceAction::Delete,
        false,
    )
    .unwrap();
    assert!(report.failures.is_empty());
    assert!(!path.exists());
    assert!(!child_path.exists());
}

#[tokio::test]
async fn strict_dates_bulk_transforms_and_noops() {
    let (directory, chats_root, identity, path, bytes) = super::archive::fixture().await;
    let key = project_key(directory.path());
    let summary = Store::list_sessions(&chats_root, &key).unwrap().remove(0);
    let last_used_at = chrono::DateTime::parse_from_rfc3339(&summary.last_used_at)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::Before(last_used_at),
        MaintenanceAction::Compress,
        false,
    )
    .unwrap();
    assert!(report.notices.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::Before(last_used_at + chrono::Duration::milliseconds(1)),
        MaintenanceAction::Compress,
        false,
    )
    .unwrap();
    assert_eq!(report.notices.len(), 1);
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::All,
        MaintenanceAction::Compress,
        false,
    )
    .unwrap();
    assert_eq!(
        report.notices,
        vec![format!("already compressed {identity}")]
    );
    Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::All,
        MaintenanceAction::Decompress,
        false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::Ids(vec![identity]),
        MaintenanceAction::Decompress,
        false,
    )
    .unwrap();
    assert_eq!(
        report.notices,
        vec![format!("already uncompressed {identity}")]
    );
}

#[tokio::test]
async fn identity_lock_survives_unlink_and_excludes_a_new_writer() {
    let (directory, chats_root, identity, path, _bytes) = super::archive::fixture().await;
    let lock = session_file::lock_identity(&path, identity).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        Store::create(
            &chats_root,
            directory.path(),
            identity,
            "mock",
            "mock",
            None
        ),
        Err(StoreError::SessionLocked { .. })
    ));
    drop(lock);
    assert!(Store::create(
        &chats_root,
        directory.path(),
        identity,
        "mock",
        "mock",
        None
    )
    .is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_clone_and_deletion_never_leave_a_surviving_clone_without_its_child() {
    for _ in 0..4 {
        let (directory, chats_root, identity, _path, _bytes) = super::archive::fixture().await;
        let root = Store::resume(&chats_root, directory.path(), identity).unwrap();
        let child_id = Uuid::new_v4();
        root.record_child_start(child_id, Uuid::new_v4(), "default", "task", "label");
        root.write_child_transcript_for_run(child_id, None, &[user_message("child task")])
            .await
            .unwrap();
        root.flush().await;
        drop(root);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let clone_task = tokio::task::spawn_blocking({
            let barrier = barrier.clone();
            let chats_root = chats_root.clone();
            let working_directory = directory.path().to_path_buf();
            move || {
                barrier.wait();
                Store::clone_session(&chats_root, &working_directory, identity, None)
            }
        });
        let delete_task = tokio::task::spawn_blocking({
            let chats_root = chats_root.clone();
            let key = project_key(directory.path());
            move || {
                barrier.wait();
                Store::maintain_sessions(
                    &chats_root,
                    &key,
                    &SessionSelection::Ids(vec![identity]),
                    MaintenanceAction::Delete,
                    false,
                )
            }
        });
        let cloned = clone_task.await.unwrap();
        let deleted = delete_task.await.unwrap().unwrap();
        assert!(deleted.failures.is_empty());
        match cloned {
            Ok(clone) => {
                assert!(
                    session_path(&chats_root, &project_key(directory.path()), child_id).exists()
                );
                assert!(Store::read_child_runs(clone.path())
                    .unwrap()
                    .iter()
                    .any(|run| run.run_id == child_id));
            }
            Err(StoreError::SessionNotFound { .. }) => {}
            Err(error) => panic!("unexpected clone failure: {error}"),
        }
    }
}
#[tokio::test]
async fn corrupt_explicit_transforms_do_not_block_independent_valid_targets() {
    let (directory, chats_root, identity, path, _bytes) = super::archive::fixture().await;
    let key = project_key(directory.path());
    let corrupt_identity = Uuid::new_v4();
    let corrupt_path = session_path(&chats_root, &key, corrupt_identity);
    std::fs::write(&corrupt_path, b"corrupt header\n").unwrap();
    let report = Store::maintain_sessions(
        &chats_root,
        &key,
        &SessionSelection::Ids(vec![identity, corrupt_identity]),
        MaintenanceAction::Compress,
        false,
    )
    .unwrap();
    assert_eq!(report.failures.len(), 1);
    assert!(report
        .notices
        .iter()
        .any(|notice| notice.contains(&identity.to_string())));
    assert!(!path.exists());
    assert_eq!(std::fs::read(corrupt_path).unwrap(), b"corrupt header\n");
}
