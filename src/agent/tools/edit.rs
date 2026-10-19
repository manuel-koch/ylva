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

//! The `edit` tool: patch an existing file in-place.
//!
//! Two modes are supported:
//! - **SearchReplace**: replace the first occurrence of `old_string` with `new_string`.
//! - **LineRange**: replace the lines from `start_line` to `end_line` (1-based, inclusive)
//!   with `new_string`.
//!
//! The patched content and a unified diff are computed eagerly at call-construction time
//! (before approval). Approval is required via sandbox path check. On approval, the
//! patched content is written to disk.

use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;

use super::validate_args_with_schema;

use super::{Tool, ToolCall, DEFAULT_TOOL_TIMEOUT_SECS};
use crate::sandbox::{ApprovalCheck, SandboxContext};

/// Number of unchanged context lines shown above and below each changed hunk in the diff preview.
const DIFF_CONTEXT_LINES: usize = 3;

/// Selects the editing strategy for the `edit` tool.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditMode {
    /// Replace occurrences of `old_string` with `new_string`.
    SearchReplace,
    /// Replace lines `start_line` through `end_line` (1-based, inclusive) with `new_string`.
    LineRangeReplace,
}

/// `edit` tool arguments. All fields are at the top level; which fields are required
/// depends on `mode` (see each field's description).
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct EditFileArgs {
    /// Absolute or workspace-relative path of the file to edit. The file must already exist.
    pub path: String,
    /// Editing strategy: `search_replace` or `line_range_replace`.
    pub mode: EditMode,
    /// (`search_replace`) The exact text to search for in the file.
    #[serde(default)]
    pub old_string: Option<String>,
    /// Replacement text used by both modes. May be empty to delete the matched text or lines.
    pub new_string: String,
    /// (`search_replace`) When `true`, replace every occurrence; default replaces only the first.
    #[serde(default)]
    pub replace_all: bool,
    /// (`line_range_replace`) First line to replace (1-based).
    #[serde(default)]
    pub start_line: Option<usize>,
    /// (`line_range_replace`) Last line to replace (1-based, inclusive). Must be ≥ `start_line`.
    #[serde(default)]
    pub end_line: Option<usize>,
}

/// The `edit` tool: patch an existing file using search-replace or line-range replacement.
#[derive(Debug)]
pub struct EditTool {
    timeout_secs: u64,
    sandbox_context: SandboxContext,
}

impl EditTool {
    pub const NAME: &'static str = "edit";

    pub fn new(sandbox_context: SandboxContext) -> Self {
        EditTool {
            timeout_secs: 0,
            sandbox_context,
        }
    }

    pub fn with_timeout(timeout_secs: u64, sandbox_context: SandboxContext) -> Self {
        EditTool {
            timeout_secs,
            sandbox_context,
        }
    }
}

impl Default for EditTool {
    fn default() -> Self {
        use crate::sandbox::Sandbox;
        EditTool::new(SandboxContext::new(std::sync::Arc::new(
            std::sync::RwLock::new(Sandbox::new()),
        )))
    }
}

impl Tool for EditTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Edit an existing file (must already exist) by replacing text. `mode: search_replace` — \
        replace `old_string` with `new_string` (first occurrence only, unless `replace_all: true`). \
        `mode: line_range_replace` — replace lines `start_line`..`end_line` (1-based, inclusive) \
        with `new_string`. The `path` argument accepts `~` and `$ENVVAR`/`${ENVVAR}` tokens; \
        they are expanded before use."
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(EditFileArgs)
    }

    fn path_like_arguments(&self) -> &'static [&'static str] {
        &["path"]
    }

    fn validate_args(&self, arguments: &Value) -> Result<(), String> {
        validate_args_with_schema(&self.input_schema(), arguments)?;
        let mode = arguments
            .get("mode")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        match mode {
            "search_replace" => {
                if arguments
                    .get("old_string")
                    .map(|value| value.is_null())
                    .unwrap_or(true)
                {
                    return Err("`old_string` is required when mode is `search_replace`".into());
                }
            }
            "line_range_replace" => {
                if arguments
                    .get("start_line")
                    .map(|value| value.is_null())
                    .unwrap_or(true)
                {
                    return Err("`start_line` is required when mode is `line_range_replace`".into());
                }
                if arguments
                    .get("end_line")
                    .map(|value| value.is_null())
                    .unwrap_or(true)
                {
                    return Err("`end_line` is required when mode is `line_range_replace`".into());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        let path_str = match arguments.get("path").and_then(|v| v.as_str()) {
            Some(value) if !value.is_empty() => value,
            Some(_) => {
                return ApprovalCheck::AutoDenied {
                    reason: "Required argument 'path' must not be empty".to_string(),
                }
            }
            None => {
                return ApprovalCheck::AutoDenied {
                    reason: "Required argument 'path' is missing or not a string".to_string(),
                }
            }
        };
        let sandbox_verdict = self
            .sandbox_context
            .check_path(std::path::Path::new(path_str));
        let approval = match sandbox_verdict {
            None => ApprovalCheck::UserApprovalRequired,
            Some(verdict) => verdict,
        };
        tracing::debug!(
            tool = "edit",
            path = path_str,
            decision = match &approval {
                ApprovalCheck::AutoApproved => "auto-approved",
                ApprovalCheck::AutoDenied { .. } => "auto-denied",
                ApprovalCheck::UserApprovalRequired => "sandbox-abstain→user-approval-required",
            },
            "tool approval decision"
        );
        approval
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_TOOL_TIMEOUT_SECS
        } else {
            self.timeout_secs
        }
    }

    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
        Box::new(create_edit_call(id, arguments, self.timeout_secs()))
    }
}

mod call;
mod patch;
mod result;

use call::create_edit_call;
pub use call::EditCall;
pub use result::{EditApplied, EditResult, EditResultMetadata};

#[cfg(test)]
mod tests;
