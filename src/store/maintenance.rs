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

//! Explicit maintenance with prevalidation and independent failure reporting.

use super::{
    archive, project_directory, session_file, session_path, Store, StoreError, StoreEvent,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum SessionSelection {
    Ids(Vec<Uuid>),
    Before(chrono::DateTime<chrono::Utc>),
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceAction {
    Delete,
    Compress,
    Decompress,
}

#[derive(Debug, Default)]
pub struct MaintenanceReport {
    pub paths: Vec<PathBuf>,
    pub notices: Vec<String>,
    pub failures: Vec<String>,
}

impl Store {
    pub fn maintain_sessions(
        chats_root: &Path,
        project_key: &str,
        selection: &SessionSelection,
        action: MaintenanceAction,
        dry_run: bool,
    ) -> Result<MaintenanceReport, StoreError> {
        archive::require_unix()?;
        if let SessionSelection::Ids(identities) = selection {
            if identities.is_empty() {
                return Err(archive::invalid(
                    chats_root,
                    "at least one session ID is required",
                ));
            }
            for identity in identities {
                let path = session_path(chats_root, project_key, *identity);
                if !archive::transcript_exists(&path) {
                    return Err(StoreError::SessionNotFound {
                        session_id: *identity,
                        path,
                    });
                }
            }
        }
        let directory = project_directory(chats_root, project_key);
        // A preview must not even create coordination/identity lock files.
        let _coordination = if dry_run || action != MaintenanceAction::Delete || !directory.is_dir()
        {
            None
        } else {
            Some(session_file::lock_project(&directory)?)
        };
        let (summaries, diagnostics) = if action != MaintenanceAction::Delete
            && matches!(selection, SessionSelection::Ids(_))
        {
            (Vec::new(), Vec::new())
        } else {
            Self::list_sessions_with_diagnostics(chats_root, project_key)?
        };
        let sessions: BTreeMap<_, _> = summaries
            .into_iter()
            .map(|summary| (summary.session_id, summary))
            .collect();
        let selected: BTreeSet<Uuid> = match selection {
            SessionSelection::Ids(identities) => {
                for identity in identities {
                    let path = session_path(chats_root, project_key, *identity);
                    if !archive::transcript_exists(&path) {
                        return Err(StoreError::SessionNotFound {
                            session_id: *identity,
                            path,
                        });
                    }
                }
                identities.iter().copied().collect()
            }
            SessionSelection::Before(before) => sessions
                .values()
                .filter(|session| {
                    (action != MaintenanceAction::Delete || session.parent_session.is_none())
                        && chrono::DateTime::parse_from_rfc3339(&session.last_used_at)
                            .expect("validated timestamp")
                            < *before
                })
                .map(|session| session.session_id)
                .collect(),
            SessionSelection::All => sessions
                .values()
                .filter(|session| {
                    action != MaintenanceAction::Delete || session.parent_session.is_none()
                })
                .map(|session| session.session_id)
                .collect(),
        };
        let mut report = MaintenanceReport {
            failures: diagnostics,
            ..MaintenanceReport::default()
        };
        if action == MaintenanceAction::Delete && !report.failures.is_empty() {
            report.failures.push(
                "deletion refused: unreadable sessions prevent safe reference discovery".into(),
            );
            return Ok(report);
        }
        let groups = if action == MaintenanceAction::Delete {
            deletion_groups(&sessions, &selected, &mut report)?
        } else {
            selected
                .into_iter()
                .map(|identity| BTreeSet::from([identity]))
                .collect()
        };
        for group in groups {
            let result = process_group(
                chats_root,
                project_key,
                &group,
                selection,
                action,
                dry_run,
                &mut report,
            );
            if let Err(error) = result {
                let diagnostic = error.to_string();
                if !report.failures.contains(&diagnostic) {
                    report.failures.push(diagnostic);
                }
            }
        }
        Ok(report)
    }
}

fn closure(roots: &BTreeSet<Uuid>, links: &BTreeMap<Uuid, BTreeSet<Uuid>>) -> BTreeSet<Uuid> {
    let mut reached = roots.clone();
    let mut pending: Vec<_> = roots.iter().copied().collect();
    while let Some(identity) = pending.pop() {
        if let Some(subagents) = links.get(&identity) {
            for subagent in subagents {
                if reached.insert(*subagent) {
                    pending.push(*subagent);
                }
            }
        }
    }
    reached
}

fn deletion_groups(
    sessions: &BTreeMap<Uuid, super::SessionSummary>,
    selected: &BTreeSet<Uuid>,
    report: &mut MaintenanceReport,
) -> Result<Vec<BTreeSet<Uuid>>, StoreError> {
    let mut links: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
    for session in sessions.values() {
        if let Some(parent) = session.parent_session {
            links.entry(parent).or_default().insert(session.session_id);
        }
        for event in Store::read_events(&session.path)? {
            if let StoreEvent::SubagentStart { id, .. } | StoreEvent::SubagentResult { id, .. } =
                event
            {
                // Only transcript identities are descendants; cloned_from is provenance.
                if sessions
                    .get(&id)
                    .is_some_and(|subagent| subagent.parent_session.is_some())
                {
                    links.entry(session.session_id).or_default().insert(id);
                }
            }
        }
    }
    let surviving_roots = sessions
        .values()
        .filter(|session| {
            session.parent_session.is_none() && !selected.contains(&session.session_id)
        })
        .map(|session| session.session_id)
        .collect();
    let retained = closure(&surviving_roots, &links);
    let mut groups: Vec<BTreeSet<Uuid>> = Vec::new();
    for identity in selected {
        if retained.contains(identity) {
            report.failures.push(format!(
                "session '{identity}' is referenced by a surviving root or clone"
            ));
            continue;
        }
        let descendants = closure(&BTreeSet::from([*identity]), &links);
        let mut group = BTreeSet::new();
        for descendant in descendants {
            if retained.contains(&descendant) {
                let notice = format!("retained shared descendant {descendant}");
                if !report.notices.contains(&notice) {
                    report.notices.push(notice);
                }
            } else if sessions.contains_key(&descendant) {
                group.insert(descendant);
            }
        }
        // Overlapping selected roots/subagents form one lock-protected group.
        let mut index = 0;
        while index < groups.len() {
            if !groups[index].is_disjoint(&group) {
                group.extend(groups.remove(index));
            } else {
                index += 1;
            }
        }
        groups.push(group);
    }
    Ok(groups)
}

pub(super) fn process_group(
    chats_root: &Path,
    project_key: &str,
    group: &BTreeSet<Uuid>,
    selection: &SessionSelection,
    action: MaintenanceAction,
    dry_run: bool,
    report: &mut MaintenanceReport,
) -> Result<(), StoreError> {
    let mut locks = Vec::new();
    for identity in group {
        let path = session_path(chats_root, project_key, *identity);
        if dry_run {
            if let Some(lock) = session_file::probe_identity(&path, *identity)? {
                locks.push(lock);
            }
        } else {
            locks.push(session_file::lock_identity(&path, *identity)?);
        }
        // Resolve after exclusion, not from a stale discovery snapshot.
        let bytes = archive::read_bytes(&path)?;
        let metadata = archive::header(&bytes, &path)?;
        archive::validate_identity(&metadata, &path)?;
        if let SessionSelection::Before(before) = selection {
            if action != MaintenanceAction::Delete || metadata.parent_session.is_none() {
                let last_used_at = super::replay::last_used_at(&bytes, &metadata, &path)?;
                if chrono::DateTime::parse_from_rfc3339(&last_used_at).expect("validated timestamp")
                    >= *before
                {
                    report.notices.push(format!(
                        "skipped {identity}: last use is not before {}",
                        before.to_rfc3339()
                    ));
                    return Ok(());
                }
            }
        }
    }
    for identity in group {
        let path = session_path(chats_root, project_key, *identity);
        let paths = archive::paths(&path);
        if dry_run {
            report.paths.extend(paths);
            if action == MaintenanceAction::Delete {
                report.notices.push(format!("would delete {identity}"));
            } else {
                let transformed = if action == MaintenanceAction::Compress {
                    !path.is_file()
                } else {
                    !archive::archive_path(&path).is_file()
                        && !archive::metadata_path(&path).is_file()
                };
                if transformed {
                    report.notices.push(format!(
                        "already {} {identity}",
                        if action == MaintenanceAction::Compress {
                            "compressed"
                        } else {
                            "uncompressed"
                        }
                    ));
                } else {
                    report.notices.push(format!(
                        "would {} {identity}",
                        if action == MaintenanceAction::Compress {
                            "compress"
                        } else {
                            "uncompress"
                        }
                    ));
                }
            }
            continue;
        }
        let span = tracing::info_span!(parent: None, "session", session_id = %identity);
        let _scope = span.enter();
        match action {
            MaintenanceAction::Delete => {
                // Remove the sidecar last so an interrupted deletion cannot
                // leave an otherwise complete archive without its metadata.
                for selected_path in paths {
                    std::fs::remove_file(&selected_path)
                        .map_err(|source| archive::io_error(&selected_path, source))?;
                    report.paths.push(selected_path);
                }
                report.notices.push(format!("deleted {identity}"));
            }
            MaintenanceAction::Compress | MaintenanceAction::Decompress => {
                let changed = if action == MaintenanceAction::Compress {
                    archive::compress_locked(&path)?
                } else {
                    archive::decompress_locked(&path)?
                };
                let operation = if action == MaintenanceAction::Compress {
                    "compressed"
                } else {
                    "decompressed"
                };
                report.notices.push(if changed {
                    format!("{operation} {identity}")
                } else {
                    format!(
                        "already {} {identity}",
                        if action == MaintenanceAction::Compress {
                            "compressed"
                        } else {
                            "uncompressed"
                        }
                    )
                });
                tracing::info!(changed, operation, "session maintenance completed");
            }
        }
    }
    Ok(())
}
