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

//! Binds a file mutation to the path identity that was sandbox-checked and approved.
//!
//! The sandbox decides on the canonical form of a path. Between that decision
//! (and a possibly long user approval) and the actual write, a symlink or
//! parent directory could be swapped. [`ApprovedTarget`] records the canonical
//! path when the call is created, refuses the write when the path now resolves
//! elsewhere, and opens the canonical path without following a final symlink.
//!
//! Residual race: the check and the open are not atomic and parent directories
//! are not pinned (no `openat`-style handling), so a swap inside that tiny
//! window is still possible.

use std::path::{Path, PathBuf};

use super::ToolError;
use crate::sandbox::SandboxContext;

#[derive(Debug, Clone)]
pub(crate) struct ApprovedTarget {
    canonical_path: PathBuf,
}

impl ApprovedTarget {
    pub(crate) fn capture(path: &str) -> Self {
        ApprovedTarget {
            canonical_path: resolve(Path::new(path)),
        }
    }

    /// Refuse when `path` no longer resolves to the approved canonical path.
    pub(crate) fn verify_unchanged(&self, tool: &str, path: &str) -> Result<(), ToolError> {
        let current = resolve(Path::new(path));
        if current == self.canonical_path {
            return Ok(());
        }
        Err(ToolError::failed(
            tool,
            format!(
                "refusing to access '{path}': it resolved to '{}' when approved but now resolves \
                to '{}'",
                self.canonical_path.display(),
                current.display()
            ),
        ))
    }

    /// Open the canonical path for reading, refusing a symlink in its final
    /// component. Call [`Self::verify_unchanged`] first.
    pub(crate) async fn open_for_read(&self) -> std::io::Result<tokio::fs::File> {
        let canonical_path = self.canonical_path.clone();
        tokio::task::spawn_blocking(move || open_for_read_without_following(&canonical_path))
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .map(tokio::fs::File::from_std)
    }

    /// Verify the path is unchanged, then write `bytes` to the canonical path,
    /// refusing a symlink in its final component.
    pub(crate) async fn write(
        &self,
        tool: &str,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), ToolError> {
        self.verify_unchanged(tool, path)?;
        let canonical_path = self.canonical_path.clone();
        let outcome =
            tokio::task::spawn_blocking(move || write_without_following(&canonical_path, &bytes))
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))
                .and_then(|result| result);
        outcome.map_err(|error| {
            tracing::error!(error = %error, "failed to write");
            ToolError::failed(tool, format!("failed to write '{path}': {error}"))
        })
    }
}

fn resolve(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    SandboxContext::canonicalize_best_effort(&absolute)
}

fn open_for_read_without_following(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn write_without_following(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[tokio::test]
    async fn writes_when_path_is_unchanged() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("out.txt").display().to_string();
        let target = ApprovedTarget::capture(&path);
        target
            .write("write", &path, b"abc".to_vec())
            .await
            .expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "abc");
    }

    #[tokio::test]
    async fn refuses_symlink_swapped_in_after_capture() {
        let dir = TempDir::new().expect("tempdir");
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "original").expect("seed victim");
        let path = dir.path().join("out.txt").display().to_string();
        let target = ApprovedTarget::capture(&path);
        symlink(&victim, &path).expect("plant symlink");

        let error = target
            .write("write", &path, b"evil".to_vec())
            .await
            .expect_err("swap must be refused");
        assert!(error.to_string().contains("now resolves"), "{error}");
        assert_eq!(std::fs::read_to_string(&victim).expect("read"), "original");
    }

    #[tokio::test]
    async fn refuses_parent_directory_swapped_after_capture() {
        let dir = TempDir::new().expect("tempdir");
        let outside = TempDir::new().expect("outside");
        let parent = dir.path().join("sub");
        std::fs::create_dir(&parent).expect("mkdir");
        let path = parent.join("out.txt").display().to_string();
        let target = ApprovedTarget::capture(&path);
        std::fs::remove_dir(&parent).expect("rmdir");
        symlink(outside.path(), &parent).expect("plant parent symlink");

        target
            .write("write", &path, b"evil".to_vec())
            .await
            .expect_err("swap must be refused");
        assert!(!outside.path().join("out.txt").exists());
    }

    #[test]
    fn final_symlink_is_not_followed_when_opening() {
        let dir = TempDir::new().expect("tempdir");
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "original").expect("seed victim");
        let link = dir.path().join("link.txt");
        symlink(&victim, &link).expect("symlink");
        write_without_following(&link, b"evil").expect_err("O_NOFOLLOW refuses");
        assert_eq!(std::fs::read_to_string(&victim).expect("read"), "original");
    }
}
