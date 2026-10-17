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

//! Process-level contract of the clap-backed argument parser: `--help`
//! renders to stdout with exit 0, usage errors exit 2 with clap's message on
//! stderr. (Unit tests in `src/cli.rs` cover the parse model itself; these
//! tests pin the exit codes and streams.)

use std::process::Command;

/// `--help` prints usage to stdout and exits 0 without starting a session.
#[test]
fn help_flag_prints_usage_and_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .arg("--help")
        .output()
        .expect("spawn the ylva binary");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "--no-tui",
        "--color",
        "--prompt",
        "--profile",
        "--log-file",
        "--log-level",
        "--log-format",
        "--conversation-id",
        "--auto-approve-read",
        "--auto-approve-web-fetch",
        "--auto-approve-spawn",
        "--max-concurrent-subagents",
        "Auto-approve spawning subagents",
        "Maximum subagents running at once",
        "init",
        "profile",
    ] {
        assert!(
            stdout.contains(expected),
            "help must list {expected}: {stdout}"
        );
    }
    assert!(!stdout.contains("child agent"));
    assert!(
        output.stderr.is_empty(),
        "help must not write to stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `--help` lists every documented exit code with its meaning, built from the
/// same `ExitCode` table the process exits with.
#[test]
fn help_lists_every_exit_code_with_its_meaning() {
    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .arg("--help")
        .output()
        .expect("spawn the ylva binary");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Exit codes:"),
        "help lacks exit codes: {stdout}"
    );
    for code in ylva::ExitCode::ALL {
        let expected = format!("{:<4}{}", u8::from(code), code.description());
        assert!(
            stdout.contains(&expected),
            "help must list `{expected}`: {stdout}"
        );
    }
}

/// An unknown flag is rejected with clap's error on stderr and exit code 2
/// (the historical usage-error code) instead of being silently ignored.
#[test]
fn unknown_flag_is_rejected_with_exit_code_two() {
    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .arg("--definitely-not-a-flag")
        .output()
        .expect("spawn the ylva binary");
    assert_eq!(output.status.code(), Some(2));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--definitely-not-a-flag"),
        "error must name the unknown flag: {stderr}"
    );
}

/// The removed `--no-color` flag keeps its migration hint and usage-error
/// exit code.
#[test]
fn removed_no_color_flag_reports_migration_hint() {
    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .arg("--no-color")
        .output()
        .expect("spawn the ylva binary");
    assert_eq!(output.status.code(), Some(2));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--no-color was replaced by --color=no"),
        "migration hint must survive the clap refactor: {stderr}"
    );
}
