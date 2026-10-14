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

//! `--profile <name>` selects a specific profile explicitly, so a missing
//! `config.toml` for it is a hard error, never a silent fallback to the
//! in-code default profile. This is spawned-binary-level behavior (argument
//! parsing plus `ProfileConfig::load`'s error path wired to `main`'s exit
//! code), so it is exercised here rather than as a `src/config.rs` unit test.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;
use uuid::Uuid;
use ylva::StoreEvent;

/// An explicitly named profile with no config file on disk exits non-zero
/// and names the profile in the error, instead of silently running with the
/// in-code default profile.
#[test]
fn an_explicitly_named_missing_profile_is_a_hard_error() {
    let ylva_home = TempDir::new().expect("tempdir");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--no-tui", "--prompt", "hi", "--profile", "never-written"])
        .output()
        .expect("spawn the ylva binary");

    assert!(
        !output.status.success(),
        "an unknown --profile must not exit zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("never-written"),
        "the error must name the missing profile: {stderr}"
    );
    assert!(
        stderr.contains("not found"),
        "the error must say the profile was not found: {stderr}"
    );
}

/// Profile validation errors identify the selected profile and its config
/// file, rather than printing only the validation detail.
#[test]
fn a_profile_validation_error_names_the_profile_config_file() {
    let ylva_home = TempDir::new().expect("tempdir");
    let profile_name = "broken";
    let profile_directory = ylva_home
        .path()
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join(profile_name);
    fs::create_dir_all(&profile_directory).expect("create profile directory");
    fs::write(
        profile_directory.join("config.toml"),
        r#"
default_engine = "missing"

[engines.available]
provider = "mock"
model = "test-model"
"#,
    )
    .expect("write profile config");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--no-tui", "--prompt", "hi", "--profile", profile_name])
        .output()
        .expect("spawn the ylva binary");

    assert!(
        !output.status.success(),
        "an invalid profile must not start successfully"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(profile_name),
        "the error must name the selected profile: {stderr}"
    );
    assert!(
        stderr.contains(
            profile_directory
                .join("config.toml")
                .to_str()
                .expect("profile path is valid UTF-8")
        ),
        "the error must name the full profile config path: {stderr}"
    );
    assert!(
        stderr.contains("references engine 'missing'"),
        "the error must retain the validation detail: {stderr}"
    );
}

/// Every file-authored agent type must resolve its configured engine before
/// startup reaches the TUI/CLI loop, even when that type is not the root type.
#[test]
fn an_invalid_agent_type_engine_is_a_startup_validation_error() {
    let ylva_home = TempDir::new().expect("tempdir");
    let profile_directory = ylva_home
        .path()
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join("broken");
    let agents_directory = profile_directory.join("agents");
    fs::create_dir_all(&agents_directory).expect("create profile directories");
    fs::write(
        profile_directory.join("config.toml"),
        r#"
default_engine = "available"

[engines.available]
provider = "mock"
model = "test-model"
"#,
    )
    .expect("write profile config");
    fs::write(
        agents_directory.join("researcher.md"),
        r#"---
engine: missing
---

Research the requested topic.
"#,
    )
    .expect("write invalid agent type");
    let log_path = ylva_home.path().join("startup-error.log");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args([
            "--no-tui",
            "--prompt",
            "hi",
            "--profile",
            "broken",
            "--log-file",
            log_path.to_str().expect("log path is valid UTF-8"),
        ])
        .output()
        .expect("spawn the ylva binary");
    assert!(
        !output.status.success(),
        "an invalid agent engine must prevent startup"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("agent type 'researcher' references engine 'missing'"),
        "the startup error must identify the agent type and engine: {stderr}"
    );
    let log = fs::read_to_string(&log_path).expect("read startup error log");
    assert!(
        log.contains("agent type 'researcher' references engine 'missing'"),
        "startup errors shown on stderr must also be written to the log: {log}"
    );
}

/// With no `--profile` flag at all, a fresh `YLVA_HOME` (so no config file
/// exists anywhere) still runs successfully against the in-code default
/// profile — the implicit case stays tolerant.
#[test]
fn no_profile_flag_tolerates_a_missing_default_profile_config() {
    let ylva_home = TempDir::new().expect("tempdir");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--no-tui", "--prompt", "hi"])
        .output()
        .expect("spawn the ylva binary");

    assert!(
        output.status.success(),
        "the implicit default profile must not fail just because no config file exists yet; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn empty_piped_stdin_uses_the_demo_prompt() {
    assert_stdin_uses_demo_prompt(b"");
}

#[test]
fn whitespace_only_piped_stdin_uses_the_demo_prompt() {
    assert_stdin_uses_demo_prompt(b" \t\n\r\n ");
}

fn assert_stdin_uses_demo_prompt(input: &[u8]) {
    let ylva_home = TempDir::new().expect("isolated Ylva home");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--no-tui", "--color=no"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn compiled binary");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("write controlled stdin");
    let output = child.wait_with_output().expect("headless completion");
    assert!(
        output.status.success(),
        "empty stdin must not prevent startup: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let chat_directory = ylva_home.path().join("chats");
    let session_path = fs::read_dir(&chat_directory)
        .expect("chat projects")
        .flat_map(|project| fs::read_dir(project.expect("project entry").path()).expect("sessions"))
        .map(|session| session.expect("session entry").path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .expect("persisted session");
    let messages = ylva::Store::read_messages(&session_path).expect("persisted messages");
    assert!(matches!(
        messages.first(),
        Some(ylva::Message::User { content, kind: ylva::UserMessageKind::Explicit })
            if content == "What makes the sky look blue?"
    ));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("What makes the sky look blue?"),
        "headless output must show the selected fallback prompt"
    );
}

/// A Ylva home whose chats root is not usable exits non-zero instead of
/// starting a transient session without persistence.
#[test]
fn an_unusable_chats_root_is_a_hard_error() {
    let ylva_home = TempDir::new().expect("tempdir");
    fs::write(ylva_home.path().join("chats"), b"not a directory").expect("create chats file");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--no-tui", "--prompt", "hi"])
        .output()
        .expect("spawn the ylva binary");

    assert!(
        !output.status.success(),
        "an unusable chats root must not start a transient session"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("store I/O error"),
        "the error must identify the persistence failure: {stderr}"
    );
}

/// A `--profile` value that fails the name rule is a usage error naming the
/// flag, so path traversal never reaches profile path construction.
#[test]
fn a_profile_name_with_path_separators_is_a_usage_error() {
    let ylva_home = TempDir::new().expect("tempdir");
    for invalid_name in ["../x", "a/b"] {
        let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
            .current_dir(ylva_home.path())
            .args(["--profile", invalid_name, "session", "list"])
            .output()
            .expect("spawn the ylva binary");

        assert_eq!(
            output.status.code(),
            Some(2),
            "--profile {invalid_name} must exit 2"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--profile"),
            "the error must name the flag: {stderr}"
        );
    }
}

/// A valid `--profile` name passes the rule and resolves to its profile.
#[test]
fn a_valid_profile_name_resolves_to_its_profile() {
    let ylva_home = TempDir::new().expect("tempdir");
    let profile_directory = ylva_home
        .path()
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join("work-2");
    fs::create_dir_all(&profile_directory).expect("create profile directory");
    fs::write(profile_directory.join("config.toml"), "").expect("write profile config");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--profile", "work-2", "session", "list"])
        .output()
        .expect("spawn the ylva binary");

    assert!(
        output.status.success(),
        "a valid profile name must resolve: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `profile list` run inside a workspace shows the workspace's and the Ylva home's
/// profiles, and marks a home profile shadowed when the workspace has the same name.
#[test]
fn profile_list_merges_workspace_and_home_profiles_with_shadow_marks() {
    let ylva_home = TempDir::new().expect("tempdir");
    let workspace = TempDir::new().expect("tempdir");
    for profile_directory in [
        ylva_home
            .path()
            .join(ylva::PROFILES_DIRECTORY_NAME)
            .join("shared"),
        ylva_home
            .path()
            .join(ylva::PROFILES_DIRECTORY_NAME)
            .join("home-only"),
        workspace
            .path()
            .join(".ylva")
            .join(ylva::PROFILES_DIRECTORY_NAME)
            .join("shared"),
        workspace
            .path()
            .join(".ylva")
            .join(ylva::PROFILES_DIRECTORY_NAME)
            .join("work"),
    ] {
        fs::create_dir_all(profile_directory).expect("create profile directory");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(workspace.path())
        .args(["profile", "list"])
        .output()
        .expect("spawn the ylva binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 5, "stdout: {stdout}");
    assert!(lines[0].starts_with("shared  ") && !lines[0].contains("shadowed"));
    assert!(lines[1].starts_with("work  ") && !lines[1].contains("shadowed"));
    assert!(
        lines[2].is_empty(),
        "scopes are separated by a blank line: {stdout}"
    );
    assert!(lines[3].starts_with("home-only  ") && !lines[3].contains("shadowed"));
    assert!(lines[4].starts_with("shared  ") && lines[4].ends_with("(shadowed by workspace)"));
}

/// `profile list` writes profile directories as `~`-relative paths when they sit
/// under the home directory, and pads the name column without a header.
#[test]
fn profile_list_abbreviates_the_home_directory_to_tilde() {
    let home = TempDir::new().expect("home tempdir");
    let home_path = fs::canonicalize(home.path()).expect("canonical home");
    let ylva_home = home_path.join(".ylva");
    let workspace_root = home_path.join("workspace");
    fs::create_dir_all(
        ylva_home
            .join(ylva::PROFILES_DIRECTORY_NAME)
            .join("home-only"),
    )
    .expect("create home profile directory");
    fs::create_dir_all(
        workspace_root
            .join(".ylva")
            .join(ylva::PROFILES_DIRECTORY_NAME)
            .join("work"),
    )
    .expect("create workspace profile directory");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &home_path)
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, &ylva_home)
        .current_dir(&workspace_root)
        .args(["profile", "list"])
        .output()
        .expect("spawn the ylva binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout,
        "work       ~/workspace/.ylva/profiles/work  (no config.toml)\n\
         \n\
         home-only  ~/.ylva/profiles/home-only       (no config.toml)\n"
    );
}

/// Write a profile config for the implicit `default` profile.
fn write_default_profile_config(ylva_home: &TempDir, contents: &str) {
    let profile_directory = ylva_home
        .path()
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join(ylva::DEFAULT_PROFILE_NAME);
    fs::create_dir_all(&profile_directory).expect("create profile directory");
    fs::write(profile_directory.join("config.toml"), contents).expect("write profile config");
}

/// Spawn the compiled binary headlessly with `YLVA_HOME` and the working
/// directory both set to `ylva_home`.
fn spawn_ylva(ylva_home: &TempDir, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--no-tui", "--prompt", "hi"])
        .args(args)
        .output()
        .expect("spawn the ylva binary")
}

/// The session id the binary printed on exit.
fn session_id_from_stderr(output: &std::process::Output) -> Uuid {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let session_line = stderr
        .lines()
        .find(|line| line.starts_with("Session: "))
        .unwrap_or_else(|| panic!("stderr must print the session id: {stderr}"));
    session_line
        .trim_start_matches("Session: ")
        .parse::<Uuid>()
        .expect("the printed session id is a uuid")
}

/// Every event of the session persisted under `ylva_home`, in write order.
fn session_events(ylva_home: &TempDir, session_id: Uuid) -> Vec<StoreEvent> {
    let chats_root = ylva_home.path().join("chats");
    let path = ylva::session_path(
        &chats_root,
        &ylva::project_key(ylva_home.path()),
        session_id,
    );
    ylva::Store::read_events(&path).expect("session file is readable")
}

/// `--agent` naming a type the selected profile does not configure exits
/// non-zero before startup, naming the flag's value and the available types.
#[test]
fn an_unknown_agent_type_is_a_startup_error() {
    let ylva_home = TempDir::new().expect("tempdir");

    let output = spawn_ylva(&ylva_home, &["--agent", "nope"]);

    assert!(
        !output.status.success(),
        "an unknown --agent type must not exit zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown agent type 'nope'"),
        "the error must name the requested type: {stderr}"
    );
    assert!(
        stderr.contains("available types: default"),
        "the error must list the configured types: {stderr}"
    );
}

/// A configured agent type is accepted by `--agent`, becomes the run's root
/// type, and lands in the new session's metadata.
#[test]
fn a_configured_agent_type_is_selectable_and_persisted() {
    let ylva_home = TempDir::new().expect("tempdir");
    write_default_profile_config(
        &ylva_home,
        "[agents.researcher]\ndescription = \"Research things.\"\n",
    );
    let log_path = ylva_home.path().join("agent-type.log");

    let output = spawn_ylva(
        &ylva_home,
        &[
            "--agent",
            "researcher",
            "--log-file",
            log_path.to_str().expect("log path is valid UTF-8"),
        ],
    );

    assert!(
        output.status.success(),
        "a configured agent type must start; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = fs::read_to_string(&log_path).expect("read log");
    assert!(
        log.contains("agent_type=researcher"),
        "the resolved root type must be logged: {log}"
    );

    let session_id = session_id_from_stderr(&output);
    let events = session_events(&ylva_home, session_id);
    let StoreEvent::Meta(meta) = &events[0] else {
        panic!("first line must be the meta header, got {:?}", events[0])
    };
    assert_eq!(
        meta.agent_type.as_deref(),
        Some("researcher"),
        "the selected type is persisted in the session metadata"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StoreEvent::AgentTypeChanged { .. })),
        "a fresh session records its type in `meta`, never as a change: {events:?}"
    );
}

/// An explicit `--agent` on a resume replaces the stored type, is appended as
/// an `agent_type_changed` record, and becomes the type later resumes restore
/// without the flag.
#[test]
fn an_explicit_agent_override_is_recorded_on_resume() {
    let ylva_home = TempDir::new().expect("tempdir");
    write_default_profile_config(
        &ylva_home,
        "[agents.researcher]\ndescription = \"Research things.\"\n",
    );

    let first = spawn_ylva(&ylva_home, &["--agent", "researcher"]);
    assert!(
        first.status.success(),
        "the first run must succeed; stderr:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let session_id = session_id_from_stderr(&first);
    assert!(
        !session_events(&ylva_home, session_id)
            .iter()
            .any(|event| matches!(event, StoreEvent::AgentTypeChanged { .. })),
        "the initial selection needs no change record"
    );

    // Resume with an explicit override: the record is appended, the header
    // stays untouched.
    let overriding_log = ylva_home.path().join("override.log");
    let second = spawn_ylva(
        &ylva_home,
        &[
            "--conversation-id",
            session_id.to_string().as_str(),
            "--agent",
            "default",
            "--log-file",
            overriding_log.to_str().expect("log path is valid UTF-8"),
        ],
    );
    assert!(
        second.status.success(),
        "the overriding resume must succeed; stderr:\n{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let events = session_events(&ylva_home, session_id);
    let changes: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            StoreEvent::AgentTypeChanged { agent_type, .. } => Some(agent_type.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        changes,
        vec!["default".to_string()],
        "the explicit override is appended as the latest selection: {events:?}"
    );
    let StoreEvent::Meta(meta) = &events[0] else {
        panic!("first line must be the meta header, got {:?}", events[0])
    };
    assert_eq!(
        meta.agent_type.as_deref(),
        Some("researcher"),
        "the creation header is never rewritten by an override"
    );
    let override_log = fs::read_to_string(&overriding_log).expect("read override log");
    assert!(
        override_log.contains("agent_type=default"),
        "the override is the type this run executed as: {override_log}"
    );

    // Resuming without the flag restores the stored (overridden) type.
    let restored_log = ylva_home.path().join("restored.log");
    let third = spawn_ylva(
        &ylva_home,
        &[
            "--conversation-id",
            session_id.to_string().as_str(),
            "--log-file",
            restored_log.to_str().expect("log path is valid UTF-8"),
        ],
    );
    assert!(
        third.status.success(),
        "the restoring resume must succeed; stderr:\n{}",
        String::from_utf8_lossy(&third.stderr)
    );
    let restored_log = fs::read_to_string(&restored_log).expect("read restored log");
    assert!(
        restored_log.contains("agent_type=default"),
        "the stored override is restored when no flag is given: {restored_log}"
    );
    let events = session_events(&ylva_home, session_id);
    let changes = events
        .iter()
        .filter(|event| matches!(event, StoreEvent::AgentTypeChanged { .. }))
        .count();
    assert_eq!(
        changes, 1,
        "a restore never appends another change record: {events:?}"
    );
}

/// A resume whose stored type no longer exists in the profile is a startup
/// error that points at `--agent`, instead of silently falling back.
#[test]
fn a_stale_stored_agent_type_is_a_startup_error() {
    let ylva_home = TempDir::new().expect("tempdir");
    write_default_profile_config(
        &ylva_home,
        "[agents.researcher]\ndescription = \"Research things.\"\n",
    );

    let first = spawn_ylva(&ylva_home, &["--agent", "researcher"]);
    assert!(
        first.status.success(),
        "the first run must succeed; stderr:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let session_id = session_id_from_stderr(&first);
    let path = ylva::session_path(
        &ylva_home.path().join("chats"),
        &ylva::project_key(ylva_home.path()),
        session_id,
    );
    retire_stored_agent_type(&path, "researcher", "retired");

    let output = spawn_ylva(
        &ylva_home,
        &["--conversation-id", session_id.to_string().as_str()],
    );

    assert!(
        !output.status.success(),
        "a stale stored type must not start"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("'retired'"),
        "the error must name the stale type: {stderr}"
    );
    assert!(
        stderr.contains("--agent"),
        "the error must point at the flag that resolves it: {stderr}"
    );
}

/// Rewrite the stored root agent type of a session file, simulating a type
/// that was removed from the profile after the session was created.
fn retire_stored_agent_type(path: &Path, from: &str, to: &str) {
    let contents = fs::read_to_string(path).expect("read session file");
    let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
    let mut header: serde_json::Value = serde_json::from_str(&lines[0]).expect("meta line is JSON");
    assert_eq!(header["agent_type"], serde_json::json!(from));
    header["agent_type"] = serde_json::json!(to);
    lines[0] = serde_json::to_string(&header).expect("serialize meta line");
    fs::write(path, format!("{}\n", lines.join("\n"))).expect("write session file");
}
