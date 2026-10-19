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

//! Eager call construction (file read, patch, diff preview) and execution.

use similar::TextDiff;
use tracing::Instrument;

use super::super::approved_target::ApprovedTarget;
use super::super::{DiffSection, Section, Tool, ToolCall, ToolCallResult, ToolError};
use super::patch::apply_edit;
use super::result::{EditApplied, EditResult};
use super::{EditFileArgs, EditTool, DIFF_CONTEXT_LINES};

/// Invocation object for a single `edit` tool call.
///
/// Built eagerly in `create_call`: for the happy path both `patched_content` and `diff` are
/// pre-computed so the diff preview is available before the user approves. When an execution
/// error occurs during that computation (file not found, `old_string` not found, …) the error
/// is stored in `error` and surfaced by `run()`; `pending_content_sections` returns nothing.
#[derive(Debug)]
pub struct EditCall {
    id: uuid::Uuid,
    raw_arguments: serde_json::Value,
    timeout_secs: u64,
    path: String,
    diff: String,
    patched_content: String,
    outcome: Option<EditApplied>,
    error: Option<ToolError>,
    approved_target: ApprovedTarget,
}

impl EditCall {
    fn ready(
        id: uuid::Uuid,
        raw_arguments: serde_json::Value,
        timeout_secs: u64,
        path: String,
        original_content: String,
        patched_content: String,
        outcome: EditApplied,
    ) -> Self {
        let diff = TextDiff::from_lines(&original_content, &patched_content)
            .unified_diff()
            .context_radius(DIFF_CONTEXT_LINES)
            .header(&format!("a/{path}"), &format!("b/{path}"))
            .to_string();
        let approved_target = ApprovedTarget::capture(&path);
        EditCall {
            id,
            raw_arguments,
            timeout_secs,
            path,
            diff,
            patched_content,
            outcome: Some(outcome),
            error: None,
            approved_target,
        }
    }

    fn failed(
        id: uuid::Uuid,
        raw_arguments: serde_json::Value,
        timeout_secs: u64,
        path: String,
        error: ToolError,
    ) -> Self {
        let approved_target = ApprovedTarget::capture(&path);
        EditCall {
            id,
            raw_arguments,
            timeout_secs,
            path,
            diff: String::new(),
            patched_content: String::new(),
            outcome: None,
            error: Some(error),
            approved_target,
        }
    }
}

impl ToolCall for EditCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }

    fn name(&self) -> &str {
        EditTool::NAME
    }

    fn arguments(&self) -> &serde_json::Value {
        &self.raw_arguments
    }

    fn argument_order(&self, key: &str) -> usize {
        EditTool::default().argument_order(key)
    }

    // `old_string`/`new_string` are already fully represented by the diff
    // section (`pending_content_sections`/`content_sections`), so keep them
    // out of the inline invocation summary to avoid showing the same content
    // twice. When the edit failed before a diff could be computed (e.g.
    // `old_string` not found), no diff section exists, so keep them visible
    // — they're the only place left to see what was searched for.
    fn is_body_argument(&self, key: &str) -> bool {
        self.error.is_none() && matches!(key, "old_string" | "new_string")
    }

    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    fn pending_content_sections(&self) -> Vec<Section> {
        if self.error.is_some() {
            return vec![];
        }
        vec![Section::Diff(DiffSection {
            label: Some(self.path.clone()),
            content: self.diff.clone(),
        })]
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
        self.raw_arguments
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        if let Some(error) = self.error {
            return Box::pin(async move { Err(error) });
        }
        let serialized = serde_json::to_string(&self.raw_arguments).unwrap_or_default();
        let span = tracing::info_span!("tool_args", args = %serialized);
        let path = self.path.clone();
        let diff = self.diff.clone();
        let patched_content = self.patched_content.clone();
        let approved_target = self.approved_target.clone();
        let outcome = self
            .outcome
            .clone()
            .expect("outcome is set when error is None");
        Box::pin(
            async move {
                approved_target
                    .write("edit", &path, patched_content.into_bytes())
                    .await?;
                Ok(Box::new(EditResult {
                    path,
                    outcome,
                    diff,
                }) as Box<dyn ToolCallResult>)
            }
            .instrument(span),
        )
    }
}

/// Build the `EditCall` synchronously: read the file and compute the patch.
///
/// Execution errors (missing file, old_string not found, …) are embedded in the returned
/// `EditCall` and surfaced when `run()` is eventually called.
pub(super) fn create_edit_call(
    id: uuid::Uuid,
    arguments: serde_json::Value,
    timeout_secs: u64,
) -> EditCall {
    let args = match serde_json::from_value::<EditFileArgs>(arguments.clone()) {
        Ok(parsed) => parsed,
        Err(error) => {
            let path = arguments
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            return EditCall::failed(
                id,
                arguments,
                timeout_secs,
                path,
                ToolError::failed("edit", format!("invalid arguments: {error}")),
            );
        }
    };

    let path = args.path.clone();

    if !std::path::Path::new(&path).exists() {
        return EditCall::failed(
            id,
            arguments,
            timeout_secs,
            path.clone(),
            ToolError::failed(
                "edit",
                format!("file '{path}' does not exist; use `write` to create new files"),
            ),
        );
    }

    let original_content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => {
            return EditCall::failed(
                id,
                arguments,
                timeout_secs,
                path.clone(),
                ToolError::failed("edit", format!("failed to read '{path}': {error}")),
            );
        }
    };

    match apply_edit(&original_content, &args) {
        Ok((patched_content, outcome)) => EditCall::ready(
            id,
            arguments,
            timeout_secs,
            path,
            original_content,
            patched_content,
            outcome,
        ),
        Err(error) => EditCall::failed(id, arguments, timeout_secs, path, error),
    }
}
