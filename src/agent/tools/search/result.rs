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

//! Truncation, text rendering and metadata of `search` results.

use schemars::JsonSchema;
use serde::Serialize;

use super::super::{ContentSection, SearchResultSection, Section, ToolCallResult};

/// Structured metadata for a successful `search` invocation.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchResultMetadata {
    /// Number of files containing at least one match.
    pub matching_files: usize,
    /// Total matching lines across all files (absent in files-with-matches mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matching_lines: Option<usize>,
    /// Present and `true` when output exceeded the line cap and was truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

/// Successful output from a `search` invocation.
#[derive(Debug)]
pub struct SearchResult {
    /// All matching sections, one per file that contained at least one match.
    pub sections: Vec<SearchResultSection>,
    /// `true` when the output-line cap was reached and some output was
    /// discarded.
    pub truncated: bool,
    /// `true` when the search was run in files-with-matches mode (no line
    /// content collected, sections carry only file paths).
    pub files_with_matches: bool,
    /// Pre-formatted text for the LLM.
    text: String,
}

impl SearchResult {
    pub(super) fn truncate_to_token_budget(&mut self, max_output_tokens: usize) {
        if crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&self.text) <= max_output_tokens {
            return;
        }
        let estimator = crate::llm::TokenEstimator::Tiktoken;
        let mut sections = Vec::new();
        let mut output_lines = Vec::new();
        let mut used_tokens = 0usize;
        'sections: for section in &self.sections {
            let mut retained_lines = Vec::new();
            if self.files_with_matches {
                let separator_tokens = usize::from(!output_lines.is_empty());
                let remaining = max_output_tokens.saturating_sub(used_tokens + separator_tokens);
                let (file_path, line_truncated) = estimator.truncate(&section.file_path, remaining);
                if file_path.is_empty() {
                    break;
                }
                let cost = estimator.estimate_tokens(&file_path) + separator_tokens;
                used_tokens += cost;
                output_lines.push(file_path.clone());
                sections.push(SearchResultSection {
                    file_path,
                    file_extension: None,
                    lines: vec![],
                });
                if line_truncated {
                    break;
                }
                continue;
            }
            for mut line in section.lines.iter().cloned() {
                let prefix = if line.is_match {
                    format!("{}:{}: ", section.file_path, line.line_number)
                } else {
                    format!("{}-{}- ", section.file_path, line.line_number)
                };
                let separator_tokens = usize::from(!output_lines.is_empty());
                let remaining = max_output_tokens.saturating_sub(used_tokens + separator_tokens);
                let prefix_tokens = estimator.estimate_tokens(&prefix);
                if prefix_tokens >= remaining {
                    break 'sections;
                }
                let content_budget = remaining.saturating_sub(prefix_tokens);
                let (content, line_truncated) = estimator.truncate(&line.content, content_budget);
                if content.is_empty() {
                    break 'sections;
                }
                line.content = content;
                let rendered = format!("{prefix}{}", line.content);
                let cost = estimator.estimate_tokens(&rendered) + separator_tokens;
                used_tokens += cost;
                output_lines.push(rendered);
                retained_lines.push(line);
                if line_truncated {
                    break 'sections;
                }
            }
            if !retained_lines.is_empty() {
                sections.push(SearchResultSection {
                    file_path: section.file_path.clone(),
                    file_extension: section.file_extension.clone(),
                    lines: retained_lines,
                });
            }
        }
        self.text = output_lines.join("\n");
        self.sections = sections;
        self.truncated = true;
    }
}

impl SearchResult {
    pub(super) fn new(
        sections: Vec<SearchResultSection>,
        truncated: bool,
        files_with_matches: bool,
    ) -> Self {
        let text = if files_with_matches {
            sections
                .iter()
                .map(|section| section.file_path.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            sections
                .iter()
                .flat_map(|section| {
                    section.lines.iter().map(|line| {
                        if line.is_match {
                            format!(
                                "{}:{}: {}",
                                section.file_path, line.line_number, line.content
                            )
                        } else {
                            format!(
                                "{}-{}- {}",
                                section.file_path, line.line_number, line.content
                            )
                        }
                    })
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        SearchResult {
            sections,
            truncated,
            files_with_matches,
            text,
        }
    }
}

impl ToolCallResult for SearchResult {
    fn text(&self) -> String {
        self.text.clone()
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        let matching_lines: usize = self
            .sections
            .iter()
            .flat_map(|s| s.lines.iter())
            .filter(|l| l.is_match)
            .count();
        let metadata = SearchResultMetadata {
            matching_files: self.sections.len(),
            matching_lines: (!self.files_with_matches).then_some(matching_lines),
            truncated: self.truncated.then_some(true),
        };
        serde_json::to_value(metadata).ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schemars::schema_for!(SearchResultMetadata))
    }

    fn content_sections(&self) -> Vec<Section> {
        if self.files_with_matches {
            // Return a single plain-text section listing matched paths.
            if self.sections.is_empty() {
                return vec![];
            }
            let paths = self
                .sections
                .iter()
                .map(|section| section.file_path.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            return vec![Section::Content(ContentSection {
                label: Some("Output".to_string()),
                content: paths,
                file_extension: None,
                line_number_offset: None,
            })];
        }
        self.sections
            .iter()
            .map(|section| Section::SearchResult(section.clone()))
            .collect()
    }
}
