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
use crate::store::{archive, session_file, MaintenanceAction, SessionSelection};

pub(super) async fn fixture() -> (tempfile::TempDir, PathBuf, Uuid, PathBuf, Vec<u8>) {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let identity = Uuid::new_v4();
    let store = Store::create(
        &chats_root,
        directory.path(),
        identity,
        "mock",
        "mock",
        Some("default"),
    )
    .unwrap();
    store.record_message(&user_message("original question"));
    store.record_message(&assistant_message("original answer"));
    store.set_title("Archive title".into()).await.unwrap();
    store.flush().await;
    let path = store.path().to_path_buf();
    let bytes = std::fs::read(&path).unwrap();
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (directory, chats_root, identity, path, bytes)
}

#[tokio::test]
async fn lossless_archive_discovery_clone_resume_and_history() {
    let (directory, chats_root, identity, path, bytes) = fixture().await;
    let meta = Store::read_session_meta(&path).unwrap();
    let events = Store::read_events(&path).unwrap();
    let original_summary = Store::list_sessions(&chats_root, &project_key(directory.path()))
        .unwrap()
        .remove(0);
    let lock = session_file::lock_identity(&path, identity).unwrap();
    assert!(archive::compress_locked(&path).unwrap());
    assert!(!path.exists());
    assert!(!archive::compress_locked(&path).unwrap());
    drop(lock);
    assert_eq!(Store::read_session_meta(&path).unwrap(), meta);
    assert_eq!(Store::read_events(&path).unwrap(), events);
    let summary = Store::list_sessions(&chats_root, &project_key(directory.path())).unwrap();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].last_used_at, original_summary.last_used_at);
    assert_eq!(summary[0].created_at, original_summary.created_at);
    assert!(summary[0].path.to_string_lossy().ends_with(".jsonl.gz"));
    let archive_bytes = std::fs::read(archive::archive_path(&path)).unwrap();
    let clone = Store::clone_session(&chats_root, directory.path(), identity, None).unwrap();
    assert_eq!(clone.title(), "Archive title");
    assert!(!path.exists());
    assert_eq!(
        std::fs::read(archive::archive_path(&path)).unwrap(),
        archive_bytes
    );
    let summaries = Store::list_sessions(&chats_root, &project_key(directory.path())).unwrap();
    assert_eq!(
        summaries
            .iter()
            .find(|summary| summary.session_id == identity)
            .unwrap()
            .last_used_at,
        original_summary.last_used_at
    );
    assert_eq!(
        clone.read_past_explicit_user_messages(50).unwrap(),
        vec!["original question"]
    );
    let resumed = Store::resume(&chats_root, directory.path(), identity).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(!archive::archive_path(&path).exists());
    assert!(!archive::metadata_path(&path).exists());
    resumed.record_message(&user_message("continued"));
    resumed.flush().await;
    assert_eq!(Store::read_messages(&path).unwrap().len(), 3);
}

#[tokio::test]
async fn every_transition_boundary_retains_a_recoverable_complete_copy() {
    for boundary in [
        "metadata_published",
        "archive_published",
        "compression_verified",
        "raw_removed",
    ] {
        let (_directory, _chats_root, identity, path, bytes) = fixture().await;
        let _lock = session_file::lock_identity(&path, identity).unwrap();
        assert!(archive::compress_with_checkpoint(&path, |stage| {
            if stage == boundary {
                Err(archive::invalid(&path, "injected interruption"))
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(archive::read_bytes(&path).unwrap(), bytes, "{boundary}");
        archive::compress_locked(&path).unwrap();
        assert_eq!(archive::read_bytes(&path).unwrap(), bytes);
    }
    for boundary in [
        "raw_published",
        "restoration_verified",
        "archive_removed",
        "metadata_removed",
    ] {
        let (_directory, _chats_root, identity, path, bytes) = fixture().await;
        let _lock = session_file::lock_identity(&path, identity).unwrap();
        archive::compress_locked(&path).unwrap();
        assert!(archive::decompress_with_checkpoint(&path, |stage| {
            if stage == boundary {
                Err(archive::invalid(&path, "injected interruption"))
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(archive::read_bytes(&path).unwrap(), bytes, "{boundary}");
        archive::decompress_locked(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn duplicates_are_read_only_and_divergence_blocks_all_mutations() {
    let (directory, chats_root, identity, path, bytes) = fixture().await;
    archive::compress_locked(&path).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(Store::read_events(&path).unwrap().len(), 3);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mut divergent = bytes.clone();
    divergent.extend_from_slice(b"\n");
    std::fs::write(&path, divergent).unwrap();
    assert!(Store::read_messages(&path)
        .unwrap_err()
        .to_string()
        .contains("divergent"));
    assert!(Store::resume(&chats_root, directory.path(), identity).is_err());
    assert!(Store::clone_session(&chats_root, directory.path(), identity, None).is_err());
    let (_, diagnostics) =
        Store::list_sessions_with_diagnostics(&chats_root, &project_key(directory.path())).unwrap();
    assert!(diagnostics[0].contains("divergent"));
    assert!(!Store::maintain_sessions(
        &chats_root,
        &project_key(directory.path()),
        &SessionSelection::Ids(vec![identity]),
        MaintenanceAction::Delete,
        false
    )
    .unwrap()
    .failures
    .is_empty());
    assert!(path.exists());
}

#[tokio::test]
async fn corrupt_archives_sidecars_and_orphans_are_explicit_errors() {
    let (directory, chats_root, identity, path, _bytes) = fixture().await;
    archive::compress_locked(&path).unwrap();
    let metadata_path = archive::metadata_path(&path);
    let mut meta: SessionMeta =
        serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
    let original = std::fs::read(&metadata_path).unwrap();
    meta.session_id = Uuid::new_v4();
    std::fs::write(&metadata_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    assert!(Store::read_events(&path).is_err());
    std::fs::write(&metadata_path, original).unwrap();
    let archive_path = archive::archive_path(&path);
    let compressed = std::fs::read(&archive_path).unwrap();
    std::fs::write(&archive_path, &compressed[..compressed.len() - 4]).unwrap();
    assert!(Store::read_session_meta(&path).is_err());
    std::fs::remove_file(&archive_path).unwrap();
    assert!(Store::read_events(&path)
        .unwrap_err()
        .to_string()
        .contains("orphan"));
    let (_, diagnostics) =
        Store::list_sessions_with_diagnostics(&chats_root, &project_key(directory.path())).unwrap();
    assert!(!diagnostics.is_empty());
    assert!(Store::resume(&chats_root, directory.path(), identity).is_err());
}
