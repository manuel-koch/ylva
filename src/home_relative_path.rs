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

//! Home-relative path labels shared by the TUI help popup and CLI output.

use std::path::Path;

/// Render `path` with the user's home directory replaced by `~`. Paths outside
/// `home_directory`, or every path when it is `None`, stay as displayed.
/// Canonical paths also match a home directory reached through a symlink.
pub fn format_home_relative_path(path: &Path, home_directory: Option<&Path>) -> String {
    let Some(home_directory) = home_directory else {
        return path.display().to_string();
    };

    let relative_path = path.strip_prefix(home_directory).ok().or_else(|| {
        let canonical_home_directory = home_directory.canonicalize().ok()?;
        path.strip_prefix(canonical_home_directory).ok()
    });
    match relative_path {
        Some(relative_path) if relative_path.as_os_str().is_empty() => "~".to_string(),
        Some(relative_path) => Path::new("~").join(relative_path).display().to_string(),
        None => path.display().to_string(),
    }
}

/// [`format_home_relative_path`] against the current user's home directory,
/// for every path shown to a person.
pub fn display_path(path: &Path) -> String {
    format_home_relative_path(path, dirs::home_dir().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn display_path_abbreviates_the_current_home_directory() {
        let Some(home_directory) = dirs::home_dir() else {
            return;
        };

        assert_eq!(display_path(&home_directory.join("ylva")), "~/ylva");
    }

    #[test]
    fn home_relative_path_uses_tilde_for_home_and_descendants() {
        let home_directory = Path::new("/Users/manuelkoch");

        assert_eq!(
            format_home_relative_path(home_directory, Some(home_directory)),
            "~"
        );
        assert_eq!(
            format_home_relative_path(
                Path::new("/Users/manuelkoch/workspace/artificial-intelligence/ylva"),
                Some(home_directory),
            ),
            "~/workspace/artificial-intelligence/ylva"
        );
    }

    #[test]
    fn home_relative_path_keeps_paths_outside_home_absolute() {
        assert_eq!(
            format_home_relative_path(
                Path::new("/tmp/ylva-home/profiles/default/config.toml"),
                Some(Path::new("/Users/manuelkoch")),
            ),
            "/tmp/ylva-home/profiles/default/config.toml"
        );
    }

    #[cfg(unix)]
    #[test]
    fn home_relative_path_matches_canonical_sources_through_a_home_alias() {
        let temporary = tempfile::tempdir().unwrap();
        let home_directory = temporary.path().join("home");
        std::fs::create_dir(&home_directory).unwrap();
        let alias = temporary.path().join("alias");
        std::os::unix::fs::symlink(&home_directory, &alias).unwrap();
        let canonical_home = home_directory.canonicalize().unwrap();
        assert_eq!(
            format_home_relative_path(&canonical_home.join(".agents/skills"), Some(&alias)),
            "~/.agents/skills",
        );
        assert_eq!(
            format_home_relative_path(&canonical_home, Some(&alias)),
            "~"
        );
        assert_eq!(
            format_home_relative_path(&alias.join(".agents/skills"), Some(&alias)),
            "~/.agents/skills",
        );
    }

    #[test]
    fn home_relative_path_keeps_every_path_absolute_without_a_home() {
        assert_eq!(
            format_home_relative_path(Path::new("/Users/manuelkoch/.ylva"), None),
            "/Users/manuelkoch/.ylva"
        );
    }

    #[test]
    fn home_relative_path_does_not_abbreviate_a_sibling_sharing_the_home_prefix() {
        assert_eq!(
            format_home_relative_path(
                Path::new("/Users/manuelkoch-backup/ylva"),
                Some(Path::new("/Users/manuelkoch")),
            ),
            "/Users/manuelkoch-backup/ylva"
        );
    }
}
