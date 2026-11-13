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

//! Named engine launch selection: session identity, validation, and repeatability.

use std::fs;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const PROFILE: &str = r#"
default_engine = "main"
utility_engine = "main"
[provider_connections.local]
provider = "mock"
[engines.main]
provider_connection = "local"
model = "configured-model"
context_length = 64000
[engines.fast]
provider_connection = "local"
model = "selected-model"
context_length = 32000
max_output_tokens = 2048
[agents.specialist]
engine = "main"
can_spawn = false
"#;

fn setup() -> TempDir {
    let home = tempfile::Builder::new().prefix("ylva-").tempdir().unwrap();
    let directory = home.path().join("profiles/default");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("config.toml"), PROFILE).unwrap();
    home
}

fn run(home: &TempDir, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("YLVA_HOME", home.path())
        .env("YLVA_NO_VERSION_CHECK", "1")
        .current_dir(home.path())
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn session_id(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|line| line.strip_prefix("Session: "))
        .unwrap()
        .to_string()
}

fn events(home: &TempDir, session: &str) -> Vec<ylva::StoreEvent> {
    let path = ylva::session_path(
        &home.path().join("chats"),
        &ylva::project_key(home.path()),
        session.parse().unwrap(),
    );
    ylva::Store::read_events(&path).unwrap()
}

#[test]
fn engine_selection_uses_complete_config_and_records_it_on_fresh_resume_and_clone() {
    let home = setup();
    let output = run(
        &home,
        &[
            "--no-tui",
            "--prompt",
            "hi",
            "--agent",
            "specialist",
            "--engine",
            "fast",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session = session_id(&output);
    let history = events(&home, &session);
    let json = history
        .iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .collect::<Vec<_>>();
    let initial = json
        .iter()
        .find(|event| event["type"] == "model_changed")
        .unwrap();
    assert_eq!(initial["model"], "selected-model");
    assert_eq!(initial["engine_name"], "fast");
    assert_eq!(initial["context_length"], 32000);
    assert_eq!(initial["max_output_tokens"], 2048);
    let meta = json.iter().find(|event| event["type"] == "meta").unwrap();
    assert_eq!(meta["model"], "selected-model");
    assert_eq!(meta["agent_type"], "specialist");
    assert!(String::from_utf8_lossy(&output.stderr).contains("--engine 'fast'"));
    let output = run(
        &home,
        &[
            "--no-tui",
            "--prompt",
            "again",
            "--conversation-id",
            &session,
            "--engine",
            "fast",
        ],
    );
    assert!(output.status.success());
    let source_path = ylva::session_path(
        &home.path().join("chats"),
        &ylva::project_key(home.path()),
        session.parse().unwrap(),
    );
    let source_before = fs::read(&source_path).unwrap();
    let output = run(
        &home,
        &[
            "--no-tui",
            "--prompt",
            "clone",
            "--conversation-id",
            &session,
            "--clone",
            "--engine",
            "fast",
        ],
    );
    assert!(output.status.success());
    assert_ne!(session_id(&output), session);
    assert_eq!(source_before, fs::read(source_path).unwrap());
    assert_eq!(
        fs::read_to_string(home.path().join("profiles/default/config.toml")).unwrap(),
        PROFILE
    );
    // The override is run-scoped: resuming without it uses the stored agent's configured engine.
    let output = run(
        &home,
        &[
            "--no-tui",
            "--prompt",
            "normal",
            "--conversation-id",
            &session,
        ],
    );
    assert!(output.status.success());
    let history = events(&home, &session);
    let last = history
        .iter()
        .rev()
        .find_map(|event| match event {
            ylva::StoreEvent::ModelChanged { model, .. } => Some(model.as_str()),
            _ => None,
        })
        .unwrap();
    assert_eq!(last, "configured-model");
}

#[test]
fn unknown_or_empty_engine_fails_before_session_creation() {
    for engine in ["missing", "", "   "] {
        let home = setup();
        let output = run(&home, &["--no-tui", "--prompt", "hi", "--engine", engine]);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("--engine"));
        assert!(!home.path().join("chats").exists());
    }
}

#[test]
fn engine_is_rejected_on_every_subcommand_before_side_effects() {
    for command in [
        vec!["init"],
        vec!["session", "list"],
        vec!["profile", "list"],
        vec!["skill", "list"],
        vec!["models"],
        vec!["trust"],
        vec!["connect", "ollama"],
    ] {
        let home = setup();
        let mut arguments = vec!["--engine", "fast"];
        arguments.extend(command);
        let output = run(&home, &arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("--engine"));
        assert!(!home.path().join("chats").exists());
    }
}

#[test]
fn removed_flags_and_repeated_or_misplaced_engine_are_usage_errors() {
    for arguments in [
        vec!["--provider", "local"],
        vec!["--model", "x"],
        vec!["--engine", "fast", "--engine", "main"],
        vec!["--engine"],
        vec!["models", "--engine", "fast"],
    ] {
        let home = setup();
        assert_eq!(
            run(&home, &arguments).status.code(),
            Some(2),
            "{arguments:?}"
        );
    }
    let home = setup();
    assert!(run(&home, &["--engine", "fast", "models", "--version"])
        .status
        .success());
}

mod common;

#[tokio::test(flavor = "multi_thread")]
async fn selected_engine_request_and_history_use_the_same_model() {
    use common::mock_llm_server::{content_frame, MockLlmConfig, MockLlmServer};
    let server = MockLlmServer::start(MockLlmConfig {
        models: vec!["selected-model".to_string()],
        frames: vec![content_frame("done")],
        end_after_frames: true,
        ..MockLlmConfig::default()
    })
    .await
    .unwrap();
    let home = setup();
    let profile = PROFILE.replace(
        "provider = \"mock\"",
        &format!(
            "provider = \"openrouter\"\napi_base_url = \"{}\"\napi_key = \"test-key\"",
            server.base_url()
        ),
    );
    fs::write(home.path().join("profiles/default/config.toml"), profile).unwrap();
    let output = run(&home, &["--no-tui", "--prompt", "hi", "--engine", "fast"]);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies: Vec<serde_json::Value> = server
        .request_bodies()
        .iter()
        .filter_map(|body| serde_json::from_str(body).ok())
        .collect();
    assert!(
        bodies
            .iter()
            .any(|body| body["model"] == "selected-model" && body["max_tokens"] == 2048),
        "{bodies:?}"
    );
    let history = events(&home, &session_id(&output));
    assert!(history.iter().any(|event| matches!(event, ylva::StoreEvent::ModelChanged { model, .. } if model == "selected-model")));
}

#[test]
fn engine_selection_requires_an_explicit_table_but_accepts_defaults() {
    for config in ["", "[provider_connections.local]\nprovider='mock'\n"] {
        let home = setup();
        fs::write(home.path().join("profiles/default/config.toml"), config).unwrap();
        let output = run(
            &home,
            &["--no-tui", "--prompt", "hi", "--engine", "default"],
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(!home.path().join("chats").exists());
    }
    let home = setup();
    fs::write(home.path().join("profiles/default/config.toml"), "default_model='inherited-model'\n[engines.default]\nprovider_connection='mock'\n[engines.fast]\nprovider_connection='mock'\ncontext_length=16000\n").unwrap();
    let output = run(
        &home,
        &[
            "--no-tui",
            "--prompt",
            "hi",
            "--engine",
            "fast",
            "--strict-config",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(events(&home, &session_id(&output))
        .iter()
        .any(|event| matches!(event,
        ylva::StoreEvent::ModelChanged { model, .. } if model == "inherited-model")));
}

#[test]
fn invalid_engine_on_resume_or_clone_does_not_modify_source_or_create_sessions() {
    let home = setup();
    let output = run(&home, &["--no-tui", "--prompt", "hi"]);
    assert!(output.status.success());
    let session = session_id(&output);
    let directory = home
        .path()
        .join("chats")
        .join(ylva::project_key(home.path()));
    let path = ylva::session_path(
        &home.path().join("chats"),
        &ylva::project_key(home.path()),
        session.parse().unwrap(),
    );
    let before = fs::read(&path).unwrap();
    let count = fs::read_dir(&directory).unwrap().count();
    for clone_session in [false, true] {
        let mut arguments = vec![
            "--no-tui",
            "--prompt",
            "hi",
            "--conversation-id",
            &session,
            "--engine",
            "missing",
        ];
        if clone_session {
            arguments.push("--clone");
        }
        assert_eq!(run(&home, &arguments).status.code(), Some(2));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), count);
    }
}
