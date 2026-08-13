use std::process::Command;

mod common;

use common::matches_utc_shape;

/// `--version` prints build provenance to stdout and exits 0 immediately,
/// short-circuiting before any provider wiring, Tokio runtime, or agent loop.
#[test]
fn version_flag_prints_build_info_and_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .arg("--version")
        .output()
        .expect("spawn the ylva binary");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("Ylva - Agent-Harness\n"));
    assert!(stdout.contains(&format!("\nVersion: v{}\n", env!("CARGO_PKG_VERSION"))));

    let commit_line = stdout
        .lines()
        .find(|line| line.starts_with("Commit:"))
        .expect("a Commit line");
    let commit = commit_line.trim_start_matches("Commit:").trim();
    let is_sha = commit.len() == 8 && commit.chars().all(|char| char.is_ascii_hexdigit());
    assert!(
        commit == "unknown" || is_sha,
        "commit must be an 8-char hex SHA-1 or `unknown`, got: {commit:?}"
    );

    let built_line = stdout
        .lines()
        .find(|line| line.starts_with("Built:"))
        .expect("a Built line");
    assert!(matches_utc_shape(
        built_line.trim_start_matches("Built:").trim()
    ));

    let profile_line = stdout
        .lines()
        .find(|line| line.starts_with("Profile:"))
        .expect("a Profile line");
    let profile = profile_line.trim_start_matches("Profile:").trim();
    assert!(
        profile == "debug" || profile == "release",
        "profile must be debug or release, got: {profile:?}"
    );

    // The version path must short-circuit before `select_provider`, so no
    // provider wiring is emitted (stderr stays empty on this path).
    assert!(
        output.stderr.is_empty(),
        "version path must not emit provider wiring: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `--version` coexists with other args and always wins, exiting 0 while
/// printing only build info (never the normal prompt/agent flow).
#[test]
fn version_flag_wins_over_prompt_arg() {
    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .args(["--version", "--prompt", "hi"])
        .output()
        .expect("spawn the ylva binary");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("Ylva - Agent-Harness\n"));
    assert!(
        !stdout.contains("User"),
        "version path must not run the agent flow: {stdout}"
    );
    assert!(
        output.stderr.is_empty(),
        "version path must not emit provider wiring: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}
