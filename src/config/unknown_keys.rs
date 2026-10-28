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

//! Detection of configuration keys no struct consumes.
//!
//! Unknown keys are tolerated so a file written for a newer Ylva still loads,
//! but they are never silent: loading returns them as [`ConfigWarning`]s. The
//! detection site neither prints nor logs; callers decide how to surface them.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::home_relative_path::display_path;

/// Every key name any configuration table accepts. Only used to suggest a
/// correction for an unknown key; a key missing here merely loses its
/// suggestion.
const KNOWN_KEYS: &[&str] = &[
    "agents",
    "allowed_models",
    "allowed_scripts",
    "allowed_skills",
    "allowed_tools",
    "api_base_url",
    "api_key",
    "auto_expand_approval",
    "auto_expand_tool",
    "backend",
    "backends",
    "base_url",
    "brave",
    "can_spawn",
    "command",
    "compact_target_ratio",
    "compaction",
    "context_length",
    "default_engine",
    "default_model",
    "description",
    "engine",
    "engines",
    "favorite_models",
    "hard_compaction_threshold",
    "input_history_limit",
    "keybindings",
    "max_concurrent_subagents",
    "max_tool_iterations",
    "model",
    "only_arguments",
    "pattern",
    "persona",
    "provider",
    "provider_connection",
    "provider_connections",
    "reason",
    "reasoning",
    "result_limit",
    "rule",
    "rules",
    "searxng",
    "soft_compaction_threshold",
    "target",
    "temperature",
    "theme",
    "timeout_sec",
    "timeout_secs",
    "tools",
    "tui",
    "utility_engine",
    "verdict",
    "web_fetch",
    "web_search",
    "with_arguments",
];

/// What a [`ConfigWarning`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarningKind {
    /// A key no configuration struct consumed.
    UnknownKey,
    /// A tool name listed in a setting that matches no registered tool.
    UnknownToolName {
        /// The offending tool name as written.
        tool_name: String,
    },
}

/// One problem found in an otherwise loadable configuration: an unknown key
/// or an unknown tool name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    /// File the setting was read from.
    pub file: PathBuf,
    /// Dotted path of the key inside that file, e.g. `engines.main.modle`.
    pub key_path: String,
    /// Closest known key name, when one is similar enough.
    pub suggestion: Option<String>,
    /// What this warning reports.
    pub kind: ConfigWarningKind,
}

impl ConfigWarning {
    /// Warning for `tool_name` listed under `key_path` that matches no
    /// registered tool.
    pub(crate) fn unknown_tool_name(file: &Path, key_path: &str, tool_name: &str) -> Self {
        ConfigWarning {
            file: file.to_path_buf(),
            key_path: key_path.to_string(),
            suggestion: None,
            kind: ConfigWarningKind::UnknownToolName {
                tool_name: tool_name.to_string(),
            },
        }
    }
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let ConfigWarningKind::UnknownToolName { tool_name } = &self.kind {
            return write!(
                formatter,
                "{}: unknown tool name `{tool_name}` in `{}`",
                display_path(&self.file),
                self.key_path
            );
        }
        write!(
            formatter,
            "{}: unknown key `{}`",
            display_path(&self.file),
            self.key_path
        )?;
        if let Some(suggestion) = &self.suggestion {
            write!(formatter, "; did you mean `{suggestion}`?")?;
        }
        Ok(())
    }
}

/// Deserialize `deserializer` into `T`, appending a warning to `warnings` for
/// every key `T` ignores. `key_prefix` is prepended to reported paths when the
/// deserialized value is a sub-table of the file (e.g. `agents`).
pub(crate) fn deserialize_reporting_unknown_keys<'de, T, D>(
    deserializer: D,
    file: &Path,
    key_prefix: &str,
    warnings: &mut Vec<ConfigWarning>,
) -> Result<T, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    serde_ignored::deserialize(deserializer, |ignored_path| {
        let rendered = ignored_path.to_string().replace(".?", "").replace('?', "");
        let key_path = match (key_prefix.is_empty(), rendered.as_str()) {
            (true, _) => rendered,
            (false, "") => key_prefix.to_string(),
            (false, _) => format!("{key_prefix}.{rendered}"),
        };
        let last_segment = key_path.rsplit('.').next().unwrap_or(&key_path);
        warnings.push(ConfigWarning {
            file: file.to_path_buf(),
            suggestion: suggest_key(last_segment),
            kind: ConfigWarningKind::UnknownKey,
            key_path,
        });
    })
}

/// The known key closest to `unknown` by edit distance, if close enough to be
/// a plausible typo.
fn suggest_key(unknown: &str) -> Option<String> {
    let unknown_length = unknown.chars().count();
    let allowed_distance = (unknown_length / 3)
        .clamp(2, 3)
        .min(unknown_length.saturating_sub(1));
    KNOWN_KEYS
        .iter()
        .map(|known| (edit_distance(unknown, known), *known))
        .filter(|(distance, _)| *distance > 0 && *distance <= allowed_distance)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, known)| known.to_string())
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right_characters: Vec<char> = right.chars().collect();
    let mut previous_row: Vec<usize> = (0..=right_characters.len()).collect();
    for (left_index, left_character) in left.chars().enumerate() {
        let mut current_row = vec![left_index + 1];
        for (right_index, right_character) in right_characters.iter().enumerate() {
            let substitution_cost = usize::from(left_character != *right_character);
            current_row.push(
                (previous_row[right_index] + substitution_cost)
                    .min(previous_row[right_index + 1] + 1)
                    .min(current_row[right_index] + 1),
            );
        }
        previous_row = current_row;
    }
    previous_row[right_characters.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Sample {
        #[serde(default)]
        api_base_url: Option<String>,
        #[serde(default)]
        nested: Option<Nested>,
    }

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Nested {
        #[serde(default)]
        model: Option<String>,
    }

    fn warnings_for(contents: &str, prefix: &str) -> Vec<ConfigWarning> {
        let mut warnings = Vec::new();
        let _: Sample = deserialize_reporting_unknown_keys(
            toml::Deserializer::parse(contents).expect("valid toml"),
            Path::new("/tmp/config.toml"),
            prefix,
            &mut warnings,
        )
        .expect("sample deserializes");
        warnings
    }

    #[test]
    fn unknown_keys_are_reported_with_full_path_and_suggestion() {
        let warnings = warnings_for(
            "api_bsae_url = \"x\"\n[nested]\nmodle = \"y\"\nunrelated_option = 1\n",
            "",
        );
        let rendered: Vec<String> = warnings.iter().map(ToString::to_string).collect();
        assert_eq!(
            rendered,
            vec![
                "/tmp/config.toml: unknown key `api_bsae_url`; did you mean `api_base_url`?",
                "/tmp/config.toml: unknown key `nested.modle`; did you mean `model`?",
                "/tmp/config.toml: unknown key `nested.unrelated_option`",
            ]
        );
    }

    #[test]
    fn known_keys_produce_no_warning() {
        assert!(warnings_for("api_base_url = \"x\"\n[nested]\nmodel = \"y\"\n", "").is_empty());
    }

    #[test]
    fn key_prefix_is_prepended_to_reported_paths() {
        let warnings = warnings_for("api_bsae_url = \"x\"\n", "agents.helper");
        assert_eq!(warnings[0].key_path, "agents.helper.api_bsae_url");
    }
}
