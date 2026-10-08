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

use super::session_file::{open_locked_session_file, LockedSessionFile};
use super::{
    SessionMeta, Store, StoreError, StoreEvent, StoreInner, TitleSnapshot, TitleState,
    TITLE_MAX_CHARS,
};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
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

    /// Rewrite the `meta` line in place (temp file + atomic `rename()`) so it
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
}
