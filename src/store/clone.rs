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

//! Cloning an existing session into a brand-new top-level session.

use std::path::Path;

use uuid::Uuid;

use super::session_file::{
    append_event_line, lock_project, open_locked_session_file, write_meta_line,
};
use super::title::{title_snapshot, TitleState};
use super::{
    archive, now_timestamp, project_key, session_file, session_path, SessionMeta, Store,
    StoreError, StoreEvent, EVENT_VERSION, SCHEMA_VERSION,
};
use crate::agent::Message;

impl Store {
    /// Create a brand-new top-level session that replays `source_id`'s
    /// logical event stream — the `--clone` path.
    ///
    /// The source is read under a nonblocking exclusive lock held only for
    /// the read: another Ylva process holding the source (or a source that is
    /// a subagent transcript) fails before anything is created. The source file
    /// itself is never repaired, rewritten, or appended to. The clone gets a
    /// fresh session id, its own `meta` header naming the source in
    /// `cloned_from` with `parent_session` unset, and every copied event
    /// except the old header — with `subagent_start`/`subagent_result` records
    /// rebased onto the new id. Subagent transcript files stay where they are
    /// and are reused as read-only history.
    ///
    /// `agent_type` is the effective root type for the clone (an explicit
    /// `--agent`, else the source's stored type, else `None` for a source
    /// written before either field existed).
    pub fn clone_session(
        chats_root: &Path,
        cwd: &Path,
        source_id: Uuid,
        agent_type: Option<&str>,
    ) -> Result<Store, StoreError> {
        let project_key = project_key(cwd);
        let source_path = session_path(chats_root, &project_key, source_id);
        let _coordination = lock_project(archive::session_directory(&source_path)?)?;
        if !archive::transcript_exists(&source_path) {
            return Err(StoreError::SessionNotFound {
                session_id: source_id,
                path: source_path,
            });
        }

        let (source_meta, events) = {
            // Exclusive nonblock: a live resume of the source — or a
            // concurrent clone of it — makes this fail without touching it.
            let _source_lock = session_file::lock_identity(&source_path, source_id)?;
            let events = Store::read_events(&source_path)?;
            let source_meta = events.iter().find_map(|event| match event {
                StoreEvent::Meta(meta) => Some(meta.clone()),
                _ => None,
            });
            let source_meta = source_meta.ok_or_else(|| StoreError::MissingSessionMeta {
                session_id: source_id,
                path: source_path.clone(),
            })?;
            if source_meta.parent_session.is_some() {
                return Err(StoreError::SessionIsSubagent {
                    session_id: source_id,
                    path: source_path,
                });
            }
            (source_meta, events)
        };

        let clone_id = Uuid::new_v4();
        let clone_path = session_path(chats_root, &project_key, clone_id);
        let mut file = open_locked_session_file(&clone_path, clone_id, false)?;
        let clone_meta = SessionMeta {
            version: EVENT_VERSION,
            created_at: now_timestamp(),
            schema_version: SCHEMA_VERSION,
            app_version: crate::build_info::BUILD_INFO.version.to_string(),
            project_key: project_key.clone(),
            provider_connection: source_meta.provider_connection.clone(),
            model: source_meta.model.clone(),
            session_id: clone_id,
            title: source_meta.title.clone(),
            parent_session: None,
            agent_type: agent_type
                .map(str::to_string)
                .or_else(|| source_meta.agent_type.clone()),
            cloned_from: Some(source_id),
        };
        let meta_line_len = write_meta_line(&mut file, &clone_path, &clone_meta)?;

        let messages: Vec<Message> = events
            .iter()
            .filter_map(|event| match event {
                StoreEvent::Message { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect();
        for event in events {
            let event = match event {
                // The source's header is replaced by `clone_meta` above.
                StoreEvent::Meta(_) => continue,
                // Parent lifecycle records now name the clone, so a later
                // resume of the clone resolves its subagent runs from its own
                // session id. The subagent transcript files themselves are
                // untouched and stay shared read-only history.
                StoreEvent::SubagentStart {
                    version,
                    created_at,
                    id,
                    parent_session,
                    call_id,
                    agent_type,
                    task,
                    label,
                } if parent_session == source_id => StoreEvent::SubagentStart {
                    version,
                    created_at,
                    id,
                    parent_session: clone_id,
                    call_id,
                    agent_type,
                    task,
                    label,
                },
                StoreEvent::SubagentResult {
                    version,
                    completed_at,
                    id,
                    parent_session,
                    call_id,
                    status,
                    result,
                    sources,
                    origin,
                } if parent_session == source_id => StoreEvent::SubagentResult {
                    version,
                    completed_at,
                    id,
                    parent_session: clone_id,
                    call_id,
                    status,
                    result,
                    sources,
                    origin,
                },
                other => other,
            };
            append_event_line(&mut file, &clone_path, &event)?;
        }

        let title_state = if clone_meta.title.is_empty() {
            Some((clone_meta.clone(), meta_line_len))
        } else {
            None
        };
        let title_snapshot = title_snapshot(&clone_meta, &messages);
        Ok(Store::spawn_writer(
            clone_id,
            project_key,
            chats_root.to_path_buf(),
            clone_path,
            file,
            TitleState {
                meta_awaiting_title: title_state,
            },
            title_snapshot,
        ))
    }
}
