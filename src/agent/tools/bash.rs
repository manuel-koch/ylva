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

//! `bash` tool: shell-command execution with process-group kill on timeout or
//! cancellation.
//!
//! stdout and stderr are read concurrently by two separate async tasks and
//! merged in true arrival order into one shared [`OutputCollector`]. Readers
//! keep draining the pipes after the caps are hit but retain nothing, and a
//! single line is never buffered beyond [`BASH_OUTPUT_LIMIT_BYTES`] + 1 bytes,
//! so memory stays bounded however much a command prints. Total output is
//! capped at [`BASH_OUTPUT_LIMIT_LINES`] whole lines or
//! [`BASH_OUTPUT_LIMIT_BYTES`] bytes, whichever is hit first; excess is
//! silently discarded (a line that alone exceeds the remaining byte budget is
//! kept as a UTF-8-safe prefix up to the cap) and the result carries
//! `truncated` metadata.
//!
//! Process-group kill is performed by a [`ProcessGroupGuard`] whose `Drop`
//! implementation sends `SIGKILL` to the whole group. The guard lives in the
//! [`run_bash`] async function, so it is dropped — killing the group — on any
//! exit path: normal completion, timeout, or future cancellation (when the
//! caller drops the `BoxFuture` returned by [`ToolCall::run`]).

mod execution;
mod output;
mod result;

use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use tracing::Instrument;

use super::{Section, Tool, ToolCall, ToolCallResult, ToolError};
use crate::sandbox::{ApprovalCheck, SandboxContext};
use execution::run_bash;
use result::label_and_command_display_sections;

/// Default timeout for bash commands (seconds). Separate from
/// [`crate::agent::DEFAULT_TOOL_TIMEOUT_SECS`] because shell commands
/// routinely take longer than the file-system-tool default.
pub const DEFAULT_BASH_TIMEOUT_SECS: u64 = 300;

/// Grace period added to [`BashCall::timeout_secs`] so the outer
/// `tokio::time::timeout` in `agent.rs` always fires *after* bash's internal
/// timeout. This guarantees bash can drain its partial output and return a
/// `ToolError::failed_with_output` before the outer safety-net fires.
pub const BASH_OUTER_TIMEOUT_GRACE_SECS: u64 = 5;

/// Maximum number of whole output lines captured from stdout + stderr
/// combined. Lines beyond this limit are silently discarded; the result
/// metadata records the truncation.
pub const BASH_OUTPUT_LIMIT_LINES: usize = 1000;

/// Maximum number of bytes captured from stdout + stderr combined,
/// independent of [`BASH_OUTPUT_LIMIT_LINES`]. Bounds worst-case memory/
/// context usage when output contains very long or unterminated lines
/// (e.g. a single huge JSON/base64 blob) that the line cap alone cannot
/// catch. Whichever cap is hit first stops collection; the result metadata
/// records the truncation.
pub const BASH_OUTPUT_LIMIT_BYTES: usize = 65_536;

// ── Argument schema ───────────────────────────────────────────────────────────

/// Arguments accepted by the `bash` tool.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct BashInputArgs {
    /// Shell command or multi-line script passed verbatim to `bash -c` and
    /// executed from the current working directory, which is the workspace
    /// root. Use relative paths directly; do not prefix the command with a
    /// redundant `cd` to the workspace root. Use `cd` only when intentionally
    /// changing directories. Runs in a new process group; stdout+stderr merged
    /// into one stream.
    pub command: String,
    /// Optional 3–6 word purpose of the shell command, e.g. "List workspace
    /// files". Shown as a dim `label=…` prop on the (possibly collapsed)
    /// tool-call bubble title and the CLI status line while the call is
    /// pending approval, and as the first section of the expanded
    /// tool-call bubble body. Omit it (or pass an empty string) when no
    /// purpose summary is needed.
    #[serde(default)]
    pub label: Option<String>,
}

// ── ToolCall ──────────────────────────────────────────────────────────────────

/// Invocation object for a single `bash` tool call.
pub struct BashCall {
    id: uuid::Uuid,
    raw_arguments: Value,
    timeout_secs: u64,
    runtime_output_token_limit: Option<usize>,
}

impl ToolCall for BashCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }

    fn name(&self) -> &str {
        BashTool::NAME
    }

    fn arguments(&self) -> &Value {
        &self.raw_arguments
    }

    fn argument_order(&self, key: &str) -> usize {
        BashTool::default().argument_order(key)
    }

    /// An absent or `null` label already drops out of
    /// [`ToolCall::title_arguments`]; an empty/whitespace-only one is treated
    /// like the argument being omitted so the title never renders `label=`.
    fn is_default_argument(&self, key: &str, value: &Value) -> bool {
        key == "label" && value.as_str().is_some_and(|label| label.trim().is_empty())
    }

    /// The expanded bubble leads with `Label` (when present) then
    /// `Command`, in every pre-execution and post-failure state. Reads the
    /// raw arguments directly so it works before schema validation.
    fn pending_content_sections(&self) -> Vec<Section> {
        let command = self
            .raw_arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let label = self.raw_arguments.get("label").and_then(Value::as_str);
        label_and_command_display_sections(command, label)
    }

    fn set_runtime_output_token_limit(&mut self, max_output_tokens: Option<usize>) {
        self.runtime_output_token_limit = max_output_tokens;
    }

    fn timeout_secs(&self) -> u64 {
        // Return internal timeout plus a grace period so the outer wrapper
        // timeout in agent.rs always fires after bash's own internal timeout,
        // allowing partial output to be collected and surfaced in the bubble.
        self.timeout_secs
            .saturating_add(BASH_OUTER_TIMEOUT_GRACE_SECS)
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, Value)> {
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
        let serialized = serde_json::to_string(&self.raw_arguments).unwrap_or_default();
        let span = tracing::info_span!("tool_args", args = %serialized);
        Box::pin(
            async move {
                let runtime_output_token_limit = self.runtime_output_token_limit;
                let args = serde_json::from_value::<BashInputArgs>(self.raw_arguments).map_err(
                    |error| ToolError::failed("bash", format!("invalid arguments: {error}")),
                )?;
                run_bash(args, self.timeout_secs).await.map(
                    |mut result| -> Box<dyn ToolCallResult> {
                        if let Some(max_output_tokens) = runtime_output_token_limit {
                            result.truncate_to_token_budget(max_output_tokens);
                        }
                        Box::new(result)
                    },
                )
            }
            .instrument(span),
        )
    }
}

// ── Tool definition ───────────────────────────────────────────────────────────

/// The `bash` tool: execute a shell command and return its merged output.
#[derive(Debug)]
pub struct BashTool {
    timeout_secs: u64,
    /// Carried for uniform construction; bash approval is unconditionally
    /// delegated to the sandbox (which currently abstains, falling back to
    /// `UserApprovalRequired`). Future command-glob rules slot in without
    /// changing this tool.
    sandbox_context: SandboxContext,
    /// LLM-facing description built at construction time from actual field values.
    description: String,
}

/// Build the LLM-facing description string from the concrete timeout and output cap.
fn build_description(timeout_secs: u64) -> String {
    format!(
        "Execute a shell command or multi-line script via `bash -c` from the current \
         working directory, which is the workspace root. Use relative paths directly; \
         do not prefix the command with a redundant `cd` to the workspace root. Use \
         `cd` only when intentionally changing directories. stdout/stderr merged in \
         arrival order. Output capped at {} lines or {} bytes, whichever is hit first \
         (excess discarded). Times out after {} s, killing the whole process group. \
         You may supply an optional short `label` (3–6 words) stating the purpose of \
         the command; it is shown on the tool-call title line while the call awaits \
         approval. Never use this tool to sleep, poll, or otherwise wait for a spawned \
         subagent to finish (e.g. `sleep`, `wait`, status-polling loops) — the harness \
         delivers each subagent's completion automatically once it is ready.",
        BASH_OUTPUT_LIMIT_LINES, BASH_OUTPUT_LIMIT_BYTES, timeout_secs,
    )
}

impl BashTool {
    pub const NAME: &'static str = "bash";

    pub fn new(sandbox_context: SandboxContext) -> Self {
        BashTool {
            timeout_secs: 0,
            sandbox_context,
            description: build_description(DEFAULT_BASH_TIMEOUT_SECS),
        }
    }

    pub fn with_timeout(timeout_secs: u64, sandbox_context: SandboxContext) -> Self {
        let effective_timeout = if timeout_secs == 0 {
            DEFAULT_BASH_TIMEOUT_SECS
        } else {
            timeout_secs
        };
        BashTool {
            timeout_secs,
            sandbox_context,
            description: build_description(effective_timeout),
        }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        use crate::sandbox::Sandbox;
        BashTool::new(SandboxContext::new(std::sync::Arc::new(
            std::sync::RwLock::new(Sandbox::new()),
        )))
    }
}

impl Tool for BashTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> schemars::schema::RootSchema {
        schema_for!(BashInputArgs)
    }

    /// Delegates to the sandbox, which structurally analyses the command
    /// (backgrounding, variable-assignment prefixes, unquoted glob/brace
    /// expansion, unanalysable constructs) and combines that with any
    /// configured `[tools.bash]` named/`CommandGlob` rules — see
    /// `SandboxContext::check_command`. When the sandbox abstains (`None`),
    /// the tool's own conservative default (`UserApprovalRequired`) applies.
    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        let command = arguments
            .get("command")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        match self.sandbox_context.check_command(command) {
            None => ApprovalCheck::UserApprovalRequired,
            Some(verdict) => verdict,
        }
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_BASH_TIMEOUT_SECS
        } else {
            self.timeout_secs
        }
    }

    fn create_call(&self, id: uuid::Uuid, arguments: Value) -> Box<dyn ToolCall> {
        Box::new(BashCall {
            id,
            raw_arguments: arguments,
            timeout_secs: self.timeout_secs(),
            runtime_output_token_limit: None,
        })
    }
}

#[cfg(test)]
mod tests;
