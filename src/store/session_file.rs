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

//! Session file locks, metadata headers and append-open repair.

use super::{SessionMeta, StoreError, StoreEvent};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use uuid::Uuid;

/// Write `meta` as a brand-new file's first line and flush it immediately,
/// so it is durable on disk before anything else is ever appended. Returns
/// the number of bytes written (the JSON line plus its trailing newline),
/// which later locates exactly where the second line begins for the
/// metadata-only title rewrite.
pub(super) fn write_meta_line(
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
/// [`super::Store::resume`] to figure out whether a still-empty title can be
/// rewritten and, if so, where the second line begins.
pub(super) fn read_meta_line_with_length(
    path: &Path,
) -> Result<Option<(SessionMeta, u64)>, StoreError> {
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

pub(super) fn file_length(path: &Path) -> Result<u64, StoreError> {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })
}

/// A session file holding its exclusive advisory lock for as long as it is
/// alive. On unix this is a real `flock(LOCK_EX | LOCK_NB)`;
/// on other platforms the file is opened unlocked, since `nix` — and with it
/// `flock` — is a unix-only dependency of this crate.
#[cfg(unix)]
pub(super) type LockedSessionFile = nix::fcntl::Flock<std::fs::File>;
#[cfg(not(unix))]
pub(super) type LockedSessionFile = std::fs::File;

/// Open (creating when missing) `path` and take the exclusive, non-blocking
/// per-session advisory lock. A conflicting open fails fast with
/// [`StoreError::SessionLocked`] instead of silently interleaving writes.
pub(super) fn open_locked_session_file(
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
pub(super) fn lock_session_file(
    file: std::fs::File,
    session_id: Uuid,
) -> Result<LockedSessionFile, StoreError> {
    use nix::fcntl::{Flock, FlockArg};

    Flock::lock(file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_file, _errno)| StoreError::SessionLocked { session_id })
}

#[cfg(not(unix))]
pub(super) fn lock_session_file(
    file: std::fs::File,
    _session_id: Uuid,
) -> Result<LockedSessionFile, StoreError> {
    Ok(file)
}

/// Append one already-built event line to an open session file and flush it.
///
/// The counterpart of [`write_meta_line`] used by [`super::Store::clone_session`],
/// which writes the whole replay before handing the file to the writer task.
pub(super) fn append_event_line(
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
/// so the append-resume that follows can never leave corruption mid-file.
/// Only ever called on a resume/append open — a pure read
/// never mutates the file.
pub(super) fn repair_trailing_corruption(
    file: &mut LockedSessionFile,
    path: &Path,
) -> Result<(), StoreError> {
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
