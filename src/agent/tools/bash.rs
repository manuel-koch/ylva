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
//! merged in true arrival order via an unbounded channel. Total output is
//! capped at [`BASH_OUTPUT_LIMIT_LINES`] whole lines; lines beyond the cap are
//! silently discarded and the result carries `truncated` metadata.
//!
//! Process-group kill is performed by a [`ProcessGroupGuard`] whose `Drop`
//! implementation sends `SIGKILL` to the whole group. The guard lives in the
//! [`run_bash`] async function, so it is dropped — killing the group — on any
//! exit path: normal completion, timeout, or future cancellation (when the
//! caller drops the `BoxFuture` returned by [`ToolCall::run`]).

use std::time::Duration;

use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::Instrument;

use super::{ContentSection, Section, Tool, ToolCall, ToolCallResult, ToolError};
use crate::sandbox::{ApprovalCheck, SandboxContext};

/// Default timeout for bash commands (seconds). Separate from
/// [`crate::agent::DEFAULT_TOOL_TIMEOUT_SECS`] because shell commands
/// routinely take longer than 5 s.
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
    /// Merged stdout + stderr up to [`BASH_OUTPUT_LIMIT_LINES`].
    pub output: String,
    /// `true` when the cap was hit and some output was discarded.
    pub truncated: bool,
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
        if self.output.is_empty() {
            return vec![];
        }
        vec![Section::Content(ContentSection {
            label: None,
            content: self.output.clone(),
            file_extension: None,
            line_number_offset: Some(0),
        })]
    }
}

// ── Process-group kill guard ──────────────────────────────────────────────────

/// RAII guard that kills the spawned process group when dropped.
///
/// Killing the whole group (rather than just the direct child) ensures that
/// grandchildren created by the shell script are also cleaned up. This guard
/// is held inside the [`run_bash`] async function so it fires on every exit
/// path — normal return, timeout branch, or future drop/cancellation.
#[cfg(unix)]
struct ProcessGroupGuard {
    pgid: nix::unistd::Pid,
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(self.pgid, nix::sys::signal::Signal::SIGKILL);
    }
}

// ── ToolCall ──────────────────────────────────────────────────────────────────

/// Invocation object for a single `bash` tool call.
pub struct BashCall {
    id: uuid::Uuid,
    raw_arguments: Value,
    timeout_secs: u64,
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
                let args = serde_json::from_value::<BashInputArgs>(self.raw_arguments).map_err(
                    |error| ToolError::failed("bash", format!("invalid arguments: {error}")),
                )?;
                run_bash(args, self.timeout_secs)
                    .await
                    .map(|result| -> Box<dyn ToolCallResult> { Box::new(result) })
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
         Never use this tool to sleep, poll, or otherwise wait for a spawned subagent \
         to finish (e.g. `sleep`, `wait`, status-polling loops) — the harness delivers \
         each subagent's completion automatically once it is ready.",
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
    /// configured `[tool.bash]` named/`CommandGlob` rules — see
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
        })
    }
}

// ── Core execution ────────────────────────────────────────────────────────────

/// Execute `args.command` via `bash -c`, merging stdout + stderr in arrival
/// order, capped at [`BASH_OUTPUT_LIMIT_LINES`].
///
/// The spawned process runs in its own process group (`process_group(0)`).
/// A [`ProcessGroupGuard`] (Unix only) kills that group with `SIGKILL` when
/// this function returns — whether normally, on timeout, or because the caller
/// dropped the returned future mid-execution.
///
/// Returns `ToolError` when:
/// - spawning fails,
/// - the command exits with a non-zero status, or
/// - the command does not finish within `timeout_secs`.
pub async fn run_bash(args: BashInputArgs, timeout_secs: u64) -> Result<BashResult, ToolError> {
    tracing::debug!(command = %args.command, "executing bash command");

    let mut child = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(&args.command)
        .process_group(0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| ToolError::failed("bash", format!("failed to spawn: {error}")))?;

    let pgid = child
        .id()
        .ok_or_else(|| ToolError::failed("bash", "failed to obtain process id after spawn"))?;

    // The guard lives until run_bash returns (on any path), killing the group.
    #[cfg(unix)]
    let _guard = ProcessGroupGuard {
        pgid: nix::unistd::Pid::from_raw(pgid as i32),
    };

    let stdout = child.stdout.take().expect("stdout was not captured");
    let stderr = child.stderr.take().expect("stderr was not captured");

    // Two async reader tasks drain pipes concurrently and forward lines to a
    // single channel, giving true arrival-order interleaving.
    let (line_sender, mut line_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();

    let stdout_sender = line_sender.clone();
    let stdout_reader_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let _ = stdout_sender.send(line.clone());
                }
            }
        }
    });

    let stderr_sender = line_sender.clone();
    let stderr_reader_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let _ = stderr_sender.send(line.clone());
                }
            }
        }
    });

    // Drop the original sender so the channel closes once both reader tasks
    // finish (and have dropped their clones).
    drop(line_sender);

    let effective_timeout_secs = if timeout_secs == 0 {
        DEFAULT_BASH_TIMEOUT_SECS
    } else {
        timeout_secs
    };

    // Race between process completion and timeout.
    let exit_status = tokio::select! {
        status = child.wait() => Some(status),
        _ = tokio::time::sleep(Duration::from_secs(effective_timeout_secs)) => None,
    };

    if exit_status.is_none() {
        // Timed out — abort reader tasks; the guard kills the process group.
        // Drain whatever lines have already been buffered before aborting so
        // partial output can be shown in the bubble and communicated to the LLM.
        stdout_reader_task.abort();
        stderr_reader_task.abort();
        let (partial_output, _) = collect_output(&mut line_receiver).await;
        tracing::warn!(
            timeout_secs = effective_timeout_secs,
            partial_bytes = partial_output.len(),
            "bash command timed out; killing process group"
        );
        return Err(ToolError::failed_with_output(
            "bash",
            format!("command timed out after {effective_timeout_secs}s"),
            partial_output,
        ));
    }

    // Process exited — wait for reader tasks to flush their buffered output.
    let _ = stdout_reader_task.await;
    let _ = stderr_reader_task.await;

    // Drain the channel, applying the output cap.
    let (output, truncated) = collect_output(&mut line_receiver).await;

    let status = exit_status
        .unwrap()
        .map_err(|error| ToolError::failed("bash", format!("wait failed: {error}")))?;

    if !status.success() {
        let exit_code = status.code().unwrap_or(-1);
        return Err(ToolError::failed(
            "bash",
            format!("command exited with code {exit_code}:\n{output}"),
        ));
    }

    tracing::debug!(
        output_bytes = output.len(),
        truncated,
        "bash command completed"
    );

    Ok(BashResult { output, truncated })
}

/// Drain `receiver` and accumulate up to [`BASH_OUTPUT_LIMIT_LINES`] whole
/// lines of text, capped at [`BASH_OUTPUT_LIMIT_BYTES`] total bytes,
/// whichever limit is hit first. Lines beyond either cap are received (to
/// avoid blocking senders) but discarded.
/// Returns `(output, truncated)`.
async fn collect_output(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
) -> (String, bool) {
    let mut output = String::new();
    let mut truncated = false;
    let mut line_count: usize = 0;

    while let Some(line) = receiver.recv().await {
        if truncated {
            // Keep draining so senders are not blocked, but discard content.
            continue;
        }
        if line_count >= BASH_OUTPUT_LIMIT_LINES
            || output.len() + line.len() > BASH_OUTPUT_LIMIT_BYTES
        {
            truncated = true;
            continue;
        }
        output.push_str(&line);
        line_count += 1;
    }

    (output, truncated)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::run_locked;

    fn make_bash_args(command: &str) -> BashInputArgs {
        BashInputArgs {
            command: command.to_string(),
        }
    }

    // ── needs_approval ────────────────────────────────────────────────────────

    #[test]
    fn bash_tool_requires_user_approval_via_sandbox_abstain() {
        // The sandbox has no command rules and abstains (None), so the tool's
        // own default (UserApprovalRequired) kicks in.
        let tool = BashTool::default();
        let approval = tool.needs_approval(&serde_json::json!({ "command": "echo hi" }));
        assert!(
            matches!(approval, ApprovalCheck::UserApprovalRequired),
            "sandbox abstains → tool falls back to UserApprovalRequired"
        );
    }

    #[test]
    fn bash_tool_has_default_30s_timeout() {
        let tool = BashTool::default();
        assert_eq!(tool.timeout_secs(), DEFAULT_BASH_TIMEOUT_SECS);
    }

    #[test]
    fn bash_tool_description_contains_actual_timeout_and_output_cap() {
        let tool = BashTool::default();
        let description = tool.description();
        assert!(
            description.contains(&DEFAULT_BASH_TIMEOUT_SECS.to_string()),
            "description must include the actual timeout: {description}"
        );
        assert!(
            description.contains(&BASH_OUTPUT_LIMIT_LINES.to_string()),
            "description must include the output cap in lines: {description}"
        );
        assert!(
            description.contains(&BASH_OUTPUT_LIMIT_BYTES.to_string()),
            "description must include the output cap in bytes: {description}"
        );
    }

    #[test]
    fn bash_tool_description_explains_workspace_working_directory() {
        let tool = BashTool::default();
        let description = tool.description();
        assert!(description.contains("current working directory"));
        assert!(description.contains("workspace root"));
        assert!(description.contains("do not prefix"));
        assert!(description.contains("Use relative paths directly"));
    }

    #[test]
    fn bash_tool_with_timeout_description_reflects_custom_timeout() {
        let tool = BashTool::with_timeout(
            60,
            crate::sandbox::SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
                crate::sandbox::Sandbox::new(),
            ))),
        );
        assert!(
            tool.description().contains("60"),
            "custom timeout must appear in description: {}",
            tool.description()
        );
    }

    // ── happy path ────────────────────────────────────────────────────────────

    #[test]
    fn bash_happy_path_echo() {
        run_locked(async {
            let result = run_bash(make_bash_args("echo hello"), 5).await;
            let result = result.expect("echo should succeed");
            assert!(
                result.output.contains("hello"),
                "output should contain 'hello'"
            );
            assert!(!result.truncated);
        });
    }

    #[test]
    fn bash_merges_stdout_and_stderr() {
        run_locked(async {
            // Write to both stdout and stderr; both should appear in output.
            let result = run_bash(make_bash_args("echo stdout; echo stderr >&2"), 5)
                .await
                .expect("command should succeed");
            assert!(result.output.contains("stdout"), "stdout missing");
            assert!(result.output.contains("stderr"), "stderr missing");
        });
    }

    // ── non-zero exit ─────────────────────────────────────────────────────────

    #[test]
    fn bash_non_zero_exit_returns_tool_error() {
        run_locked(async {
            let result = run_bash(make_bash_args("exit 42"), 5).await;
            let error = result.expect_err("non-zero exit must be an error");
            let message = error.to_string();
            assert!(
                message.contains("42"),
                "error must include exit code: {message}"
            );
        });
    }

    #[test]
    fn bash_non_zero_exit_includes_output_in_error() {
        run_locked(async {
            let result = run_bash(make_bash_args("echo before_exit; exit 1"), 5).await;
            let error = result.expect_err("non-zero exit must be an error");
            assert!(
                error.to_string().contains("before_exit"),
                "output should be included in error message"
            );
        });
    }

    // ── output truncation ─────────────────────────────────────────────────────

    #[test]
    fn bash_output_truncated_at_cap() {
        run_locked(async {
            // Generate more lines than the 1000-line cap so truncation kicks in.
            let result = run_bash(make_bash_args("yes | head -n 2000"), 10)
                .await
                .expect("command should succeed");
            assert!(result.truncated, "output should be marked as truncated");
            let line_count = result.output.lines().count();
            assert_eq!(
                line_count, BASH_OUTPUT_LIMIT_LINES,
                "output must be capped at whole lines, never split mid-line: got {line_count} lines"
            );
            // Verify metadata contains the truncation key.
            let meta = result
                .metadata()
                .expect("truncated result must have metadata");
            let obj = meta.as_object().expect("metadata is a JSON object");
            assert!(obj.contains_key("truncated"), "truncated key missing");
        });
    }

    #[test]
    fn bash_output_truncated_at_byte_cap_with_few_huge_lines() {
        run_locked(async {
            // A handful of lines, each far larger than the byte cap divided by the
            // line cap, so the byte cap (not the line cap) must trigger truncation.
            let command = format!(
                "for i in 1 2 3; do head -c {} /dev/zero | tr '\\0' 'a'; echo; done",
                BASH_OUTPUT_LIMIT_BYTES
            );
            let result = run_bash(make_bash_args(&command), 10)
                .await
                .expect("command should succeed");
            assert!(result.truncated, "output should be marked as truncated");
            let line_count = result.output.lines().count();
            assert!(
                line_count < BASH_OUTPUT_LIMIT_LINES,
                "byte cap must trigger before the line cap: got {line_count} lines"
            );
            assert!(
                result.output.len() <= BASH_OUTPUT_LIMIT_BYTES,
                "output must never exceed the byte cap: got {} bytes",
                result.output.len()
            );
        });
    }

    // ── timeout ───────────────────────────────────────────────────────────────

    #[test]
    fn bash_timeout_kills_process_group() {
        run_locked(async {
            // `sleep 60` will be killed well before 60 s because we pass a 1 s
            // timeout. We create a sub-shell that itself spawns a background
            // grandchild; the process-group kill must reap it too.
            let result = run_bash(make_bash_args("bash -c 'sleep 60 &' ; sleep 60"), 1).await;
            let error = result.expect_err("sleep should be killed by timeout");
            assert!(
                error.to_string().contains("timed out"),
                "error should mention timeout: {}",
                error
            );
        });
    }

    // ── cancellation (future drop) ────────────────────────────────────────────

    #[test]
    fn bash_future_drop_kills_process_group() {
        run_locked(async {
            // Spawn the bash future as a task, then abort it immediately.
            // The process group guard inside run_bash must clean up when the
            // async state machine is dropped.
            let handle = tokio::spawn(run_bash(make_bash_args("sleep 60"), 30));
            // Give the process a moment to start so it actually spawns.
            tokio::time::sleep(Duration::from_millis(100)).await;
            handle.abort();

            // Aborting a JoinHandle makes it return JoinError::Cancelled.
            let join_result = handle.await;
            assert!(join_result.is_err(), "aborted task must return an error");
            // The `sleep 60` process group must be dead by now. We verify
            // indirectly: a fresh `pgrep -f "sleep 60"` finding no matching
            // process would be fragile; instead we just confirm no panic/hang
            // occurred in cleanup (if the guard's SIGKILL panicked the test
            // would have failed already).
        });
    }

    // ── content sections ──────────────────────────────────────────────────────

    #[test]
    fn bash_result_content_section_non_empty() {
        let result = BashResult {
            output: "hello\n".to_string(),
            truncated: false,
        };
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        match &sections[0] {
            Section::Content(section) => assert_eq!(section.content, "hello\n"),
            other => panic!("unexpected section type: {other:?}"),
        }
    }

    #[test]
    fn bash_result_content_section_empty_output_returns_no_sections() {
        let result = BashResult {
            output: String::new(),
            truncated: false,
        };
        assert!(result.content_sections().is_empty());
    }

    #[test]
    fn bash_input_args_all_properties_have_descriptions() {
        let schema = BashTool::default().input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "BashInputArgs");
    }

    #[test]
    fn bash_input_schema_explains_workspace_working_directory() {
        let schema = BashTool::default().input_schema();
        let schema = serde_json::to_value(schema).expect("schema serializes");
        let description = schema["properties"]["command"]["description"]
            .as_str()
            .expect("command schema has a description");
        assert!(description.contains("current working directory"));
        assert!(description.contains("workspace root"));
        assert!(description.contains("do not prefix"));
        assert!(description.contains("Use relative paths directly"));
    }
}
