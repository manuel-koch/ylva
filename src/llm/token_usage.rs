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

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use uuid::Uuid;

/// Source for the opt-in metadata-only token observation log.
#[derive(Debug, Clone, Default)]
pub(crate) enum TokenUsageDumpSource {
    /// Resolve `YLVA_TOKEN_USAGE_DUMP_PATH` when the model request starts.
    #[default]
    Environment,
    /// Use an explicit path (used by tests and embedders).
    #[cfg(test)]
    Template(PathBuf),
}

impl TokenUsageDumpSource {
    #[cfg(test)]
    pub(crate) fn from_template(path: impl Into<PathBuf>) -> Self {
        Self::Template(path.into())
    }

    fn path(&self) -> Option<PathBuf> {
        match self {
            Self::Environment => resolve_token_usage_dump_path(|name| std::env::var_os(name)),
            #[cfg(test)]
            Self::Template(path) => Some(path.clone()),
        }
    }
}

fn resolve_token_usage_dump_path(
    lookup: impl FnOnce(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    lookup("YLVA_TOKEN_USAGE_DUMP_PATH").map(PathBuf::from)
}

/// One local estimate paired with usage returned by that same model request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TokenUsageRecord {
    pub request_id: Uuid,
    pub timestamp: String,
    pub session_id: Option<Uuid>,
    /// Provider implementation/connection identifier used for this request.
    pub provider: String,
    pub provider_connection: String,
    pub model: String,
    pub estimated_input_tokens: usize,
    pub estimate_source: &'static str,
    pub reported_input_tokens: Option<usize>,
    pub reported_source: Option<&'static str>,
}

static APPEND_LOCK: Mutex<()> = Mutex::new(());

/// Append one metadata-only record to the configured JSONL file.
///
/// I/O errors are logged and ignored so diagnostics cannot break a model
/// request. The record type intentionally has no prompt/body fields.
pub(crate) fn append_token_usage_record(source: &TokenUsageDumpSource, record: &TokenUsageRecord) {
    let Some(path) = source.path() else {
        return;
    };
    append_token_usage_record_to_path(&path, record);
}

fn append_token_usage_record_to_path(path: &Path, record: &TokenUsageRecord) {
    let _lock = APPEND_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if let Err(error) = std::fs::create_dir_all(parent) {
        tracing::warn!(path = %path.display(), error = %error, "failed to create token usage dump directory");
        return;
    }
    let serialized = match serde_json::to_vec(record) {
        Ok(serialized) => serialized,
        Err(error) => {
            tracing::warn!(error = %error, "failed to serialize token usage record");
            return;
        }
    };
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(mut file) => {
            if let Err(error) = file
                .write_all(&serialized)
                .and_then(|()| file.write_all(b"\n"))
            {
                tracing::warn!(path = %path.display(), error = %error, "failed to append token usage record");
            }
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "failed to open token usage dump");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_metadata_only_jsonl_records_to_the_configured_path() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let path = temporary_directory.path().join("nested/usage.jsonl");
        let source = TokenUsageDumpSource::from_template(&path);
        let first_record = TokenUsageRecord {
            request_id: Uuid::new_v4(),
            timestamp: "2026-10-03T12:34:56.789Z".to_string(),
            session_id: Some(Uuid::new_v4()),
            provider: "openrouter".to_string(),
            provider_connection: "hosted".to_string(),
            model: "test/model".to_string(),
            estimated_input_tokens: 42,
            estimate_source: "cl100k_base_request_json",
            reported_input_tokens: Some(47),
            reported_source: Some("openai_compatible.usage.prompt_tokens"),
        };
        let second_record = TokenUsageRecord {
            request_id: Uuid::new_v4(),
            reported_input_tokens: None,
            reported_source: None,
            ..first_record.clone()
        };

        append_token_usage_record(&source, &first_record);
        append_token_usage_record(&source, &second_record);

        let contents = std::fs::read_to_string(path).expect("usage dump");
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2, "one JSON record is appended per request");
        let first: serde_json::Value = serde_json::from_str(lines[0]).expect("first JSON record");
        let second: serde_json::Value = serde_json::from_str(lines[1]).expect("second JSON record");
        assert_eq!(first["request_id"], first_record.request_id.to_string());
        assert_eq!(first["timestamp"], first_record.timestamp);
        assert_eq!(
            first["session_id"],
            first_record.session_id.unwrap().to_string()
        );
        assert_eq!(first["estimated_input_tokens"], 42);
        assert_eq!(first["reported_input_tokens"], 47);
        assert_eq!(
            first["reported_source"],
            "openai_compatible.usage.prompt_tokens"
        );
        assert!(second["reported_input_tokens"].is_null());
        assert!(second["reported_source"].is_null());
        assert!(!contents.contains("conversation"));
        assert_eq!(first["model"], "test/model");
    }

    #[test]
    fn environment_dump_path_is_opt_in_and_uses_the_documented_variable() {
        assert_eq!(resolve_token_usage_dump_path(|_| None), None);
        assert_eq!(
            resolve_token_usage_dump_path(|name| {
                assert_eq!(name, "YLVA_TOKEN_USAGE_DUMP_PATH");
                Some(std::ffi::OsString::from("logs/token-usage.jsonl"))
            }),
            Some(PathBuf::from("logs/token-usage.jsonl"))
        );
    }
}
