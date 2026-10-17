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

//! Filesystem helpers for creating and replacing files safely.
//!
//! [`private_open_options`] and [`create_private_dir_all`]: on Unix, created
//! files are readable only by the owner (`0600`) and created directories are
//! accessible only by the owner (`0700`). Existing files and directories keep
//! their permissions. Other platforms use the defaults.
//!
//! [`replace_file`]: the single atomic and durable way to publish a file.

use std::fs::{DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use uuid::Uuid;

/// Atomically and durably replaces (or creates) `path` with `contents`.
///
/// The bytes go to a new temporary file in the same directory, which gets
/// `unix_mode` (ignored on other platforms) and is synced before it is renamed
/// over `path`; the directory is synced afterwards. Readers therefore see either
/// the old or the new file, never a partial one, and a failure leaves the old
/// file untouched. The temporary file is removed on failure.
pub(crate) fn replace_file(path: &Path, contents: &[u8], unix_mode: u32) -> io::Result<()> {
    let temporary_path = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = write_and_publish(&temporary_path, path, contents, unix_mode);
    if result.is_err() {
        if let Err(error) = std::fs::remove_file(&temporary_path) {
            if error.kind() != io::ErrorKind::NotFound {
                tracing::warn!(path = %temporary_path.display(), %error, "failed to remove temporary file");
            }
        }
    }
    result
}

fn write_and_publish(
    temporary_path: &Path,
    path: &Path,
    contents: &[u8],
    unix_mode: u32,
) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(unix_mode);
    }
    let mut file = options.open(temporary_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Creation mode is reduced by the umask; set the exact mode before publishing.
        file.set_permissions(std::fs::Permissions::from_mode(unix_mode))?;
    }
    #[cfg(not(unix))]
    let _ = unix_mode;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temporary_path, path)?;
    sync_parent_directory(path)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Open options whose creation mode is owner-only on Unix.
pub(crate) fn private_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Like [`std::fs::create_dir_all`], creating missing directories owner-only on Unix.
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory_entries(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .expect("read directory")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn replace_file_creates_and_overwrites_without_leaving_temporary_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("target.txt");

        replace_file(&path, b"first", 0o644).expect("create");
        replace_file(&path, b"second", 0o644).expect("overwrite");

        assert_eq!(std::fs::read(&path).expect("read"), b"second");
        assert_eq!(directory_entries(temp.path()), vec!["target.txt"]);
    }

    #[test]
    fn replace_file_failure_keeps_the_original_and_removes_the_temporary_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("target");
        std::fs::create_dir(&path).expect("directory in the way");
        std::fs::write(path.join("child"), b"original").expect("child");

        let result = replace_file(&path, b"new", 0o644);

        assert!(
            result.is_err(),
            "renaming a file over a non-empty directory fails"
        );
        assert_eq!(
            std::fs::read(path.join("child")).expect("child"),
            b"original"
        );
        assert_eq!(directory_entries(temp.path()), vec!["target"]);
    }

    #[cfg(unix)]
    #[test]
    fn replace_file_applies_the_requested_mode_regardless_of_umask_or_the_replaced_file() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("target");
        std::fs::write(&path, b"old").expect("old");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("chmod");

        replace_file(&path, b"new", 0o755).expect("replace");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
    }
}
