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

//! Maps a directory inside a linked git worktree to the equivalent path of the
//! main worktree, so sessions are shared across worktrees of one repository.
//! Git metadata is read directly; no `git` process is spawned.

use std::path::{Component, Path, PathBuf};

use crate::sandbox::SandboxContext;

const GITDIR_PREFIX: &str = "gitdir:";
const COMMON_DIRECTORY_FILE_NAME: &str = "commondir";

/// Path in the main worktree equivalent to `canonical_cwd`, when `canonical_cwd`
/// lies inside a linked worktree. Returns `None` for non-git directories, main
/// checkouts, submodules and unreadable or malformed metadata. The returned
/// path is an identifier only and need not exist.
pub(super) fn equivalent_main_worktree_path(canonical_cwd: &Path) -> Option<PathBuf> {
    let linked_root = canonical_cwd.ancestors().find(|ancestor| {
        ancestor
            .join(crate::GIT_ENTRY_NAME)
            .symlink_metadata()
            .is_ok()
    })?;
    let git_entry = linked_root.join(crate::GIT_ENTRY_NAME);
    if git_entry.is_dir() {
        return None;
    }
    let anchor = match linked_worktree_anchor(&git_entry) {
        Some(anchor) => anchor,
        None => {
            tracing::debug!(
                git_entry = %git_entry.display(),
                "git metadata not resolvable as linked worktree; using launch directory"
            );
            return None;
        }
    };
    let relative = canonical_cwd.strip_prefix(linked_root).ok()?;
    if relative.as_os_str().is_empty() {
        return Some(anchor);
    }
    Some(anchor.join(relative))
}

fn linked_worktree_anchor(git_file: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(git_file).ok()?;
    let gitdir_value = content
        .lines()
        .find_map(|line| line.trim().strip_prefix(GITDIR_PREFIX))?
        .trim();
    if gitdir_value.is_empty() {
        return None;
    }
    let gitdir = resolve_relative(git_file.parent()?, gitdir_value);
    let commondir_value = std::fs::read_to_string(gitdir.join(COMMON_DIRECTORY_FILE_NAME)).ok()?;
    let commondir_value = commondir_value.trim();
    if commondir_value.is_empty() {
        return None;
    }
    let common_directory =
        SandboxContext::canonicalize_best_effort(&resolve_relative(&gitdir, commondir_value));
    if common_directory.file_name()? == crate::GIT_ENTRY_NAME {
        common_directory.parent().map(Path::to_path_buf)
    } else {
        Some(common_directory)
    }
}

fn resolve_relative(base: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let mut resolved = base.to_path_buf();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    resolved
}
