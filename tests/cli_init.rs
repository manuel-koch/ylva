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

use tempfile::TempDir;

mod common;

use common::mock_llm_server::{content_frame, MockLlmConfig, MockLlmServer};
use common::{assert_request_uses_system_prompt, run_ylva_with_provider};

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
    let environment_path = ylva_home
        .path()
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join(ylva::DEFAULT_PROFILE_NAME)
        .join(ylva::PROFILE_ENV_FILE_NAME);
    assert!(fs::read_to_string(environment_path)
        .expect("read environment starter")
        .starts_with('#'));
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
fn workspace_init_seeds_the_profile_starters() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let workspace_root = TempDir::new().expect("workspace tempdir");
    let output = run_ylva(
        ylva_home.path(),
        workspace_root.path(),
        &["init", "--workspace"],
    );

    assert!(
        output.status.success(),
        "workspace init succeeds: {}",
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
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("ylva trust"),
        "init --workspace tells the user to trust the seeded configuration"
    );
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
    .contains("example-ollama-local"));
    let gitignore = fs::read_to_string(
        workspace_root
            .path()
            .join(ylva::YLVA_DIR_NAME)
            .join(".gitignore"),
    )
    .expect("read workspace gitignore");
    for entry in [".env", ".envrc", "chats/"] {
        assert!(
            gitignore.lines().any(|line| line == entry),
            "workspace gitignore lists {entry}"
        );
    }
}

#[test]
fn workspace_init_does_not_treat_global_home_as_workspace_root() {
    let home_root = TempDir::new().expect("home root tempdir");
    let ylva_home = home_root.path().join(".ylva");
    let workspace_root = home_root.path().join("project");
    let global_config_path = ylva_home
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join(ylva::DEFAULT_PROFILE_NAME)
        .join(ylva::PROFILE_CONFIG_FILE_NAME);
    fs::create_dir_all(global_config_path.parent().expect("global config parent"))
        .expect("create global config directory");
    fs::write(&global_config_path, "global config").expect("write global config");
    fs::create_dir(&workspace_root).expect("create workspace directory");

    let output = run_ylva(&ylva_home, &workspace_root, &["init", "--workspace"]);

    assert!(
        output.status.success(),
        "workspace init succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let workspace_config_path = workspace_root
        .join(ylva::YLVA_DIR_NAME)
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join(ylva::DEFAULT_PROFILE_NAME)
        .join(ylva::PROFILE_CONFIG_FILE_NAME);
    assert!(
        workspace_config_path.is_file(),
        "workspace config should be written below the current workspace"
    );
    let workspace_environment_path = workspace_root
        .join(ylva::YLVA_DIR_NAME)
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join(ylva::DEFAULT_PROFILE_NAME)
        .join(ylva::PROFILE_ENV_FILE_NAME);
    assert!(fs::read_to_string(workspace_environment_path)
        .expect("read workspace environment starter")
        .starts_with('#'));
    let gitignore = fs::read_to_string(workspace_root.join(ylva::YLVA_DIR_NAME).join(".gitignore"))
        .expect("read workspace gitignore");
    assert!(gitignore.lines().any(|line| line == ".env"));
    assert!(gitignore.lines().any(|line| line == "chats/"));
    assert_eq!(
        fs::read_to_string(global_config_path).expect("read global config"),
        "global config"
    );
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
