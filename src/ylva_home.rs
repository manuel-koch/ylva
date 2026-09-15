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

//! Single source of truth for the Ylva home that holds all per-user state
//! (`chats/`, `profiles/`, …) — `~/.ylva` in production.
//!
//! The home is resolved from the dedicated [`YLVA_HOME_ENVIRONMENT_VARIABLE`]
//! first and only falls back to `<operating-system-home>/.ylva` when that
//! variable is unset or empty. When set, `YLVA_HOME` names the home
//! **directly** — matching the convention of every other `*_HOME` variable in
//! this ecosystem (`CARGO_HOME`, `RUSTUP_HOME`, `GOPATH`, `XDG_DATA_HOME`,
//! …), all of which point straight at the tool's state directory rather than
//! at a parent that then gets a hidden dot-folder appended. `YLVA_HOME` is
//! *the* Ylva home, not the parent of one. The dedicated variable exists so a
//! test — or a user juggling several isolated setups — can redirect *Ylva's*
//! state without overriding the real `HOME`, which every other executable a
//! spawned process might invoke also relies on (shell rc files, git and ssh
//! config lookups, credential helpers, …).

use std::ffi::OsString;
use std::path::PathBuf;

/// Environment variable naming the Ylva home directly (no further
/// `.ylva` nesting applied when set).
pub const YLVA_HOME_ENVIRONMENT_VARIABLE: &str = "YLVA_HOME";

/// Directory name used for Ylva's home and workspace-local configuration.
pub const YLVA_DIR_NAME: &str = ".ylva";

/// Resolve the per-user Ylva home: `$YLVA_HOME` verbatim when set and
/// non-empty, else `<operating-system-home>/.ylva`. A workspace-local
/// `.ylva` directory has the same name but is a separate configuration scope.
///
/// `None` when neither the override nor the operating system's home directory
/// can be resolved; callers map that to their own domain error.
pub fn ylva_home() -> Option<PathBuf> {
    resolve_ylva_home(
        std::env::var_os(YLVA_HOME_ENVIRONMENT_VARIABLE),
        dirs::home_dir(),
    )
}

/// Pure core of [`ylva_home`], kept separate so it is testable without
/// mutating the process environment (`docs/known-test-issues.md`).
fn resolve_ylva_home(
    override_value: Option<OsString>,
    home_directory: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(value) = override_value.filter(|value| !value.is_empty()) {
        // The override names the home directly - no ".ylva" suffix applied.
        return Some(PathBuf::from(value));
    }
    Some(home_directory?.join(YLVA_DIR_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_override_wins_over_the_home_directory_and_names_the_home_directly() {
        let home = resolve_ylva_home(
            Some(OsString::from("/elsewhere")),
            Some(PathBuf::from("/home/user")),
        );
        assert_eq!(
            home,
            Some(Path::new("/elsewhere").to_path_buf()),
            "an explicit YLVA_HOME is the home itself, not its parent"
        );
    }

    #[test]
    fn an_unset_override_falls_back_to_the_home_directory() {
        let home = resolve_ylva_home(None, Some(PathBuf::from("/home/user")));
        assert_eq!(home, Some(Path::new("/home/user/.ylva").to_path_buf()));
    }

    #[test]
    fn an_empty_override_is_ignored_like_an_unset_one() {
        let home = resolve_ylva_home(Some(OsString::new()), Some(PathBuf::from("/home/user")));
        assert_eq!(home, Some(Path::new("/home/user/.ylva").to_path_buf()));
    }

    #[test]
    fn without_an_override_or_a_home_directory_there_is_no_home() {
        assert_eq!(resolve_ylva_home(None, None), None);
    }

    #[test]
    fn the_override_alone_is_enough_without_a_home_directory() {
        let home = resolve_ylva_home(Some(OsString::from("/elsewhere")), None);
        assert_eq!(home, Some(Path::new("/elsewhere").to_path_buf()));
    }
}
