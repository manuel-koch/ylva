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

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

mod common;

use common::mock_llm_server::{content_frame, MockLlmConfig, MockLlmServer};

fn run_ylva(ylva_home: &Path, working_directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ylva"));
    command
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home)
        .current_dir(working_directory)
        .args(arguments);
    command.output().expect("spawn the ylva binary")
}

fn run_ylva_with_provider(
    ylva_home: &Path,
    working_directory: &Path,
    provider_base_url: &str,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ylva"));
    command
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home)
        .env("OLLAMA_BASE_URL", provider_base_url)
        .current_dir(working_directory)
        .args(["--no-tui", "--prompt", "hello"]);
    command.output().expect("spawn the ylva binary")
}

#[test]
fn init_seeds_home_files_and_force_controls_overwrites() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let first = run_ylva(ylva_home.path(), ylva_home.path(), &["init"]);
    assert!(
        first.status.success(),
        "init succeeds: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    assert!(ylva_home.path().join("SOUL.md").is_file());
    assert!(ylva_home
        .path()
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join(ylva::DEFAULT_PROFILE_NAME)
        .join(ylva::PROFILE_CONFIG_FILE_NAME)
        .is_file());
    assert!(!ylva_home.path().join("agents").exists());

    let soul_path = ylva_home.path().join("SOUL.md");
    fs::write(&soul_path, "edited persona").expect("edit seeded soul");
    let second = run_ylva(ylva_home.path(), ylva_home.path(), &["init"]);
    assert!(second.status.success());
    assert_eq!(
        fs::read_to_string(&soul_path).expect("read untouched soul"),
        "edited persona"
    );

    let forced = run_ylva(ylva_home.path(), ylva_home.path(), &["init", "--force"]);
    assert!(
        forced.status.success(),
        "forced init succeeds: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert!(fs::read_to_string(soul_path)
        .expect("read forced soul")
        .starts_with("You are Ylva"));
}

#[test]
fn project_init_seeds_only_the_shared_profile_starter() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let workspace_root = TempDir::new().expect("workspace tempdir");
    let output = run_ylva(
        ylva_home.path(),
        workspace_root.path(),
        &["init", "--project"],
    );

    assert!(
        output.status.success(),
        "project init succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(workspace_root
        .path()
        .join(ylva::YLVA_DIR_NAME)
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join(ylva::DEFAULT_PROFILE_NAME)
        .join(ylva::PROFILE_CONFIG_FILE_NAME)
        .is_file());
    assert!(!workspace_root.path().join(".ylva/SOUL.md").exists());
    assert!(!workspace_root.path().join(".ylva/agents").exists());
    assert!(fs::read_to_string(
        workspace_root
            .path()
            .join(ylva::YLVA_DIR_NAME)
            .join(ylva::PROFILES_DIRECTORY_NAME)
            .join(ylva::DEFAULT_PROFILE_NAME)
            .join(ylva::PROFILE_CONFIG_FILE_NAME)
    )
    .expect("read placeholder config")
    .contains("example.invalid"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_persona_uses_project_then_home_then_embedded_precedence() {
    let server = MockLlmServer::start(MockLlmConfig {
        frames: vec![content_frame("answer")],
        end_after_frames: true,
        ..MockLlmConfig::default()
    })
    .await
    .expect("mock server starts");
    let ylva_home = TempDir::new().expect("home tempdir");
    let workspace_root = TempDir::new().expect("workspace tempdir");
    fs::create_dir_all(workspace_root.path().join(".ylva")).expect("workspace config directory");
    fs::write(ylva_home.path().join("SOUL.md"), "home persona").expect("write home persona");
    fs::write(
        workspace_root.path().join(".ylva/SOUL.md"),
        "workspace persona",
    )
    .expect("write workspace persona");

    let first = tokio::task::spawn_blocking({
        let ylva_home = ylva_home.path().to_path_buf();
        let workspace_root = workspace_root.path().to_path_buf();
        let provider_base_url = server.base_url();
        move || run_ylva_with_provider(&ylva_home, &workspace_root, &provider_base_url)
    })
    .await
    .expect("project run completes");
    assert!(first.status.success(), "project run succeeds");
    assert_request_uses_system_prompt(&server, "workspace persona");

    fs::remove_file(workspace_root.path().join(".ylva/SOUL.md")).expect("remove workspace persona");
    let second = tokio::task::spawn_blocking({
        let ylva_home = ylva_home.path().to_path_buf();
        let workspace_root = workspace_root.path().to_path_buf();
        let provider_base_url = server.base_url();
        move || run_ylva_with_provider(&ylva_home, &workspace_root, &provider_base_url)
    })
    .await
    .expect("home run completes");
    assert!(second.status.success(), "home run succeeds");
    assert_request_uses_system_prompt(&server, "home persona");

    fs::remove_file(ylva_home.path().join("SOUL.md")).expect("remove home persona");
    let third = tokio::task::spawn_blocking({
        let ylva_home = ylva_home.path().to_path_buf();
        let workspace_root = workspace_root.path().to_path_buf();
        let provider_base_url = server.base_url();
        move || run_ylva_with_provider(&ylva_home, &workspace_root, &provider_base_url)
    })
    .await
    .expect("built-in run completes");
    assert!(third.status.success(), "built-in run succeeds");
    assert_request_uses_system_prompt(&server, "You are Ylva");
    server.abort();
}

fn assert_request_uses_system_prompt(server: &MockLlmServer, expected: &str) {
    let found = server.request_bodies().into_iter().filter_map(|body| {
        let value = serde_json::from_str::<Value>(&body).ok()?;
        let messages = value.get("messages")?.as_array()?;
        let first = messages.first()?;
        let content = first.get("content")?.as_str()?;
        Some((
            first.get("role")?.as_str()?.to_string(),
            content.to_string(),
        ))
    });
    assert!(
        found
            .into_iter()
            .any(|(role, content)| role == "system" && content.contains(expected)),
        "a chat request must contain system prompt {expected:?}; requests: {:?}",
        server.request_bodies()
    );
}
