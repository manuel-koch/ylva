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

//! One writer task owns the locked file, flush barriers and title rewrite.

use super::session_file::{lock_project, LockedSessionFile};
use super::{
    SessionMeta, Store, StoreError, StoreEvent, StoreInner, TitleSnapshot, TitleState,
    TITLE_MAX_CHARS,
};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tracing::Instrument;
use uuid::Uuid;

/// Capacity of the channel feeding the writer task. Large enough that a burst
/// of committed events (a turn plus its tool calls) never makes the agent loop
/// wait on disk.
const WRITER_CHANNEL_CAPACITY: usize = 256;

pub(super) enum WriterCommand {
    Event(Box<StoreEvent>),
    SetTitle {
        title: String,
        result_tx: oneshot::Sender<Result<(), StoreError>>,
    },
    Flush(oneshot::Sender<()>),
}

/// Reopens the raw handle after a published title rewrite; a seam so tests can
/// simulate failure.
pub(super) type ReopenFile = fn(&mut LockedSessionFile, &std::path::Path) -> std::io::Result<()>;

/// State owned by the writer task: the locked file and command receiver.
pub(super) struct SessionWriter {
    pub(super) file: LockedSessionFile,
    pub(super) path: PathBuf,
    pub(super) session_id: Uuid,
    /// `Some((meta, meta_line_len))` until a generated title is persisted:
    /// `meta` is the header as currently written to disk (title still
    /// empty), and `meta_line_len` is the byte length of that first line
    /// (JSON plus newline), i.e. where the second line begins — needed to
    /// rewrite just the first line once the title is known. `None` once
    /// finalized (or if this session's header already carried a title).
    pub(super) meta_awaiting_title: Option<(SessionMeta, u64)>,
    pub(super) command_rx: mpsc::Receiver<WriterCommand>,
    pub(super) reopen_file: ReopenFile,
    /// Set when the raw handle may point at a replaced inode; every later
    /// write is refused instead of appending to an unpublished file.
    pub(super) failure: Option<String>,
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
    pub(super) fn append(&mut self, event: StoreEvent) {
        self.write_line(&event);
    }

    /// Rewrite the `meta` line in place (temp file + atomic `rename()`) so it
    /// carries the supplied generated title. A no-op once already finalized.
    pub(super) fn set_title(&mut self, title: &str) -> Result<(), StoreError> {
        if let Some(failure) = &self.failure {
            return Err(self.failed_error(failure));
        }
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
    /// crash mid-rewrite never corrupts it. Reopen the raw handle while retaining
    /// the separate identity lock so later appends reach the published inode.
    fn rewrite_meta_line(&mut self, meta: &SessionMeta, meta_line_len: u64) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(meta_line_len))?;
        let mut remainder = Vec::new();
        self.file.read_to_end(&mut remainder)?;

        let new_line =
            serde_json::to_string(&StoreEvent::Meta(meta.clone())).expect("meta always serializes");
        let mut contents = Vec::with_capacity(new_line.len() + 1 + remainder.len());
        contents.extend_from_slice(new_line.as_bytes());
        contents.push(b'\n');
        contents.extend_from_slice(&remainder);
        crate::private_fs::replace_file(&self.path, &contents, 0o600)?;

        if let Err(error) = (self.reopen_file)(&mut self.file, &self.path) {
            self.failure = Some(format!(
                "session file was replaced but could not be reopened: {error}"
            ));
            return Err(error);
        }
        Ok(())
    }

    fn failed_error(&self, failure: &str) -> StoreError {
        StoreError::Io {
            path: self.path.clone(),
            source: std::io::Error::other(failure.to_owned()),
        }
    }

    fn write_line(&mut self, event: &StoreEvent) {
        if let Some(failure) = &self.failure {
            tracing::error!(path = %self.path.display(), %failure, "writer failed; dropping store event");
            return;
        }
        let _coordination = if matches!(
            event,
            StoreEvent::SubagentStart { .. } | StoreEvent::SubagentResult { .. }
        ) {
            match lock_project(self.path.parent().expect("session directory")) {
                Ok(lock) => Some(lock),
                Err(error) => {
                    tracing::error!(%error, "failed to coordinate subagent reference publication");
                    return;
                }
            }
        } else {
            None
        };
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
        } else {
            tracing::debug!(stage = "appended", "session event persisted");
        }
    }
}

impl Store {
    pub(super) fn spawn_writer(
        session_id: Uuid,
        project_key: String,
        chats_root: PathBuf,
        path: PathBuf,
        file: LockedSessionFile,
        title_state: TitleState,
        title_snapshot: TitleSnapshot,
    ) -> Store {
        let (command_tx, command_rx) = mpsc::channel::<WriterCommand>(WRITER_CHANNEL_CAPACITY);
        let span = if title_state
            .meta_awaiting_title
            .as_ref()
            .is_some_and(|(meta, _)| meta.parent_session.is_some())
        {
            tracing::info_span!("session", session_id = %session_id)
        } else {
            tracing::info_span!(parent: None, "session", session_id = %session_id)
        };
        let writer_span = tracing::debug_span!(parent: &span, "session_writer");
        tokio::spawn(
            run_writer(SessionWriter {
                file,
                path: path.clone(),
                session_id,
                meta_awaiting_title: title_state.meta_awaiting_title,
                command_rx,
                reopen_file: LockedSessionFile::reopen,
                failure: None,
            })
            .instrument(writer_span),
        );
        Store {
            inner: Arc::new(StoreInner {
                session_id,
                project_key,
                chats_root,
                path,
                command_tx,
                title_snapshot: Mutex::new(title_snapshot),
                span,
            }),
        }
    }
}
