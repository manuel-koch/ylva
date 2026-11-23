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

//! Pinned read-only transcript capture. Archive maintenance keeps its own resolver.

use super::{archive, SessionMeta, Store};
use crate::strict_json;
use flate2::read::MultiGzDecoder;
use nix::errno::Errno;
use nix::fcntl::{open, openat, AtFlags, OFlag};
use nix::sys::stat::{fstatat, FileStat, Mode};
use serde_json::Value;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

const CAPTURE_ATTEMPTS: usize = 3;
const READ_BUFFER_SIZE: usize = 64 * 1024;
const OPEN_FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_NONBLOCK)
    .union(OFlag::O_CLOEXEC);

/// The representations validated when pinning a logical transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportRepresentation {
    /// Only the plain transcript was present.
    Plain,
    /// Only the gzip transcript and matching sidecar were present.
    Gzip,
    /// The plain and gzip transcripts were both present and equal.
    PlainAndGzip,
}

/// Files observed in a successful snapshot, not looked up again after capture.
#[derive(Debug)]
pub struct ExportCapture {
    /// Validated transcript representation.
    pub representation: ExportRepresentation,
    /// Observed plain transcript path, if present.
    pub plain_path: Option<PathBuf>,
    /// Observed gzip transcript path, if present.
    pub gzip_path: Option<PathBuf>,
    /// Observed archive metadata sidecar path, if present.
    pub sidecar_path: Option<PathBuf>,
}

/// Why a read-only snapshot could not be captured.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// No transcript is available.
    #[error("session transcript not found")]
    Missing,
    /// A stable representation violates the store's read contract.
    #[error("{0}")]
    Corrupt(&'static str),
    /// Representations kept changing during capture.
    #[error("session representations changed during capture")]
    Unstable,
    /// The caller stopped the capture.
    #[error("snapshot capture interrupted")]
    Interrupted,
    /// Accessing a source or writing the caller's spool failed.
    #[error("snapshot I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// A read-only directory handle scoped to one project key.
pub struct ExportReader {
    project: Option<(File, PathBuf)>,
}

impl Store {
    /// Pin the resolved chat root and open its project directory without
    /// following a project symlink. A missing root or project is not created.
    ///
    /// # Errors
    /// Rejects invalid project keys and directories that cannot be opened safely.
    pub fn export_reader(
        chats_root: &Path,
        project_key: &str,
    ) -> Result<ExportReader, SnapshotError> {
        let mut components = Path::new(project_key).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(SnapshotError::Corrupt("invalid project key"));
        }
        let project = match chats_root.canonicalize() {
            Ok(root_path) => {
                let root = open(&root_path, OPEN_FLAGS | OFlag::O_DIRECTORY, Mode::empty())
                    .map_err(io::Error::from)?;
                match openat(
                    &root,
                    project_key,
                    OPEN_FLAGS | OFlag::O_DIRECTORY,
                    Mode::empty(),
                ) {
                    Ok(project) => Some((File::from(project), root_path.join(project_key))),
                    Err(Errno::ENOENT) => None,
                    Err(Errno::ELOOP | Errno::ENOTDIR) => {
                        return Err(SnapshotError::Corrupt("project is not a real directory"));
                    }
                    Err(error) => return Err(io::Error::from(error).into()),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(ExportReader { project })
    }
}

impl ExportReader {
    /// Stream one logical transcript into an independent, seekable scratch file.
    /// The caller owns the scratch file and must discard it on error.
    ///
    /// # Errors
    /// Reports missing, corrupt or unstable sources, I/O errors, or interruption.
    pub fn capture(
        &self,
        session_id: Uuid,
        spool: &mut File,
        interrupted: &impl Fn() -> bool,
    ) -> Result<ExportCapture, SnapshotError> {
        let (project, project_path) = self.project.as_ref().ok_or(SnapshotError::Missing)?;
        let names = [
            format!("{session_id}.jsonl"),
            format!("{session_id}.jsonl.gz"),
            format!("{session_id}.meta.json"),
        ];
        let path = project_path.join(&names[0]);
        for _ in 0..CAPTURE_ATTEMPTS {
            if interrupted() {
                return Err(SnapshotError::Interrupted);
            }
            let before = observe(project, &names)?;
            spool.set_len(0)?;
            spool.rewind()?;
            let result = capture_attempt(project, &names, &before, &path, spool, interrupted);
            if matches!(result, Err(SnapshotError::Interrupted)) {
                return Err(SnapshotError::Interrupted);
            }
            let after = observe(project, &names)?;
            let plain_only = before[1..].iter().chain(&after[1..]).all(Option::is_none);
            let changed = if plain_only && result.is_ok() {
                // A title rewrite may replace the inode while a plain snapshot is read.
                // Keep the opened prefix rather than reopening it for the newer title.
                after[0].as_ref().is_none_or(|entry| !is_regular(entry))
            } else {
                !same_entry(&before[0], &after[0], false)
                    || !same_entry(&before[1], &after[1], true)
                    || !same_entry(&before[2], &after[2], true)
            };
            if !changed {
                let representation = result?;
                let [plain_path, gzip_path, sidecar_path] = std::array::from_fn(|index| {
                    before[index]
                        .as_ref()
                        .map(|_| project_path.join(&names[index]))
                });
                return Ok(ExportCapture {
                    representation,
                    plain_path,
                    gzip_path,
                    sidecar_path,
                });
            }
        }
        Err(SnapshotError::Unstable)
    }
}

fn observe(project: &File, names: &[String; 3]) -> io::Result<[Option<FileStat>; 3]> {
    let stat = |name: &String| match fstatat(project, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(stat)),
        Err(Errno::ENOENT) => Ok(None),
        Err(error) => Err(io::Error::from(error)),
    };
    Ok([stat(&names[0])?, stat(&names[1])?, stat(&names[2])?])
}

fn is_regular(stat: &FileStat) -> bool {
    stat.st_mode & libc::S_IFMT == libc::S_IFREG
}

fn same_entry(left: &Option<FileStat>, right: &Option<FileStat>, immutable: bool) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.st_dev == right.st_dev
                && left.st_ino == right.st_ino
                && (!immutable
                    || (left.st_size, left.st_mtime, left.st_ctime)
                        == (right.st_size, right.st_mtime, right.st_ctime))
        }
        _ => false,
    }
}

fn open_source(project: &File, name: &str, observed: &FileStat) -> Result<File, SnapshotError> {
    if !is_regular(observed) {
        return Err(SnapshotError::Corrupt("source entry is not a regular file"));
    }
    let descriptor = openat(project, name, OPEN_FLAGS, Mode::empty()).map_err(io::Error::from)?;
    let stat = nix::sys::stat::fstat(&descriptor).map_err(io::Error::from)?;
    if !is_regular(&stat) {
        return Err(SnapshotError::Corrupt("source entry is not a regular file"));
    }
    Ok(File::from(descriptor))
}

fn capture_attempt(
    project: &File,
    names: &[String; 3],
    observed: &[Option<FileStat>; 3],
    path: &Path,
    spool: &mut File,
    interrupted: &impl Fn() -> bool,
) -> Result<ExportRepresentation, SnapshotError> {
    let [plain, gzip, sidecar] = std::array::from_fn(|index| {
        observed[index]
            .as_ref()
            .map(|entry| open_source(project, &names[index], entry))
            .transpose()
    });
    let plain = plain?;
    let gzip = gzip?;
    let sidecar = sidecar?.map(read_sidecar).transpose()?;
    let representation = match (plain, gzip) {
        (Some(plain), gzip) => {
            let length = plain.metadata()?.len();
            if copy_prefix(plain.take(length), spool, interrupted)? != length {
                return Err(SnapshotError::Corrupt("plain transcript was truncated"));
            }
            validate_header(spool, path, sidecar.as_ref(), gzip.is_some())?;
            if let Some(gzip) = gzip {
                compare_archive(gzip, spool, interrupted)?;
                ExportRepresentation::PlainAndGzip
            } else {
                ExportRepresentation::Plain
            }
        }
        (None, Some(gzip)) => {
            copy_prefix(ArchiveReader(MultiGzDecoder::new(gzip)), spool, interrupted)?;
            validate_header(spool, path, sidecar.as_ref(), true)?;
            ExportRepresentation::Gzip
        }
        (None, None) if sidecar.is_some() => {
            return Err(SnapshotError::Corrupt("orphan metadata without transcript"));
        }
        (None, None) => return Err(SnapshotError::Missing),
    };
    spool.rewind()?;
    Ok(representation)
}

fn read_sidecar(file: File) -> Result<SessionMeta, SnapshotError> {
    let mut bytes = Vec::new();
    BufReader::new(file).read_to_end(&mut bytes)?;
    let fields = strict_json::object(&bytes)
        .map_err(|_| SnapshotError::Corrupt("invalid archive metadata sidecar"))?;
    serde_json::from_value(Value::Object(fields))
        .map_err(|_| SnapshotError::Corrupt("invalid archive metadata sidecar"))
}

fn validate_header(
    spool: &mut File,
    path: &Path,
    sidecar: Option<&SessionMeta>,
    archived: bool,
) -> Result<(), SnapshotError> {
    spool.rewind()?;
    let mut first_line = Vec::new();
    BufReader::new(&mut *spool).read_until(b'\n', &mut first_line)?;
    let header = strict_json::object(&first_line)
        .ok()
        .filter(|fields| fields.get("type").and_then(Value::as_str) == Some("meta"));
    if let Some(fields) = &header {
        let session_id = fields
            .get("session_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok());
        let project_key = fields.get("project_key").and_then(Value::as_str);
        if session_id
            .is_some_and(|id| path.file_stem() != Some(std::ffi::OsStr::new(&id.to_string())))
            || project_key.is_some_and(|key| {
                path.parent().and_then(Path::file_name) != Some(std::ffi::OsStr::new(key))
            })
        {
            return Err(SnapshotError::Corrupt(
                "transcript identity does not match project and UUID",
            ));
        }
    }
    if archived || sidecar.is_some() {
        let meta = header
            .and_then(|fields| serde_json::from_value::<SessionMeta>(Value::Object(fields)).ok())
            .ok_or(SnapshotError::Corrupt("no readable meta header"))?;
        archive::validate_identity(&meta, path)
            .map_err(|_| SnapshotError::Corrupt("invalid archive identity or timestamp"))?;
        if sidecar != Some(&meta) {
            return Err(SnapshotError::Corrupt(
                "missing or inconsistent archive metadata",
            ));
        }
    }
    Ok(())
}

struct ArchiveReader(MultiGzDecoder<File>);

impl Read for ArchiveReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer).map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidInput
                    | io::ErrorKind::InvalidData
                    | io::ErrorKind::UnexpectedEof
            ) {
                io::Error::new(io::ErrorKind::InvalidData, "corrupt gzip archive")
            } else {
                error
            }
        })
    }
}

fn copy_prefix(
    mut reader: impl Read,
    writer: &mut File,
    interrupted: &impl Fn() -> bool,
) -> Result<u64, SnapshotError> {
    let mut buffer = vec![0; READ_BUFFER_SIZE];
    let mut length = 0;
    loop {
        if interrupted() {
            return Err(SnapshotError::Interrupted);
        }
        let count = reader.read(&mut buffer).map_err(read_error)?;
        if count == 0 {
            return Ok(length);
        }
        writer.write_all(&buffer[..count])?;
        length += count as u64;
    }
}

fn read_error(error: io::Error) -> SnapshotError {
    if error.kind() == io::ErrorKind::InvalidData {
        SnapshotError::Corrupt("corrupt gzip archive")
    } else {
        error.into()
    }
}

fn compare_archive(
    gzip: File,
    spool: &mut File,
    interrupted: &impl Fn() -> bool,
) -> Result<(), SnapshotError> {
    spool.rewind()?;
    let mut decoded = ArchiveReader(MultiGzDecoder::new(gzip));
    let mut archive_buffer = vec![0; READ_BUFFER_SIZE];
    let mut plain_buffer = vec![0; READ_BUFFER_SIZE];
    let mut diverged = false;
    loop {
        if interrupted() {
            return Err(SnapshotError::Interrupted);
        }
        let count = decoded.read(&mut archive_buffer).map_err(read_error)?;
        if count == 0 {
            break;
        }
        if !diverged {
            diverged = match spool.read_exact(&mut plain_buffer[..count]) {
                Ok(()) => plain_buffer[..count] != archive_buffer[..count],
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => true,
                Err(error) => return Err(error.into()),
            };
        }
    }
    if diverged || spool.read(&mut plain_buffer[..1])? != 0 {
        return Err(SnapshotError::Corrupt("divergent raw/archive copies"));
    }
    Ok(())
}
