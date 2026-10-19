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

//! Applied-edit outcome and the result type of a successful `edit` call.

use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::Serialize;

use super::super::{DiffSection, Section, ToolCallResult};

/// Structured outcome data captured at patch-application time, mode-specific.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditApplied {
    /// Outcome of a `SearchReplace` edit.
    SearchReplace {
        /// Number of replacements made (1 when `replace_all` is `false`, ≥ 1 otherwise).
        replacements_made: usize,
    },
    /// Outcome of a `LineRange` edit.
    LineRangeReplace {
        /// First line replaced (1-based).
        start_line: usize,
        /// Last line replaced (1-based, inclusive).
        end_line: usize,
    },
}

/// Result metadata returned to the LLM after a successful `edit` call.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EditResultMetadata {
    /// For `SearchReplace` mode: number of replacements made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacements_made: Option<usize>,
    /// For `LineRange` mode: first line replaced (1-based).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    /// For `LineRange` mode: last line replaced (1-based, inclusive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
}

/// Output of a successful `edit` tool invocation.
#[derive(Debug)]
pub struct EditResult {
    pub path: String,
    pub outcome: EditApplied,
    pub diff: String,
}

impl ToolCallResult for EditResult {
    fn text(&self) -> String {
        match &self.outcome {
            EditApplied::SearchReplace { replacements_made } => {
                format!(
                    "Edited '{}': {replacements_made} replacement(s) made",
                    self.path
                )
            }
            EditApplied::LineRangeReplace {
                start_line,
                end_line,
            } => {
                format!(
                    "Edited '{}': replaced lines {start_line}–{end_line}",
                    self.path
                )
            }
        }
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        let meta = match &self.outcome {
            EditApplied::SearchReplace { replacements_made } => EditResultMetadata {
                replacements_made: Some(*replacements_made),
                start_line: None,
                end_line: None,
            },
            EditApplied::LineRangeReplace {
                start_line,
                end_line,
            } => EditResultMetadata {
                replacements_made: None,
                start_line: Some(*start_line),
                end_line: Some(*end_line),
            },
        };
        serde_json::to_value(meta).ok()
    }

    fn metadata_schema(&self) -> Option<RootSchema> {
        Some(schema_for!(EditResultMetadata))
    }

    fn content_sections(&self) -> Vec<Section> {
        if self.diff.is_empty() {
            return vec![];
        }
        vec![Section::Diff(DiffSection {
            label: Some(self.path.clone()),
            content: self.diff.clone(),
        })]
    }
}
