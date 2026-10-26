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

//! Display sections and result type of a successful `bash` call.

use schemars::JsonSchema;

use super::super::{ContentSection, Section, ToolCallResult};

/// Body sections shared by the pending bubble and the completed result:
/// `Label` first when the model supplied a non-blank purpose string, then
/// `Command` when the command is non-empty. The suppression rules mirror
/// the title-argument display rules (blank label treated as omitted).
pub(super) fn label_and_command_display_sections(
    command: &str,
    label: Option<&str>,
) -> Vec<Section> {
    let mut sections = Vec::new();
    if let Some(label) = label.filter(|label| !label.trim().is_empty()) {
        sections.push(Section::Content(ContentSection {
            label: Some("Label".to_string()),
            content: label.to_string(),
            file_extension: None,
            line_number_offset: None,
        }));
    }
    if !command.is_empty() {
        sections.push(Section::Content(ContentSection {
            label: Some("Command".to_string()),
            content: command.to_string(),
            // Shell grammar for syntect so the expanded bubble renders the
            // command with syntax highlighting (Label/Result stay plain).
            file_extension: Some("sh".to_string()),
            line_number_offset: None,
        }));
    }
    sections
}

// ── Result ────────────────────────────────────────────────────────────────────

/// Structured metadata for a successful `bash` invocation.
#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct BashResultMetadata {
    /// Total bytes in the merged stdout + stderr returned.
    pub output_bytes: usize,
    /// Present and `true` when output exceeded the line cap and was truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

/// Successful output from a `bash` invocation.
#[derive(Debug)]
pub struct BashResult {
    /// Merged stdout + stderr up to [`super::BASH_OUTPUT_LIMIT_LINES`].
    pub output: String,
    /// `true` when the cap was hit and some output was discarded.
    pub truncated: bool,
    /// Purpose label supplied with the call, kept so the completed bubble
    /// can lead with the same `Label` section the pending bubble showed.
    pub(super) label: Option<String>,
    /// The executed command, kept for the completed bubble's `Command`
    /// section (also carried by the call's persisted arguments).
    pub(super) command: String,
}

impl ToolCallResult for BashResult {
    fn text(&self) -> String {
        self.output.clone()
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        let metadata = BashResultMetadata {
            output_bytes: self.output.len(),
            truncated: self.truncated.then_some(true),
        };
        serde_json::to_value(metadata).ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schemars::schema_for!(BashResultMetadata))
    }

    fn content_sections(&self) -> Vec<Section> {
        let mut sections = label_and_command_display_sections(&self.command, self.label.as_deref());
        if !self.output.is_empty() {
            // Truncated capture is an incomplete display of the result —
            // same vocabulary as a failure's partial-output section.
            let label = if self.truncated {
                "Partial output"
            } else {
                "Output"
            };
            sections.push(Section::Content(ContentSection {
                label: Some(label.to_string()),
                content: self.output.clone(),
                file_extension: None,
                line_number_offset: None,
            }));
        }
        sections
    }
}

impl BashResult {
    /// Keep the head of the output within the per-request context allowance and
    /// flag it as truncated.
    pub(super) fn truncate_to_token_budget(&mut self, max_output_tokens: usize) {
        let (output, truncated) =
            crate::llm::TokenEstimator::Tiktoken.truncate(&self.output, max_output_tokens);
        if truncated {
            self.output = output;
            self.truncated = true;
        }
    }
}
