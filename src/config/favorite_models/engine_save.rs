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

//! Raw-TOML persistence of named engines created or updated from the model
//! picker's parameters page.
//!
//! Shares the source tracking of [`FavoriteModelsWriter`] so favorites and
//! engine saves never invalidate each other's snapshot. Only the touched
//! `[engines.<name>]` table is edited; connections are never created and
//! credentials (`api_key`, `api_base_url`) are never copied to a new engine.

use std::io;
use std::path::Path;

use toml_edit::{value, DocumentMut, Item, Table, Value};

use super::{FavoriteModelsWriter, FavoriteSaveError, FavoriteSaveOutcome, CONNECTION_SECTION_KEY};
use crate::llm::engine::{EngineConfig, DEFAULT_COMPACT_TARGET_RATIO};
use crate::private_fs::replace_file;

const ENGINES_SECTION_KEY: &str = "engines";
const MAXIMUM_ENGINE_NAME_LENGTH: usize = 64;

/// Keys that bind an engine to credentials; a copied engine never inherits them.
const CREDENTIAL_KEYS: [&str; 2] = ["api_key", "api_base_url"];

/// Desired state of one optional key of an engine table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSaveChange<T> {
    /// Leave the key as it is (or absent for a new engine).
    Keep,
    Set(T),
    Remove,
}

/// Whether to add a new engine table or edit an existing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSaveMode {
    /// Add `[engines.<name>]`, cloning the origin's table (without
    /// credentials) when `copy_from` is set.
    Create {
        name: String,
        copy_from: Option<String>,
    },
    /// Edit the existing `[engines.<name>]` table in place.
    Update { name: String },
}

impl EngineSaveMode {
    pub fn name(&self) -> &str {
        match self {
            Self::Create { name, .. } | Self::Update { name } => name,
        }
    }
}

/// One engine save from the picker's parameters page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSaveRequest {
    pub mode: EngineSaveMode,
    /// Existing `[provider_connections.<name>]` the engine uses.
    pub provider_connection: String,
    pub model: EngineSaveChange<String>,
    pub reasoning: EngineSaveChange<String>,
    pub context_length: EngineSaveChange<usize>,
    pub max_output_tokens: EngineSaveChange<usize>,
}

impl EngineSaveRequest {
    /// The engine definition this request produces, derived in memory from the
    /// origin definition (the copied or updated engine) or standard defaults.
    ///
    /// Credentials and endpoints are never carried over: they resolve from the
    /// provider connection.
    pub fn resulting_config(&self, origin: Option<&EngineConfig>) -> EngineConfig {
        let mut config = origin.cloned().unwrap_or_else(|| EngineConfig {
            provider_connection: self.provider_connection.clone(),
            model: String::new(),
            temperature: None,
            context_length: None,
            max_output_tokens: None,
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: DEFAULT_COMPACT_TARGET_RATIO,
            soft_compaction_threshold: crate::agent::compaction::DEFAULT_SOFT_COMPACTION_THRESHOLD,
            hard_compaction_threshold: crate::agent::compaction::DEFAULT_HARD_COMPACTION_THRESHOLD,
        });
        if matches!(self.mode, EngineSaveMode::Create { .. }) {
            config.api_key = None;
            config.api_base_url = None;
        }
        config.provider_connection = self.provider_connection.clone();
        if let Some(model) = self.model.resulting(Some(&config.model)) {
            config.model = model.clone();
        }
        config.reasoning = self.reasoning.resulting(config.reasoning.as_ref()).cloned();
        config.context_length = self
            .context_length
            .resulting(config.context_length.as_ref())
            .copied();
        config.max_output_tokens = self
            .max_output_tokens
            .resulting(config.max_output_tokens.as_ref())
            .copied();
        config
    }

    /// Lines `key = value` of the editable settings after the save, for the
    /// preview. Unset optional settings read as their default meaning.
    pub fn preview_lines(&self, origin: Option<&EngineConfig>) -> Vec<String> {
        let config = self.resulting_config(origin);
        vec![
            format!("provider_connection = {}", config.provider_connection),
            format!("model = {}", config.model),
            format!(
                "reasoning = {}",
                config.reasoning.as_deref().unwrap_or("(provider default)")
            ),
            format!(
                "context_length = {}",
                config
                    .context_length
                    .map_or("(model default)".to_string(), |tokens| tokens.to_string())
            ),
            format!(
                "max_output_tokens = {}",
                config
                    .max_output_tokens
                    .map_or("(no limit)".to_string(), |tokens| tokens.to_string())
            ),
        ]
    }
}

/// Check the engine-name grammar: 1–64 ASCII characters, starting with a
/// letter or digit, then letters, digits, `_` or `-`. Case-sensitive.
pub fn validate_engine_name(name: &str) -> Result<(), String> {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return Err("the engine name must not be empty".to_string());
    };
    if name.len() > MAXIMUM_ENGINE_NAME_LENGTH {
        return Err(format!(
            "the engine name must have at most {MAXIMUM_ENGINE_NAME_LENGTH} characters"
        ));
    }
    if !first.is_ascii_alphanumeric()
        || !characters.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(
            "the engine name must start with a letter or digit and contain only ASCII \
             letters, digits, '_' or '-'"
                .to_string(),
        );
    }
    Ok(())
}

impl FavoriteModelsWriter {
    /// Create or update one engine table in the loaded profile file.
    pub fn save_engine(
        &mut self,
        request: &EngineSaveRequest,
    ) -> Result<FavoriteSaveOutcome, FavoriteSaveError> {
        self.save_engine_with_publisher(request, replace_file)
    }

    pub(super) fn save_engine_with_publisher(
        &mut self,
        request: &EngineSaveRequest,
        publish: impl FnOnce(&Path, &[u8], u32) -> io::Result<()>,
    ) -> Result<FavoriteSaveOutcome, FavoriteSaveError> {
        self.edit_with_publisher(publish, |writer, document| {
            writer.apply_engine(document, request)
        })
    }

    fn apply_engine(
        &self,
        document: &mut DocumentMut,
        request: &EngineSaveRequest,
    ) -> Result<bool, FavoriteSaveError> {
        let name = request.mode.name();
        validate_engine_name(name).map_err(|reason| FavoriteSaveError::Unsupported { reason })?;
        let root = document.as_table_mut();
        if !root
            .get(CONNECTION_SECTION_KEY)
            .and_then(Item::as_table_like)
            .is_some_and(|section| section.contains_key(&request.provider_connection))
        {
            return Err(FavoriteSaveError::MissingConnectionTable {
                connection: request.provider_connection.clone(),
                path: self.path.clone(),
            });
        }
        let unsupported = |reason: &str| FavoriteSaveError::Unsupported {
            reason: format!(
                "{}: {reason}",
                crate::home_relative_path::display_path(&self.path)
            ),
        };
        if !root.contains_key(ENGINES_SECTION_KEY) {
            let mut section = Table::new();
            section.set_implicit(true);
            root.insert(ENGINES_SECTION_KEY, Item::Table(section));
        }
        let engines = root
            .get_mut(ENGINES_SECTION_KEY)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| unsupported("'engines' is not a table"))?;

        match &request.mode {
            EngineSaveMode::Create { name, copy_from } => {
                if engines.contains_key(name) {
                    return Err(FavoriteSaveError::EngineExists {
                        name: name.clone(),
                        path: self.path.clone(),
                    });
                }
                let mut table = match copy_from {
                    Some(origin) => engines
                        .get(origin)
                        .cloned()
                        .and_then(|item| item.into_table().ok())
                        .ok_or_else(|| FavoriteSaveError::EngineMissing {
                            name: origin.clone(),
                            path: self.path.clone(),
                        })?,
                    None => Table::new(),
                };
                for key in CREDENTIAL_KEYS {
                    table.remove(key);
                }
                table.remove("provider");
                table.set_implicit(false);
                table.insert(
                    "provider_connection",
                    value(request.provider_connection.as_str()),
                );
                apply_table_like_changes(&mut table, request);
                engines.insert(name, Item::Table(table));
            }
            EngineSaveMode::Update { name } => {
                let table = engines
                    .get_mut(name)
                    .and_then(Item::as_table_like_mut)
                    .ok_or_else(|| FavoriteSaveError::EngineMissing {
                        name: name.clone(),
                        path: self.path.clone(),
                    })?;
                let before = table_snapshot(table);
                apply_table_like_changes(table, request);
                if table_snapshot(table) == before {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

fn table_snapshot(table: &dyn toml_edit::TableLike) -> String {
    table
        .iter()
        .map(|(key, item)| format!("{key}={item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn apply_table_like_changes(table: &mut dyn toml_edit::TableLike, request: &EngineSaveRequest) {
    apply_change(
        table,
        "model",
        request.model.as_ref().map(|model| model.as_str().into()),
    );
    apply_change(
        table,
        "reasoning",
        request
            .reasoning
            .as_ref()
            .map(|effort| effort.as_str().into()),
    );
    apply_change(
        table,
        "context_length",
        request
            .context_length
            .as_ref()
            .map(|tokens| integer(*tokens)),
    );
    apply_change(
        table,
        "max_output_tokens",
        request
            .max_output_tokens
            .as_ref()
            .map(|tokens| integer(*tokens)),
    );
}

fn integer(tokens: usize) -> Value {
    Value::from(i64::try_from(tokens).unwrap_or(i64::MAX))
}

impl<T> EngineSaveChange<T> {
    fn as_ref(&self) -> EngineSaveChange<&T> {
        match self {
            Self::Keep => EngineSaveChange::Keep,
            Self::Set(item) => EngineSaveChange::Set(item),
            Self::Remove => EngineSaveChange::Remove,
        }
    }
}

impl<T> EngineSaveChange<T> {
    /// The value the key holds after the change, given its value before.
    fn resulting<'a>(&'a self, current: Option<&'a T>) -> Option<&'a T> {
        match self {
            Self::Keep => current,
            Self::Set(item) => Some(item),
            Self::Remove => None,
        }
    }
}

impl<T> EngineSaveChange<&T> {
    fn map<U>(self, convert: impl FnOnce(&T) -> U) -> EngineSaveChange<U> {
        match self {
            EngineSaveChange::Keep => EngineSaveChange::Keep,
            EngineSaveChange::Set(item) => EngineSaveChange::Set(convert(item)),
            EngineSaveChange::Remove => EngineSaveChange::Remove,
        }
    }
}

/// Apply one tri-state change, keeping the comment/spacing of a replaced value.
fn apply_change(table: &mut dyn toml_edit::TableLike, key: &str, change: EngineSaveChange<Value>) {
    match change {
        EngineSaveChange::Keep => {}
        EngineSaveChange::Remove => {
            table.remove(key);
        }
        EngineSaveChange::Set(mut new_value) => {
            if let Some(existing) = table.get(key).and_then(Item::as_value) {
                if existing.to_string().trim() == new_value.to_string().trim() {
                    return;
                }
                *new_value.decor_mut() = existing.decor().clone();
            }
            table.insert(key, Item::Value(new_value));
        }
    }
}
