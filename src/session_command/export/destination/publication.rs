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

use super::{final_state, resolve_prospective, Destination, Directory, Identity};
use crate::hexadecimal::hex_digest;
use crate::session_command::export::interrupt::{check, InterruptibleReader};
use nix::dir::Dir;
use nix::errno::Errno;
use nix::fcntl::{open, openat, renameat, AtFlags, Flock, FlockArg, OFlag};
use nix::sys::stat::{fstatat, mkdirat, Mode};
use nix::unistd::{linkat, unlinkat, UnlinkatFlags};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

const READ_DIRECTORY: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_DIRECTORY)
    .union(OFlag::O_CLOEXEC)
    .union(OFlag::O_NOFOLLOW);

#[derive(Debug, thiserror::Error)]
pub(in crate::session_command::export) enum PublishError {
    #[error("{0}")]
    Failed(#[from] io::Error),
    #[error("published but could not confirm durable publication: {0}")]
    Uncertain(io::Error),
}

pub(in crate::session_command::export) struct Publisher<'a> {
    chats: &'a Directory,
    created: HashMap<PathBuf, Identity>,
    pub created_paths: Vec<PathBuf>,
    #[cfg(test)]
    pub sync_published_directory: fn(&File) -> io::Result<()>,
}

impl<'a> Publisher<'a> {
    pub fn new(chats: &'a Directory) -> Self {
        Self {
            chats,
            created: HashMap::new(),
            created_paths: Vec::new(),
            #[cfg(test)]
            sync_published_directory: File::sync_all,
        }
    }

    pub fn standalone(
        &mut self,
        destination: &Destination,
        source: impl Read,
        interrupted: &impl Fn() -> bool,
    ) -> Result<String, PublishError> {
        let parent = self.prepare_parent(&destination.parent, interrupted)?;
        self.commit(&parent, destination, source, interrupted)
    }

    pub fn bundle(
        &mut self,
        directory: &Directory,
        interrupted: &impl Fn() -> bool,
    ) -> io::Result<Flock<File>> {
        let parent = self.prepare_parent(directory, interrupted)?;
        let mut entries = Dir::openat(&*parent, ".", READ_DIRECTORY, Mode::empty())?;
        for entry in entries.iter() {
            if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
                return Err(io::Error::other("bundle is no longer empty"));
            }
        }
        Ok(parent)
    }

    fn check_location(&self, parent: &Directory) -> io::Result<()> {
        if resolve_prospective(&parent.requested)? != parent.path {
            return Err(io::Error::other(
                "destination parent alias changed after preflight",
            ));
        }
        let current_chats = Directory::resolve(&self.chats.requested)?;
        if self.chats.contains(parent) || current_chats.contains(&Directory::resolve(&parent.path)?)
        {
            return Err(io::Error::other(
                "destination parent is inside the chat store",
            ));
        }
        Ok(())
    }

    fn prepare_parent(
        &mut self,
        directory: &Directory,
        interrupted: &impl Fn() -> bool,
    ) -> io::Result<Flock<File>> {
        self.check_location(directory)?;
        let mut current = root_directory(directory)?;
        let mut path = PathBuf::from("/");
        for entry in &directory.entries[1..] {
            check(interrupted)?;
            path.push(&entry.name);
            let expected = entry.identity.or_else(|| self.created.get(&path).copied());
            let next = match (open_directory(&current, &entry.name), expected) {
                (Ok(file), Some(expected)) if Identity::of(&file.metadata()?) == expected => file,
                (Err(Errno::ENOENT), None) => {
                    mkdirat(&current, entry.name.as_os_str(), Mode::S_IRWXU)?;
                    self.created_paths.push(path.clone());
                    let file = open_directory(&current, &entry.name)?;
                    current.sync_all()?;
                    self.created
                        .insert(path.clone(), Identity::of(&file.metadata()?));
                    file
                }
                (Err(error), _) if error != Errno::ENOENT => return Err(error.into()),
                _ => return Err(changed_parent()),
            };
            current = next;
        }
        let locked = lock(current, interrupted)?;
        self.recheck(&locked, directory)?;
        Ok(locked)
    }

    fn recheck(&self, pinned: &File, parent: &Directory) -> io::Result<()> {
        self.check_location(parent)?;
        let mut current = root_directory(parent)?;
        let mut path = PathBuf::from("/");
        for entry in &parent.entries[1..] {
            path.push(&entry.name);
            current = open_directory(&current, &entry.name)?;
            let expected = entry.identity.or_else(|| self.created.get(&path).copied());
            if Some(Identity::of(&current.metadata()?)) != expected {
                return Err(changed_parent());
            }
        }
        if Identity::of(&current.metadata()?) != Identity::of(&pinned.metadata()?) {
            return Err(changed_parent());
        }
        Ok(())
    }

    pub fn commit(
        &self,
        parent: &Flock<File>,
        destination: &Destination,
        source: impl Read,
        interrupted: &impl Fn() -> bool,
    ) -> Result<String, PublishError> {
        check(interrupted)?;
        self.recheck(parent, &destination.parent)?;
        check_state(parent, destination)?;
        let mut temporary = AdjacentFile::new(parent)?;
        let mut writer = HashingWriter::new(&mut temporary.file);
        io::copy(
            &mut InterruptibleReader::new(source, interrupted),
            &mut writer,
        )?;
        let digest = writer.digest();
        temporary.file.sync_all()?;
        self.recheck(parent, &destination.parent)?;
        check_state(parent, destination)?;
        check(interrupted)?;

        if destination.initial.is_some() {
            renameat(
                &**parent,
                temporary.name.as_str(),
                &**parent,
                destination.name.as_os_str(),
            )
            .map_err(io::Error::from)?;
            temporary.remove_on_drop = false;
        } else {
            // linkat is no-clobber even if a non-cooperating writer creates the
            // name after the last state comparison.
            linkat(
                &**parent,
                temporary.name.as_str(),
                &**parent,
                destination.name.as_os_str(),
                AtFlags::empty(),
            )
            .map_err(io::Error::from)?;
            temporary.unlink().map_err(PublishError::Uncertain)?;
        }
        #[cfg(not(test))]
        let sync = File::sync_all;
        #[cfg(test)]
        let sync = self.sync_published_directory;
        sync(parent).map_err(PublishError::Uncertain)?;
        Ok(digest)
    }
}

pub(in crate::session_command::export) struct HashingWriter<W> {
    writer: W,
    hasher: Sha256,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            hasher: Sha256::new(),
        }
    }

    pub fn digest(self) -> String {
        hex_digest(&self.hasher.finalize())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.writer.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn changed_parent() -> io::Error {
    io::Error::other("destination parent changed after preflight")
}

fn root_directory(directory: &Directory) -> io::Result<File> {
    let root = File::from(open("/", READ_DIRECTORY, Mode::empty())?);
    if Some(Identity::of(&root.metadata()?)) != directory.entries[0].identity {
        return Err(changed_parent());
    }
    Ok(root)
}

fn open_directory(parent: &File, name: &OsStr) -> nix::Result<File> {
    openat(parent, name, READ_DIRECTORY, Mode::empty()).map(File::from)
}

fn lock(mut directory: File, interrupted: &impl Fn() -> bool) -> io::Result<Flock<File>> {
    loop {
        check(interrupted)?;
        match Flock::lock(directory, FlockArg::LockExclusiveNonblock) {
            Ok(locked) => return Ok(locked),
            Err((file, Errno::EWOULDBLOCK | Errno::EINTR)) => {
                directory = file;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err((_, error)) => return Err(error.into()),
        }
    }
}

fn check_state(parent: &File, destination: &Destination) -> io::Result<()> {
    let current = final_state(fstatat(
        parent,
        destination.name.as_os_str(),
        AtFlags::AT_SYMLINK_NOFOLLOW,
    ))?;
    if current != destination.initial {
        return Err(io::Error::other("destination changed after preflight"));
    }
    Ok(())
}

struct AdjacentFile<'a> {
    parent: &'a File,
    name: String,
    file: File,
    remove_on_drop: bool,
}

impl<'a> AdjacentFile<'a> {
    fn new(parent: &'a File) -> io::Result<Self> {
        let name = format!(".ylva-export-{}.tmp", Uuid::new_v4());
        let file = File::from(openat(
            parent,
            name.as_str(),
            OFlag::O_WRONLY | OFlag::O_EXCL | OFlag::O_CREAT | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )?);
        let temporary = Self {
            parent,
            name,
            file,
            remove_on_drop: true,
        };
        temporary
            .file
            .set_permissions(Permissions::from_mode(0o600))?;
        Ok(temporary)
    }

    fn unlink(&mut self) -> io::Result<()> {
        unlinkat(self.parent, self.name.as_str(), UnlinkatFlags::NoRemoveDir)?;
        self.remove_on_drop = false;
        Ok(())
    }
}

impl Drop for AdjacentFile<'_> {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = self.unlink();
        }
    }
}
