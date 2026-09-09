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

//! Append-only JSONL persistence for conversations and child-agent runs
//! (blueprint §4H).
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
//! makes the store process-crash-safe but not power-cut-safe (§4H decision
//! 10). A truncated trailing line left behind by a power cut is repaired on
//! the next resume (§4H decision 6).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::agent::{Message, UserMessageKind};
use crate::sandbox::SandboxContext;
use crate::ylva_home::ylva_root;

/// Version of the whole-file schema, written into the `meta` header. Bumped
/// only for a semantic break that needs an upgrader (§4H decision 4).
pub const SCHEMA_VERSION: u32 = 1;

/// Version stamped on every individual event line, so an old reader can take
/// an event-type-specific path even without the header (§4H decision 4).
pub const EVENT_VERSION: u32 = 1;

/// Maximum number of characters of the first user message kept as the
/// session's auto-generated `meta` title (§4H decision 8).
pub const TITLE_MAX_CHARS: usize = 80;

/// Capacity of the channel feeding the writer task. Large enough that a burst
/// of committed events (a turn plus its tool calls) never makes the agent loop
/// wait on disk.
const WRITER_CHANNEL_CAPACITY: usize = 256;

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
}

/// One persisted JSONL line.
///
/// Serialized with `#[serde(tag = "type")]` and **never**
/// `#[serde(deny_unknown_fields)]`, so a reader built against an older or
/// newer schema still loads the file: unknown fields are ignored and unknown
/// `type` values fail only that one line (§4H decision 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoreEvent {
    /// The session's version/metadata header — always the first line.
    Meta(SessionMeta),
    /// One committed conversation message.
    Message {
        #[serde(default)]
        v: u32,
        message: Message,
    },
    /// A child-agent run is about to start (§4G *Persistence*).
    ChildStart {
        #[serde(default)]
        v: u32,
        id: Uuid,
        parent_session: Uuid,
        agent_type: String,
        task: String,
        created_at: String,
    },
    /// A child-agent run finished (§4G *Persistence*).
    ChildResult {
        #[serde(default)]
        v: u32,
        id: Uuid,
        parent_session: Uuid,
        status: String,
        #[serde(default)]
        result: Option<String>,
        #[serde(default)]
        sources: Vec<String>,
        completed_at: String,
    },
    /// The provider-connection/model actually serving this session changed — written
    /// once on **every process start** (a fresh session or a resume alike,
    /// since resuming under a different `--profile`/config can resolve to a
    /// different provider or model than last time) and again after every
    /// successful `/models` switch mid-run. `meta.provider_connection`/
    /// `meta.model` only
    /// capture the pair resolved at creation; this event is the
    /// authoritative, replayable history of every value that has served the
    /// session since, including that first one.
    ModelChanged {
        #[serde(default)]
        v: u32,
        provider_connection: String,
        model: String,
        changed_at: String,
    },
}

/// The `meta` header record written as the first line of every session file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    #[serde(default)]
    pub v: u32,
    pub schema_version: u32,
    pub app_version: String,
    pub created_at: String,
    /// `project_key` of the launch directory — see [`project_key`].
    pub project_key: String,
    /// The provider-connection/model resolved for this session at creation time.
    /// Mirrors the very first [`StoreEvent::ModelChanged`] record; kept here
    /// too so a session's initial backend is readable from the header alone,
    /// without scanning the transcript. Any later change (a `/models`
    /// switch, or a different resolution on a fresh process start) is
    /// tracked only by `model_changed` — these two fields are never rewritten
    /// after creation.
    /// Named provider connection.
    pub provider_connection: String,
    pub model: String,
    pub session_id: Uuid,
    /// First user message, truncated to [`TITLE_MAX_CHARS`] and flattened to
    /// one line. Empty when the session committed no user turn.
    #[serde(default)]
    pub title: String,
    /// Set only for a child-agent's own session file, naming the parent's
    /// `session_id` (mirrors the parent-side `ChildStart`/`ChildResult`
    /// events so the link is discoverable from either file). `None` for
    /// every top-level session and for `/clear` rollovers, which start a
    /// fresh session but are not a child run — omitted from the JSON line
    /// entirely rather than serialized as `null`, so existing top-level
    /// sessions' header shape is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<Uuid>,
}

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

/// `~/.ylva/chats`, the production root of all persisted sessions.
///
/// The `~/.ylva` root comes from [`ylva_root`], i.e. the dedicated
/// `YLVA_HOME` environment variable before the operating system's home
/// directory. That variable is the seam an end-to-end test uses to redirect a
/// spawned `ylva` process's persistence into a temporary directory without
/// touching the real `HOME`.
pub fn default_chats_root() -> Result<PathBuf, StoreError> {
    let root = ylva_root().ok_or(StoreError::HomeDirectoryNotFound)?;
    Ok(root.join("chats"))
}

/// Stable content hash grouping every session launched from the same physical
/// directory (§4H decision 2).
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

/// Current UTC timestamp in RFC 3339 (`…Z`) shape, second precision — the one
/// timestamp format written to the store, so `created_at` values sort
/// lexicographically.
pub fn now_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Derive a session title from the first user message: single-line, trimmed,
/// and truncated to [`TITLE_MAX_CHARS`] characters (§4H decision 8).
fn title_from_first_user_message(text: &str) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    single_line.chars().take(TITLE_MAX_CHARS).collect()
}

/// Write `meta` as a brand-new file's first line and flush it immediately,
/// so it is durable on disk before anything else is ever appended. Returns
/// the number of bytes written (the JSON line plus its trailing newline),
/// which later locates exactly where the second line begins for the
/// title-finalizing rewrite in [`SessionWriter::finalize_title`].
fn write_meta_line(
    file: &mut LockedSessionFile,
    path: &Path,
    meta: &SessionMeta,
) -> Result<u64, StoreError> {
    let line =
        serde_json::to_string(&StoreEvent::Meta(meta.clone())).expect("meta always serializes");
    file.write_all(line.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.flush())
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(line.len() as u64 + 1)
}

/// Parse an existing file's first line as `meta`, alongside its raw byte
/// length (JSON plus trailing newline) — the counterpart read used on
/// [`Store::resume`] to figure out whether a still-empty title needs
/// finalizing and, if so, where the second line begins.
fn read_meta_line_with_length(path: &Path) -> Option<(SessionMeta, u64)> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).ok()?;
    let mut first_line = String::new();
    let bytes_read = std::io::BufReader::new(file)
        .read_line(&mut first_line)
        .ok()?;
    match serde_json::from_str::<StoreEvent>(first_line.trim_end()) {
        Ok(StoreEvent::Meta(meta)) => Some((meta, bytes_read as u64)),
        _ => None,
    }
}

/// Scan an existing file for the first committed **explicit user** message,
/// used on [`Store::resume`] to re-seed the title source in memory when the
/// on-disk title is still empty (the process that will finalize it may not
/// be the one that originally captured it).
fn first_explicit_user_message(path: &Path) -> Option<String> {
    Store::read_messages(path)
        .ok()?
        .into_iter()
        .find_map(|message| match message {
            Message::User {
                content,
                kind: UserMessageKind::Explicit,
            } => Some(content),
            _ => None,
        })
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
    event_tx: mpsc::Sender<StoreEvent>,
    flush_tx: mpsc::Sender<oneshot::Sender<()>>,
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
    /// held only in memory). The title is filled in later, once the first
    /// conversation turn completes, by an atomic rewrite of just that first
    /// line (§4H decision 8) — never by delaying the durability of anything
    /// that follows it.
    pub fn create(
        chats_root: &Path,
        cwd: &Path,
        session_id: Uuid,
        provider_connection: &str,
        model: &str,
    ) -> Result<Store, StoreError> {
        let project_key = project_key(cwd);
        let path = session_path(chats_root, &project_key, session_id);
        let mut file = open_locked_session_file(&path, session_id, false)?;
        let meta_awaiting_title = if file_length(&path)? == 0 {
            let meta = SessionMeta {
                v: EVENT_VERSION,
                schema_version: SCHEMA_VERSION,
                app_version: crate::build_info::BUILD_INFO.version.to_string(),
                created_at: now_timestamp(),
                project_key: project_key.clone(),
                provider_connection: provider_connection.to_string(),
                model: model.to_string(),
                session_id,
                title: String::new(),
                parent_session: None,
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
                pending_title_source: None,
            },
        ))
    }

    /// Re-attach an **existing** session file for append (`--conversation-id`).
    ///
    /// A session id with no matching file is a hard error
    /// ([`StoreError::SessionNotFound`]) — never a silent fresh-session
    /// fallback (§4H decision 5). A corrupt trailing line (a write cut short
    /// by a power loss) is truncated before the first append so the repaired
    /// file never carries mid-file corruption (§4H decision 6). If the prior
    /// run never got far enough to finalize the title (killed mid-turn), the
    /// title source is re-derived from the transcript so this run can still
    /// finish the job once its own first turn completes.
    pub fn resume(chats_root: &Path, cwd: &Path, session_id: Uuid) -> Result<Store, StoreError> {
        let project_key = project_key(cwd);
        let path = session_path(chats_root, &project_key, session_id);
        if !path.is_file() {
            return Err(StoreError::SessionNotFound { session_id, path });
        }
        let mut file = open_locked_session_file(&path, session_id, true)?;
        repair_trailing_corruption(&mut file, &path)?;
        let (meta_awaiting_title, pending_title_source) = match read_meta_line_with_length(&path) {
            Some((meta, meta_line_len)) if meta.title.is_empty() => {
                let title_source = first_explicit_user_message(&path);
                (Some((meta, meta_line_len)), title_source)
            }
            _ => (None, None),
        };
        Ok(Store::spawn_writer(
            session_id,
            project_key,
            chats_root.to_path_buf(),
            path,
            file,
            TitleState {
                meta_awaiting_title,
                pending_title_source,
            },
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
    ) -> Result<Store, StoreError> {
        let directory = project_directory(&self.inner.chats_root, &self.inner.project_key);
        let session_id = Uuid::new_v4();
        let path = directory.join(format!("{session_id}.jsonl"));
        let mut file = open_locked_session_file(&path, session_id, false)?;
        let meta = SessionMeta {
            v: EVENT_VERSION,
            schema_version: SCHEMA_VERSION,
            app_version: crate::build_info::BUILD_INFO.version.to_string(),
            created_at: now_timestamp(),
            project_key: self.inner.project_key.clone(),
            provider_connection: provider_connection.to_string(),
            model: model.to_string(),
            session_id,
            title: String::new(),
            parent_session,
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
                pending_title_source: None,
            },
        ))
    }

    fn spawn_writer(
        session_id: Uuid,
        project_key: String,
        chats_root: PathBuf,
        path: PathBuf,
        file: LockedSessionFile,
        title_state: TitleState,
    ) -> Store {
        let (event_tx, event_rx) = mpsc::channel::<StoreEvent>(WRITER_CHANNEL_CAPACITY);
        let (flush_tx, flush_rx) = mpsc::channel::<oneshot::Sender<()>>(WRITER_CHANNEL_CAPACITY);
        tokio::spawn(run_writer(SessionWriter {
            file,
            path: path.clone(),
            session_id,
            meta_awaiting_title: title_state.meta_awaiting_title,
            pending_title_source: title_state.pending_title_source,
            event_rx,
            flush_rx,
        }));
        Store {
            inner: Arc::new(StoreInner {
                session_id,
                project_key,
                chats_root,
                path,
                event_tx,
                flush_tx,
            }),
        }
    }

    pub fn session_id(&self) -> Uuid {
        self.inner.session_id
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

    /// Sending end of the writer channel. The agent loop enqueues committed
    /// events in commit order; the writer task appends them serially, so the
    /// on-disk order matches the commit order without the loop ever blocking
    /// on disk (§4H decision 10).
    pub fn sender(&self) -> mpsc::Sender<StoreEvent> {
        self.inner.event_tx.clone()
    }

    /// Enqueue one already-built event.
    pub fn record_event(&self, event: StoreEvent) {
        if let Err(error) = self.inner.event_tx.try_send(event) {
            tracing::warn!(
                session_id = %self.inner.session_id,
                %error,
                "dropping store event: writer channel unavailable"
            );
        }
    }

    /// Enqueue one committed conversation message.
    pub fn record_message(&self, message: &Message) {
        self.record_event(StoreEvent::Message {
            v: EVENT_VERSION,
            message: message.clone(),
        });
    }

    /// Enqueue a `model_changed` record: written on every process start
    /// (fresh session or resume) and again after every successful `/models`
    /// switch. See [`StoreEvent::ModelChanged`] for why this — not
    /// `meta.provider_connection`/`meta.model` — is the authoritative history.
    pub fn record_model_changed(&self, provider_connection: &str, model: &str) {
        self.record_event(StoreEvent::ModelChanged {
            v: EVENT_VERSION,
            provider_connection: provider_connection.to_string(),
            model: model.to_string(),
            changed_at: now_timestamp(),
        });
    }

    /// Enqueue the `child_start` record written to the **parent** session
    /// before a child run begins (§4G *Persistence*).
    pub fn record_child_start(&self, run_id: Uuid, agent_type: &str, task: &str) {
        self.record_event(StoreEvent::ChildStart {
            v: EVENT_VERSION,
            id: run_id,
            parent_session: self.inner.session_id,
            agent_type: agent_type.to_string(),
            task: task.to_string(),
            created_at: now_timestamp(),
        });
    }

    /// Enqueue the `child_result` record written to the **parent** session
    /// when a child run completes (§4G *Persistence*).
    pub fn record_child_result(
        &self,
        run_id: Uuid,
        status: &str,
        result: Option<String>,
        sources: Vec<String>,
    ) {
        self.record_event(StoreEvent::ChildResult {
            v: EVENT_VERSION,
            id: run_id,
            parent_session: self.inner.session_id,
            status: status.to_string(),
            result,
            sources,
            completed_at: now_timestamp(),
        });
    }

    /// Write a completed child-agent transcript **once**, into its own
    /// sub-session file under the same `project_key`, with its own `meta`
    /// header auto-titled from the child's task (§4H decision 9). Child
    /// transcripts are never streamed live.
    ///
    /// `provider_connection_and_model` is the child's own resolved engine —
    /// which can differ from the parent's. `None` (the child's engine
    /// configuration could not be resolved) falls back to an empty
    /// `meta.provider_connection`/`model`
    /// pair and skips the initial [`StoreEvent::ModelChanged`] record rather
    /// than persisting a misleading empty one.
    pub async fn write_child_transcript(
        &self,
        provider_connection_and_model: Option<(&str, &str)>,
        task: &str,
        messages: &[Message],
    ) -> Result<Uuid, StoreError> {
        let (provider_connection, model) = provider_connection_and_model.unwrap_or(("", ""));
        let child_store =
            self.start_new_session(provider_connection, model, Some(self.inner.session_id))?;
        if provider_connection_and_model.is_some() {
            child_store.record_model_changed(provider_connection, model);
        }
        child_store.record_event(StoreEvent::Message {
            v: EVENT_VERSION,
            message: Message::User {
                content: task.to_string(),
                kind: UserMessageKind::Explicit,
            },
        });
        for message in messages {
            child_store.record_message(message);
        }
        child_store.flush().await;
        Ok(child_store.session_id())
    }

    /// Barrier: resolves once every event enqueued before this call has been
    /// written and flushed to the OS. Per-line `flush()` already happens in
    /// the writer; this only lets a caller (a test, or a shutdown path) wait
    /// for the queue to drain.
    pub async fn flush(&self) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self.inner.flush_tx.send(ack_tx).await.is_err() {
            return;
        }
        let _ = ack_rx.await;
    }

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
    /// on disk (§4H decision 6).
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

fn file_length(path: &Path) -> Result<u64, StoreError> {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })
}

/// A session file holding its exclusive advisory lock for as long as it is
/// alive. On unix this is a real `flock(LOCK_EX | LOCK_NB)` (§4H decision 7);
/// on other platforms the file is opened unlocked, since `nix` — and with it
/// `flock` — is a unix-only dependency of this crate.
#[cfg(unix)]
type LockedSessionFile = nix::fcntl::Flock<std::fs::File>;
#[cfg(not(unix))]
type LockedSessionFile = std::fs::File;

/// Open (creating when missing) `path` and take the exclusive, non-blocking
/// per-session advisory lock. A conflicting open fails fast with
/// [`StoreError::SessionLocked`] instead of silently interleaving writes.
fn open_locked_session_file(
    path: &Path,
    session_id: Uuid,
    must_exist: bool,
) -> Result<LockedSessionFile, StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(!must_exist)
        .open(path)
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    lock_session_file(file, session_id)
}

#[cfg(unix)]
fn lock_session_file(
    file: std::fs::File,
    session_id: Uuid,
) -> Result<LockedSessionFile, StoreError> {
    use nix::fcntl::{Flock, FlockArg};

    Flock::lock(file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_file, _errno)| StoreError::SessionLocked { session_id })
}

#[cfg(not(unix))]
fn lock_session_file(
    file: std::fs::File,
    _session_id: Uuid,
) -> Result<LockedSessionFile, StoreError> {
    Ok(file)
}

/// Truncate a corrupt trailing JSON line (a write cut short by a power loss)
/// so the append-resume that follows can never leave corruption mid-file
/// (§4H decision 6). Only ever called on a resume/append open — a pure read
/// never mutates the file.
fn repair_trailing_corruption(file: &mut LockedSessionFile, path: &Path) -> Result<(), StoreError> {
    let content = std::fs::read_to_string(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut valid_end: u64 = 0;
    let mut offset: u64 = 0;
    let mut corrupt = false;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        let complete = line.ends_with('\n');
        // A blank line counts as intact: it carries no event, so nothing was
        // lost, and truncating at it would discard the valid lines after it.
        let intact = complete
            && (trimmed.trim().is_empty() || serde_json::from_str::<StoreEvent>(trimmed).is_ok());
        if intact {
            offset += line.len() as u64;
            valid_end = offset;
        } else {
            corrupt = true;
            break;
        }
    }
    if corrupt || valid_end != content.len() as u64 {
        tracing::warn!(
            path = %path.display(),
            truncated_bytes = content.len() as u64 - valid_end,
            "repairing corrupt trailing line of session file"
        );
        file.set_len(valid_end).map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    file.seek(SeekFrom::End(0))
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

/// The still-unfinalized-title state carried from [`Store::create`],
/// [`Store::resume`], or [`Store::start_new_session`] into the spawned
/// writer task — bundled into one type so [`Store::spawn_writer`] doesn't
/// need a separate parameter for each field.
struct TitleState {
    /// `Some((meta, meta_line_len))` until the title has been finalized:
    /// `meta` is the header as currently written to disk (title still
    /// empty), and `meta_line_len` is the byte length of that first line
    /// (JSON plus newline), i.e. where the second line begins — needed to
    /// rewrite just the first line in place once the title is known. `None`
    /// if this session's header already carried a title (e.g. on resume, a
    /// prior run already finished the job).
    meta_awaiting_title: Option<(SessionMeta, u64)>,
    /// The first committed **explicit user** message's content, captured as
    /// soon as it is seen (or re-derived from the transcript on resume), so
    /// the title can be filled in once the turn it belongs to completes.
    pending_title_source: Option<String>,
}

/// State owned by the writer task: the locked file plus both receiving ends.
struct SessionWriter {
    file: LockedSessionFile,
    path: PathBuf,
    session_id: Uuid,
    /// `Some((meta, meta_line_len))` until the title has been finalized:
    /// `meta` is the header as currently written to disk (title still
    /// empty), and `meta_line_len` is the byte length of that first line
    /// (JSON plus newline), i.e. where the second line begins — needed to
    /// rewrite just the first line in place once the title is known. `None`
    /// once finalized (or if this session's header already carried a title
    /// on resume, meaning a prior run already finished the job).
    meta_awaiting_title: Option<(SessionMeta, u64)>,
    /// The first committed **explicit user** message's content, captured as
    /// soon as it is seen (or re-derived from the transcript on resume), so
    /// the title can be filled in once the turn it belongs to completes.
    /// `None` until then; stays `None` for a turn that opens with only
    /// implicit/tool messages.
    pending_title_source: Option<String>,
    event_rx: mpsc::Receiver<StoreEvent>,
    flush_rx: mpsc::Receiver<oneshot::Sender<()>>,
}

/// The dedicated writer task: appends one JSON line per event and flushes it,
/// keeping every store write off the streaming/TUI hot path.
async fn run_writer(mut writer: SessionWriter) {
    loop {
        tokio::select! {
            biased;
            event = writer.event_rx.recv() => {
                match event {
                    Some(event) => writer.append(event),
                    None => break,
                }
            }
            ack = writer.flush_rx.recv() => {
                let Some(ack) = ack else { break };
                // Drain everything enqueued before the flush request so the
                // barrier really covers all prior events.
                while let Ok(event) = writer.event_rx.try_recv() {
                    writer.append(event);
                }
                let _ = ack.send(());
            }
        }
    }
    // Drain anything still queued after the senders were dropped.
    while let Ok(event) = writer.event_rx.try_recv() {
        writer.append(event);
    }
    writer.finalize_title_if_pending();
}

impl SessionWriter {
    /// Every event is written and flushed immediately, in commit order — no
    /// event is ever held back only in memory, so a crash right after commit
    /// never loses it. The `meta` header's title, however, may still be a
    /// placeholder on disk (`meta_awaiting_title` is `Some`): once the first
    /// conversation turn completes (the first committed `Message::Assistant`
    /// reply), the title is finalized by rewriting just that first line.
    fn append(&mut self, event: StoreEvent) {
        if self.meta_awaiting_title.is_some() && self.pending_title_source.is_none() {
            if let StoreEvent::Message {
                message:
                    Message::User {
                        content,
                        kind: UserMessageKind::Explicit,
                    },
                ..
            } = &event
            {
                self.pending_title_source = Some(content.clone());
            }
        }
        self.write_line(&event);
        let turn_is_complete = matches!(
            &event,
            StoreEvent::Message {
                message: Message::Assistant { .. },
                ..
            }
        );
        if turn_is_complete {
            self.finalize_title_if_pending();
        }
    }

    /// If the title hasn't been finalized yet and a title source is known,
    /// rewrite the `meta` line in place (temp file + atomic `rename()`, the
    /// same pattern used for schema-upgrade rewrites — §4H decision 4) so it
    /// carries the resolved title. A no-op once already finalized, and a
    /// no-op (left for a later turn or for [`Store::resume`] to retry) if no
    /// explicit user message has been seen yet.
    fn finalize_title_if_pending(&mut self) {
        let Some((mut meta, meta_line_len)) = self.meta_awaiting_title.take() else {
            return;
        };
        let Some(title_source) = &self.pending_title_source else {
            // No explicit user message seen yet (e.g. a turn that opened
            // with only implicit/tool messages) — keep waiting.
            self.meta_awaiting_title = Some((meta, meta_line_len));
            return;
        };
        meta.title = title_from_first_user_message(title_source);
        if let Err(error) = self.rewrite_meta_line(&meta, meta_line_len) {
            tracing::error!(path = %self.path.display(), %error, "failed to finalize session title");
            // Leave it unfinalized so a later turn, or the next resume, can
            // retry rather than losing the title forever.
            self.meta_awaiting_title = Some((meta, meta_line_len));
        }
    }

    /// Rewrite only the file's first line (the `meta` header) to carry the
    /// now-known title, leaving every line after it untouched. Done via a
    /// temp file in the same directory plus an atomic `rename()` over the
    /// original path, so a reader never observes a half-written file and a
    /// crash mid-rewrite never corrupts it — then the writer's own file
    /// handle and advisory lock are reacquired against the new inode so
    /// later appends keep landing at the same path.
    fn rewrite_meta_line(&mut self, meta: &SessionMeta, meta_line_len: u64) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(meta_line_len))?;
        let mut remainder = Vec::new();
        self.file.read_to_end(&mut remainder)?;

        let new_line =
            serde_json::to_string(&StoreEvent::Meta(meta.clone())).expect("meta always serializes");
        let temp_path = self.path.with_extension("jsonl.rewrite-tmp");
        {
            let mut temp_file = std::fs::File::create(&temp_path)?;
            temp_file.write_all(new_line.as_bytes())?;
            temp_file.write_all(b"\n")?;
            temp_file.write_all(&remainder)?;
            temp_file.flush()?;
        }
        std::fs::rename(&temp_path, &self.path)?;

        let mut new_file = open_locked_session_file(&self.path, self.session_id, true)
            .map_err(|_| std::io::Error::other("failed to re-lock session file after rewrite"))?;
        new_file.seek(SeekFrom::End(0))?;
        self.file = new_file;
        Ok(())
    }

    fn write_line(&mut self, event: &StoreEvent) {
        let line = match serde_json::to_string(event) {
            Ok(line) => line,
            Err(error) => {
                tracing::error!(path = %self.path.display(), %error, "failed to serialize store event");
                return;
            }
        };
        let file: &mut std::fs::File = &mut self.file;
        if let Err(error) = file
            .write_all(line.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.flush())
        {
            tracing::error!(path = %self.path.display(), %error, "failed to append store event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AssistantStatus, Message};

    fn read_events(path: &Path) -> Vec<StoreEvent> {
        std::fs::read_to_string(path)
            .expect("session file readable")
            .lines()
            .map(|line| serde_json::from_str::<StoreEvent>(line).expect("parseable store event"))
            .collect()
    }

    fn user_message(text: &str) -> Message {
        Message::User {
            content: text.to_string(),
            kind: UserMessageKind::Explicit,
        }
    }

    fn assistant_message(text: &str) -> Message {
        Message::Assistant {
            content: text.to_string(),
            status: AssistantStatus::Complete,
            tool_calls: Vec::new(),
        }
    }

    #[tokio::test]
    async fn round_trip_writes_meta_header_then_events_in_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let session_id = Uuid::new_v4();
        let store = Store::create(&chats_root, temp.path(), session_id, "openrouter", "gpt-4o")
            .expect("store created");

        store.record_message(&user_message("hello store"));
        store.record_message(&Message::Assistant {
            content: "hi there".to_string(),
            status: AssistantStatus::Complete,
            tool_calls: vec![],
        });
        store.flush().await;

        let events = read_events(store.path());
        assert_eq!(events.len(), 3, "meta + two messages: {events:?}");
        let StoreEvent::Meta(meta) = &events[0] else {
            panic!("first line must be the meta header, got {:?}", events[0]);
        };
        assert_eq!(meta.schema_version, SCHEMA_VERSION);
        assert_eq!(meta.session_id, session_id);
        assert_eq!(meta.project_key, project_key(temp.path()));
        assert_eq!(meta.title, "hello store");
        assert!(matches!(
            &events[1],
            StoreEvent::Message { message: Message::User { content, .. }, .. } if content == "hello store"
        ));
        assert!(matches!(
            &events[2],
            StoreEvent::Message { message: Message::Assistant { content, .. }, .. } if content == "hi there"
        ));
    }

    #[tokio::test]
    async fn a_user_message_is_durable_on_disk_before_any_reply_arrives() {
        // Every commit must be flushed to disk immediately, even though the
        // meta header's title is only finalized once the turn completes.
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
        )
        .expect("store created");

        store.record_message(&user_message("are you still there"));
        store.flush().await;

        let events = read_events(store.path());
        assert_eq!(events.len(), 2, "meta + the user message: {events:?}");
        let StoreEvent::Meta(meta) = &events[0] else {
            panic!("first line must be the meta header, got {:?}", events[0]);
        };
        assert_eq!(meta.title, "", "title stays empty until the turn completes");
        assert!(matches!(
            &events[1],
            StoreEvent::Message { message: Message::User { content, .. }, .. }
                if content == "are you still there"
        ));
    }

    #[tokio::test]
    async fn resuming_a_session_that_never_got_a_reply_still_finalizes_its_title() {
        // A prior run committed a user message and was killed before the
        // assistant replied: the title is still empty on disk. Resuming and
        // completing the turn should finalize it, re-deriving the title
        // source from the transcript rather than relying on in-memory state
        // from the crashed process.
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let session_id = Uuid::new_v4();
        let first_run = Store::create(&chats_root, temp.path(), session_id, "openrouter", "gpt-4o")
            .expect("store created");
        first_run.record_message(&user_message("orphaned question"));
        first_run.flush().await;
        drop(first_run);
        // Give the writer task a chance to observe the dropped senders and
        // release the advisory lock before re-opening the same session.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resumed = Store::resume(&chats_root, temp.path(), session_id).expect("resume succeeds");
        resumed.record_message(&assistant_message("late reply"));
        resumed.flush().await;

        let events = read_events(resumed.path());
        let StoreEvent::Meta(meta) = &events[0] else {
            panic!("first line must be the meta header, got {:?}", events[0]);
        };
        assert_eq!(meta.title, "orphaned question");
    }

    #[tokio::test]
    async fn context_forwards_every_committed_message_exactly_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
        )
        .expect("store created");

        let mut context = crate::agent::Context::new();
        context.set_store(store.clone());
        context.push_user("write a haiku".to_string());
        context.push_assistant("a haiku".to_string(), AssistantStatus::Complete);
        store.flush().await;

        let events = read_events(store.path());
        let messages: Vec<&Message> = events
            .iter()
            .filter_map(|event| match event {
                StoreEvent::Message { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(
            messages.len(),
            2,
            "the two system prompts installed by Context::new are not persisted, and each \
             committed message is forwarded exactly once: {messages:?}"
        );
        assert!(matches!(
            messages[0],
            Message::User { content, .. } if content == "write a haiku"
        ));
        assert!(matches!(
            messages[1],
            Message::Assistant { content, status: AssistantStatus::Complete, .. }
                if content == "a haiku"
        ));
    }

    #[tokio::test]
    async fn a_context_without_a_store_commits_messages_in_memory_only() {
        let mut context = crate::agent::Context::new();
        context.push_user("no persistence configured".to_string());
        assert!(context.messages().iter().any(|message| matches!(
            message,
            Message::User { content, .. } if content == "no persistence configured"
        )));
    }

    #[tokio::test]
    async fn create_takes_an_exclusive_lock_that_conflicting_opens_report() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let session_id = Uuid::new_v4();
        let first = Store::create(&chats_root, temp.path(), session_id, "openrouter", "gpt-4o")
            .expect("first open succeeds");
        first.record_message(&user_message("first"));
        first.flush().await;

        let conflict = Store::resume(&chats_root, temp.path(), session_id);
        assert!(
            matches!(conflict, Err(StoreError::SessionLocked { session_id: locked }) if locked == session_id),
            "second open must fail fast with a lock conflict, got {conflict:?}"
        );
    }

    #[tokio::test]
    async fn resume_of_unknown_session_id_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let missing = Uuid::new_v4();

        let result = Store::resume(&chats_root, temp.path(), missing);

        assert!(
            matches!(result, Err(StoreError::SessionNotFound { session_id, .. }) if session_id == missing),
            "resuming an unknown id must not create a new session, got {result:?}"
        );
        assert!(!session_path(&chats_root, &project_key(temp.path()), missing).exists());
    }

    #[tokio::test]
    async fn resume_appends_to_the_existing_file_without_overwriting_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let session_id = Uuid::new_v4();
        {
            let store = Store::create(&chats_root, temp.path(), session_id, "openrouter", "gpt-4o")
                .expect("store created");
            store.record_message(&user_message("first turn"));
            store.flush().await;
        }
        // Give the writer task a chance to observe the dropped senders and
        // release the advisory lock before re-opening the same session.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resumed = Store::resume(&chats_root, temp.path(), session_id).expect("resume succeeds");
        resumed.record_message(&user_message("second turn"));
        resumed.flush().await;

        let events = read_events(resumed.path());
        assert_eq!(events.len(), 3, "meta + both turns: {events:?}");
        assert!(matches!(&events[0], StoreEvent::Meta(_)));
        assert!(matches!(
            &events[1],
            StoreEvent::Message { message: Message::User { content, .. }, .. } if content == "first turn"
        ));
        assert!(matches!(
            &events[2],
            StoreEvent::Message { message: Message::User { content, .. }, .. } if content == "second turn"
        ));
    }

    #[tokio::test]
    async fn resume_truncates_a_corrupt_trailing_line_before_appending() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let session_id = Uuid::new_v4();
        {
            let store = Store::create(&chats_root, temp.path(), session_id, "openrouter", "gpt-4o")
                .expect("store created");
            store.record_message(&user_message("first turn"));
            store.flush().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let path = session_path(&chats_root, &project_key(temp.path()), session_id);
        let mut corrupted = std::fs::read_to_string(&path).expect("read");
        corrupted.push_str("{\"type\":\"message\",\"v\":1,\"mess");
        std::fs::write(&path, &corrupted).expect("write corrupt tail");

        let resumed =
            Store::resume(&chats_root, temp.path(), session_id).expect("resume repairs the tail");
        resumed.record_message(&user_message("after repair"));
        resumed.flush().await;

        let events = read_events(&path);
        assert_eq!(events.len(), 3, "corrupt tail dropped: {events:?}");
        assert!(matches!(
            &events[2],
            StoreEvent::Message { message: Message::User { content, .. }, .. } if content == "after repair"
        ));
    }

    #[tokio::test]
    async fn read_only_listing_never_mutates_a_file_with_a_corrupt_tail() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let session_id = Uuid::new_v4();
        {
            let store = Store::create(&chats_root, temp.path(), session_id, "openrouter", "gpt-4o")
                .expect("store created");
            store.record_message(&user_message("only turn"));
            store.flush().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let path = session_path(&chats_root, &project_key(temp.path()), session_id);
        let mut corrupted = std::fs::read_to_string(&path).expect("read");
        corrupted.push_str("{\"type\":\"mes");
        std::fs::write(&path, &corrupted).expect("write corrupt tail");
        let before = std::fs::read_to_string(&path).expect("read");

        let summaries =
            Store::list_sessions(&chats_root, &project_key(temp.path())).expect("listing succeeds");
        let messages = Store::read_messages(&path).expect("messages readable");

        assert_eq!(summaries.len(), 1);
        assert_eq!(messages.len(), 1, "corrupt tail skipped in memory");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            before,
            "a pure read must never mutate the session file"
        );
    }

    #[test]
    fn project_key_is_stable_for_one_directory_and_differs_across_directories() {
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .expect("subscriber mutex poisoned");
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).expect("mkdir");
        std::fs::create_dir_all(&second).expect("mkdir");

        assert_eq!(project_key(&first), project_key(&first));
        assert_ne!(project_key(&first), project_key(&second));
        assert_eq!(project_key(&first).len(), 64, "sha-256 hex digest");
    }

    #[cfg(unix)]
    #[test]
    fn project_key_is_identical_for_two_symlinks_to_the_same_directory() {
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .expect("subscriber mutex poisoned");
        let temp = tempfile::tempdir().expect("tempdir");
        let real = temp.path().join("real");
        let other = temp.path().join("other");
        std::fs::create_dir_all(&real).expect("mkdir");
        std::fs::create_dir_all(&other).expect("mkdir");
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        assert_eq!(
            project_key(&real),
            project_key(&link),
            "a symlink and its target are one physical directory"
        );
        assert_ne!(project_key(&real), project_key(&other));
    }

    #[tokio::test]
    async fn list_sessions_returns_newest_first_and_skips_headerless_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let key = project_key(temp.path());

        let older = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
        )
        .expect("older session");
        older.record_message(&user_message("older question"));
        older.record_message(&assistant_message("older reply"));
        older.flush().await;
        let newer = Store::create(&chats_root, temp.path(), Uuid::new_v4(), "ollama", "llama3")
            .expect("newer session");
        newer.record_message(&user_message("newer question"));
        newer.record_message(&assistant_message("newer reply"));
        newer.flush().await;
        // Force a deterministic ordering independent of the one-second
        // timestamp resolution.
        rewrite_meta_created_at(older.path(), "2020-01-01T00:00:00Z");
        rewrite_meta_created_at(newer.path(), "2030-01-01T00:00:00Z");
        std::fs::write(
            project_directory(&chats_root, &key).join("garbage.jsonl"),
            "not json at all\n",
        )
        .expect("write garbage file");

        let summaries = Store::list_sessions(&chats_root, &key).expect("listing succeeds");

        assert_eq!(summaries.len(), 2, "unreadable file skipped: {summaries:?}");
        assert_eq!(summaries[0].session_id, newer.session_id());
        assert_eq!(summaries[0].title, "newer question");
        assert_eq!(summaries[0].provider_connection, "ollama");
        assert_eq!(summaries[0].model, "llama3");
        assert_eq!(summaries[1].session_id, older.session_id());
    }

    #[tokio::test]
    async fn list_sessions_of_unknown_project_key_is_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let summaries = Store::list_sessions(&temp.path().join("chats"), "deadbeef")
            .expect("listing an unknown project is not an error");
        assert!(summaries.is_empty());
    }

    #[tokio::test]
    async fn child_records_link_to_the_parent_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let parent_id = Uuid::new_v4();
        let store = Store::create(&chats_root, temp.path(), parent_id, "openrouter", "gpt-4o")
            .expect("store created");
        let run_id = Uuid::new_v4();

        store.record_message(&user_message("delegate this"));
        store.record_child_start(run_id, "research", "investigate the bug");
        store.record_child_result(
            run_id,
            "success",
            Some("found it".to_string()),
            vec!["src/agent.rs".to_string()],
        );
        store.flush().await;

        let events = read_events(store.path());
        let start = events
            .iter()
            .find(|event| matches!(event, StoreEvent::ChildStart { .. }))
            .expect("a child_start record");
        let StoreEvent::ChildStart {
            id,
            parent_session,
            agent_type,
            task,
            ..
        } = start
        else {
            unreachable!()
        };
        assert_eq!(*id, run_id);
        assert_eq!(*parent_session, parent_id);
        assert_eq!(agent_type, "research");
        assert_eq!(task, "investigate the bug");

        let result = events
            .iter()
            .find(|event| matches!(event, StoreEvent::ChildResult { .. }))
            .expect("a child_result record");
        let StoreEvent::ChildResult {
            id,
            parent_session,
            status,
            sources,
            ..
        } = result
        else {
            unreachable!()
        };
        assert_eq!(*id, run_id);
        assert_eq!(*parent_session, parent_id);
        assert_eq!(status, "success");
        assert_eq!(sources, &["src/agent.rs".to_string()]);
    }

    #[tokio::test]
    async fn child_transcript_is_written_to_its_own_session_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
        )
        .expect("store created");

        let child_session_id = store
            .write_child_transcript(
                Some(("ollama", "llama3")),
                "summarize the module",
                &[Message::Assistant {
                    content: "done".to_string(),
                    status: AssistantStatus::Complete,
                    tool_calls: vec![],
                }],
            )
            .await
            .expect("child transcript written");

        let child_path = session_path(&chats_root, store.project_key(), child_session_id);
        let events = read_events(&child_path);
        let StoreEvent::Meta(meta) = &events[0] else {
            panic!("child transcript needs its own meta header")
        };
        assert_eq!(meta.title, "summarize the module");
        assert_eq!(meta.provider_connection, "ollama");
        assert_eq!(meta.model, "llama3");
        assert_eq!(
            meta.parent_session,
            Some(store.session_id()),
            "the child's own meta header names its parent, mirroring the \
             parent-side child_start/child_result records"
        );
        let StoreEvent::ModelChanged {
            provider_connection,
            model,
            ..
        } = &events[1]
        else {
            panic!("second line must record the child's initial provider/model: {events:?}")
        };
        assert_eq!(provider_connection, "ollama");
        assert_eq!(model, "llama3");
        assert_eq!(
            events.len(),
            4,
            "meta + model_changed + task + assistant reply: {events:?}"
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
    async fn record_model_changed_persists_every_switch_independent_of_meta() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        // Deliberately a different initial provider/model than the ones
        // switched to below, so the assertion can't pass by accident.
        let store = Store::create(&chats_root, temp.path(), Uuid::new_v4(), "ollama", "llama3")
            .expect("store created");

        store.record_model_changed("openrouter", "gpt-4o");
        store.record_model_changed("openrouter", "gpt-4o-mini");
        store.flush().await;

        let events = read_events(store.path());
        let StoreEvent::Meta(meta) = &events[0] else {
            panic!("first line must be the meta header")
        };
        assert_eq!(
            (meta.provider_connection.as_str(), meta.model.as_str()),
            ("ollama", "llama3"),
            "meta keeps the provider/model resolved at creation, never rewritten by later switches"
        );
        let changes: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                StoreEvent::ModelChanged {
                    provider_connection,
                    model,
                    ..
                } => Some((provider_connection, model)),
                _ => None,
            })
            .collect();
        assert_eq!(
            changes,
            vec![
                (&"openrouter".to_string(), &"gpt-4o".to_string()),
                (&"openrouter".to_string(), &"gpt-4o-mini".to_string()),
            ],
            "every switch is recorded, in order, independent of `meta`: {events:?}"
        );
    }

    #[tokio::test]
    async fn a_clear_rollovers_meta_header_has_no_parent_session_either() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
        )
        .expect("store created");
        store.record_message(&user_message("hi"));
        store.flush().await;

        // `/clear` rolls over to a fresh session but is not a child run.
        let rolled_over = store
            .start_new_session("openrouter", "gpt-4o", None)
            .expect("rollover session created");
        rolled_over.record_message(&user_message("hi again"));
        rolled_over.flush().await;

        let StoreEvent::Meta(meta) = &read_events(rolled_over.path())[0] else {
            panic!("first line must be the meta header")
        };
        assert_eq!(meta.parent_session, None);
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

    #[test]
    fn title_from_first_user_message_is_single_line_and_bounded() {
        let long = "x".repeat(TITLE_MAX_CHARS * 2);
        assert_eq!(
            title_from_first_user_message(&long).chars().count(),
            TITLE_MAX_CHARS
        );
        assert_eq!(
            title_from_first_user_message("  first\nsecond   third "),
            "first second third"
        );
    }

    #[tokio::test]
    async fn a_session_without_events_still_gets_its_meta_header() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
        )
        .expect("store created");
        let path = store.path().to_path_buf();
        drop(store);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = read_events(&path);
        assert!(matches!(events.as_slice(), [StoreEvent::Meta(_)]));
    }

    /// Rewrite the `created_at` of a session file's `meta` line so ordering
    /// assertions do not depend on wall-clock resolution.
    fn rewrite_meta_created_at(path: &Path, created_at: &str) {
        let content = std::fs::read_to_string(path).expect("read");
        let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
        let mut event: serde_json::Value =
            serde_json::from_str(&lines[0]).expect("meta line is JSON");
        event["created_at"] = serde_json::json!(created_at);
        lines[0] = serde_json::to_string(&event).expect("serialize");
        std::fs::write(path, format!("{}\n", lines.join("\n"))).expect("write");
    }
}
