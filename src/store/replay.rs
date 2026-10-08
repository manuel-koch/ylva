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

//! Read-only queries and reconstruction of session and child-run state.

use super::session_file::read_meta_line_with_length;
use super::{project_directory, ChildRunRecord, SessionMeta, Store, StoreError, StoreEvent};
use crate::agent::{Message, UserMessageKind};
use crate::child_agent_manager::FinishResultOrigin;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Read-only summary of one persisted session, built from its `meta` header
/// line alone — never from the full transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub session_id: Uuid,
    pub created_at: String,
    pub title: String,
    pub provider_connection: String,
    pub model: String,
    /// Set only for a child-agent's own session file. See
    /// [`SessionMeta::parent_session`].
    pub parent_session: Option<Uuid>,
    /// Absolute path of the session's `.jsonl` file.
    pub path: PathBuf,
}

/// One session's full persisted state, as [`Store::read_session_state`]
/// returns it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionState {
    /// Every event in append order — the record the TUI replays and the
    /// clone path copies, including messages, selection changes, child-run
    /// lifecycle records and compaction snapshots.
    pub events: Vec<StoreEvent>,
    /// Every committed message, in order — the durable transcript.
    pub messages: Vec<Message>,
    /// The latest compaction snapshot, if the session ever compacted.
    /// `None` for sessions written before the event existed.
    pub compaction: Option<crate::agent::compaction::CompactionSnapshot>,
    /// That snapshot's boundary in persisted-transcript coordinates (system
    /// prompts excluded) — what the TUI uses to place its compaction marker.
    /// `None` when the session never compacted, or when the snapshot predates
    /// the field.
    pub compaction_transcript_boundary: Option<usize>,
    pub model_projection: Option<crate::agent::compaction::ModelProjectionSnapshot>,
}

impl Store {
    /// Every persisted session of `project_key`, newest first.
    ///
    /// Only each file's first (`meta`) line is parsed — never the transcript —
    /// and this is a **pure read**: no lock is taken and no file is ever
    /// modified, not even one whose header fails to parse (such a file is
    /// skipped with a `warn!` instead of failing the whole listing).
    pub fn list_sessions(
        chats_root: &Path,
        project_key: &str,
    ) -> Result<Vec<SessionSummary>, StoreError> {
        let directory = project_directory(chats_root, project_key);
        if !directory.is_dir() {
            return Ok(Vec::new());
        }
        let entries = std::fs::read_dir(&directory).map_err(|source| StoreError::Io {
            path: directory.clone(),
            source,
        })?;
        let mut summaries = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(directory = %directory.display(), %error, "skipping unreadable session directory entry");
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                continue;
            }
            match read_session_summary(&path) {
                Some(summary) => summaries.push(summary),
                None => {
                    tracing::warn!(path = %path.display(), "skipping session file without a readable meta header");
                }
            }
        }
        summaries.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(summaries)
    }

    /// Every committed message of a persisted session, in append order.
    /// A pure read: unparseable lines are skipped in memory, never repaired
    /// on disk.
    pub fn read_messages(path: &Path) -> Result<Vec<Message>, StoreError> {
        let content = std::fs::read_to_string(path).map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(content
            .lines()
            .filter_map(|line| serde_json::from_str::<StoreEvent>(line).ok())
            .filter_map(|event| match event {
                StoreEvent::Message { message, .. } => Some(message),
                _ => None,
            })
            .collect())
    }

    /// Every persisted event of a session, in append order — the full
    /// logical record (meta header, messages, child-run lifecycle records,
    /// model/agent selection changes). A pure read: unparseable lines are
    /// skipped in memory, never repaired on disk, matching
    /// [`Store::read_messages`]. This is the read path for callers that need
    /// more than the transcript (session cloning, resume-time selection
    /// restoration), never a substitute for it in the message path.
    pub fn read_events(path: &Path) -> Result<Vec<StoreEvent>, StoreError> {
        let content = std::fs::read_to_string(path).map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(content
            .lines()
            .filter_map(|line| serde_json::from_str::<StoreEvent>(line).ok())
            .collect())
    }

    /// The `meta` header of a persisted session, if the file exists and its
    /// first line parses. A pure read: no lock is taken and nothing is
    /// modified, so it is safe to call before [`Store::resume`] decides
    /// anything about the run.
    pub fn read_session_meta(path: &Path) -> Result<Option<SessionMeta>, StoreError> {
        if !path.is_file() {
            return Ok(None);
        }
        Ok(read_meta_line_with_length(path)?.map(|(meta, _line_length)| meta))
    }

    /// The root agent type a resumed session should continue with: the last
    /// [`StoreEvent::AgentTypeChanged`] wins, else the `meta` header's
    /// `agent_type`, else `None` (a session written before either existed —
    /// callers treat that as the `default` agent type).
    pub fn read_latest_agent_type(path: &Path) -> Result<Option<String>, StoreError> {
        let events = Self::read_events(path)?;
        let mut latest = None;
        for event in &events {
            match event {
                StoreEvent::Meta(meta) => latest = meta.agent_type.clone(),
                StoreEvent::AgentTypeChanged { agent_type, .. } => {
                    latest = Some(agent_type.clone())
                }
                _ => {}
            }
        }
        Ok(latest)
    }

    /// Everything needed to rebuild one session: the full event stream (TUI
    /// replay, cloning), the committed messages (transcript hydration), and
    /// the latest compaction snapshot that shapes the model view.
    ///
    /// A pure read like every other `Store::read_*` path: unparseable lines
    /// are skipped in memory and nothing on disk is touched.
    pub fn read_session_state(path: &Path) -> Result<SessionState, StoreError> {
        let events = Self::read_events(path)?;
        let mut messages = Vec::new();
        let mut compaction: Option<crate::agent::compaction::CompactionSnapshot> = None;
        let mut compaction_transcript_boundary: Option<usize> = None;
        let mut model_projection: Option<crate::agent::compaction::ModelProjectionSnapshot> = None;
        for event in &events {
            match event {
                StoreEvent::Message { message, .. } => messages.push(message.clone()),
                StoreEvent::Compaction {
                    boundary,
                    transcript_boundary,
                    summary,
                    ..
                } => {
                    // Append-only supersession: the newest snapshot wins, and
                    // a boundary never rewinds.
                    let is_newer = compaction.as_ref().is_none_or(|current| {
                        match (transcript_boundary, compaction_transcript_boundary) {
                            (Some(next), Some(previous)) => *next >= previous,
                            _ => *boundary >= current.boundary,
                        }
                    });
                    if is_newer {
                        compaction = Some(crate::agent::compaction::CompactionSnapshot {
                            boundary: *boundary,
                            summary: summary.clone(),
                        });
                        compaction_transcript_boundary = *transcript_boundary;
                    }
                }
                StoreEvent::ModelProjection { projection } => {
                    if model_projection.as_ref().is_none_or(|current| {
                        projection.projection_revision > current.projection_revision
                    }) {
                        model_projection = Some(projection.as_ref().clone());
                    }
                }
                _ => {}
            }
        }
        Ok(SessionState {
            events,
            messages,
            compaction,
            compaction_transcript_boundary,
            model_projection,
        })
    }

    /// Read explicit user messages from prior top-level sessions in this
    /// project, oldest session first.
    ///
    /// The current session is excluded because its messages are replayed
    /// separately by the TUI, and child-agent transcripts are excluded so
    /// generated subagent tasks do not appear as root-agent draft history.
    /// Only the newest `limit` messages are selected, then returned oldest
    /// first for input-history navigation.
    /// Unreadable session files are skipped with a warning, matching the
    /// best-effort behavior of [`Store::list_sessions`].
    pub fn read_past_explicit_user_messages(
        &self,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let session_summaries =
            Self::list_sessions(&self.inner.chats_root, &self.inner.project_key)?;

        let mut newest_user_messages = Vec::new();
        let mut seen_user_messages = std::collections::HashSet::new();
        for session_summary in session_summaries {
            if session_summary.session_id == self.inner.session_id
                || session_summary.parent_session.is_some()
            {
                continue;
            }
            let messages = match Self::read_messages(&session_summary.path) {
                Ok(messages) => messages,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        path = %session_summary.path.display(),
                        "skipping unreadable prior session while loading input history"
                    );
                    continue;
                }
            };
            let mut session_user_messages: Vec<_> = messages
                .into_iter()
                .filter_map(|message| match message {
                    Message::User {
                        content,
                        kind: UserMessageKind::Explicit,
                    } => Some(content),
                    _ => None,
                })
                .collect();
            session_user_messages.reverse();
            for user_message in session_user_messages {
                if newest_user_messages.len() == limit {
                    break;
                }
                if seen_user_messages.insert(user_message.clone()) {
                    newest_user_messages.push(user_message);
                }
            }
            if newest_user_messages.len() == limit {
                break;
            }
        }
        newest_user_messages.reverse();
        Ok(newest_user_messages)
    }

    /// Read child-run lifecycle records from a parent session without
    /// modifying the file. Legacy records missing correlation metadata remain
    /// readable through serde defaults, but cannot be joined to a tool bubble.
    pub fn read_child_runs(path: &Path) -> Result<Vec<ChildRunRecord>, StoreError> {
        let content = std::fs::read_to_string(path).map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut runs = Vec::new();
        let mut starts = std::collections::HashMap::<Uuid, usize>::new();
        for line in content.lines() {
            let Ok(event) = serde_json::from_str::<StoreEvent>(line) else {
                continue;
            };
            match event {
                StoreEvent::ChildStart {
                    created_at,
                    id,
                    parent_session,
                    call_id,
                    agent_type,
                    task,
                    label,
                    ..
                } => {
                    starts.insert(id, runs.len());
                    runs.push(ChildRunRecord {
                        run_id: id,
                        call_id,
                        parent_session,
                        agent_type,
                        task,
                        label,
                        created_at,
                        completed_at: None,
                        status: None,
                        result: None,
                        sources: Vec::new(),
                        origin: FinishResultOrigin::default(),
                        duration: None,
                    });
                }
                StoreEvent::ChildResult {
                    completed_at,
                    id,
                    call_id,
                    status,
                    result,
                    sources,
                    origin,
                    ..
                } => {
                    if let Some(index) = starts.get(&id).copied() {
                        let run = &mut runs[index];
                        if run.call_id.is_nil() {
                            run.call_id = call_id;
                        }
                        run.duration = elapsed_duration(&run.created_at, &completed_at);
                        run.completed_at = Some(completed_at);
                        run.status = Some(status);
                        run.result = result;
                        run.sources = sources;
                        run.origin = origin;
                    }
                }
                _ => {}
            }
        }
        Ok(runs)
    }
}

fn elapsed_duration(started_at: &str, completed_at: &str) -> Option<std::time::Duration> {
    let started_at = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    let completed_at = chrono::DateTime::parse_from_rfc3339(completed_at).ok()?;
    completed_at.signed_duration_since(started_at).to_std().ok()
}

/// Parse only the first line of `path` as the session's `meta` header.
fn read_session_summary(path: &Path) -> Option<SessionSummary> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).ok()?;
    let mut first_line = String::new();
    std::io::BufReader::new(file)
        .read_line(&mut first_line)
        .ok()?;
    match serde_json::from_str::<StoreEvent>(first_line.trim_end()) {
        Ok(StoreEvent::Meta(meta)) => Some(SessionSummary {
            session_id: meta.session_id,
            created_at: meta.created_at,
            title: meta.title,
            provider_connection: meta.provider_connection,
            model: meta.model,
            parent_session: meta.parent_session,
            path: path.to_path_buf(),
        }),
        _ => None,
    }
}
