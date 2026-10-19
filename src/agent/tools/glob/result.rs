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

//! Match counting, ordering and rendering of `glob` results.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use super::super::{ContentSection, Section, ToolCallResult};

/// Counts matching descendants by path type.
///
/// `total` is always the sum of `files` and `directories`. The search root is
/// never included in any of these values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GlobMatchCounts {
    /// Number of matching file paths.
    pub files: usize,
    /// Number of matching directory paths.
    pub directories: usize,
    /// Sum of `files` and `directories`.
    pub total: usize,
}

impl GlobMatchCounts {
    pub(super) fn add_path_kind(&mut self, path_kind: GlobPathKind) {
        match path_kind {
            GlobPathKind::File => self.files += 1,
            GlobPathKind::Directory => self.directories += 1,
        }
        self.total += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GlobPathKind {
    File,
    Directory,
}

#[derive(Debug)]
pub(super) struct GlobCandidate {
    pub(super) path: PathBuf,
    pub(super) path_kind: GlobPathKind,
}

impl GlobCandidate {
    pub(super) fn from_path(path: PathBuf) -> Self {
        let path_kind = if std::fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_dir())
            .unwrap_or(false)
        {
            GlobPathKind::Directory
        } else {
            GlobPathKind::File
        };
        Self { path, path_kind }
    }
}

/// Structured metadata for a successful `glob` tool invocation.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GlobResultMetadata {
    /// Number of matching file paths returned.
    pub files: usize,
    /// Number of matching directory paths returned.
    pub directories: usize,
    /// Sum of `files` and `directories`.
    pub total: usize,
    /// `true` when `limit` omitted additional matching paths.
    pub truncated: bool,
}

/// Output of a successful `glob` tool invocation.
#[derive(Debug)]
pub struct GlobResult {
    /// Paths included in this response. Empty when `only_count=true`.
    pub matched_paths: Vec<String>,
    /// Matching descendants returned after type filters and `limit` are applied.
    pub matches: GlobMatchCounts,
    /// `true` when `limit` omitted additional matching paths.
    pub truncated: bool,
    matched_path_kinds: Vec<GlobPathKind>,
}

impl GlobResult {
    pub(super) fn new(
        matched_paths: Vec<String>,
        matched_path_kinds: Vec<GlobPathKind>,
        matches: GlobMatchCounts,
        truncated: bool,
    ) -> Self {
        GlobResult {
            matched_paths,
            matched_path_kinds,
            matches,
            truncated,
        }
    }

    pub(super) fn truncate_to_token_budget(&mut self, max_output_tokens: usize) {
        let estimator = crate::llm::TokenEstimator::Tiktoken;
        let mut retained_paths = Vec::new();
        let mut used_tokens = 0usize;
        for path in &self.matched_paths {
            let separator_tokens = usize::from(!retained_paths.is_empty());
            let remaining = max_output_tokens.saturating_sub(used_tokens + separator_tokens);
            let (path_prefix, truncated_path) = estimator.truncate(path, remaining);
            if path_prefix.is_empty() {
                break;
            }
            let cost = estimator.estimate_tokens(&path_prefix) + separator_tokens;
            used_tokens += cost;
            retained_paths.push(path_prefix);
            if truncated_path {
                break;
            }
        }
        if retained_paths.len() < self.matched_paths.len()
            || retained_paths
                .iter()
                .zip(&self.matched_paths)
                .any(|(left, right)| left != right)
        {
            self.truncated = true;
            self.matched_paths = retained_paths;
            self.matched_path_kinds.truncate(self.matched_paths.len());
            self.matches = self.matched_path_kinds.iter().fold(
                GlobMatchCounts::default(),
                |mut counts, path_kind| {
                    counts.add_path_kind(*path_kind);
                    counts
                },
            );
        }
    }
}

impl ToolCallResult for GlobResult {
    fn text(&self) -> String {
        if self.matched_paths.is_empty() && self.matches.total > 0 {
            // only_count mode: no paths in response but the LLM needs the number.
            return self.matches.total.to_string();
        }
        self.matched_paths.join("\n")
    }

    fn display_summary(&self) -> Vec<(String, String)> {
        let mut summary = Vec::new();
        if self.matches.files > 0 {
            summary.push(("files".to_string(), self.matches.files.to_string()));
        }
        if self.matches.directories > 0 {
            summary.push((
                "directories".to_string(),
                self.matches.directories.to_string(),
            ));
        }
        if self.matches.total == 0 || (self.matches.files > 0 && self.matches.directories > 0) {
            summary.push(("total".to_string(), self.matches.total.to_string()));
        }
        if self.truncated {
            summary.push(("truncated".to_string(), "true".to_string()));
        }
        summary
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        let metadata = GlobResultMetadata {
            files: self.matches.files,
            directories: self.matches.directories,
            total: self.matches.total,
            truncated: self.truncated,
        };
        serde_json::to_value(metadata).ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schemars::schema_for!(GlobResultMetadata))
    }

    fn content_sections(&self) -> Vec<Section> {
        let text = self.matched_paths.join("\n");
        if text.is_empty() {
            return vec![];
        }
        vec![Section::Content(ContentSection {
            label: None,
            content: text,
            file_extension: None,
            line_number_offset: Some(0),
        })]
    }
}
