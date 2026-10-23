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

//! One integration test per headless exit code, asserted through the
//! compiled binary's `status.code()`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;
use ylva::{ExitCode, YLVA_HOME_ENVIRONMENT_VARIABLE};

mod common;

use common::mock_llm_server::{content_frame, tool_call_frame, MockLlmConfig, MockLlmServer};

/// A headless `ylva` process whose stdout and stderr go to files, so a test
/// can poll the output for a marker before sending a signal.
struct HeadlessRun {
    child: Child,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    _home: TempDir,
}

impl HeadlessRun {
    fn spawn(provider_base_url: &str, extra_arguments: &[&str]) -> Self {
        let home = TempDir::new().expect("tempdir");
        let stdout_path = home.path().join("stdout.txt");
        let stderr_path = home.path().join("stderr.txt");
        let child = Command::new(env!("CARGO_BIN_EXE_ylva"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env(YLVA_HOME_ENVIRONMENT_VARIABLE, home.path())
            .env("OLLAMA_BASE_URL", provider_base_url)
            .current_dir(home.path())
            .args(["--no-tui", "--prompt", "run it"])
            .args(extra_arguments)
            .stdout(std::fs::File::create(&stdout_path).expect("stdout file"))
            .stderr(std::fs::File::create(&stderr_path).expect("stderr file"))
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn the ylva binary");
        HeadlessRun {
            child,
            stdout_path,
            stderr_path,
            _home: home,
        }
    }

    /// Wait until stdout contains `marker`, proving the run reached that point.
    #[cfg(unix)]
    async fn wait_for_stdout(&self, marker: &str) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if std::fs::read_to_string(&self.stdout_path)
                    .unwrap_or_default()
                    .contains(marker)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("stdout never showed {marker:?}"));
    }

    #[cfg(unix)]
    fn send_signal(&self, signal: nix::sys::signal::Signal) {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        let process_id = Pid::from_raw(self.child.id() as i32);
        kill(process_id, signal).expect("send signal");
    }

    #[cfg(unix)]
    fn send_interrupt(&self) {
        self.send_signal(nix::sys::signal::Signal::SIGINT);
    }

    /// Wait for the process and return its exit code plus captured output.
    async fn finish(mut self) -> (Option<i32>, String, String) {
        let stdout_path = self.stdout_path.clone();
        let stderr_path = self.stderr_path.clone();
        let status = tokio::task::spawn_blocking(move || self.child.wait())
            .await
            .expect("join waiter")
            .expect("wait for ylva");
        (
            status.code(),
            read_file(&stdout_path),
            read_file(&stderr_path),
        )
    }
}

fn read_file(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Start a mock provider that streams `frames` for each connection in order.
async fn mock_provider(
    config: MockLlmConfig,
    connection_frames: Vec<Vec<String>>,
) -> MockLlmServer {
    MockLlmServer::start_with_connection_frames(config, connection_frames)
        .await
        .expect("mock server starts")
}

#[tokio::test(flavor = "multi_thread")]
async fn successful_run_exits_with_success() {
    let server = mock_provider(
        MockLlmConfig {
            frames: vec![content_frame("hello from the model")],
            end_after_frames: true,
            ..MockLlmConfig::default()
        },
        Vec::new(),
    )
    .await;

    let run = HeadlessRun::spawn(&server.base_url(), &[]);
    let (code, stdout, stderr) = run.finish().await;

    assert_eq!(
        code,
        Some(i32::from(u8::from(ExitCode::Success))),
        "{stderr}"
    );
    assert!(stdout.contains("hello from the model"), "{stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_failure_exits_with_general_error() {
    let server = MockLlmServer::start(MockLlmConfig {
        statuses: vec![400],
        fallback_status: 400,
        ..MockLlmConfig::default()
    })
    .await
    .expect("mock server starts");

    let run = HeadlessRun::spawn(&server.base_url(), &[]);
    let (code, stdout, stderr) = run.finish().await;

    assert_eq!(
        code,
        Some(i32::from(u8::from(ExitCode::GeneralError))),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("---[ Error ]---"), "{stdout}");
    assert!(stderr.contains("Resume with:"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_approval_exits_with_approval_denied_and_run_continues() {
    let server = mock_provider(
        MockLlmConfig {
            frames: vec![content_frame("understood")],
            end_after_frames: true,
            ..MockLlmConfig::default()
        },
        vec![
            vec![tool_call_frame(
                0,
                "bash",
                r#"{"command":"touch denied-by-approval.txt"}"#,
            )],
            vec![content_frame("understood")],
        ],
    )
    .await;

    let run = HeadlessRun::spawn(&server.base_url(), &[]);
    let (code, stdout, stderr) = run.finish().await;

    assert_eq!(
        code,
        Some(i32::from(u8::from(ExitCode::ApprovalDenied))),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("denied: tool requires approval"),
        "{stdout}"
    );
    assert!(
        stdout.contains("understood"),
        "the run must continue after a denial: {stdout}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn interrupted_stream_exits_with_interrupted_and_prints_resume_hint() {
    let server = mock_provider(
        MockLlmConfig {
            frames: vec![content_frame("partial answer ")],
            end_after_frames: false,
            ..MockLlmConfig::default()
        },
        Vec::new(),
    )
    .await;

    let run = HeadlessRun::spawn(&server.base_url(), &[]);
    run.wait_for_stdout("partial answer").await;
    run.send_interrupt();
    let (code, stdout, stderr) = run.finish().await;

    assert_eq!(
        code,
        Some(i32::from(u8::from(ExitCode::Interrupted))),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("---[ Interrupted ]---"), "{stdout}");
    assert!(stderr.contains("Resume with:"), "{stderr}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn terminate_signal_exits_with_interrupted_like_sigint() {
    let server = mock_provider(
        MockLlmConfig {
            frames: vec![content_frame("partial answer ")],
            end_after_frames: false,
            ..MockLlmConfig::default()
        },
        Vec::new(),
    )
    .await;

    let run = HeadlessRun::spawn(&server.base_url(), &[]);
    run.wait_for_stdout("partial answer").await;
    run.send_signal(nix::sys::signal::Signal::SIGTERM);
    let (code, stdout, stderr) = run.finish().await;

    assert_eq!(
        code,
        Some(i32::from(u8::from(ExitCode::Interrupted))),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stderr.contains("Resume with:"), "{stderr}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn interrupt_during_a_tool_run_exits_with_interrupted() {
    let server = mock_provider(
        MockLlmConfig {
            frames: vec![content_frame("unused")],
            end_after_frames: true,
            ..MockLlmConfig::default()
        },
        vec![vec![tool_call_frame(
            0,
            "bash",
            r#"{"command":"sleep 30"}"#,
        )]],
    )
    .await;

    let run = HeadlessRun::spawn(&server.base_url(), &["--auto-approve-bash"]);
    run.wait_for_stdout("sleep 30").await;
    run.send_interrupt();
    let (code, stdout, stderr) = run.finish().await;

    assert_eq!(
        code,
        Some(i32::from(u8::from(ExitCode::Interrupted))),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stderr.contains("Resume with:"), "{stderr}");
}

fn run_without_provider(arguments: &[&str]) -> std::process::Output {
    let home = TempDir::new().expect("tempdir");
    Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(YLVA_HOME_ENVIRONMENT_VARIABLE, home.path())
        .current_dir(home.path())
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .expect("run the ylva binary")
}

#[test]
fn conversation_without_provider_is_a_usage_error_with_setup_guidance() {
    let output = run_without_provider(&["--no-tui", "--prompt", "hello"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(i32::from(u8::from(ExitCode::UsageError))),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stderr.contains("ylva connect ollama"), "{stderr}");
    assert!(stderr.contains("ylva connect openrouter"), "{stderr}");
    assert!(!stdout.contains("mock response"), "{stdout}");
}

#[test]
fn session_listing_works_without_provider() {
    let output = run_without_provider(&["session", "list"]);

    assert_eq!(
        output.status.code(),
        Some(i32::from(u8::from(ExitCode::Success))),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
