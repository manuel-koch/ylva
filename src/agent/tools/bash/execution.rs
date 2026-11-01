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

//! Process spawning, timeout handling and process-group cleanup.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::super::ToolError;
use super::output::{spawn_line_reader, take_collected_output, OutputCollector};
use super::result::BashResult;
use super::{BashInputArgs, DEFAULT_BASH_TIMEOUT_SECS};

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

// ── Core execution ────────────────────────────────────────────────────────────

/// Execute `args.command` via `bash -c`, merging stdout + stderr in arrival
/// order, capped at [`super::BASH_OUTPUT_LIMIT_LINES`].
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

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(ToolError::failed("bash", "failed to capture output pipes"));
    };

    // Two async reader tasks drain pipes concurrently into one collector,
    // giving true arrival-order interleaving.
    let collector = Arc::new(Mutex::new(OutputCollector::new()));
    let stdout_reader_task = spawn_line_reader(stdout, Arc::clone(&collector));
    let stderr_reader_task = spawn_line_reader(stderr, Arc::clone(&collector));

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

    let Some(exit_status) = exit_status else {
        // Timed out — abort reader tasks; the guard kills the process group.
        // Keep whatever lines were already collected so partial output can be
        // shown in the bubble and communicated to the LLM.
        stdout_reader_task.abort();
        stderr_reader_task.abort();
        let (output, is_output_truncated) = take_collected_output(&collector);
        tracing::warn!(
            timeout_secs = effective_timeout_secs,
            partial_bytes = output.len(),
            "bash command timed out; killing process group"
        );
        return Err(ToolError::failed_with_output(
            "bash",
            format!("command timed out after {effective_timeout_secs}s"),
            output,
            is_output_truncated,
        ));
    };

    // Process exited — wait for reader tasks to flush their buffered output.
    let _ = stdout_reader_task.await;
    let _ = stderr_reader_task.await;

    let (output, truncated) = take_collected_output(&collector);

    let status =
        exit_status.map_err(|error| ToolError::failed("bash", format!("wait failed: {error}")))?;

    if !status.success() {
        let exit_code = status.code().unwrap_or(-1);
        // The one-line header is the error; the output is shown separately.
        return Err(ToolError::failed_with_output(
            "bash",
            format!("command exited with code {exit_code}"),
            output,
            truncated,
        ));
    }

    tracing::debug!(
        output_bytes = output.len(),
        truncated,
        "bash command completed"
    );

    Ok(BashResult {
        output,
        truncated,
        label: args.label,
        command: args.command,
    })
}
