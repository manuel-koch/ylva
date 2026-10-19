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

//! Append-only JSONL persistence for conversations and subagent runs.
//! See `docs/persistence.md` for the on-disk and replay contracts.
//!
//! On-disk layout:
//!
//! ```text
//! <chats_root>/<project_key>/<session_id>.jsonl
//! ```
//!
//! where `<chats_root>` is `~/.ylva/chats` in production
//! ([`default_chats_root`]) and `<project_key>` is the SHA-256 hex digest of
//! the canonicalized working directory the session was launched from
//! ([`project_key`]).
//!
//! Every line is one [`StoreEvent`]; the **first** line of a session file is
//! always the `meta` header. Only *committed* events are written — live
//! streaming fragments never touch disk. Writes go through a dedicated writer
//! task fed by an `mpsc` channel so the agent loop and the TUI never block on
//! disk I/O; each line is written and `flush()`ed (no per-line `fsync`), which
//! makes the store process-crash-safe but not power-cut-safe. A truncated
//! trailing line left behind by a power cut is repaired on the next resume.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::agent::Message;
use crate::sandbox::SandboxContext;
use crate::ylva_home::ylva_home;

mod archive;
mod clone;
mod error;
mod event;
mod maintenance;
mod replay;
mod session_file;
mod subagent;
mod title;
mod writer;

pub use error::StoreError;
pub use event::{
    now_timestamp, SessionMeta, StoreEvent, SubagentRunRecord, EVENT_VERSION, SCHEMA_VERSION,
    TITLE_MAX_CHARS,
};
pub use maintenance::{MaintenanceAction, MaintenanceReport, SessionSelection};
pub use replay::{SessionState, SessionSummary};

use session_file::{
    file_length, lock_project, open_locked_session_file, read_meta_line_with_length,
    repair_trailing_corruption, write_meta_line,
};
use title::{title_snapshot, TitleSnapshot, TitleState};
use writer::WriterCommand;

/// `~/.ylva/chats`, the production root of all persisted sessions.
///
/// The Ylva home comes from [`ylva_home`], i.e. the dedicated
/// `YLVA_HOME` environment variable before the operating system's home
/// directory. That variable is the seam an end-to-end test uses to redirect a
/// spawned `ylva` process's persistence into a temporary directory without
/// touching the real `HOME`.
pub fn default_chats_root() -> Result<PathBuf, StoreError> {
    let home = ylva_home().ok_or(StoreError::HomeDirectoryNotFound)?;
    Ok(home.join("chats"))
}

/// Stable content hash grouping every session launched from the same physical
/// directory.
///
/// The path is canonicalized first (symlinks resolved, `..` collapsed) with
/// the very same helper the sandbox uses for its workspace root, so two paths
/// that resolve to one directory — a symlink and its target, or macOS's
/// `/var` and `/private/var` — always produce the same key. Deliberately not
/// the git root: a plain, non-git directory is a first-class project.
pub fn project_key(cwd: &Path) -> String {
    let canonical = SandboxContext::canonicalize_best_effort(cwd);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_os_str().as_encoded_bytes());
    format!("{:x}", hasher.finalize())
}

/// Directory holding every session file of one project.
pub fn project_directory(chats_root: &Path, project_key: &str) -> PathBuf {
    chats_root.join(project_key)
}

/// Path of one session's JSONL file.
pub fn session_path(chats_root: &Path, project_key: &str, session_id: Uuid) -> PathBuf {
    project_directory(chats_root, project_key).join(format!("{session_id}.jsonl"))
}

/// A handle onto one open session file.
///
/// Cheap to clone — every clone shares the same writer task, the same
/// `flock`ed file handle, and therefore the same append order. The advisory
/// lock is released once the last clone is dropped and the writer task has
/// drained.
#[derive(Clone)]
pub struct Store {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    session_id: Uuid,
    project_key: String,
    chats_root: PathBuf,
    path: PathBuf,
    command_tx: mpsc::Sender<WriterCommand>,
    title_snapshot: Mutex<TitleSnapshot>,
    span: tracing::Span,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Store")
            .field("session_id", &self.inner.session_id)
            .field("path", &self.inner.path)
            .finish()
    }
}

impl Store {
    /// Open a **fresh** session file, taking the per-session advisory lock.
    ///
    /// The `meta` header is written immediately with an empty `title`, so
    /// every commit is durable the instant it is written (no event is ever
    /// held only in memory). A driver later supplies a generated title through
    /// [`Store::set_title`], which atomically rewrites just that first line
    /// without delaying later event durability.
    pub fn create(
        chats_root: &Path,
        cwd: &Path,
        session_id: Uuid,
        provider_connection: &str,
        model: &str,
        agent_type: Option<&str>,
    ) -> Result<Store, StoreError> {
        let project_key = project_key(cwd);
        let path = session_path(chats_root, &project_key, session_id);
        let _coordination = lock_project(archive::session_directory(&path)?)?;
        let mut file = open_locked_session_file(&path, session_id, false)?;
        let meta_awaiting_title = if file_length(&path)? == 0 {
            let meta = SessionMeta {
                version: EVENT_VERSION,
                created_at: now_timestamp(),
                schema_version: SCHEMA_VERSION,
                app_version: crate::build_info::BUILD_INFO.version.to_string(),
                project_key: project_key.clone(),
                provider_connection: provider_connection.to_string(),
                model: model.to_string(),
                session_id,
                title: String::new(),
                parent_session: None,
                agent_type: agent_type.map(str::to_string),
                cloned_from: None,
            };
            let meta_line_len = write_meta_line(&mut file, &path, &meta)?;
            Some((meta, meta_line_len))
        } else {
            None
        };
        Ok(Store::spawn_writer(
            session_id,
            project_key,
            chats_root.to_path_buf(),
            path,
            file,
            TitleState {
                meta_awaiting_title,
            },
            TitleSnapshot::default(),
        ))
    }

    /// Re-attach an **existing** session file for append (`--conversation-id`).
    ///
    /// A session id with no matching file is a hard error
    /// ([`StoreError::SessionNotFound`]) — never a silent fresh-session
    /// fallback. A corrupt trailing line (a write cut short
    /// by a power loss) is truncated before the first append so the repaired
    /// file never carries mid-file corruption. If the prior
    /// run never got far enough to generate the title (killed mid-turn), the
    /// title inputs are re-derived from the transcript so this run can retry
    /// generation immediately.
    pub fn resume(chats_root: &Path, cwd: &Path, session_id: Uuid) -> Result<Store, StoreError> {
        let project_key = project_key(cwd);
        let path = session_path(chats_root, &project_key, session_id);
        if !archive::transcript_exists(&path) {
            return Err(StoreError::SessionNotFound { session_id, path });
        }
        let mut file = open_locked_session_file(&path, session_id, true)?;
        repair_trailing_corruption(&mut file, &path)?;
        let (meta_awaiting_title, title_snapshot) = match read_meta_line_with_length(&path)? {
            Some((meta, meta_line_len)) => {
                let messages = Store::read_messages(&path)?;
                let snapshot = title_snapshot(&meta, &messages);
                let title_state = if meta.title.is_empty() {
                    Some((meta, meta_line_len))
                } else {
                    None
                };
                (title_state, snapshot)
            }
            None => (None, TitleSnapshot::default()),
        };
        Ok(Store::spawn_writer(
            session_id,
            project_key,
            chats_root.to_path_buf(),
            path,
            file,
            TitleState {
                meta_awaiting_title,
            },
            title_snapshot,
        ))
    }

    /// Start a brand-new session in the same project directory: used both by
    /// the `/clear` slash command (resets the conversation mid-run, no
    /// parent) and by [`Store::write_subagent_transcript`] (a subagent run,
    /// `parent_session` names this store's own `session_id`).
    pub fn start_new_session(
        &self,
        provider_connection: &str,
        model: &str,
        parent_session: Option<Uuid>,
        agent_type: Option<&str>,
    ) -> Result<Store, StoreError> {
        self.start_new_session_with_id(
            Uuid::new_v4(),
            provider_connection,
            model,
            parent_session,
            agent_type,
        )
    }

    fn start_new_session_with_id(
        &self,
        session_id: Uuid,
        provider_connection: &str,
        model: &str,
        parent_session: Option<Uuid>,
        agent_type: Option<&str>,
    ) -> Result<Store, StoreError> {
        let directory = project_directory(&self.inner.chats_root, &self.inner.project_key);
        let path = directory.join(format!("{session_id}.jsonl"));
        let _coordination = lock_project(&directory)?;
        let mut file = open_locked_session_file(&path, session_id, false)?;
        let meta = SessionMeta {
            version: EVENT_VERSION,
            created_at: now_timestamp(),
            schema_version: SCHEMA_VERSION,
            app_version: crate::build_info::BUILD_INFO.version.to_string(),
            project_key: self.inner.project_key.clone(),
            provider_connection: provider_connection.to_string(),
            model: model.to_string(),
            session_id,
            title: String::new(),
            parent_session,
            agent_type: agent_type.map(str::to_string),
            cloned_from: None,
        };
        let meta_line_len = write_meta_line(&mut file, &path, &meta)?;
        Ok(Store::spawn_writer(
            session_id,
            self.inner.project_key.clone(),
            self.inner.chats_root.clone(),
            path,
            file,
            TitleState {
                meta_awaiting_title: Some((meta, meta_line_len)),
            },
            TitleSnapshot::default(),
        ))
    }

    pub fn session_id(&self) -> Uuid {
        self.inner.session_id
    }

    pub(crate) fn span(&self) -> tracing::Span {
        self.inner.span.clone()
    }

    pub fn project_key(&self) -> &str {
        &self.inner.project_key
    }

    pub fn chats_root(&self) -> &Path {
        &self.inner.chats_root
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Enqueue one already-built event.
    pub fn record_event(&self, event: StoreEvent) {
        if let Err(error) = self
            .inner
            .command_tx
            .try_send(WriterCommand::Event(Box::new(event.clone())))
        {
            tracing::warn!(
                session_id = %self.inner.session_id,
                %error,
                "dropping store event: writer channel unavailable"
            );
            return;
        }
        if let StoreEvent::Message { message, .. } = event {
            self.update_title_snapshot(&message);
        }
    }

    /// Enqueue one committed conversation message.
    pub fn record_message(&self, message: &Message) {
        self.record_event(StoreEvent::Message {
            version: EVENT_VERSION,
            timestamp: now_timestamp(),
            message: message.clone(),
        });
    }

    /// Enqueue a `model_changed` record: written on every process start
    /// (fresh session or resume) and again after every successful `/models`
    /// switch. See [`StoreEvent::ModelChanged`] for why this — not
    /// `meta.provider_connection`/`meta.model` — is the authoritative history.
    pub fn record_model_changed(&self, provider_connection: &str, model: &str) {
        self.record_event(StoreEvent::ModelChanged {
            version: EVENT_VERSION,
            changed_at: now_timestamp(),
            provider_connection: provider_connection.to_string(),
            model: model.to_string(),
        });
    }

    /// Enqueue an `agent_type_changed` record: written when an explicit
    /// `--agent <type>` override on a resumed session changes the active
    /// root agent type. See [`StoreEvent::AgentTypeChanged`] for why this —
    /// not `meta.agent_type` — is the authoritative history.
    pub fn record_agent_type_changed(&self, agent_type: &str) {
        self.record_event(StoreEvent::AgentTypeChanged {
            version: EVENT_VERSION,
            changed_at: now_timestamp(),
            agent_type: agent_type.to_string(),
        });
    }

    /// Enqueue a `compaction` record: written by the outer loop when a
    /// background summary is applied to the model view. See
    /// [`StoreEvent::Compaction`] for the append-only supersession rules.
    /// `boundary` indexes the live model context, where the rebuilt leading
    /// system prompts occupy the first slots; `leading_system_len` is how many
    /// of those there are, so the event can also carry the cut in
    /// persisted-transcript coordinates for replay-only readers (the TUI).
    pub fn record_compaction(&self, boundary: usize, leading_system_len: usize, summary: &str) {
        self.record_event(StoreEvent::Compaction {
            version: EVENT_VERSION,
            compacted_at: now_timestamp(),
            boundary,
            transcript_boundary: Some(boundary.saturating_sub(leading_system_len)),
            summary: summary.to_string(),
        });
    }

    pub fn record_model_projection(
        &self,
        projection: crate::agent::compaction::ModelProjectionSnapshot,
    ) {
        self.record_event(StoreEvent::ModelProjection {
            projection: Box::new(projection),
        });
    }

    /// Barrier: resolves once every event enqueued before this call has been
    /// written and flushed to the OS. Per-line `flush()` already happens in
    /// the writer; this only lets a caller (a test, or a shutdown path) wait
    /// for the queue to drain.
    pub async fn flush(&self) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self
            .inner
            .command_tx
            .send(WriterCommand::Flush(ack_tx))
            .await
            .is_err()
        {
            return;
        }
        let _ = ack_rx.await;
    }
}

#[cfg(test)]
mod tests;
