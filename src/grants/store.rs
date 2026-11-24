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

//! Persistent grants: `<workspace-root>/.ylva/profiles/<profile>/grants.toml`.
//!
//! The file holds `[[grants]]` entries (`tool`, `rule`, and `name` for
//! `load_skill`). It is covered by workspace trust (ADR 0003) and loaded only
//! from the workspace. Saving preserves unrelated entries and comments,
//! refuses to overwrite an edit made behind Ylva's back, and publishes
//! atomically with owner-only permissions for new files.

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

use super::{Grant, GrantError, GrantTool};
use crate::home_relative_path::display_path;
use crate::private_fs::{create_private_dir_all, replace_file};
use crate::ylva_home::YLVA_DIR_NAME;

/// File name of the persistent grants inside a profile directory.
pub const GRANTS_FILE_NAME: &str = "grants.toml";

const NEW_FILE_MODE: u32 = 0o600;

#[derive(Debug, thiserror::Error)]
pub enum GrantStoreError {
    #[error("cannot read {}: {source}", display_path(path))]
    Unreadable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid grants file {}: {reason}", display_path(path))]
    Malformed { path: PathBuf, reason: String },
    #[error(
        "{} changed since it was loaded; nothing was saved. Review the file, then retry \
         (restart Ylva to reload it) or use a one-time or temporary grant",
        display_path(path)
    )]
    ExternalEdit { path: PathBuf },
    #[error(
        "{} would be created inside the Ylva home; persistent grants belong to a workspace. \
         Use a one-time or temporary grant",
        display_path(path)
    )]
    YlvaHome { path: PathBuf },
    #[error(
        "cannot write {}: {source}{}",
        display_path(path),
        if *marker_created { " (the new .ylva directory was created and remains)" } else { "" }
    )]
    Write {
        path: PathBuf,
        marker_created: bool,
        #[source]
        source: io::Error,
    },
    #[error(
        "writing {} failed ({source}) but the new grant is in the file; it will apply after \
         restart and `ylva trust`, not in this run",
        display_path(path)
    )]
    PossiblyPublished {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("persistent grants are not available: {0}")]
    Unavailable(String),
}

/// What a successful save did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    pub path: PathBuf,
    /// The save created the workspace `.ylva` directory.
    pub marker_created: bool,
    /// The identical entry was already in the file; nothing was written.
    pub already_present: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredGrant {
    tool: String,
    name: Option<String>,
    rule: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredGrantsFile {
    #[serde(default)]
    grants: Vec<StoredGrant>,
}

/// Parse and validate the contents of a grants file.
pub(super) fn parse_grants(contents: &str, path: &Path) -> Result<Vec<Grant>, GrantStoreError> {
    let malformed = |reason: String| GrantStoreError::Malformed {
        path: path.to_path_buf(),
        reason,
    };
    let file: StoredGrantsFile =
        toml::from_str(contents).map_err(|error| malformed(error.to_string()))?;
    file.grants
        .iter()
        .enumerate()
        .map(|(index, stored)| {
            let tool = GrantTool::from_name(&stored.tool).ok_or_else(|| {
                malformed(format!("grants[{index}]: unknown tool '{}'", stored.tool))
            })?;
            Grant::from_stored(tool, stored.name.as_deref(), stored.rule.as_deref())
                .map_err(|GrantError(reason)| malformed(format!("grants[{index}]: {reason}")))
        })
        .collect()
}

/// Writer for one profile's workspace `grants.toml`.
///
/// Callers serialize saves (see `GrantState`). The store remembers the
/// content it loaded or last saved and refuses to overwrite anything else.
#[derive(Debug)]
pub struct GrantStore {
    path: PathBuf,
    workspace_root: PathBuf,
    ylva_home: Option<PathBuf>,
    snapshot: Option<Vec<u8>>,
}

impl GrantStore {
    /// The grants file of `profile_name` in `workspace_root`.
    pub fn grants_path(workspace_root: &Path, profile_name: &str) -> PathBuf {
        workspace_root
            .join(YLVA_DIR_NAME)
            .join(crate::config::PROFILES_DIRECTORY_NAME)
            .join(profile_name)
            .join(GRANTS_FILE_NAME)
    }

    /// Read and validate the grants of `profile_name`. A missing file means
    /// no grants (there is no home fallback).
    pub fn load(
        workspace_root: &Path,
        profile_name: &str,
        ylva_home: Option<PathBuf>,
    ) -> Result<(GrantStore, Vec<Grant>), GrantStoreError> {
        let path = Self::grants_path(workspace_root, profile_name);
        let snapshot = read_optional(&path)?;
        let grants = match &snapshot {
            Some(bytes) => parse_grants(&String::from_utf8_lossy(bytes), &path)?,
            None => Vec::new(),
        };
        Ok((
            GrantStore {
                path,
                workspace_root: workspace_root.to_path_buf(),
                ylva_home,
                snapshot,
            },
            grants,
        ))
    }

    /// A store for saving only, without reading the file: used when loading
    /// is disabled for the invocation is *not* wanted, so this is for tests
    /// and tooling that start from a known empty state.
    pub fn without_existing_file(
        workspace_root: &Path,
        profile_name: &str,
        ylva_home: Option<PathBuf>,
    ) -> GrantStore {
        GrantStore {
            path: Self::grants_path(workspace_root, profile_name),
            workspace_root: workspace_root.to_path_buf(),
            ylva_home,
            snapshot: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The workspace `.ylva` directory a save would have to create, if it
    /// does not exist yet (creating it makes the directory a workspace).
    pub fn marker_to_create(&self) -> Option<PathBuf> {
        let marker = self.workspace_root.join(YLVA_DIR_NAME);
        (!marker.exists()).then_some(marker)
    }

    /// Why a persistent save is impossible, if so: the `.ylva` directory
    /// would be the Ylva home.
    pub fn refusal(&self) -> Option<GrantStoreError> {
        let marker = self.workspace_root.join(YLVA_DIR_NAME);
        let home = self.ylva_home.as_ref()?;
        let same = marker == *home
            || matches!(
                (std::fs::canonicalize(&marker), std::fs::canonicalize(home)),
                (Ok(marker), Ok(home)) if marker == home
            );
        same.then_some(GrantStoreError::YlvaHome { path: marker })
    }

    /// Append `grant` as a `[[grants]]` entry.
    pub fn append(&mut self, grant: &Grant) -> Result<SaveOutcome, GrantStoreError> {
        if let Some(refusal) = self.refusal() {
            return Err(refusal);
        }
        let current = read_optional(&self.path)?;
        if current != self.snapshot {
            return Err(GrantStoreError::ExternalEdit {
                path: self.path.clone(),
            });
        }
        let existing_text = current
            .as_deref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        let existing_grants = parse_grants(&existing_text, &self.path)?;
        if existing_grants.contains(grant) {
            return Ok(SaveOutcome {
                path: self.path.clone(),
                marker_created: false,
                already_present: true,
            });
        }
        let malformed = |error: toml_edit::TomlError| GrantStoreError::Malformed {
            path: self.path.clone(),
            reason: error.to_string(),
        };
        let mut document: DocumentMut = existing_text.parse().map_err(malformed)?;
        let array = document
            .entry("grants")
            .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or_else(|| GrantStoreError::Malformed {
                path: self.path.clone(),
                reason: "`grants` must be an array of tables".to_string(),
            })?;
        let mut entry = Table::new();
        entry["tool"] = value(grant.tool.name());
        if let Some(name) = grant.stored_name() {
            entry["name"] = value(name);
        }
        if let Some(rule) = grant.stored_rule() {
            entry["rule"] = value(rule);
        }
        array.push(entry);
        let serialized = document.to_string().into_bytes();

        let marker_created = self.marker_to_create().is_some();
        let publish = || -> io::Result<()> {
            create_private_dir_all(self.path.parent().unwrap_or_else(|| Path::new(".")))?;
            replace_file(&self.path, &serialized, existing_mode(&self.path))
        };
        match publish() {
            Ok(()) => {
                self.snapshot = Some(serialized);
                Ok(SaveOutcome {
                    path: self.path.clone(),
                    marker_created,
                    already_present: false,
                })
            }
            Err(source) => {
                let published = std::fs::read(&self.path).ok().as_deref() == Some(&serialized);
                if published {
                    self.snapshot = Some(serialized);
                    Err(GrantStoreError::PossiblyPublished {
                        path: self.path.clone(),
                        source,
                    })
                } else {
                    Err(GrantStoreError::Write {
                        path: self.path.clone(),
                        marker_created: marker_created && !self.marker_to_create().is_some(),
                        source,
                    })
                }
            }
        }
    }
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, GrantStoreError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(GrantStoreError::Unreadable {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Existing files keep their permissions; new files are owner-only.
fn existing_mode(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            return metadata.permissions().mode() & 0o777;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    NEW_FILE_MODE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        (directory, root)
    }

    fn read_grant() -> Grant {
        Grant::from_stored(GrantTool::Read, None, Some("./src/main.rs")).unwrap()
    }

    #[test]
    fn missing_file_means_no_grants() {
        let (_directory, root) = workspace();
        let (_store, grants) = GrantStore::load(&root, "default", None).unwrap();
        assert!(grants.is_empty());
    }

    #[test]
    fn entries_are_loaded_and_validated() {
        let (_directory, root) = workspace();
        let path = GrantStore::grants_path(&root, "default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[[grants]]\ntool = \"read\"\nrule = \"./src/*\"\n\n[[grants]]\ntool = \"web_search\"\n\n\
             [[grants]]\ntool = \"load_skill\"\nname = \"x\"\nrule = \"./s/SKILL.md\"\n",
        )
        .unwrap();
        let (_store, grants) = GrantStore::load(&root, "default", None).unwrap();
        assert_eq!(grants.len(), 3);
        assert_eq!(grants[1].tool, GrantTool::WebSearch);
    }

    #[test]
    fn malformed_files_are_rejected_with_a_clear_error() {
        let (_directory, root) = workspace();
        let path = GrantStore::grants_path(&root, "default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for contents in [
            "not toml [",
            "[[grants]]\ntool = \"nope\"\nrule = \"x\"\n",
            "[[grants]]\ntool = \"read\"\n",
            "[[grants]]\ntool = \"read\"\nrule = \"**\"\n",
            "[[grants]]\ntool = \"read\"\nrule = \"./a\"\nextra = 1\n",
            "[[grants]]\ntool = \"web_search\"\nrule = \"x\"\n",
            "[[grants]]\ntool = \"load_skill\"\nrule = \"./a\"\n",
            "[[grants]]\ntool = \"bash\"\nrule = \"python3 -c x\"\n",
            "version = 1\n",
        ] {
            std::fs::write(&path, contents).unwrap();
            let error = GrantStore::load(&root, "default", None).unwrap_err();
            assert!(
                matches!(error, GrantStoreError::Malformed { .. }),
                "{contents:?}: {error}"
            );
        }
    }

    #[test]
    fn append_creates_a_private_file_and_marker() {
        let (_directory, root) = workspace();
        let (mut store, _grants) = GrantStore::load(&root, "default", None).unwrap();
        assert!(store.marker_to_create().is_some());
        let outcome = store.append(&read_grant()).unwrap();
        assert!(outcome.marker_created);
        let contents = std::fs::read_to_string(store.path()).unwrap();
        assert!(contents.contains("tool = \"read\""));
        assert!(contents.contains("rule = \"./src/main.rs\""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let (_store, grants) = GrantStore::load(&root, "default", None).unwrap();
        assert_eq!(grants, vec![read_grant()]);
    }

    #[test]
    fn append_preserves_comments_unrelated_entries_and_permissions() {
        let (_directory, root) = workspace();
        let path = GrantStore::grants_path(&root, "default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# my notes\n[[grants]]\ntool = \"web_search\" # trailing\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        let (mut store, _grants) = GrantStore::load(&root, "default", None).unwrap();
        let outcome = store.append(&read_grant()).unwrap();
        assert!(!outcome.marker_created);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("# my notes\n[[grants]]\ntool = \"web_search\" # trailing\n"));
        assert!(contents.contains("rule = \"./src/main.rs\""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
    }

    #[test]
    fn append_is_idempotent_for_an_identical_entry() {
        let (_directory, root) = workspace();
        let (mut store, _grants) = GrantStore::load(&root, "default", None).unwrap();
        store.append(&read_grant()).unwrap();
        let outcome = store.append(&read_grant()).unwrap();
        assert!(outcome.already_present);
        let contents = std::fs::read_to_string(store.path()).unwrap();
        assert_eq!(contents.matches("[[grants]]").count(), 1);
    }

    #[test]
    fn external_edits_are_never_overwritten() {
        let (_directory, root) = workspace();
        let (mut store, _grants) = GrantStore::load(&root, "default", None).unwrap();
        store.append(&read_grant()).unwrap();
        let edited = "# edited\n".to_string() + &std::fs::read_to_string(store.path()).unwrap();
        std::fs::write(store.path(), &edited).unwrap();
        let error = store
            .append(&Grant::from_stored(GrantTool::WebSearch, None, None).unwrap())
            .unwrap_err();
        assert!(
            matches!(error, GrantStoreError::ExternalEdit { .. }),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(store.path()).unwrap(), edited);
    }

    #[test]
    fn file_created_behind_the_stores_back_is_an_external_edit() {
        let (_directory, root) = workspace();
        let (mut store, _grants) = GrantStore::load(&root, "default", None).unwrap();
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), "").unwrap();
        assert!(matches!(
            store.append(&read_grant()),
            Err(GrantStoreError::ExternalEdit { .. })
        ));
    }

    #[test]
    fn saving_into_the_ylva_home_is_refused() {
        let (_directory, root) = workspace();
        let home = root.join(YLVA_DIR_NAME);
        let (mut store, _grants) = GrantStore::load(&root, "default", Some(home)).unwrap();
        assert!(matches!(
            store.append(&read_grant()),
            Err(GrantStoreError::YlvaHome { .. })
        ));
        assert!(!root.join(YLVA_DIR_NAME).exists());
    }

    #[test]
    fn malformed_existing_file_blocks_saving() {
        let (_directory, root) = workspace();
        let path = GrantStore::grants_path(&root, "default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "broken [").unwrap();
        assert!(GrantStore::load(&root, "default", None).is_err());
    }
}
