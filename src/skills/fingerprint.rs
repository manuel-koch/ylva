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

//! Bounded, discovery-time equality checks; fingerprints never authorize resources.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::SKILL_FILE_NAME;

const EXCLUDED_RESOURCE_NAMES: [&str; 2] = [crate::GIT_ENTRY_NAME, crate::DS_STORE_FILE_NAME];

pub(super) const MAX_RESOURCE_ENTRIES: usize = 1024;
pub(super) const MAX_RESOURCE_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_RESOURCE_DEPTH: usize = 32;

struct Fingerprint {
    root: PathBuf,
    source: PathBuf,
    hash: Sha256,
    entries: usize,
    remaining_bytes: u64,
}

pub(super) fn calculate(source: &Path, instruction_hash: &[u8; 32]) -> io::Result<[u8; 32]> {
    let mut fingerprint = Fingerprint {
        root: source
            .parent()
            .expect("canonical source has a parent")
            .to_owned(),
        source: source.to_owned(),
        hash: Sha256::new(),
        entries: 0,
        remaining_bytes: MAX_RESOURCE_BYTES,
    };
    fingerprint.hash.update(instruction_hash);
    fingerprint.visit(&fingerprint.root.clone(), Path::new(""), &mut Vec::new())?;
    Ok(fingerprint.hash.finalize().into())
}

impl Fingerprint {
    fn field(&mut self, bytes: &[u8]) {
        self.hash.update((bytes.len() as u64).to_le_bytes());
        self.hash.update(bytes);
    }

    fn visit(
        &mut self,
        path: &Path,
        relative: &Path,
        ancestors: &mut Vec<PathBuf>,
    ) -> io::Result<()> {
        if !relative.as_os_str().is_empty()
            && path.file_name().is_some_and(|name| {
                EXCLUDED_RESOURCE_NAMES
                    .iter()
                    .any(|&excluded| name == excluded)
            })
        {
            return Ok(());
        }
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(&self.root) {
            return Err(io::Error::other(
                "resource symlink escapes the skill directory",
            ));
        }
        if !relative.as_os_str().is_empty()
            && (canonical == self.source
                || path.file_name().is_some_and(|name| name == SKILL_FILE_NAME)
                || canonical
                    .file_name()
                    .is_some_and(|name| name == SKILL_FILE_NAME))
        {
            return Ok(());
        }
        self.field(relative.as_os_str().as_encoded_bytes());
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.is_symlink() {
            self.field(b"symlink");
            self.field(std::fs::read_link(path)?.as_os_str().as_encoded_bytes());
        }
        let metadata = std::fs::metadata(&canonical)?;
        if metadata.is_dir() {
            if ancestors.len() >= MAX_RESOURCE_DEPTH || ancestors.contains(&canonical) {
                return Err(io::Error::other(format!(
                    "resource cycle or depth exceeds {MAX_RESOURCE_DEPTH}"
                )));
            }
            self.field(b"directory");
            let mut children = Vec::new();
            for entry in std::fs::read_dir(&canonical)? {
                self.entries += 1;
                if self.entries > MAX_RESOURCE_ENTRIES {
                    return Err(io::Error::other(format!(
                        "resource entries exceed {MAX_RESOURCE_ENTRIES}"
                    )));
                }
                children.push(entry?.file_name());
            }
            children.sort();
            ancestors.push(canonical);
            for name in children {
                self.visit(&path.join(&name), &relative.join(name), ancestors)?;
            }
            ancestors.pop();
        } else if metadata.is_file() {
            self.field(b"file");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                self.hash
                    .update((metadata.permissions().mode() & 0o111).to_le_bytes());
            }
            if metadata.len() > self.remaining_bytes {
                return Err(io::Error::other(format!(
                    "resource bytes exceed {MAX_RESOURCE_BYTES}"
                )));
            }
            self.hash.update(metadata.len().to_le_bytes());
            let mut file = std::fs::File::open(&canonical)?;
            let mut buffer = [0; 8192];
            let mut bytes_read = 0;
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                bytes_read += count as u64;
                if bytes_read > metadata.len() {
                    return Err(io::Error::other("resource changed during comparison"));
                }
                self.remaining_bytes -= count as u64;
                self.hash.update(&buffer[..count]);
            }
            if bytes_read != metadata.len() {
                return Err(io::Error::other("resource changed during comparison"));
            }
        } else {
            return Err(io::Error::other(
                "resource is not a regular file or directory",
            ));
        }
        Ok(())
    }
}
