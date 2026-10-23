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

//! Content-hash trust for the workspace configuration layer.
//!
//! A cloned repository can ship `.ylva/profiles/<name>/config.toml`, `.env`
//! and `.ylva/agents/**`. Those files steer provider endpoints (and thereby
//! where credentials are sent), `${NAME}` interpolation and tool approval
//! rules, so they are only used after the user approved their exact content
//! with `ylva trust` — comparable to `direnv allow`. Approvals are kept in
//! `<ylva_home>/trusted-workspaces.toml`, keyed by canonical workspace path and
//! profile name, as a SHA-256 over every covered file. Any effective edit to
//! the profile's files or to the shared `.ylva/agents` files changes the hash
//! and revokes the approval; other profiles are unaffected.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::home_relative_path::display_path;
use crate::private_fs::{create_private_dir_all, replace_file};
use crate::ylva_home::YLVA_DIR_NAME;

use super::profile::AGENTS_DIRECTORY_NAME;
use super::{PROFILES_DIRECTORY_NAME, PROFILE_CONFIG_FILE_NAME, PROFILE_ENV_FILE_NAME};

/// File name of the trust store inside the Ylva home.
pub const TRUST_STORE_FILE_NAME: &str = "trusted-workspaces.toml";

const TRUST_STORE_FILE_MODE: u32 = 0o600;

/// Trust decision for one workspace and profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceTrust {
    /// The workspace ships no profile-relevant configuration; nothing to gate.
    NoConfiguration,
    /// The current content equals the approved content.
    Trusted,
    /// Configuration exists and was never approved.
    Untrusted,
    /// Configuration was approved before but its content changed since.
    Changed,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkspaceTrustError {
    #[error("cannot read {}: {source}", display_path(path))]
    Unreadable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("malformed trust store at {}: {source}", display_path(path))]
    MalformedStore {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("cannot write trust store at {}: {source}", display_path(path))]
    StoreWrite {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustStore {
    /// Canonical workspace path → profile name → SHA-256 hex digest.
    #[serde(default)]
    workspaces: BTreeMap<String, BTreeMap<String, String>>,
}

impl TrustStore {
    fn load(path: &Path) -> Result<Self, WorkspaceTrustError> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(WorkspaceTrustError::Unreadable {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        toml::from_str(&contents).map_err(|source| WorkspaceTrustError::MalformedStore {
            path: path.to_path_buf(),
            source,
        })
    }

    fn save(&self, path: &Path) -> Result<(), WorkspaceTrustError> {
        let serialized = toml::to_string_pretty(self).expect("a trust store always serializes");
        let write = |parent: &Path| -> io::Result<()> {
            create_private_dir_all(parent)?;
            replace_file(path, serialized.as_bytes(), TRUST_STORE_FILE_MODE)
        };
        write(path.parent().unwrap_or_else(|| Path::new("."))).map_err(|source| {
            WorkspaceTrustError::StoreWrite {
                path: path.to_path_buf(),
                source,
            }
        })
    }
}

fn trust_store_path(ylva_home: &Path) -> PathBuf {
    ylva_home.join(TRUST_STORE_FILE_NAME)
}

fn workspace_key(workspace_root: &Path) -> String {
    workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf())
        .display()
        .to_string()
}

/// Every existing file whose content the trust decision covers, sorted.
pub(crate) fn covered_files(
    workspace_root: &Path,
    profile_name: &str,
) -> Result<Vec<PathBuf>, WorkspaceTrustError> {
    let configuration_root = workspace_root.join(YLVA_DIR_NAME);
    let profile_directory = configuration_root
        .join(PROFILES_DIRECTORY_NAME)
        .join(profile_name);
    let mut files = Vec::new();
    for path in [
        profile_directory.join(PROFILE_CONFIG_FILE_NAME),
        profile_directory.join(PROFILE_ENV_FILE_NAME),
    ] {
        if path.exists() {
            files.push(path);
        }
    }
    collect_files(&configuration_root.join(AGENTS_DIRECTORY_NAME), &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), WorkspaceTrustError> {
    let unreadable = |path: &Path, source| WorkspaceTrustError::Unreadable {
        path: path.to_path_buf(),
        source,
    };
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(unreadable(directory, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| unreadable(directory, error))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| unreadable(&path, error))?;
        if file_type.is_dir() {
            collect_files(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

/// Reduce `config.toml` and `.env` to their effective content so comments,
/// whitespace, quoting style and key order do not change the fingerprint.
/// Content that does not parse (and is rejected by the loader anyway) and all
/// other files are hashed verbatim.
fn normalized_contents(path: &Path, contents: Vec<u8>) -> Vec<u8> {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return contents;
    };
    let normalized = match file_name {
        PROFILE_CONFIG_FILE_NAME => normalize_toml(&contents),
        PROFILE_ENV_FILE_NAME => normalize_dotenv(&contents),
        _ => None,
    };
    normalized.map_or(contents, String::into_bytes)
}

fn normalize_toml(contents: &[u8]) -> Option<String> {
    let document: toml::Table = toml::from_str(std::str::from_utf8(contents).ok()?).ok()?;
    Some(sorted_toml(&toml::Value::Table(document)))
}

/// Canonical text of a TOML value with table keys in sorted order.
fn sorted_toml(value: &toml::Value) -> String {
    match value {
        toml::Value::Table(table) => {
            let mut keys = table.keys().collect::<Vec<_>>();
            keys.sort();
            let entries = keys
                .into_iter()
                .map(|key| format!("{key:?}={}", sorted_toml(&table[key])))
                .collect::<Vec<_>>();
            format!("{{{}}}", entries.join(","))
        }
        toml::Value::Array(values) => {
            let entries = values.iter().map(sorted_toml).collect::<Vec<_>>();
            format!("[{}]", entries.join(","))
        }
        scalar => scalar.to_string(),
    }
}

fn normalize_dotenv(contents: &[u8]) -> Option<String> {
    let mut variables = BTreeMap::new();
    for entry in dotenvy::from_read_iter(contents) {
        let (name, value) = entry.ok()?;
        variables.insert(name, value);
    }
    Some(
        variables
            .into_iter()
            .map(|(name, value)| format!("{name:?}={value:?}\n"))
            .collect(),
    )
}

/// SHA-256 over the relative path and content of every covered file.
fn fingerprint(workspace_root: &Path, files: &[PathBuf]) -> Result<String, WorkspaceTrustError> {
    let mut hasher = Sha256::new();
    for path in files {
        let contents = std::fs::read(path).map_err(|source| WorkspaceTrustError::Unreadable {
            path: path.clone(),
            source,
        })?;
        let contents = normalized_contents(path, contents);
        let relative = path.strip_prefix(workspace_root).unwrap_or(path);
        hasher.update(relative.display().to_string().as_bytes());
        hasher.update([0]);
        hasher.update((contents.len() as u64).to_le_bytes());
        hasher.update(&contents);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Decide whether the workspace's configuration for `profile_name` may be used.
pub(crate) fn workspace_trust(
    ylva_home: &Path,
    workspace_root: &Path,
    profile_name: &str,
) -> Result<WorkspaceTrust, WorkspaceTrustError> {
    let files = covered_files(workspace_root, profile_name)?;
    if files.is_empty() {
        return Ok(WorkspaceTrust::NoConfiguration);
    }
    let current = fingerprint(workspace_root, &files)?;
    let store = TrustStore::load(&trust_store_path(ylva_home))?;
    let approved = store
        .workspaces
        .get(&workspace_key(workspace_root))
        .and_then(|profiles| profiles.get(profile_name));
    Ok(match approved {
        Some(approved) if *approved == current => WorkspaceTrust::Trusted,
        Some(_) => WorkspaceTrust::Changed,
        None => WorkspaceTrust::Untrusted,
    })
}

/// Approve the current content; returns the covered files (empty when the
/// workspace has nothing to approve, in which case nothing is stored).
pub(crate) fn trust_workspace(
    ylva_home: &Path,
    workspace_root: &Path,
    profile_name: &str,
) -> Result<Vec<PathBuf>, WorkspaceTrustError> {
    let files = covered_files(workspace_root, profile_name)?;
    if files.is_empty() {
        return Ok(files);
    }
    let digest = fingerprint(workspace_root, &files)?;
    let store_path = trust_store_path(ylva_home);
    let mut store = TrustStore::load(&store_path)?;
    store
        .workspaces
        .entry(workspace_key(workspace_root))
        .or_default()
        .insert(profile_name.to_string(), digest);
    store.save(&store_path)?;
    Ok(files)
}

/// Remove an approval; `true` when one existed.
pub(crate) fn revoke_workspace_trust(
    ylva_home: &Path,
    workspace_root: &Path,
    profile_name: &str,
) -> Result<bool, WorkspaceTrustError> {
    let store_path = trust_store_path(ylva_home);
    let mut store = TrustStore::load(&store_path)?;
    let key = workspace_key(workspace_root);
    let Some(profiles) = store.workspaces.get_mut(&key) else {
        return Ok(false);
    };
    let removed = profiles.remove(profile_name).is_some();
    if profiles.is_empty() {
        store.workspaces.remove(&key);
    }
    if removed {
        store.save(&store_path)?;
    }
    Ok(removed)
}

/// Reminder for commands that just changed workspace configuration; `None`
/// when the profile's workspace configuration is trusted, absent or its
/// status cannot be determined.
pub(crate) fn workspace_trust_notice(workspace_root: &Path, profile_name: &str) -> Option<String> {
    let ylva_home = crate::ylva_home()?;
    match workspace_trust(&ylva_home, workspace_root, profile_name).ok()? {
        WorkspaceTrust::NoConfiguration | WorkspaceTrust::Trusted => None,
        WorkspaceTrust::Untrusted | WorkspaceTrust::Changed => {
            let profile_option = if profile_name == super::DEFAULT_PROFILE_NAME {
                String::new()
            } else {
                format!("--profile {profile_name} ")
            };
            Some(format!(
                "Ylva refuses to start in this workspace until you review its configuration \
                 and run `ylva {profile_option}trust`."
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _directory: tempfile::TempDir,
        home: PathBuf,
        workspace: PathBuf,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().expect("tempdir");
        let home = directory.path().join("home");
        let workspace = directory.path().join("workspace");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(workspace.join(".ylva/profiles/work")).expect("workspace");
        Fixture {
            _directory: directory,
            home,
            workspace,
        }
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("parent directory");
        std::fs::write(path, contents).expect("write");
    }

    fn status(fixture: &Fixture) -> WorkspaceTrust {
        workspace_trust(&fixture.home, &fixture.workspace, "work").expect("status")
    }

    #[test]
    fn workspace_without_configuration_needs_no_trust() {
        let fixture = fixture();
        assert_eq!(status(&fixture), WorkspaceTrust::NoConfiguration);
        assert!(trust_workspace(&fixture.home, &fixture.workspace, "work")
            .expect("trust")
            .is_empty());
        assert!(!fixture.home.join(TRUST_STORE_FILE_NAME).exists());
    }

    #[test]
    fn configuration_is_untrusted_until_approved_and_trusted_afterwards() {
        let fixture = fixture();
        write(
            &fixture.workspace.join(".ylva/profiles/work/config.toml"),
            "default_engine = \"a\"\n",
        );
        assert_eq!(status(&fixture), WorkspaceTrust::Untrusted);

        let files = trust_workspace(&fixture.home, &fixture.workspace, "work").expect("trust");

        assert_eq!(files.len(), 1);
        assert_eq!(status(&fixture), WorkspaceTrust::Trusted);
    }

    #[test]
    fn editing_any_covered_file_revokes_trust() {
        let fixture = fixture();
        let config = fixture.workspace.join(".ylva/profiles/work/config.toml");
        write(&config, "a = 1\n");
        trust_workspace(&fixture.home, &fixture.workspace, "work").expect("trust");

        write(&config, "a = 2\n");
        assert_eq!(status(&fixture), WorkspaceTrust::Changed);

        write(&config, "a = 1\n");
        assert_eq!(status(&fixture), WorkspaceTrust::Trusted);

        write(
            &fixture.workspace.join(".ylva/profiles/work/.env"),
            "OLLAMA_BASE_URL=http://evil\n",
        );
        assert_eq!(status(&fixture), WorkspaceTrust::Changed);
    }

    #[test]
    fn formatting_only_changes_keep_the_trust() {
        let fixture = fixture();
        let config = fixture.workspace.join(".ylva/profiles/work/config.toml");
        let environment = fixture.workspace.join(".ylva/profiles/work/.env");
        write(
            &config,
            "default_engine = \"a\"\n[engines.a]\nmodel = \"m\"\nprovider = 'p'\n",
        );
        write(&environment, "FIRST=1\nSECOND=\"two\"\n");
        trust_workspace(&fixture.home, &fixture.workspace, "work").expect("trust");

        write(
            &config,
            "# comment\ndefault_engine = \"a\"\n\n[engines.a]\n  provider = \"p\"   # inline\n  model = \"m\"\n",
        );
        write(&environment, "# note\nSECOND=two\nFIRST=1\n");
        assert_eq!(status(&fixture), WorkspaceTrust::Trusted);

        write(
            &config,
            "default_engine = \"a\"\n[engines.a]\nmodel = \"other\"\nprovider = \"p\"\n",
        );
        assert_eq!(status(&fixture), WorkspaceTrust::Changed);
    }

    #[test]
    fn project_agent_files_are_covered() {
        let fixture = fixture();
        write(
            &fixture.workspace.join(".ylva/profiles/work/config.toml"),
            "a = 1\n",
        );
        trust_workspace(&fixture.home, &fixture.workspace, "work").expect("trust");

        write(&fixture.workspace.join(".ylva/agents/helper.md"), "agent");

        assert_eq!(status(&fixture), WorkspaceTrust::Changed);
    }

    #[test]
    fn trust_is_scoped_to_the_workspace_and_profile() {
        let fixture = fixture();
        write(
            &fixture.workspace.join(".ylva/profiles/work/config.toml"),
            "a = 1\n",
        );
        write(
            &fixture.workspace.join(".ylva/profiles/other/config.toml"),
            "a = 1\n",
        );
        trust_workspace(&fixture.home, &fixture.workspace, "work").expect("trust");

        assert_eq!(
            workspace_trust(&fixture.home, &fixture.workspace, "other").expect("status"),
            WorkspaceTrust::Untrusted
        );
        let copy = fixture.workspace.parent().expect("parent").join("copy");
        std::fs::create_dir_all(copy.join(".ylva/profiles/work")).expect("copy");
        write(&copy.join(".ylva/profiles/work/config.toml"), "a = 1\n");
        assert_eq!(
            workspace_trust(&fixture.home, &copy, "work").expect("status"),
            WorkspaceTrust::Untrusted
        );
    }

    #[test]
    fn revoke_removes_the_approval_once() {
        let fixture = fixture();
        write(
            &fixture.workspace.join(".ylva/profiles/work/config.toml"),
            "a = 1\n",
        );
        trust_workspace(&fixture.home, &fixture.workspace, "work").expect("trust");

        assert!(revoke_workspace_trust(&fixture.home, &fixture.workspace, "work").expect("revoke"));
        assert_eq!(status(&fixture), WorkspaceTrust::Untrusted);
        assert!(
            !revoke_workspace_trust(&fixture.home, &fixture.workspace, "work").expect("revoke")
        );
    }

    #[cfg(unix)]
    #[test]
    fn trust_store_is_private_and_malformed_store_is_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = fixture();
        write(
            &fixture.workspace.join(".ylva/profiles/work/config.toml"),
            "a = 1\n",
        );
        trust_workspace(&fixture.home, &fixture.workspace, "work").expect("trust");
        let store_path = fixture.home.join(TRUST_STORE_FILE_NAME);
        let mode = std::fs::metadata(&store_path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, TRUST_STORE_FILE_MODE);

        std::fs::write(&store_path, "workspaces = 3").expect("corrupt");
        assert!(matches!(
            workspace_trust(&fixture.home, &fixture.workspace, "work"),
            Err(WorkspaceTrustError::MalformedStore { .. })
        ));
    }
}
