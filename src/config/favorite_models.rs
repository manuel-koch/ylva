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

//! Narrow raw-TOML editor that persists `favorite_models` membership.
//!
//! The editor never serializes the resolved [`ProfileConfig`](super::ProfileConfig).
//! It edits the raw text of the one profile file that was actually loaded,
//! touching only the `favorite_models` key of one existing provider-connection
//! table (`provider_connections`; standard tables, inline tables and dotted
//! keys are supported). Everything else —
//! comments, unknown fields, credentials, interpolation expressions, blank
//! lines — stays as written. It never creates profiles, connections or tables,
//! never writes `.env`, and never touches the workspace trust store.
//!
//! # Source tracking
//!
//! A [`FavoriteModelsWriter`] owns a snapshot of the source: its resolved path,
//! file identity (device and inode on Unix) and raw contents, captured from
//! the document used to load the profile. Before each publication the file is
//! re-read and compared with the snapshot. Any difference (disappearance,
//! replacement, symlink retargeting, external edits) is reported as
//! [`FavoriteSaveError::SourceChanged`]: there is no rebase, the user must
//! restart Ylva. The snapshot advances only after a confirmed successful save.
//!
//! # Known limitation
//!
//! Publication is an atomic same-directory rename ([`replace_file`]). A rename
//! is not a compare-and-swap: an editor that does not cooperate with this
//! writer can still modify the file between the final comparison and the
//! rename, and that edit would be lost. This narrow race is an accepted
//! limitation, not a lost-update guarantee. Writes within one process are
//! serialized by `&mut self`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, RawString, Value};

use super::layered::ConfigLayer;
use super::profile::resolve_profile_config_file;
use super::ConfigError;
use crate::home_relative_path::display_path;
use crate::private_fs::replace_file;

#[cfg(test)]
mod tests;

/// Key of the favorites list inside a provider-connection table.
const FAVORITE_MODELS_KEY: &str = "favorite_models";

/// Section that holds the provider-connection tables.
const CONNECTION_SECTION_KEY: &str = "provider_connections";

/// Unix mode used on platforms without permission bits.
#[cfg(not(unix))]
const DEFAULT_FILE_MODE: u32 = 0o644;

/// Configuration layer whose file backs the saved favorites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FavoriteSourceLayer {
    /// A profile file in the Ylva home (possibly shared between workspaces).
    Home,
    /// A workspace profile file; changing it requires `ylva trust` again.
    Workspace,
}

impl FavoriteSourceLayer {
    /// Map a loaded configuration layer; built-in contents have no file to save to.
    pub fn from_config_layer(layer: ConfigLayer) -> Option<Self> {
        match layer {
            ConfigLayer::Home => Some(Self::Home),
            ConfigLayer::Workspace => Some(Self::Workspace),
            ConfigLayer::BuiltIn => None,
        }
    }
}

/// Result of a successful [`FavoriteModelsWriter::set_favorite`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavoriteSaveOutcome {
    /// Layer of the file that holds the favorites.
    pub layer: FavoriteSourceLayer,
    /// Path of the profile file as captured from the loaded profile.
    pub path: PathBuf,
    /// `false` when the file already had the desired membership (nothing written).
    pub changed: bool,
    /// `true` when an error occurred after the file was replaced (for example a
    /// directory sync failure). The new contents were confirmed by re-reading,
    /// but their durability across a crash is not guaranteed.
    pub publication_uncertain: bool,
}

/// Why favorites could not be saved.
#[derive(Debug, thiserror::Error)]
pub enum FavoriteSaveError {
    /// The source cannot hold favorites (for example an unsupported TOML shape).
    #[error("favorites cannot be saved: {reason}")]
    Unsupported { reason: String },
    /// The profile file differs from what Ylva loaded or last saved.
    #[error(
        "the profile file {} changed outside Ylva ({detail}); restart Ylva to load it, \
         the favorite was not saved",
        display_path(path)
    )]
    SourceChanged { path: PathBuf, detail: String },
    /// A failure before publication left the source untouched; trying again is safe.
    #[error(
        "the favorite could not be saved to {}: {detail}; the file is unchanged, try again",
        display_path(path)
    )]
    Retryable { path: PathBuf, detail: String },
    /// Removal is ambiguous because `${...}` entries might expand to the model ID.
    #[error(
        "cannot remove '{model_id}' from favorite_models of provider connection \
         '{connection}' in {}: the list contains interpolation expressions that may expand \
         to it; edit the file by hand",
        display_path(path)
    )]
    AmbiguousInterpolation {
        connection: String,
        model_id: String,
        path: PathBuf,
    },
    /// The profile has no table for the connection; none is ever created.
    #[error(
        "provider connection '{connection}' has no table in {}; favorites are only saved \
         to an existing connection table",
        display_path(path)
    )]
    MissingConnectionTable { connection: String, path: PathBuf },
    /// An error occurred after the replacement may have been published.
    #[error(
        "it is unclear whether the favorite was saved to {} ({detail}); restart Ylva and \
         check the file",
        display_path(path)
    )]
    PostPublishUncertain { path: PathBuf, detail: String },
}

impl FavoriteSaveError {
    /// Whether the same save may be attempted again without restarting.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable { .. })
    }
}

#[cfg(unix)]
type FileIdentity = (u64, u64);
#[cfg(not(unix))]
type FileIdentity = ();

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> FileIdentity {}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> u32 {
    DEFAULT_FILE_MODE
}

/// What was read from the source file just now.
struct CurrentSource {
    target_path: PathBuf,
    identity: FileIdentity,
    contents: String,
    read_only: bool,
    mode: u32,
}

/// The identity and contents the file must still have to be written safely.
struct SourceSnapshot {
    target_path: PathBuf,
    identity: Option<FileIdentity>,
    contents: String,
}

/// Persists favorite-model membership into the raw profile TOML it was loaded from.
///
/// See the [module documentation](self) for semantics and the accepted race
/// limitation. Operations are blocking; async callers wrap them in
/// `spawn_blocking`.
pub struct FavoriteModelsWriter {
    path: PathBuf,
    layer: FavoriteSourceLayer,
    snapshot: SourceSnapshot,
}

impl FavoriteModelsWriter {
    /// Capture the source from the document actually used to load the profile.
    ///
    /// `raw_contents` is the text that was parsed. The file identity is taken
    /// from `path` now; if the file cannot be inspected, every later save
    /// reports [`FavoriteSaveError::SourceChanged`].
    pub fn from_loaded_source(
        path: PathBuf,
        raw_contents: String,
        layer: FavoriteSourceLayer,
    ) -> Self {
        let (target_path, identity) = match fs::canonicalize(&path)
            .and_then(|canonical| fs::metadata(&canonical).map(|meta| (canonical, meta)))
        {
            Ok((canonical, metadata)) => (canonical, Some(file_identity(&metadata))),
            Err(_) => (path.clone(), None),
        };
        Self {
            path,
            layer,
            snapshot: SourceSnapshot {
                target_path,
                identity,
                contents: raw_contents,
            },
        }
    }

    /// Resolve and capture the profile file through the same workspace/home
    /// layering [`ProfileConfig::load`](super::ProfileConfig::load) uses.
    ///
    /// Returns `Ok(None)` when the profile comes from built-in defaults and
    /// therefore has no file to save to.
    pub fn for_profile(
        profiles_root: &Path,
        profile_name: &str,
        workspace_root: Option<&Path>,
    ) -> Result<Option<Self>, ConfigError> {
        let Some(resolved) =
            resolve_profile_config_file(profiles_root, profile_name, workspace_root)?
        else {
            return Ok(None);
        };
        Ok(FavoriteSourceLayer::from_config_layer(resolved.layer)
            .map(|layer| Self::from_loaded_source(resolved.path, resolved.contents, layer)))
    }

    /// Set the exact membership of `model_id` in the favorites of `connection`.
    ///
    /// This is a desired-state operation, not a toggle: adding an existing
    /// (case-sensitive) ID or removing an absent one succeeds with
    /// `changed == false` and writes nothing. Adding appends; removing deletes
    /// every exact duplicate and drops the key when the list becomes empty.
    pub fn set_favorite(
        &mut self,
        connection: &str,
        model_id: &str,
        favorite: bool,
    ) -> Result<FavoriteSaveOutcome, FavoriteSaveError> {
        self.set_favorite_with_publisher(connection, model_id, favorite, replace_file)
    }

    fn set_favorite_with_publisher(
        &mut self,
        connection: &str,
        model_id: &str,
        favorite: bool,
        publish: impl FnOnce(&Path, &[u8], u32) -> io::Result<()>,
    ) -> Result<FavoriteSaveOutcome, FavoriteSaveError> {
        let current = self.read_current_source()?;
        self.verify_unchanged(&current)?;
        if current.read_only {
            return Err(FavoriteSaveError::Retryable {
                path: self.path.clone(),
                detail: "the file is read-only".to_string(),
            });
        }

        let mut document: DocumentMut =
            current
                .contents
                .parse()
                .map_err(
                    |error: toml_edit::TomlError| FavoriteSaveError::Unsupported {
                        reason: format!(
                            "{} is not editable TOML: {error}",
                            display_path(&self.path)
                        ),
                    },
                )?;
        let changed = self.apply_membership(&mut document, connection, model_id, favorite)?;
        if !changed {
            return Ok(self.outcome(false, false));
        }

        let intended_contents = document.to_string();
        let publish_result = publish(
            &current.target_path,
            intended_contents.as_bytes(),
            current.mode,
        );
        let reconciliation = self.read_current_source();
        self.reconcile(current, intended_contents, publish_result, reconciliation)
    }

    fn outcome(&self, changed: bool, publication_uncertain: bool) -> FavoriteSaveOutcome {
        FavoriteSaveOutcome {
            layer: self.layer,
            path: self.path.clone(),
            changed,
            publication_uncertain,
        }
    }

    /// Decide what a publication attempt left behind by re-reading the file.
    fn reconcile(
        &mut self,
        before: CurrentSource,
        intended_contents: String,
        publish_result: io::Result<()>,
        after: Result<CurrentSource, FavoriteSaveError>,
    ) -> Result<FavoriteSaveOutcome, FavoriteSaveError> {
        let publication_failed = publish_result.is_err();
        let uncertain = |detail: String| FavoriteSaveError::PostPublishUncertain {
            path: self.path.clone(),
            detail,
        };
        let after = match after {
            Ok(after) => after,
            Err(_) => {
                return Err(uncertain(match publish_result {
                    Ok(()) => "the file could not be re-read after saving".to_string(),
                    Err(error) => format!("{error}; the file could not be re-read"),
                }))
            }
        };

        if after.contents == intended_contents && after.target_path == before.target_path {
            self.snapshot = SourceSnapshot {
                target_path: after.target_path,
                identity: Some(after.identity),
                contents: intended_contents,
            };
            return Ok(self.outcome(true, publication_failed));
        }
        match publish_result {
            Err(error)
                if after.contents == before.contents
                    && after.identity == before.identity
                    && after.target_path == before.target_path =>
            {
                Err(FavoriteSaveError::Retryable {
                    path: self.path.clone(),
                    detail: error.to_string(),
                })
            }
            Err(error) => Err(uncertain(format!(
                "{error}; the file now has unexpected contents"
            ))),
            Ok(()) => Err(uncertain(
                "the file was modified again right after saving".to_string(),
            )),
        }
    }

    fn read_current_source(&self) -> Result<CurrentSource, FavoriteSaveError> {
        let source_changed = |detail: &str| FavoriteSaveError::SourceChanged {
            path: self.path.clone(),
            detail: detail.to_string(),
        };
        let retryable = |error: io::Error| FavoriteSaveError::Retryable {
            path: self.path.clone(),
            detail: error.to_string(),
        };
        let classify = |error: io::Error| match error.kind() {
            io::ErrorKind::NotFound => source_changed("it no longer exists"),
            io::ErrorKind::InvalidData => source_changed("it is no longer valid UTF-8 text"),
            _ => retryable(error),
        };
        let target_path = fs::canonicalize(&self.path).map_err(classify)?;
        let metadata = fs::metadata(&target_path).map_err(classify)?;
        let contents = fs::read_to_string(&target_path).map_err(classify)?;
        Ok(CurrentSource {
            identity: file_identity(&metadata),
            read_only: metadata.permissions().readonly(),
            mode: file_mode(&metadata),
            target_path,
            contents,
        })
    }

    fn verify_unchanged(&self, current: &CurrentSource) -> Result<(), FavoriteSaveError> {
        let detail = if current.target_path != self.snapshot.target_path {
            "it was replaced or now resolves to a different file"
        } else if self.snapshot.identity != Some(current.identity) {
            "the file was replaced"
        } else if current.contents != self.snapshot.contents {
            "its contents were modified"
        } else {
            return Ok(());
        };
        Err(FavoriteSaveError::SourceChanged {
            path: self.path.clone(),
            detail: detail.to_string(),
        })
    }

    /// Edit `favorite_models` of the existing connection table; returns whether
    /// the document changed.
    fn apply_membership(
        &self,
        document: &mut DocumentMut,
        connection: &str,
        model_id: &str,
        favorite: bool,
    ) -> Result<bool, FavoriteSaveError> {
        let unsupported = |reason: String| FavoriteSaveError::Unsupported { reason };
        let root = document.as_table_mut();
        if !root
            .get(CONNECTION_SECTION_KEY)
            .and_then(Item::as_table_like)
            .is_some_and(|section| section.contains_key(connection))
        {
            return Err(FavoriteSaveError::MissingConnectionTable {
                connection: connection.to_string(),
                path: self.path.clone(),
            });
        }
        let connection_table = root
            .get_mut(CONNECTION_SECTION_KEY)
            .and_then(Item::as_table_like_mut)
            .and_then(|section| section.get_mut(connection))
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| {
                unsupported(format!("provider connection '{connection}' is not a table"))
            })?;

        if !connection_table.contains_key(FAVORITE_MODELS_KEY) {
            if !favorite {
                return Ok(false);
            }
            let mut array = Array::new();
            array.push(model_id);
            connection_table.insert(FAVORITE_MODELS_KEY, Item::Value(Value::Array(array)));
            return Ok(true);
        }
        let array = connection_table
            .get_mut(FAVORITE_MODELS_KEY)
            .and_then(Item::as_array_mut)
            .ok_or_else(|| unsupported("favorite_models is not an array".to_string()))?;
        if array.iter().any(|entry| entry.as_str().is_none()) {
            return Err(unsupported(
                "favorite_models contains a non-string entry".to_string(),
            ));
        }
        let literal_matches = array
            .iter()
            .filter(|entry| entry.as_str() == Some(model_id))
            .count();

        if favorite {
            if literal_matches > 0 {
                return Ok(false);
            }
            append_entry(array, model_id);
            return Ok(true);
        }
        if literal_matches == 0 {
            if array
                .iter()
                .any(|entry| entry.as_str().is_some_and(|text| text.contains("${")))
            {
                return Err(FavoriteSaveError::AmbiguousInterpolation {
                    connection: connection.to_string(),
                    model_id: model_id.to_string(),
                    path: self.path.clone(),
                });
            }
            return Ok(false);
        }
        array.retain(|entry| entry.as_str() != Some(model_id));
        if array.is_empty() {
            connection_table.remove(FAVORITE_MODELS_KEY);
        }
        Ok(true)
    }
}

/// Append `model_id`, continuing the multi-line layout (newline and
/// indentation, without comments) of an existing array.
fn append_entry(array: &mut Array, model_id: &str) {
    let layout_prefix: Option<String> = array
        .iter()
        .last()
        .and_then(|entry| entry.decor().prefix())
        .and_then(RawString::as_str)
        .and_then(|prefix| prefix.rfind('\n').map(|index| &prefix[index..]))
        .map(str::to_string);
    match layout_prefix {
        Some(prefix) => {
            let mut value = Value::from(model_id);
            value.decor_mut().set_prefix(prefix);
            value.decor_mut().set_suffix("");
            array.push_formatted(value);
        }
        None => array.push(model_id),
    }
}
