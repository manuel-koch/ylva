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

//! Read-only representation resolution and crash-safe lossless transitions.

use super::{SessionMeta, StoreError, StoreEvent};
use flate2::{read::MultiGzDecoder, write::GzEncoder, Compression};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(super) fn invalid(path: &Path, reason: impl ToString) -> StoreError {
    StoreError::InvalidRepresentation {
        path: path.into(),
        reason: reason.to_string(),
    }
}

pub(super) fn io_error(path: &Path, source: std::io::Error) -> StoreError {
    StoreError::Io {
        path: path.into(),
        source,
    }
}

/// Parent directory of a session file; a path without one is invalid.
pub(super) fn session_directory(path: &Path) -> Result<&Path, StoreError> {
    path.parent()
        .ok_or_else(|| StoreError::InvalidRepresentation {
            path: path.into(),
            reason: "session path has no parent directory".to_string(),
        })
}

pub(super) fn raw_path(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|extension| extension == "gz") {
        path.with_extension("")
    } else {
        path.to_path_buf()
    }
}

pub(super) fn archive_path(path: &Path) -> PathBuf {
    raw_path(path).with_extension("jsonl.gz")
}
pub(super) fn metadata_path(path: &Path) -> PathBuf {
    raw_path(path).with_extension("meta.json")
}

pub(super) fn transcript_exists(path: &Path) -> bool {
    raw_path(path).is_file() || archive_path(path).is_file()
}

pub(super) fn header(bytes: &[u8], path: &Path) -> Result<SessionMeta, StoreError> {
    let first_line = bytes
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    match serde_json::from_slice::<StoreEvent>(first_line) {
        Ok(StoreEvent::Meta(meta)) => Ok(meta),
        _ => Err(invalid(path, "no readable meta header")),
    }
}

pub(super) fn validate_identity(meta: &SessionMeta, path: &Path) -> Result<(), StoreError> {
    let raw = raw_path(path);
    let session_id = raw
        .file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| Uuid::parse_str(name).ok());
    let project_key = raw
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str());
    if session_id != Some(meta.session_id) || project_key != Some(meta.project_key.as_str()) {
        return Err(invalid(
            path,
            "metadata identity does not match file name and project",
        ));
    }
    chrono::DateTime::parse_from_rfc3339(&meta.created_at)
        .map_err(|error| invalid(path, format!("invalid creation timestamp: {error}")))?;
    Ok(())
}

fn decode(path: &Path) -> Result<Vec<u8>, StoreError> {
    let file = std::fs::File::open(path).map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    MultiGzDecoder::new(file)
        .read_to_end(&mut bytes)
        .map_err(|source| invalid(path, format!("corrupt gzip archive: {source}")))?;
    Ok(bytes)
}

/// Resolves equivalent copies without repairing or changing any files.
pub(super) fn read_bytes(path: &Path) -> Result<Vec<u8>, StoreError> {
    let raw = raw_path(path);
    let archive = archive_path(path);
    let sidecar = metadata_path(path);
    let raw_bytes = if raw.is_file() {
        Some(std::fs::read(&raw).map_err(|source| io_error(&raw, source))?)
    } else {
        None
    };
    let sidecar_meta = if sidecar.is_file() {
        let bytes = std::fs::read(&sidecar).map_err(|source| io_error(&sidecar, source))?;
        let meta: SessionMeta =
            serde_json::from_slice(&bytes).map_err(|error| invalid(&sidecar, error))?;
        validate_identity(&meta, &raw)?;
        Some(meta)
    } else {
        None
    };
    if archive.is_file() {
        let archive_bytes = decode(&archive)?;
        let meta = header(&archive_bytes, &archive)?;
        validate_identity(&meta, &archive)?;
        if sidecar_meta.as_ref() != Some(&meta) {
            return Err(invalid(
                &archive,
                "missing or inconsistent archive metadata",
            ));
        }
        if let Some(raw_bytes) = raw_bytes {
            if raw_bytes != archive_bytes {
                return Err(invalid(&raw, "divergent raw/archive copies"));
            }
            return Ok(raw_bytes);
        }
        return Ok(archive_bytes);
    }
    if let Some(raw_bytes) = raw_bytes {
        if let Some(meta) = sidecar_meta {
            if header(&raw_bytes, &raw)? != meta {
                return Err(invalid(&sidecar, "metadata does not match raw header"));
            }
        }
        return Ok(raw_bytes);
    }
    if sidecar_meta.is_some() {
        return Err(invalid(&sidecar, "orphan metadata without transcript"));
    }
    Err(io_error(
        &raw,
        std::io::Error::new(std::io::ErrorKind::NotFound, "session transcript not found"),
    ))
}

pub(super) fn paths(path: &Path) -> Vec<PathBuf> {
    [raw_path(path), archive_path(path), metadata_path(path)]
        .into_iter()
        .filter(|path| path.is_file())
        .collect()
}

pub(super) fn require_unix() -> Result<(), StoreError> {
    if cfg!(unix) {
        Ok(())
    } else {
        Err(StoreError::UnsupportedMaintenance)
    }
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    let directory = session_directory(path)?;
    std::fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error(directory, source))
}

fn publish(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    crate::private_fs::replace_file(path, bytes, 0o600).map_err(|source| io_error(path, source))
}

fn remove(path: &Path) -> Result<(), StoreError> {
    if path.exists() {
        std::fs::remove_file(path).map_err(|source| io_error(path, source))?;
        sync_directory(path)?;
    }
    Ok(())
}

/// Caller owns the stable identity lock throughout the transition.
pub(super) fn compress_locked(path: &Path) -> Result<bool, StoreError> {
    compress_with_checkpoint(path, |_| Ok(()))
}

pub(super) fn compress_with_checkpoint(
    path: &Path,
    checkpoint: impl Fn(&str) -> Result<(), StoreError>,
) -> Result<bool, StoreError> {
    require_unix()?;
    let raw = raw_path(path);
    let archive = archive_path(path);
    let bytes = read_bytes(path)?;
    let meta = header(&bytes, path)?;
    validate_identity(&meta, path)?;
    if !raw.is_file() {
        return Ok(false);
    }
    if !archive.is_file() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&bytes)
            .map_err(|source| io_error(&archive, source))?;
        let encoded = encoder
            .finish()
            .map_err(|source| io_error(&archive, source))?;
        let mut decoded = Vec::new();
        MultiGzDecoder::new(encoded.as_slice())
            .read_to_end(&mut decoded)
            .map_err(|source| io_error(&archive, source))?;
        if decoded != bytes {
            return Err(invalid(&archive, "archive verification failed"));
        }
        publish(
            &metadata_path(path),
            &serde_json::to_vec(&meta).map_err(|error| invalid(path, error))?,
        )?;
        checkpoint("metadata_published")?;
        publish(&archive, &encoded)?;
        checkpoint("archive_published")?;
    }
    // Revalidate both complete copies before removing either one.
    read_bytes(path)?;
    for destination in [&archive, &metadata_path(path)] {
        std::fs::File::open(destination)
            .and_then(|file| file.sync_all())
            .map_err(|source| io_error(destination, source))?;
    }
    sync_directory(path)?;
    checkpoint("compression_verified")?;
    remove(&raw)?;
    checkpoint("raw_removed")?;
    Ok(true)
}

pub(super) fn decompress_locked(path: &Path) -> Result<bool, StoreError> {
    decompress_with_checkpoint(path, |_| Ok(()))
}

pub(super) fn decompress_with_checkpoint(
    path: &Path,
    checkpoint: impl Fn(&str) -> Result<(), StoreError>,
) -> Result<bool, StoreError> {
    require_unix()?;
    let raw = raw_path(path);
    let archive = archive_path(path);
    let sidecar = metadata_path(path);
    let bytes = read_bytes(path)?;
    if !archive.is_file() && !sidecar.is_file() {
        return Ok(false);
    }
    if !raw.is_file() {
        publish(&raw, &bytes)?;
    }
    checkpoint("raw_published")?;
    read_bytes(path)?;
    std::fs::File::open(&raw)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error(&raw, source))?;
    sync_directory(path)?;
    checkpoint("restoration_verified")?;
    remove(&archive)?;
    checkpoint("archive_removed")?;
    remove(&sidecar)?;
    checkpoint("metadata_removed")?;
    Ok(true)
}
