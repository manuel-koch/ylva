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

//! Append-only JSONL persistence for conversations and child-agent runs.
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

use crate::agent::{Message, UserMessageKind};
use crate::child_agent_manager::FinishResultOrigin;
use crate::sandbox::SandboxContext;
use crate::ylva_home::ylva_home;

mod event;
mod replay;
mod session_file;
mod writer;

pub use event::{
    now_timestamp, ChildRunRecord, SessionMeta, StoreEvent, EVENT_VERSION, SCHEMA_VERSION,
    TITLE_MAX_CHARS,
};
pub use replay::{SessionState, SessionSummary};

use session_file::{
    append_event_line, file_length, open_locked_session_file, read_meta_line_with_length,
    repair_trailing_corruption, write_meta_line,
};
use writer::WriterCommand;

/// Errors surfaced by the store. Distinct variants let callers tell a lock
/// conflict, a missing session, and a plain I/O failure apart instead of
/// pattern-matching on an opaque error string.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("session '{session_id}' not found at {}", path.display())]
    SessionNotFound { session_id: Uuid, path: PathBuf },
    #[error("session '{session_id}' is open in another Ylva process")]
    SessionLocked { session_id: Uuid },
    #[error("cannot resolve the home directory for ~/.ylva")]
    HomeDirectoryNotFound,
    #[error("store I/O error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("session '{session_id}' writer is no longer available")]
    WriterClosed { session_id: Uuid },
    #[error(
        "session '{session_id}' is a child transcript at {}; only top-level sessions can be \
         cloned",
        path.display()
    )]
    SessionIsChild { session_id: Uuid, path: PathBuf },
    #[error(
        "session '{session_id}' at {} has no readable metadata header; it cannot be cloned",
        path.display()
    )]
    MissingSessionMeta { session_id: Uuid, path: PathBuf },
}

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

/// Rebuild the title inputs from committed transcript messages.
fn title_snapshot(meta: &SessionMeta, messages: &[Message]) -> TitleSnapshot {
    let mut snapshot = TitleSnapshot {
        title: meta.title.clone(),
        ..TitleSnapshot::default()
    };
    for message in messages {
        match message {
            Message::User {
                content,
                kind: UserMessageKind::Explicit,
            } if snapshot.first_user_message.is_none() => {
                snapshot.first_user_message = Some(content.clone());
            }
            Message::Assistant {
                content,
                status: crate::agent::AssistantStatus::Complete,
                ..
            } if snapshot.first_user_message.is_some()
                && snapshot.first_complete_assistant_message.is_none()
                && !content.trim().is_empty() =>
            {
                snapshot.first_complete_assistant_message = Some(content.clone());
            }
            _ => {}
        }
    }
    snapshot
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
        if !path.is_file() {
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

    /// Create a brand-new top-level session that replays `source_id`'s
    /// logical event stream — the `--clone` path.
    ///
    /// The source is read under a nonblocking exclusive lock held only for
    /// the read: another Ylva process holding the source (or a source that is
    /// a child transcript) fails before anything is created. The source file
    /// itself is never repaired, rewritten, or appended to. The clone gets a
    /// fresh session id, its own `meta` header naming the source in
    /// `cloned_from` with `parent_session` unset, and every copied event
    /// except the old header — with `child_start`/`child_result` records
    /// rebased onto the new id. Child transcript files stay where they are
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
        if !source_path.is_file() {
            return Err(StoreError::SessionNotFound {
                session_id: source_id,
                path: source_path,
            });
        }

        let (source_meta, events) = {
            // Exclusive nonblock: a live resume of the source — or a
            // concurrent clone of it — makes this fail without touching it.
            let _source_lock = open_locked_session_file(&source_path, source_id, true)?;
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
                return Err(StoreError::SessionIsChild {
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
                // resume of the clone resolves its child runs from its own
                // session id. The child transcript files themselves are
                // untouched and stay shared read-only history.
                StoreEvent::ChildStart {
                    version,
                    created_at,
                    id,
                    parent_session,
                    call_id,
                    agent_type,
                    task,
                    label,
                } if parent_session == source_id => StoreEvent::ChildStart {
                    version,
                    created_at,
                    id,
                    parent_session: clone_id,
                    call_id,
                    agent_type,
                    task,
                    label,
                },
                StoreEvent::ChildResult {
                    version,
                    completed_at,
                    id,
                    parent_session,
                    call_id,
                    status,
                    result,
                    sources,
                    origin,
                } if parent_session == source_id => StoreEvent::ChildResult {
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

    /// Start a brand-new session in the same project directory: used both by
    /// the `/clear` slash command (resets the conversation mid-run, no
    /// parent) and by [`Store::write_child_transcript`] (a child-agent run,
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

    /// Return the currently persisted session title, or an empty string while
    /// title generation is still pending.
    pub fn title(&self) -> String {
        self.inner
            .title_snapshot
            .lock()
            .expect("title snapshot mutex is not poisoned")
            .title
            .clone()
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

    /// Return the utility-engine prompt once the first explicit user message
    /// and first non-empty complete assistant reply have both been committed.
    pub fn title_prompt(&self) -> Option<String> {
        let snapshot = self
            .inner
            .title_snapshot
            .lock()
            .expect("title snapshot mutex is not poisoned");
        if !snapshot.title.is_empty() {
            tracing::debug!(
                session_id = %self.inner.session_id,
                stage = "skipped",
                reason = "title_already_set",
                "session title generation skipped"
            );
            return None;
        }
        let Some(user_message) = snapshot.first_user_message.as_deref() else {
            tracing::debug!(
                session_id = %self.inner.session_id,
                stage = "skipped",
                reason = "awaiting_explicit_user_message",
                "session title generation is not ready"
            );
            return None;
        };
        let Some(assistant_message) = snapshot.first_complete_assistant_message.as_deref() else {
            tracing::debug!(
                session_id = %self.inner.session_id,
                stage = "skipped",
                reason = "awaiting_complete_assistant_reply",
                "session title generation is not ready"
            );
            return None;
        };
        let prompt = format!(
            "Generate a concise title for this conversation.\n\
             Return only one bare title, with no markdown, quotation marks, or explanation.\
             \n\n<user_message>\n{user_message}\n</user_message>\n\
             <assistant_message>\n{assistant_message}\n</assistant_message>",
        );
        tracing::debug!(
            session_id = %self.inner.session_id,
            stage = "prompt_ready",
            user_message_chars = user_message.chars().count(),
            assistant_message_chars = assistant_message.chars().count(),
            prompt_chars = prompt.chars().count(),
            "session title prompt is ready"
        );
        Some(prompt)
    }

    /// Enqueue a metadata-only title rewrite on the session writer.
    pub async fn set_title(&self, title: String) -> Result<(), StoreError> {
        if !self
            .inner
            .title_snapshot
            .lock()
            .expect("title snapshot mutex is not poisoned")
            .title
            .is_empty()
        {
            return Ok(());
        }
        let (result_tx, result_rx) = oneshot::channel();
        let title_for_snapshot = title.clone();
        self.inner
            .command_tx
            .send(WriterCommand::SetTitle { title, result_tx })
            .await
            .map_err(|_| StoreError::WriterClosed {
                session_id: self.inner.session_id,
            })?;
        let result = result_rx.await.map_err(|_| StoreError::WriterClosed {
            session_id: self.inner.session_id,
        })?;
        result?;
        let mut snapshot = self
            .inner
            .title_snapshot
            .lock()
            .expect("title snapshot mutex is not poisoned");
        snapshot.title = title_for_snapshot;
        Ok(())
    }

    fn update_title_snapshot(&self, message: &Message) {
        let mut snapshot = self
            .inner
            .title_snapshot
            .lock()
            .expect("title snapshot mutex is not poisoned");
        match message {
            Message::User {
                content,
                kind: UserMessageKind::Explicit,
            } if snapshot.first_user_message.is_none() => {
                snapshot.first_user_message = Some(content.clone());
            }
            Message::Assistant {
                content,
                status: crate::agent::AssistantStatus::Complete,
                ..
            } if snapshot.first_user_message.is_some()
                && snapshot.first_complete_assistant_message.is_none()
                && !content.trim().is_empty() =>
            {
                snapshot.first_complete_assistant_message = Some(content.clone());
            }
            _ => {}
        }
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

    /// Enqueue the `child_start` record written to the **parent** session
    /// before a child run begins.
    pub fn record_child_start(
        &self,
        run_id: Uuid,
        call_id: Uuid,
        agent_type: &str,
        task: &str,
        label: &str,
    ) {
        self.record_event(StoreEvent::ChildStart {
            version: EVENT_VERSION,
            created_at: now_timestamp(),
            id: run_id,
            parent_session: self.inner.session_id,
            call_id,
            agent_type: agent_type.to_string(),
            task: task.to_string(),
            label: label.to_string(),
        });
    }

    /// Enqueue the `child_result` record written to the **parent** session
    /// when a child run completes.
    pub fn record_child_result(
        &self,
        run_id: Uuid,
        call_id: Uuid,
        status: &str,
        result: Option<String>,
        sources: Vec<String>,
        origin: FinishResultOrigin,
    ) {
        self.record_event(StoreEvent::ChildResult {
            version: EVENT_VERSION,
            completed_at: now_timestamp(),
            id: run_id,
            parent_session: self.inner.session_id,
            call_id,
            status: status.to_string(),
            result,
            sources,
            origin,
        });
    }

    /// Write a completed child-agent transcript **once**, into its own
    /// sub-session file under the same `project_key`, with its own `meta`
    /// header titled asynchronously from the first explicit user message and
    /// complete assistant reply in `messages`. Child
    /// transcripts are never streamed live.
    ///
    /// `provider_connection_and_model` is the child's own resolved engine —
    /// which can differ from the parent's. `None` (the child's engine
    /// configuration could not be resolved) falls back to an empty
    /// `meta.provider_connection`/`model`
    /// pair and skips the initial [`StoreEvent::ModelChanged`] record rather
    /// than persisting a misleading empty one. The supplied messages retain
    /// their order and are persisted as given.
    pub async fn write_child_transcript(
        &self,
        provider_connection_and_model: Option<(&str, &str)>,
        messages: &[Message],
    ) -> Result<Uuid, StoreError> {
        Ok(self
            .write_child_transcript_store(provider_connection_and_model, messages)
            .await?
            .session_id())
    }

    /// Write a child transcript and retain its store handle for follow-up
    /// metadata operations such as utility-engine title generation.
    pub async fn write_child_transcript_store(
        &self,
        provider_connection_and_model: Option<(&str, &str)>,
        messages: &[Message],
    ) -> Result<Store, StoreError> {
        let (provider_connection, model) = provider_connection_and_model.unwrap_or(("", ""));
        let child_store = self.start_new_session(
            provider_connection,
            model,
            Some(self.inner.session_id),
            None,
        )?;
        if provider_connection_and_model.is_some() {
            child_store.record_model_changed(provider_connection, model);
        }
        child_store.record_child_transcript_messages(messages);
        child_store.flush().await;
        Ok(child_store)
    }

    /// Write a child transcript using the child run id as its session id.
    /// Parent `child_start` records already persist that run id, so this stable
    /// path lets replay locate the transcript without adding another
    /// cross-file identifier to the parent event schema.
    pub async fn write_child_transcript_for_run(
        &self,
        run_id: Uuid,
        provider_connection_and_model: Option<(&str, &str)>,
        messages: &[Message],
    ) -> Result<Uuid, StoreError> {
        Ok(self
            .write_child_transcript_for_run_store(run_id, provider_connection_and_model, messages)
            .await?
            .session_id())
    }

    /// Write a child transcript using the run id and retain its store handle
    /// for follow-up metadata operations.
    pub async fn write_child_transcript_for_run_store(
        &self,
        run_id: Uuid,
        provider_connection_and_model: Option<(&str, &str)>,
        messages: &[Message],
    ) -> Result<Store, StoreError> {
        let (provider_connection, model) = provider_connection_and_model.unwrap_or(("", ""));
        let child_store = self.start_new_session_with_id(
            run_id,
            provider_connection,
            model,
            Some(self.inner.session_id),
            None,
        )?;
        if provider_connection_and_model.is_some() {
            child_store.record_model_changed(provider_connection, model);
        }
        child_store.record_child_transcript_messages(messages);
        child_store.flush().await;
        Ok(child_store)
    }

    fn record_child_transcript_messages(&self, messages: &[Message]) {
        for message in messages {
            self.record_message(message);
        }
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

/// The title rewrite state carried into the spawned writer task.
struct TitleState {
    /// `Some((meta, meta_line_len))` until a generated title is persisted:
    /// `meta` is the header as currently written to disk (title still
    /// empty), and `meta_line_len` is the byte length of that first line
    /// (JSON plus newline), i.e. where the second line begins — needed to
    /// rewrite just the first line once the title is known. `None` if this
    /// session's header already carried a title.
    meta_awaiting_title: Option<(SessionMeta, u64)>,
}

#[derive(Default)]
struct TitleSnapshot {
    title: String,
    first_user_message: Option<String>,
    first_complete_assistant_message: Option<String>,
}

#[cfg(test)]
mod tests;
