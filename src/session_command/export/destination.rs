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

use super::interrupt::check;
use super::inventory::Member;
use super::layout::{home_for_path, resolve_home, Layout};
use icu_casemap::CaseMapper;
use icu_normalizer::DecomposingNormalizer;
use nix::sys::stat::FileStat;
use std::ffi::{OsStr, OsString};
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

pub(super) mod publication;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(PartialEq, Eq)]
struct FileState {
    identity: Identity,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileState {
    #[allow(
        clippy::unnecessary_cast,
        reason = "stat field widths depend on the Unix target"
    )]
    fn regular(stat: FileStat) -> io::Result<Self> {
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_nlink != 1 {
            return Err(io::Error::other(
                "destination must be a regular file with one link",
            ));
        }
        Ok(Self {
            identity: Identity {
                device: stat.st_dev as u64,
                inode: stat.st_ino as u64,
            },
            length: stat.st_size as u64,
            modified: (stat.st_mtime, stat.st_mtime_nsec),
            changed: (stat.st_ctime, stat.st_ctime_nsec),
        })
    }
}

fn final_state(stat: nix::Result<FileStat>) -> io::Result<Option<FileState>> {
    match stat {
        Ok(stat) => FileState::regular(stat).map(Some),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn absolute_directory(path: &Path) -> io::Result<PathBuf> {
    std::path::absolute(if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    })
}

/// Resolve directory aliases without creating missing components.
fn resolve_prospective(absolute_path: &Path) -> io::Result<PathBuf> {
    let mut resolved = PathBuf::from("/");
    for component in absolute_path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                match fs::symlink_metadata(&resolved) {
                    Ok(metadata) if metadata.is_dir() => {}
                    Ok(metadata) if metadata.is_symlink() => {
                        resolved = resolved.canonicalize()?;
                        if !fs::metadata(&resolved)?.is_dir() {
                            return Err(io::Error::other("a parent is not a directory"));
                        }
                    }
                    Ok(_) => return Err(io::Error::other("a parent is not a directory")),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            Component::Prefix(_) => return Err(io::Error::other("unsupported path prefix")),
        }
    }
    Ok(resolved)
}

#[derive(Clone)]
struct Entry {
    name: OsString,
    identity: Option<Identity>,
}

impl Entry {
    fn equivalent_to(&self, other: &Self) -> bool {
        match (self.identity, other.identity) {
            (Some(left), Some(right)) => left == right,
            _ if self.name == other.name => true,
            _ => self
                .name
                .to_str()
                .zip(other.name.to_str())
                .is_some_and(|(left, right)| {
                    let decomposition = DecomposingNormalizer::new_nfd();
                    let case = CaseMapper::new();
                    decomposition.normalize(&case.fold_string(&decomposition.normalize(left)))
                        == decomposition
                            .normalize(&case.fold_string(&decomposition.normalize(right)))
                }),
        }
    }
}

fn contains_location(outer: &[Entry], inner: &[Entry]) -> bool {
    // Compare relative to the last existing identity, not only path text:
    // a directory may be reachable at different spellings or mount points.
    let Some(anchor) = outer.iter().rposition(|entry| entry.identity.is_some()) else {
        return false;
    };
    let suffix = &outer[anchor + 1..];
    inner.iter().enumerate().any(|(index, entry)| {
        entry.identity == outer[anchor].identity
            && inner
                .get(index + 1..index + 1 + suffix.len())
                .is_some_and(|candidate| {
                    suffix
                        .iter()
                        .zip(candidate)
                        .all(|(left, right)| left.equivalent_to(right))
                })
    })
}

pub(super) struct Directory {
    requested: PathBuf,
    path: PathBuf,
    entries: Vec<Entry>,
}

impl Directory {
    pub(super) fn resolve(path: &Path) -> io::Result<Self> {
        let requested = absolute_directory(path)?;
        let path = resolve_prospective(&requested)?;
        let mut entries = Vec::new();
        for ancestor in path.ancestors() {
            let identity = match fs::symlink_metadata(ancestor) {
                Ok(metadata) if metadata.is_dir() => Some(Identity::of(&metadata)),
                Ok(_) => {
                    return Err(io::Error::other(
                        "a resolved parent is not a real directory",
                    ))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            entries.push(Entry {
                name: ancestor
                    .file_name()
                    .unwrap_or_else(|| OsStr::new("/"))
                    .to_owned(),
                identity,
            });
        }
        entries.reverse();
        Ok(Self {
            requested,
            path,
            entries,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn bundle(path: &Path) -> io::Result<Self> {
        let leaf: PathBuf = path.components().collect();
        match fs::symlink_metadata(leaf) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(io::Error::other(
                    "bundle destination is not a real directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let directory = Self::resolve(path)?;
        if directory
            .entries
            .last()
            .is_some_and(|entry| entry.identity.is_some())
            && fs::read_dir(&directory.path)?.next().transpose()?.is_some()
        {
            return Err(io::Error::other("bundle destination is not empty"));
        }
        Ok(directory)
    }

    pub fn contains(&self, path: &Directory) -> bool {
        contains_location(&self.entries, &path.entries)
    }
}

pub(super) struct Destination {
    parent: Rc<Directory>,
    name: OsString,
    initial: Option<FileState>,
}

fn destination_file_name(path: &Path) -> io::Result<&OsStr> {
    let bytes = path.as_os_str().as_bytes();
    let trailing = bytes.rsplit(|byte| *byte == b'/').next();
    if bytes.contains(&0) || matches!(trailing, Some(b"" | b"." | b"..")) {
        return Err(io::Error::other("destination must name a file"));
    }
    path.file_name()
        .ok_or_else(|| io::Error::other("destination must name a file"))
}

pub(super) fn manifest_path(path: &Path) -> io::Result<PathBuf> {
    destination_file_name(path)?;
    if path == Path::new("~") {
        return Err(io::Error::other("destination must name a file"));
    }
    let home = home_for_path(Some(path)).map_err(io::Error::other)?;
    Ok(resolve_home(path, home.as_deref()).with_extension("json"))
}

impl Destination {
    fn standalone(path: &Path, force: bool) -> io::Result<Self> {
        let name = destination_file_name(path)?;
        let parent = Rc::new(Directory::resolve(path.parent().unwrap_or(Path::new(".")))?);
        let initial = final_state(nix::sys::stat::lstat(&parent.path.join(name)))?;
        if initial.is_some() && !force {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination already exists",
            ));
        }
        Ok(Self {
            parent,
            name: name.to_owned(),
            initial,
        })
    }

    fn location(&self) -> Vec<Entry> {
        let mut entries = self.parent.entries.clone();
        entries.push(Entry {
            name: self.name.clone(),
            identity: self.initial.as_ref().map(|state| state.identity),
        });
        entries
    }

    pub fn path(&self) -> PathBuf {
        self.parent.path.join(&self.name)
    }
}

pub(super) struct Plan {
    pub bundles: Vec<BundleDestinations>,
    pub manifest: Option<Destination>,
}

pub(super) struct BundleDestinations {
    pub directory: Option<Rc<Directory>>,
    pub transcripts: Vec<Option<Destination>>,
}

impl Plan {
    pub fn preflight(
        layout: &Layout,
        staged: impl IntoIterator<Item = Vec<Member>>,
        extension: &str,
        chats: &Directory,
        manifest: Option<&Path>,
        force: bool,
        interrupted: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        check(interrupted)?;
        let manifest = manifest
            .map(|path| Destination::standalone(path, force))
            .transpose()?;
        let manifest_location = manifest.as_ref().map(Destination::location);
        if manifest_location
            .as_ref()
            .is_some_and(|path| contains_location(&chats.entries, path))
        {
            return Err(io::Error::other(
                "manifest destination is inside the chat store",
            ));
        }
        let mut bundles: Vec<BundleDestinations> = Vec::new();
        for (index, members) in staged.into_iter().enumerate() {
            check(interrupted)?;
            let mut bundle = BundleDestinations {
                directory: None,
                transcripts: Vec::new(),
            };
            if !members.is_empty() {
                match layout {
                    Layout::Stdout => bundle.transcripts.push(None),
                    Layout::File(path) => {
                        let destination = Destination::standalone(path, force)?;
                        let location = destination.location();
                        if contains_location(&chats.entries, &location) {
                            return Err(io::Error::other(
                                "transcript destination is inside the chat store",
                            ));
                        }
                        if manifest_location
                            .as_ref()
                            .is_some_and(|manifest| overlaps(manifest, &location))
                        {
                            return Err(io::Error::other(
                                "manifest and transcript destinations overlap",
                            ));
                        }
                        bundle.transcripts.push(Some(destination));
                    }
                    Layout::Bundles(paths) => {
                        let directory = Rc::new(Directory::bundle(&paths[index])?);
                        if chats.contains(&directory) {
                            return Err(io::Error::other(
                                "bundle destination is inside the chat store",
                            ));
                        }
                        if bundles
                            .iter()
                            .filter_map(|bundle| bundle.directory.as_ref())
                            .any(|other| overlaps(&directory.entries, &other.entries))
                        {
                            return Err(io::Error::other("bundle destinations overlap"));
                        }
                        if manifest_location
                            .as_ref()
                            .is_some_and(|manifest| overlaps(manifest, &directory.entries))
                        {
                            return Err(io::Error::other(
                                "manifest must be outside bundle directories",
                            ));
                        }
                        bundle.transcripts.extend(members.into_iter().map(|member| {
                            Some(Destination {
                                parent: Rc::clone(&directory),
                                name: member.filename(extension).into(),
                                initial: None,
                            })
                        }));
                        bundle.directory = Some(directory);
                    }
                }
            }
            bundles.push(bundle);
        }
        check(interrupted)?;
        Ok(Self { bundles, manifest })
    }
}

fn overlaps(left: &[Entry], right: &[Entry]) -> bool {
    contains_location(left, right) || contains_location(right, left)
}
