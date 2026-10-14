use std::fs;
use std::process::Command;

mod common;

use common::matches_utc_shape;
use tempfile::TempDir;

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

    // The version path must short-circuit before `select_provider_from_env`, so no
    // provider wiring is emitted (stderr stays empty on this path).
    assert!(
        output.stderr.is_empty(),
        "version path must not emit provider wiring: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `--version` names the Ylva home and the workspace root this run resolves:
/// `YLVA_HOME` verbatim, and the nearest `.ylva`/`.git` ancestor of the cwd.
#[test]
fn version_flag_reports_the_ylva_home_and_workspace_root() {
    let ylva_home = TempDir::new().expect("isolated Ylva home");
    let workspace_root = TempDir::new().expect("workspace tempdir");
    fs::create_dir(workspace_root.path().join(".ylva")).expect("workspace .ylva");
    let expected_workspace_root =
        fs::canonicalize(workspace_root.path()).expect("canonical workspace");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .arg("--version")
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(workspace_root.path())
        .output()
        .expect("spawn the ylva binary");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected_home_line = format!("\nYlva home: {}\n", ylva_home.path().display());
    assert!(
        stdout.contains(&expected_home_line),
        "expected the resolved Ylva home line, got: {stdout}"
    );
    let expected_workspace_line =
        format!("\nWorkspace root: {}\n", expected_workspace_root.display());
    assert!(
        stdout.contains(&expected_workspace_line),
        "expected the discovered workspace root line, got: {stdout}"
    );
}

/// With `HOME` set, `--version` writes the Ylva home and workspace root as
/// `~`-relative paths when they sit under the home directory.
#[test]
fn version_flag_abbreviates_the_home_directory_to_tilde() {
    let home = TempDir::new().expect("home tempdir");
    let home_path = fs::canonicalize(home.path()).expect("canonical home");
    let ylva_home = home_path.join(".ylva");
    let workspace_root = home_path.join("workspace");
    fs::create_dir_all(workspace_root.join(".ylva")).expect("workspace .ylva");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .arg("--version")
        .env("HOME", &home_path)
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, &ylva_home)
        .current_dir(&workspace_root)
        .output()
        .expect("spawn the ylva binary");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\nYlva home: ~/.ylva\n"),
        "expected the Ylva home under ~, got: {stdout}"
    );
    assert!(
        stdout.contains("\nWorkspace root: ~/workspace\n"),
        "expected the workspace root under ~, got: {stdout}"
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
        stdout.lines().all(|line| !line.starts_with("User")),
        "version path must not run the agent flow: {stdout}"
    );
    assert!(
        output.stderr.is_empty(),
        "version path must not emit provider wiring: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}
