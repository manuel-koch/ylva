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
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::agent::{Message, UserMessageKind};
use crate::child_agent_manager::FinishResultOrigin;
use crate::sandbox::SandboxContext;
use crate::ylva_home::ylva_home;

/// Version of the whole-file schema, written into the `meta` header. Bumped
/// only for a semantic break that needs an upgrader (§4H decision 4).
pub const SCHEMA_VERSION: u32 = 1;

/// Version stamped on every individual event line, so an old reader can take
/// an event-type-specific path even without the header (§4H decision 4).
pub const EVENT_VERSION: u32 = 1;

/// Maximum number of characters kept in a generated session `meta` title.
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
        #[serde(default, alias = "v")]
        version: u32,
        /// The instant this message became part of the transcript (commit
        /// time — for a user message effectively "sent", for an assistant
        /// message "response fully assembled", since `Context::commit` is
        /// the single point every message passes through). Empty for lines
        /// written before this field existed; `#[serde(default)]` reads
        /// those back as `""` rather than failing.
        #[serde(default)]
        timestamp: String,
        message: Message,
    },
    /// A child-agent run is about to start (§4G *Persistence*).
    ChildStart {
        #[serde(default, alias = "v")]
        version: u32,
        created_at: String,
        id: Uuid,
        parent_session: Uuid,
        #[serde(default)]
        call_id: Uuid,
        agent_type: String,
        task: String,
        #[serde(default)]
        label: String,
    },
    /// A child-agent run finished (§4G *Persistence*).
    ChildResult {
        #[serde(default, alias = "v")]
        version: u32,
        completed_at: String,
        id: Uuid,
        parent_session: Uuid,
        #[serde(default)]
        call_id: Uuid,
        status: String,
        #[serde(default)]
        result: Option<String>,
        #[serde(default)]
        sources: Vec<String>,
        #[serde(default)]
        origin: FinishResultOrigin,
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
        #[serde(default, alias = "v")]
        version: u32,
        changed_at: String,
        provider_connection: String,
        model: String,
    },
    /// The root agent type serving this session changed — written when an
    /// explicit `--agent <type>` override on a resumed session differs from
    /// the type the session was already using. `meta.agent_type` only
    /// captures the type resolved at creation, so — exactly like
    /// [`StoreEvent::ModelChanged`] for the model — this event is the
    /// authoritative, replayable history of every later selection, and the
    /// last one wins when a later run resolves the session's type.
    AgentTypeChanged {
        #[serde(default, alias = "v")]
        version: u32,
        changed_at: String,
        agent_type: String,
    },
    /// The model-facing context was compacted through `boundary`: transcript
    /// messages `[..boundary]` are represented from here on by `summary`,
    /// while every one of them stays in this append-only file (blueprint §4C
    /// *record vs view*).
    ///
    /// Appended, never rewritten: a later event with a larger boundary
    /// supersedes an earlier one, so replay keeps the latest snapshot and the
    /// verbatim tail after its boundary.
    Compaction {
        #[serde(default, alias = "v")]
        version: u32,
        compacted_at: String,
        /// First transcript index kept verbatim as the tail, counted in the
        /// live model context (the rebuilt leading system prompts included).
        boundary: usize,
        /// The same cut expressed in the **persisted transcript**: the leading
        /// system prompts, which are rebuilt on every run and never written to
        /// this file, are excluded. The TUI positions its compaction marker
        /// with this value because it replays the file without building a
        /// model context. `None` for events written before the marker
        /// existed — the TUI then shows no marker for that snapshot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript_boundary: Option<usize>,
        summary: String,
    },
}

/// The `meta` header record written as the first line of every session file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    #[serde(default, alias = "v")]
    pub version: u32,
    pub created_at: String,
    pub schema_version: u32,
    pub app_version: String,
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
    /// Utility-engine-generated one-line title, truncated to
    /// [`TITLE_MAX_CHARS`]. Empty while title generation is pending or when
    /// the session has no completed turn.
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
    /// The root agent type resolved for this session at creation time (or
    /// when `/clear` started its replacement session). `None` for sessions
    /// written before this field existed — those read back as the `default`
    /// agent type. Later selections are tracked only by
    /// [`StoreEvent::AgentTypeChanged`]; this field is never rewritten after
    /// creation, and it is omitted from the JSON line while unset so existing
    /// headers keep their shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// The session this one was created from by `--clone`. `None` for every
    /// session that was not cloned — omitted from the JSON line rather than
    /// serialized as `null`, so existing headers keep their shape. The
    /// source file is never rewritten, so this link is one-way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<Uuid>,
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

/// Current UTC timestamp in RFC 3339 (`…Z`) shape, millisecond precision —
/// the one timestamp format written to the store, so `created_at`/
/// `changed_at`/`completed_at`/`timestamp` values sort lexicographically.
/// Millisecond (not second) precision so back-to-back events — e.g. a fast
/// child's `child_start`/`child_result` pair, or several tool-call turns in
/// one second — don't collide on the same value.
pub fn now_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Write `meta` as a brand-new file's first line and flush it immediately,
/// so it is durable on disk before anything else is ever appended. Returns
/// the number of bytes written (the JSON line plus its trailing newline),
/// which later locates exactly where the second line begins for the
/// metadata-only title rewrite.
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
/// [`Store::resume`] to figure out whether a still-empty title can be
/// rewritten and, if so, where the second line begins.
fn read_meta_line_with_length(path: &Path) -> Result<Option<(SessionMeta, u64)>, StoreError> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut first_line = String::new();
    let bytes_read = std::io::BufReader::new(file)
        .read_line(&mut first_line)
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(
        match serde_json::from_str::<StoreEvent>(first_line.trim_end()) {
            Ok(StoreEvent::Meta(meta)) => Some((meta, bytes_read as u64)),
            _ => None,
        },
    )
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
    /// fallback (§4H decision 5). A corrupt trailing line (a write cut short
    /// by a power loss) is truncated before the first append so the repaired
    /// file never carries mid-file corruption (§4H decision 6). If the prior
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

    fn spawn_writer(
        session_id: Uuid,
        project_key: String,
        chats_root: PathBuf,
        path: PathBuf,
        file: LockedSessionFile,
        title_state: TitleState,
        title_snapshot: TitleSnapshot,
    ) -> Store {
        let (command_tx, command_rx) = mpsc::channel::<WriterCommand>(WRITER_CHANNEL_CAPACITY);
        tokio::spawn(run_writer(SessionWriter {
            file,
            path: path.clone(),
            session_id,
            meta_awaiting_title: title_state.meta_awaiting_title,
            command_rx,
        }));
        Store {
            inner: Arc::new(StoreInner {
                session_id,
                project_key,
                chats_root,
                path,
                command_tx,
                title_snapshot: Mutex::new(title_snapshot),
            }),
        }
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

    /// Enqueue the `child_start` record written to the **parent** session
    /// before a child run begins (§4G *Persistence*).
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
    /// when a child run completes (§4G *Persistence*).
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
                    let is_newer = compaction
                        .as_ref()
                        .is_none_or(|current| *boundary >= current.boundary);
                    if is_newer {
                        compaction = Some(crate::agent::compaction::CompactionSnapshot {
                            boundary: *boundary,
                            summary: summary.clone(),
                        });
                        compaction_transcript_boundary = *transcript_boundary;
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

/// Joined child lifecycle metadata reconstructed from a parent session.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildRunRecord {
    pub run_id: Uuid,
    pub call_id: Uuid,
    pub parent_session: Uuid,
    pub agent_type: String,
    pub task: String,
    pub label: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub status: Option<String>,
    pub result: Option<String>,
    pub sources: Vec<String>,
    pub origin: FinishResultOrigin,
    pub duration: Option<std::time::Duration>,
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

/// Append one already-built event line to an open session file and flush it.
///
/// The counterpart of [`write_meta_line`] used by [`Store::clone_session`],
/// which writes the whole replay before handing the file to the writer task.
fn append_event_line(
    file: &mut LockedSessionFile,
    path: &Path,
    event: &StoreEvent,
) -> Result<(), StoreError> {
    let line = serde_json::to_string(event).expect("event always serializes");
    file.write_all(line.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.flush())
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })
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

enum WriterCommand {
    Event(Box<StoreEvent>),
    SetTitle {
        title: String,
        result_tx: oneshot::Sender<Result<(), StoreError>>,
    },
    Flush(oneshot::Sender<()>),
}

/// State owned by the writer task: the locked file and command receiver.
struct SessionWriter {
    file: LockedSessionFile,
    path: PathBuf,
    session_id: Uuid,
    /// `Some((meta, meta_line_len))` until a generated title is persisted:
    /// `meta` is the header as currently written to disk (title still
    /// empty), and `meta_line_len` is the byte length of that first line
    /// (JSON plus newline), i.e. where the second line begins — needed to
    /// rewrite just the first line once the title is known. `None` once
    /// finalized (or if this session's header already carried a title).
    meta_awaiting_title: Option<(SessionMeta, u64)>,
    command_rx: mpsc::Receiver<WriterCommand>,
}

/// The dedicated writer task: appends one JSON line per event and flushes it,
/// keeping every store write off the streaming/TUI hot path.
async fn run_writer(mut writer: SessionWriter) {
    loop {
        tokio::select! {
            command = writer.command_rx.recv() => {
                match command {
                    Some(WriterCommand::Event(event)) => writer.append(*event),
                    Some(WriterCommand::SetTitle { title, result_tx }) => {
                        let _ = result_tx.send(writer.set_title(&title));
                    }
                    Some(WriterCommand::Flush(ack)) => {
                        // Drain everything enqueued before the flush request
                        // so the barrier covers events and title commands.
                        while let Ok(command) = writer.command_rx.try_recv() {
                            match command {
                                WriterCommand::Event(event) => writer.append(*event),
                                WriterCommand::SetTitle { title, result_tx } => {
                                    let _ = result_tx.send(writer.set_title(&title));
                                }
                                WriterCommand::Flush(nested_ack) => {
                                    let _ = nested_ack.send(());
                                }
                            }
                        }
                        let _ = ack.send(());
                    }
                    None => break,
                }
            }
        }
    }
    // Drain anything still queued after the senders were dropped.
    while let Ok(command) = writer.command_rx.try_recv() {
        match command {
            WriterCommand::Event(event) => writer.append(*event),
            WriterCommand::SetTitle { title, result_tx } => {
                let _ = result_tx.send(writer.set_title(&title));
            }
            WriterCommand::Flush(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

impl SessionWriter {
    /// Every event is written and flushed immediately, in commit order.
    fn append(&mut self, event: StoreEvent) {
        self.write_line(&event);
    }

    /// Rewrite the `meta` line in place (temp file + atomic `rename()`, the
    /// same pattern used for schema-upgrade rewrites — §4H decision 4) so it
    /// carries the supplied generated title. A no-op once already finalized.
    fn set_title(&mut self, title: &str) -> Result<(), StoreError> {
        let Some((mut meta, meta_line_len)) = self.meta_awaiting_title.take() else {
            return Ok(());
        };
        let title = title.trim();
        if title.is_empty() {
            self.meta_awaiting_title = Some((meta, meta_line_len));
            return Ok(());
        }
        let title_chars = title.chars().count();
        tracing::debug!(
            session_id = %self.session_id,
            stage = "persist_started",
            title_chars,
            "rewriting session metadata with generated title"
        );
        meta.title = title.chars().take(TITLE_MAX_CHARS).collect();
        if let Err(error) = self.rewrite_meta_line(&meta, meta_line_len) {
            tracing::error!(
                session_id = %self.session_id,
                path = %self.path.display(),
                stage = "persist_failed",
                %error,
                "failed to persist session title"
            );
            self.meta_awaiting_title = Some((meta, meta_line_len));
            return Err(StoreError::Io {
                path: self.path.clone(),
                source: error,
            });
        }
        tracing::debug!(
            session_id = %self.session_id,
            stage = "persisted",
            title_chars,
            "session title metadata rewrite completed"
        );
        Ok(())
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
    use crate::agent::{AssistantStatus, Message, ToolCallRequest};

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
        let store = Store::create(
            &chats_root,
            temp.path(),
            session_id,
            "openrouter",
            "gpt-4o",
            None,
        )
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
        assert_eq!(
            meta.title, "",
            "the store does not copy conversation text into the title"
        );
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
            None,
        )
        .expect("store created");

        store.record_message(&user_message("are you still there"));
        store.flush().await;

        assert!(
            store.title_prompt().is_none(),
            "a user message without a completed reply is not title-ready"
        );
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
    async fn an_interrupted_assistant_reply_does_not_make_title_ready() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("store created");

        store.record_message(&user_message("unfinished question"));
        store.record_message(&Message::Assistant {
            content: "partial reply".to_string(),
            status: AssistantStatus::Interrupted,
            tool_calls: Vec::new(),
        });
        store.flush().await;

        assert!(
            store.title_prompt().is_none(),
            "interrupted replies must not trigger title generation"
        );
    }

    #[tokio::test]
    async fn an_empty_tool_call_completion_does_not_hide_the_first_text_reply() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("store created");

        store.record_message(&user_message("count the files"));
        store.record_message(&Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id: Uuid::new_v4(),
                name: "glob".to_string(),
                arguments: serde_json::json!({"pattern": "**/*"}),
                persisted_title_arguments: Vec::new(),
            }],
        });
        assert!(
            store.title_prompt().is_none(),
            "an assistant tool-call envelope is not the completed text reply"
        );

        let session_id = store.session_id();
        store.flush().await;
        drop(store);
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resumed = Store::resume(&chats_root, temp.path(), session_id).expect("resume succeeds");
        resumed.record_message(&assistant_message("There are 42 files."));
        let prompt = resumed
            .title_prompt()
            .expect("the first text reply should make the title ready");
        assert!(prompt.contains("<assistant_message>\nThere are 42 files."));
        assert!(!prompt.contains("<assistant_message>\n</assistant_message>"));
    }

    #[tokio::test]
    async fn resuming_a_session_rebuilds_title_prompt_inputs() {
        // A prior run committed a user message and was killed before the
        // assistant replied: the title is still empty on disk. Resuming and
        // completing the turn should rebuild both prompt inputs from the
        // transcript rather than relying on in-memory state from the crashed
        // process.
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let session_id = Uuid::new_v4();
        let first_run = Store::create(
            &chats_root,
            temp.path(),
            session_id,
            "openrouter",
            "gpt-4o",
            None,
        )
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

        let prompt = resumed.title_prompt().expect("title prompt is ready");
        assert!(prompt.contains("<user_message>\norphaned question"));
        assert!(prompt.contains("<assistant_message>\nlate reply"));
        assert!(prompt.contains("Return only one bare title"));
        let events = read_events(resumed.path());
        let StoreEvent::Meta(meta) = &events[0] else {
            panic!("first line must be the meta header, got {:?}", events[0]);
        };
        assert_eq!(meta.title, "");
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
            None,
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
    async fn every_committed_message_carries_a_millisecond_precision_timestamp() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("store created");

        let mut context = crate::agent::Context::new();
        context.set_store(store.clone());
        context.push_user("write a haiku".to_string());
        context.push_assistant("a haiku".to_string(), AssistantStatus::Complete);
        store.flush().await;

        let events = read_events(store.path());
        let timestamps: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                StoreEvent::Message { timestamp, .. } => Some(timestamp.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(timestamps.len(), 2, "one timestamp per committed message");
        for timestamp in timestamps {
            assert!(
                chrono::DateTime::parse_from_rfc3339(timestamp).is_ok(),
                "expected an RFC 3339 timestamp, got {timestamp:?}"
            );
            assert!(
                timestamp.contains('.'),
                "expected millisecond precision (a fractional-seconds part), got {timestamp:?}"
            );
        }
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
        let first = Store::create(
            &chats_root,
            temp.path(),
            session_id,
            "openrouter",
            "gpt-4o",
            None,
        )
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
            let store = Store::create(
                &chats_root,
                temp.path(),
                session_id,
                "openrouter",
                "gpt-4o",
                None,
            )
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
            let store = Store::create(
                &chats_root,
                temp.path(),
                session_id,
                "openrouter",
                "gpt-4o",
                None,
            )
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
            let store = Store::create(
                &chats_root,
                temp.path(),
                session_id,
                "openrouter",
                "gpt-4o",
                None,
            )
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
            None,
        )
        .expect("older session");
        older.record_message(&user_message("older question"));
        older.record_message(&assistant_message("older reply"));
        older.flush().await;
        let newer = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "ollama",
            "llama3",
            None,
        )
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
        assert_eq!(summaries[0].title, "");
        assert_eq!(summaries[0].provider_connection, "ollama");
        assert_eq!(summaries[0].model, "llama3");
        assert_eq!(summaries[1].session_id, older.session_id());
    }

    #[tokio::test]
    async fn past_explicit_user_messages_are_deduplicated_and_skip_current_and_children() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");

        let older = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("older session");
        older.record_message(&user_message("older prompt"));
        older.record_message(&user_message("duplicate prompt"));
        older.record_message(&user_message("older tail"));
        older.record_message(&Message::User {
            content: "implicit note".to_string(),
            kind: UserMessageKind::ImplicitToolNote,
        });
        older.flush().await;

        let newer = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("newer session");
        newer.record_message(&user_message("newer head"));
        newer.record_message(&user_message("duplicate prompt"));
        newer.record_message(&user_message("newer tail"));
        newer.flush().await;

        let child = older
            .write_child_transcript_for_run_store(
                Uuid::new_v4(),
                None,
                &[user_message("child task")],
            )
            .await
            .expect("child session");
        child.flush().await;

        let current = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("current session");
        current.record_message(&user_message("current prompt"));
        current.flush().await;

        rewrite_meta_created_at(older.path(), "2020-01-01T00:00:00Z");
        rewrite_meta_created_at(newer.path(), "2030-01-01T00:00:00Z");
        rewrite_meta_created_at(current.path(), "2040-01-01T00:00:00Z");

        assert_eq!(
            current
                .read_past_explicit_user_messages(50)
                .expect("history reads"),
            vec![
                "older prompt".to_string(),
                "older tail".to_string(),
                "newer head".to_string(),
                "duplicate prompt".to_string(),
                "newer tail".to_string()
            ]
        );
        assert_eq!(
            current
                .read_past_explicit_user_messages(1)
                .expect("limited history reads"),
            vec!["newer tail".to_string()]
        );
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
        let store = Store::create(
            &chats_root,
            temp.path(),
            parent_id,
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("store created");
        let run_id = Uuid::new_v4();

        store.record_message(&user_message("delegate this"));
        store.record_child_start(
            run_id,
            Uuid::new_v4(),
            "research",
            "investigate the bug",
            "Investigate",
        );
        store.record_child_result(
            run_id,
            Uuid::new_v4(),
            "success",
            Some("found it".to_string()),
            vec!["src/agent.rs".to_string()],
            FinishResultOrigin::Agent,
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
            origin,
            ..
        } = result
        else {
            unreachable!()
        };
        assert_eq!(*id, run_id);
        assert_eq!(*parent_session, parent_id);
        assert_eq!(status, "success");
        assert_eq!(sources, &["src/agent.rs".to_string()]);
        assert_eq!(*origin, FinishResultOrigin::Agent);
    }

    #[tokio::test]
    async fn read_child_runs_pairs_finished_and_orphaned_runs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("parent store created");
        let finished_run_id = Uuid::new_v4();
        let finished_call_id = Uuid::new_v4();
        let orphaned_run_id = Uuid::new_v4();
        let orphaned_call_id = Uuid::new_v4();

        store.record_child_start(
            finished_run_id,
            finished_call_id,
            "research",
            "find the regression",
            "Find regression",
        );
        store.record_child_result(
            finished_run_id,
            finished_call_id,
            "success",
            Some("found it".to_string()),
            vec!["src/agent.rs".to_string()],
            FinishResultOrigin::Agent,
        );
        store.record_child_start(
            orphaned_run_id,
            orphaned_call_id,
            "review",
            "review the patch",
            "Review patch",
        );
        store.flush().await;

        let runs = Store::read_child_runs(store.path()).expect("child records readable");
        assert_eq!(runs.len(), 2);

        let finished = runs
            .iter()
            .find(|run| run.run_id == finished_run_id)
            .expect("finished run present");
        assert_eq!(finished.call_id, finished_call_id);
        assert_eq!(finished.label, "Find regression");
        assert_eq!(finished.status.as_deref(), Some("success"));
        assert_eq!(finished.result.as_deref(), Some("found it"));
        assert_eq!(finished.sources, vec!["src/agent.rs".to_string()]);
        assert!(finished.completed_at.is_some());
        assert!(finished.duration.is_some());

        let orphaned = runs
            .iter()
            .find(|run| run.run_id == orphaned_run_id)
            .expect("orphaned run present");
        assert_eq!(orphaned.call_id, orphaned_call_id);
        assert_eq!(orphaned.label, "Review patch");
        assert!(orphaned.completed_at.is_none());
        assert!(orphaned.status.is_none());
        assert!(orphaned.result.is_none());
        assert!(orphaned.duration.is_none());
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
            None,
        )
        .expect("store created");
        let context_messages = vec![
            Message::System {
                content: "SOUL".to_string(),
            },
            Message::System {
                content: "tools schema".to_string(),
            },
            Message::System {
                content: "child briefing".to_string(),
            },
            user_message("summarize the module"),
            assistant_message("done"),
        ];

        let child_session_id = store
            .write_child_transcript(Some(("ollama", "llama3")), &context_messages)
            .await
            .expect("child transcript written");

        let child_path = session_path(&chats_root, store.project_key(), child_session_id);
        let events = read_events(&child_path);
        let StoreEvent::Meta(meta) = &events[0] else {
            panic!("child transcript needs its own meta header")
        };
        assert_eq!(
            meta.title, "",
            "child transcript titles are generated by the driver, not copied by the store"
        );
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
        let persisted_messages: Vec<Message> = events
            .into_iter()
            .filter_map(|event| match event {
                StoreEvent::Message { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(persisted_messages, context_messages);
    }

    #[tokio::test]
    async fn child_transcript_does_not_duplicate_task_already_in_context() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("store created");
        let run_id = Uuid::new_v4();
        let task = "summarize the module";
        let context_messages = vec![
            Message::System {
                content: "SOUL".to_string(),
            },
            Message::System {
                content: "tools schema".to_string(),
            },
            Message::System {
                content: "child briefing".to_string(),
            },
            user_message(task),
            assistant_message("done"),
        ];

        let child_session_id = store
            .write_child_transcript_for_run(run_id, Some(("ollama", "llama3")), &context_messages)
            .await
            .expect("child transcript written");

        let child_path = session_path(&chats_root, store.project_key(), child_session_id);
        let persisted_messages: Vec<Message> = read_events(&child_path)
            .into_iter()
            .filter_map(|event| match event {
                StoreEvent::Message { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(
            persisted_messages, context_messages,
            "the child transcript must preserve the Context message order without \
             prepending a duplicate task"
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
            None,
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
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "ollama",
            "llama3",
            None,
        )
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
    async fn meta_records_the_root_agent_type_and_omits_it_when_unset() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");

        let selected = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "ollama",
            "llama3",
            Some("researcher"),
        )
        .expect("store created");
        selected.flush().await;
        let StoreEvent::Meta(selected_meta) = &read_events(selected.path())[0] else {
            panic!("first line must be the meta header")
        };
        assert_eq!(
            selected_meta.agent_type.as_deref(),
            Some("researcher"),
            "the resolved root agent type is part of the creation header"
        );

        // A session written before the field existed: no `agent_type` key at
        // all, so readers fall back to `default` instead of failing.
        let legacy = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "ollama",
            "llama3",
            None,
        )
        .expect("store created");
        legacy.flush().await;
        let raw_first_line = std::fs::read_to_string(legacy.path())
            .expect("session file readable")
            .lines()
            .next()
            .expect("at least one line")
            .to_string();
        assert!(
            !raw_first_line.contains("agent_type"),
            "an unset agent type must not serialize at all: {raw_first_line}"
        );
        assert_eq!(
            Store::read_latest_agent_type(legacy.path()).expect("legacy file is readable"),
            None,
            "a header without the field reads back as unset"
        );
    }

    #[tokio::test]
    async fn read_latest_agent_type_follows_the_last_agent_type_changed_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "ollama",
            "llama3",
            Some("default"),
        )
        .expect("store created");

        assert_eq!(
            Store::read_latest_agent_type(store.path()).expect("readable"),
            Some("default".to_string()),
            "before any override the header's type is the active one"
        );

        store.record_agent_type_changed("researcher");
        store.flush().await;
        assert_eq!(
            Store::read_latest_agent_type(store.path()).expect("readable"),
            Some("researcher".to_string()),
            "an override supersedes the header without rewriting it"
        );

        store.record_agent_type_changed("default");
        store.flush().await;
        let events = read_events(store.path());
        let StoreEvent::Meta(meta) = &events[0] else {
            panic!("first line must be the meta header")
        };
        assert_eq!(
            meta.agent_type.as_deref(),
            Some("default"),
            "the header still names the type resolved at creation"
        );
        assert_eq!(
            Store::read_latest_agent_type(store.path()).expect("readable"),
            Some("default".to_string()),
            "the last override wins over every earlier one"
        );
    }

    #[tokio::test]
    async fn compaction_events_carry_the_transcript_relative_boundary_for_replay() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "ollama",
            "llama3",
            None,
        )
        .expect("store created");
        store.record_message(&user_message("first turn"));
        // The live boundary counts the two rebuilt system prompts; the
        // persisted transcript does not contain them.
        store.record_compaction(5, 2, "summarized prefix");
        store.flush().await;

        let events = read_events(store.path());
        let compaction = events
            .iter()
            .find_map(|event| match event {
                StoreEvent::Compaction {
                    boundary,
                    transcript_boundary,
                    ..
                } => Some((*boundary, *transcript_boundary)),
                _ => None,
            })
            .expect("the compaction event");
        assert_eq!(compaction, (5, Some(3)));

        let state = Store::read_session_state(store.path()).expect("state readable");
        assert_eq!(
            state.compaction.map(|snapshot| snapshot.boundary),
            Some(5),
            "the model-view boundary is unchanged for the context restore"
        );
        assert_eq!(
            state.compaction_transcript_boundary,
            Some(3),
            "replay-only readers get the cut without the system prompts"
        );
    }

    #[test]
    fn a_compaction_event_written_before_the_field_still_reads() {
        // An event from before the marker existed: no `transcript_boundary`
        // key at all. The reader tolerates it and reports no marker position.
        let legacy = r#"{"type":"compaction","version":1,"compacted_at":"2026-01-01T00:00:00.000Z","boundary":4,"summary":"old summary"}"#;
        let event: StoreEvent = serde_json::from_str(legacy).expect("the legacy line parses");
        let StoreEvent::Compaction {
            boundary,
            transcript_boundary,
            ..
        } = event
        else {
            panic!("a compaction line");
        };
        assert_eq!(boundary, 4);
        assert_eq!(transcript_boundary, None);
    }

    #[tokio::test]
    async fn compaction_events_round_trip_and_the_latest_snapshot_wins() {
        use crate::agent::compaction::CompactionSnapshot;

        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "ollama",
            "llama3",
            None,
        )
        .expect("store created");
        store.record_message(&user_message("first turn"));
        store.record_compaction(3, 0, "first summary");
        store.record_compaction(7, 0, "second summary");
        store.flush().await;

        let events = read_events(store.path());
        let compactions: Vec<(u32, usize, String)> = events
            .iter()
            .filter_map(|event| match event {
                StoreEvent::Compaction {
                    version,
                    boundary,
                    summary,
                    ..
                } => Some((*version, *boundary, summary.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            compactions,
            vec![
                (EVENT_VERSION, 3, "first summary".to_string()),
                (EVENT_VERSION, 7, "second summary".to_string()),
            ],
            "each snapshot is appended, in order, with the event version stamped on it"
        );

        let state = Store::read_session_state(store.path()).expect("state readable");
        assert_eq!(
            state.events.len(),
            events.len(),
            "the full event stream comes back for replay and cloning"
        );
        assert_eq!(state.messages, vec![user_message("first turn")]);
        assert_eq!(
            state.compaction,
            Some(CompactionSnapshot {
                boundary: 7,
                summary: "second summary".to_string(),
            }),
            "the newest snapshot supersedes the older one"
        );
    }

    #[tokio::test]
    async fn a_session_written_before_compaction_existed_reads_back_without_a_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "ollama",
            "llama3",
            None,
        )
        .expect("store created");
        store.record_message(&user_message("legacy turn"));
        store.flush().await;

        let state = Store::read_session_state(store.path()).expect("state readable");
        assert_eq!(state.messages, vec![user_message("legacy turn")]);
        assert!(
            state.compaction.is_none(),
            "a legacy session has no snapshot to restore: {state:?}"
        );
        assert!(!state
            .events
            .iter()
            .any(|event| matches!(event, StoreEvent::Compaction { .. })));
    }

    #[tokio::test]
    async fn reading_a_session_state_never_touches_the_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "ollama",
            "llama3",
            None,
        )
        .expect("store created");
        store.record_message(&user_message("turn"));
        store.record_compaction(1, 0, "summarized");
        store.flush().await;
        let path = store.path().to_path_buf();
        let session_id = store.session_id();
        let before = std::fs::read(&path).expect("session file readable");
        // Release the writer lock so the resume below is the process that
        // opens the file, exactly as a later `--conversation-id` run would.
        drop(store);

        // The dropped handle's writer task releases the advisory lock
        // asynchronously, so retry briefly — exactly what a later process
        // would do after the previous one exited.
        let resumed = {
            let mut attempts = 0;
            loop {
                match Store::resume(&chats_root, temp.path(), session_id) {
                    Ok(resumed) => break resumed,
                    Err(StoreError::SessionLocked { .. }) if attempts < 100 => {
                        attempts += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    Err(error) => panic!("resume succeeds: {error}"),
                }
            }
        };
        resumed.flush().await;
        let state = Store::read_session_state(&path).expect("state readable");
        assert_eq!(state.compaction.map(|snapshot| snapshot.boundary), Some(1));
        drop(resumed);

        let after = std::fs::read(&path).expect("session file readable");
        assert_eq!(
            before, after,
            "a resume plus a state read must not append, rewrite or repair anything"
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
            None,
        )
        .expect("store created");
        store.record_message(&user_message("hi"));
        store.flush().await;

        // `/clear` rolls over to a fresh session but is not a child run.
        let rolled_over = store
            .start_new_session("openrouter", "gpt-4o", None, None)
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

    #[tokio::test]
    async fn set_title_trims_and_bounds_only_the_metadata_line() {
        let long = "x".repeat(TITLE_MAX_CHARS * 2);
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("store created");
        store.record_message(&user_message("preserve this user message"));
        store.record_message(&assistant_message("preserve this assistant reply"));
        store.flush().await;
        let events_before_title = read_events(store.path());
        store
            .set_title(format!("  {long}\nignored "))
            .await
            .expect("title rewrite succeeds");
        let StoreEvent::Meta(meta) = &read_events(store.path())[0] else {
            panic!("first line must be the meta header")
        };
        assert_eq!(meta.title.chars().count(), TITLE_MAX_CHARS);
        assert!(meta.title.chars().all(|character| character == 'x'));
        let events_after_title = read_events(store.path());
        assert_eq!(
            events_after_title[1..],
            events_before_title[1..],
            "title rewrite must preserve every event after the metadata line"
        );
        let bytes_after_first_title = std::fs::read(store.path()).expect("session file readable");
        store
            .set_title("a different title".to_string())
            .await
            .expect("idempotent title rewrite succeeds");
        assert_eq!(
            std::fs::read(store.path()).expect("session file readable"),
            bytes_after_first_title,
            "a second title write must not alter the session file"
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
            None,
        )
        .expect("store created");
        let path = store.path().to_path_buf();
        drop(store);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = read_events(&path);
        assert!(matches!(events.as_slice(), [StoreEvent::Meta(_)]));
    }

    #[tokio::test]
    async fn cloning_replays_the_source_into_a_new_session_and_leaves_the_source_untouched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let source_id = Uuid::new_v4();
        let source = Store::create(
            &chats_root,
            temp.path(),
            source_id,
            "openrouter",
            "gpt-4o",
            Some("explorer"),
        )
        .expect("source created");
        source.record_message(&user_message("first turn"));
        source.record_message(&assistant_message("first reply"));
        source.record_model_changed("ollama", "llama3.2");
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();
        source.record_child_start(run_id, call_id, "research", "investigate", "Investigate");
        source.record_child_result(
            run_id,
            call_id,
            "success",
            Some("found it".to_string()),
            vec!["src/store.rs".to_string()],
            FinishResultOrigin::Agent,
        );
        source
            .set_title("Copied title".to_string())
            .await
            .expect("title set");
        source.flush().await;
        let source_path = source.path().to_path_buf();
        let source_bytes_before = std::fs::read(&source_path).expect("source readable");
        drop(source);
        // Dropping the store only closes the writer task asynchronously; the
        // clone's exclusive nonblock lock must wait for that file handle.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let clone = Store::clone_session(&chats_root, temp.path(), source_id, Some("default"))
            .expect("clone created");
        clone.flush().await;
        let clone_id = clone.session_id();
        let clone_path = clone.path().to_path_buf();

        assert_ne!(clone_id, source_id, "the clone gets a fresh session id");
        assert_eq!(
            std::fs::read(&source_path).expect("source readable"),
            source_bytes_before,
            "the source file stays byte-for-byte unchanged"
        );

        let clone_meta = Store::read_session_meta(&clone_path)
            .expect("clone meta readable")
            .expect("clone meta present");
        assert_eq!(clone_meta.session_id, clone_id);
        assert_eq!(
            clone_meta.cloned_from,
            Some(source_id),
            "the clone names its source"
        );
        assert_eq!(
            clone_meta.parent_session, None,
            "a clone is a top-level session, not a child of the source"
        );
        assert_eq!(clone_meta.provider_connection, "openrouter");
        assert_eq!(clone_meta.model, "gpt-4o");
        assert_eq!(
            clone_meta.title, "Copied title",
            "the source title is carried over"
        );
        assert_eq!(
            clone_meta.agent_type.as_deref(),
            Some("default"),
            "an explicit --agent applies to the clone"
        );

        let state = Store::read_session_state(&clone_path).expect("clone state readable");
        assert_eq!(
            state.messages,
            vec![user_message("first turn"), assistant_message("first reply")],
            "the clone replays the full transcript"
        );

        let clone_events = read_events(&clone_path);
        assert!(
            !clone_events.iter().any(
                |event| matches!(event, StoreEvent::Meta(meta) if meta.session_id == source_id)
            ),
            "the source header is not copied: {clone_events:?}"
        );
        let child_start = clone_events
            .iter()
            .find_map(|event| match event {
                StoreEvent::ChildStart {
                    id,
                    parent_session,
                    call_id: copied_call_id,
                    ..
                } => Some((*id, *parent_session, *copied_call_id)),
                _ => None,
            })
            .expect("child_start copied");
        assert_eq!(child_start.0, run_id, "the child run id is preserved");
        assert_eq!(
            child_start.1, clone_id,
            "the lifecycle record is rebased onto the clone"
        );
        assert_eq!(child_start.2, call_id, "the spawn call id is preserved");
        let child_result_parent = clone_events
            .iter()
            .find_map(|event| match event {
                StoreEvent::ChildResult {
                    id, parent_session, ..
                } => Some((*id, *parent_session)),
                _ => None,
            })
            .expect("child_result copied");
        assert_eq!(child_result_parent, (run_id, clone_id));

        // New writes land only on the clone.
        clone.record_message(&user_message("second turn"));
        clone.flush().await;
        assert_eq!(
            Store::read_messages(&clone_path)
                .expect("clone messages")
                .len(),
            3
        );
        assert_eq!(
            Store::read_messages(&source_path)
                .expect("source messages")
                .len(),
            2,
            "the source transcript gains nothing from the clone run"
        );
    }

    #[tokio::test]
    async fn cloning_a_child_transcript_is_rejected_without_creating_a_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let source_id = Uuid::new_v4();
        let source = Store::create(
            &chats_root,
            temp.path(),
            source_id,
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("source created");
        let child_id = source
            .write_child_transcript(None, &[user_message("child work")])
            .await
            .expect("child transcript written");
        drop(source);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let result = Store::clone_session(&chats_root, temp.path(), child_id, None);

        assert!(
            matches!(
                result,
                Err(StoreError::SessionIsChild { session_id, .. }) if session_id == child_id
            ),
            "a child transcript must not be cloned, got {result:?}"
        );
        let project_directory = project_directory(&chats_root, &project_key(temp.path()));
        let session_files = std::fs::read_dir(&project_directory)
            .expect("project directory readable")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
            .count();
        assert_eq!(
            session_files, 2,
            "the source and its child transcript are the only session files"
        );
    }

    #[tokio::test]
    async fn cloning_a_locked_source_fails_without_creating_a_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let source_id = Uuid::new_v4();
        let source = Store::create(
            &chats_root,
            temp.path(),
            source_id,
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("source created");
        source.record_message(&user_message("live session"));
        source.flush().await;

        let result = Store::clone_session(&chats_root, temp.path(), source_id, None);

        assert!(
            matches!(
                result,
                Err(StoreError::SessionLocked { session_id }) if session_id == source_id
            ),
            "a source held open by another handle must fail the clone, got {result:?}"
        );
        let project_directory = project_directory(&chats_root, &project_key(temp.path()));
        let session_files = std::fs::read_dir(&project_directory)
            .expect("project directory readable")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
            .count();
        assert_eq!(session_files, 1, "no clone file was created");
    }

    #[tokio::test]
    async fn cloning_an_unknown_source_is_a_hard_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let missing = Uuid::new_v4();

        let result = Store::clone_session(&chats_root, temp.path(), missing, None);

        assert!(
            matches!(
                result,
                Err(StoreError::SessionNotFound { session_id, .. }) if session_id == missing
            ),
            "cloning an unknown id must not start a fresh session, got {result:?}"
        );
        assert!(!session_path(&chats_root, &project_key(temp.path()), missing).exists());
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
