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

//! Shared workspace discovery and per-file configuration resolution.

use std::path::{Path, PathBuf};

use crate::ylva_home::{ylva_home, YLVA_DIR_NAME};

use super::ConfigError;

/// The layer that supplied a resolved configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLayer {
    Workspace,
    Home,
    BuiltIn,
}

/// Raw contents and provenance of one resolved configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFile {
    pub contents: String,
    pub layer: ConfigLayer,
    pub path: PathBuf,
}

/// Resolve one relative file from workspace, Ylva home, and built-in layers.
///
/// The workspace root is the directory containing `.ylva`; the Ylva home is
/// the directory containing Ylva's `~/.ylva/` home layout. Resolution is
/// whole-file replacement: the first existing file wins and lower layers are
/// not consulted for that file. The built-in contents are supplied by code,
/// not read from a repository file.
pub fn resolve_file(
    relative_path: &Path,
    workspace_root: Option<&Path>,
    ylva_home: Option<&Path>,
    built_in_contents: Option<&str>,
) -> Result<Option<ResolvedFile>, ConfigError> {
    let workspace_path = workspace_root.map(|root| root.join(YLVA_DIR_NAME).join(relative_path));
    let home_path = ylva_home.map(|home| home.join(relative_path));
    let built_in_path = PathBuf::from("<built-in>").join(relative_path);

    resolve_file_at_paths(
        workspace_path.as_deref(),
        home_path.as_deref(),
        &built_in_path,
        built_in_contents,
    )
}

/// Resolve a file when callers already have the concrete paths for each
/// layer. This supports callers whose argument is a `profiles/` directory
/// rather than the containing Ylva home.
pub(crate) fn resolve_file_at_paths(
    workspace_path: Option<&Path>,
    home_path: Option<&Path>,
    built_in_path: &Path,
    built_in_contents: Option<&str>,
) -> Result<Option<ResolvedFile>, ConfigError> {
    if let Some(path) = workspace_path {
        if let Some(contents) = read_external_file(path)? {
            return Ok(Some(ResolvedFile {
                contents,
                layer: ConfigLayer::Workspace,
                path: path.to_path_buf(),
            }));
        }
    }

    if let Some(path) = home_path {
        if let Some(contents) = read_external_file(path)? {
            return Ok(Some(ResolvedFile {
                contents,
                layer: ConfigLayer::Home,
                path: path.to_path_buf(),
            }));
        }
    }

    Ok(built_in_contents.map(|contents| ResolvedFile {
        contents: contents.to_string(),
        layer: ConfigLayer::BuiltIn,
        path: built_in_path.to_path_buf(),
    }))
}

fn read_external_file(path: &Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Unreadable {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Discover the nearest workspace root from `start`.
///
/// The nearest ancestor containing either `.ylva/` or `.git` is returned.
/// The global Ylva home is not treated as a workspace `.ylva` marker.
/// `None` means neither marker exists before the filesystem root.
pub fn discover_workspace_root(start: &Path) -> Option<PathBuf> {
    let home = ylva_home();
    discover_workspace_root_with_home(start, home.as_deref())
}

fn discover_workspace_root_with_home(start: &Path, ylva_home: Option<&Path>) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let workspace_config_directory = ancestor.join(YLVA_DIR_NAME);
        if workspace_config_directory.is_dir()
            && !is_ylva_home_path(&workspace_config_directory, ylva_home)
        {
            return Some(ancestor.to_path_buf());
        }
        if ancestor.join(".git").exists() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn is_ylva_home_path(candidate: &Path, ylva_home: Option<&Path>) -> bool {
    let Some(ylva_home) = ylva_home else {
        return false;
    };
    if candidate == ylva_home {
        return true;
    }
    match (
        std::fs::canonicalize(candidate),
        std::fs::canonicalize(ylva_home),
    ) {
        (Ok(candidate), Ok(ylva_home)) => candidate == ylva_home,
        _ => false,
    }
}

/// Discover the workspace root from the current working directory.
pub fn current_workspace_root() -> Option<PathBuf> {
    let current_directory = std::env::current_dir().ok()?;
    discover_workspace_root(&current_directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DEFAULT_PROFILE_NAME, PROFILES_DIRECTORY_NAME, PROFILE_CONFIG_FILE_NAME};

    fn write_layer_file(root: &Path, relative_path: &Path, contents: &str) {
        let path = root.join(relative_path);
        std::fs::create_dir_all(path.parent().expect("layer file parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write layer file");
    }

    #[test]
    fn built_in_file_is_used_when_disk_layers_are_missing() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let resolved = resolve_file(
            &Path::new(PROFILES_DIRECTORY_NAME)
                .join(DEFAULT_PROFILE_NAME)
                .join(PROFILE_CONFIG_FILE_NAME),
            Some(&temporary_directory.path().join("workspace")),
            Some(&temporary_directory.path().join("home")),
            Some("built-in"),
        )
        .expect("resolve built-in file")
        .expect("built-in file exists");

        assert_eq!(resolved.contents, "built-in");
        assert_eq!(resolved.layer, ConfigLayer::BuiltIn);
    }

    #[test]
    fn home_file_wins_over_the_built_in_layer() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let ylva_home = temporary_directory.path().join("home");
        write_layer_file(&ylva_home, Path::new("SOUL.md"), "home");

        let resolved = resolve_file(
            Path::new("SOUL.md"),
            None,
            Some(&ylva_home),
            Some("built-in"),
        )
        .expect("resolve home file")
        .expect("home file exists");

        assert_eq!(resolved.contents, "home");
        assert_eq!(resolved.layer, ConfigLayer::Home);
        assert_eq!(resolved.path, ylva_home.join("SOUL.md"));
    }

    #[test]
    fn workspace_file_wins_over_home_and_built_in_layers() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let workspace_root = temporary_directory.path().join("workspace");
        let ylva_home = temporary_directory.path().join("home");
        write_layer_file(
            &workspace_root.join(YLVA_DIR_NAME),
            Path::new("SOUL.md"),
            "workspace",
        );
        write_layer_file(&ylva_home, Path::new("SOUL.md"), "home");

        let resolved = resolve_file(
            Path::new("SOUL.md"),
            Some(&workspace_root),
            Some(&ylva_home),
            Some("built-in"),
        )
        .expect("resolve workspace file")
        .expect("workspace file exists");

        assert_eq!(resolved.contents, "workspace");
        assert_eq!(resolved.layer, ConfigLayer::Workspace);
        assert_eq!(
            resolved.path,
            workspace_root.join(YLVA_DIR_NAME).join("SOUL.md")
        );
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            resolve_file(
                Path::new("missing.txt"),
                Some(&temporary_directory.path().join("workspace")),
                Some(&temporary_directory.path().join("home")),
                None,
            )
            .expect("resolve missing file"),
            None
        );
    }

    #[test]
    fn workspace_root_prefers_the_nearest_ylva_directory() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let outer = temporary_directory.path().join("outer");
        let inner = outer.join("inner");
        let nested = inner.join("nested");
        std::fs::create_dir_all(&nested).expect("create nested directories");
        std::fs::create_dir(outer.join(YLVA_DIR_NAME)).expect("outer .ylva");
        std::fs::create_dir(inner.join(YLVA_DIR_NAME)).expect("inner .ylva");

        assert_eq!(discover_workspace_root(&nested), Some(inner));
    }

    #[test]
    fn workspace_root_uses_the_nearest_git_marker_when_no_ylva_exists() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let outer = temporary_directory.path().join("outer");
        let inner = outer.join("inner");
        let nested = inner.join("nested");
        std::fs::create_dir_all(&nested).expect("create nested directories");
        std::fs::write(outer.join(".git"), "gitdir: elsewhere").expect("outer git marker");
        std::fs::write(inner.join(".git"), "gitdir: elsewhere").expect("inner git marker");

        assert_eq!(discover_workspace_root(&nested), Some(inner));
    }

    #[test]
    fn workspace_root_does_not_skip_a_nearer_git_marker_for_an_outer_ylva() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let outer = temporary_directory.path().join("outer");
        let workspace = outer.join("workspace");
        let nested = workspace.join("nested");
        std::fs::create_dir_all(&nested).expect("create nested directories");
        std::fs::create_dir(outer.join(YLVA_DIR_NAME)).expect("outer .ylva");
        std::fs::write(workspace.join(".git"), "gitdir: elsewhere").expect("workspace git marker");

        assert_eq!(discover_workspace_root(&nested), Some(workspace));
    }

    #[test]
    fn workspace_root_is_absent_without_either_marker() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let nested = temporary_directory.path().join("nested");
        std::fs::create_dir(&nested).expect("nested directory");

        assert_eq!(discover_workspace_root(&nested), None);
    }

    #[test]
    fn workspace_root_ignores_the_global_home_marker() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let home_root = temporary_directory.path();
        let ylva_home = home_root.join(YLVA_DIR_NAME);
        let nested = home_root.join("project").join("nested");
        std::fs::create_dir(&ylva_home).expect("global Ylva home");
        std::fs::create_dir_all(&nested).expect("nested project directory");

        assert_eq!(
            discover_workspace_root_with_home(&nested, Some(&ylva_home)),
            None
        );
    }
}
