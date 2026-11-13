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

//! Compiled-binary listing contracts: trusted loading, environment wiring,
//! connection coverage, presentation and read-only outcomes.

use std::fs;
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use tempfile::TempDir;

mod common;
use common::mock_llm_server::MockLlmServer;

struct Fixture {
    home: TempDir,
    workspace: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            home: TempDir::new().unwrap(),
            workspace: TempDir::new().unwrap(),
        }
    }
    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ylva"));
        command
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("YLVA_HOME", self.home.path())
            .current_dir(self.workspace.path())
            .stdin(Stdio::null())
            .args(arguments);
        command
    }
    fn write_profile(&self, workspace: bool, file: &str, contents: &str) {
        let root = if workspace {
            self.workspace.path().join(".ylva")
        } else {
            self.home.path().to_path_buf()
        };
        let directory = root.join("profiles/default");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(file), contents).unwrap();
    }
    fn assert_read_only(&self) {
        assert!(!self.home.path().join("chats").exists());
        assert!(!self.workspace.path().join(".ylva/chats").exists());
    }
}

fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn connection(name: &str, url: &str) -> String {
    format!("[provider_connections.{name}]\nprovider='ollama'\napi_base_url='{url}'\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_all_connections_once_with_favorites_allowlists_scores_and_blank_separation() {
    let first = MockLlmServer::start_ollama_catalog(
        vec![
            "zulu".into(),
            "vendor/beta".into(),
            "apple".into(),
            "forbidden".into(),
        ],
        Duration::ZERO,
    )
    .await
    .unwrap();
    let second = MockLlmServer::start_ollama_catalog(vec!["second".into()], Duration::ZERO)
        .await
        .unwrap();
    let fixture = Fixture::new();
    let config = format!("{}favorite_models=['zulu', 'vendor/beta', 'zulu']\nallowed_models=['zulu', 'vendor/beta', 'apple']\n{}", connection("alpha", &first.base_url()), connection("beta", &second.base_url()));
    fixture.write_profile(false, "config.toml", &config);
    fs::write(fixture.home.path().join("model-scores.toml"), "beta=8.5").unwrap();
    let mut command = fixture.command(&["--color", "no", "models"]);
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    assert_code(&output, 0);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("\n\nbeta:\n"), "{text}");
    assert!(text.find("⚑ zulu\n").unwrap() < text.find("⚑ vendor/beta\n").unwrap());
    assert!(text.find("⚑ vendor/beta\n").unwrap() < text.find("  apple\n").unwrap());
    assert!(text.contains("score: 8.5"));
    assert!(!text.contains("forbidden"));
    assert!(!text.contains('\x1b'));
    assert_eq!(text.matches("zulu").count(), 1);
    // One /api/tags request per connection; Ollama enriches every catalog
    // row via /api/show before the engine applies the connection allowlist.
    assert_eq!(first.request_headers().len(), 5);
    assert_eq!(second.request_headers().len(), 2);
    assert_eq!(
        fs::read_to_string(fixture.home.path().join("profiles/default/config.toml")).unwrap(),
        config
    );
    fixture.assert_read_only();
}

#[tokio::test(flavor = "multi_thread")]
async fn all_flag_lists_models_the_allowlist_would_hide() {
    let server = MockLlmServer::start_ollama_catalog(
        vec!["allowed".into(), "forbidden".into()],
        Duration::ZERO,
    )
    .await
    .unwrap();
    let fixture = Fixture::new();
    let config = format!(
        "{}allowed_models=['allowed']\n",
        connection("alpha", &server.base_url())
    );
    fixture.write_profile(false, "config.toml", &config);
    for (arguments, lists_forbidden) in [
        (vec!["--color", "no", "models"], false),
        (vec!["--color", "no", "models", "--all"], true),
    ] {
        let mut command = fixture.command(&arguments);
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        assert_code(&output, 0);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("  allowed\n"), "{text}");
        assert_eq!(text.contains("  forbidden\n"), lists_forbidden, "{text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_favorites_and_only_engines_narrow_the_listing() {
    let server =
        MockLlmServer::start_ollama_catalog(vec!["zulu".into(), "apple".into()], Duration::ZERO)
            .await
            .unwrap();
    let fixture = Fixture::new();
    let config = format!(
        "{}favorite_models=['zulu']\n[engines.default]\nprovider_connection='alpha'\nmodel='zulu'\ncontext_length=16000\nreasoning='high'\n[engines.plain]\nprovider_connection='alpha'\nmodel='apple'\n",
        connection("alpha", &server.base_url())
    );
    fixture.write_profile(false, "config.toml", &config);
    let run = |arguments: Vec<&'static str>| {
        let mut command = fixture.command(&arguments);
        async move {
            let output = tokio::task::spawn_blocking(move || command.output().unwrap())
                .await
                .unwrap();
            assert_code(&output, 0);
            String::from_utf8(output.stdout).unwrap()
        }
    };

    let favorites = run(vec!["--color", "no", "models", "--only-favorites"]).await;
    assert!(favorites.contains("⚑ zulu\n"), "{favorites}");
    assert!(!favorites.contains("apple"), "{favorites}");

    let requests_before = server.request_headers().len();
    let engines = run(vec!["--color", "no", "models", "--only-engines"]).await;
    assert_eq!(server.request_headers().len(), requests_before);
    assert!(
        engines.contains(
            "engines:\n  ⚑ default\n      connection: alpha  model: zulu  context: 16K  reasoning: high\n"
        ),
        "{engines}"
    );
    assert!(
        engines.contains("    plain\n      connection: alpha  model: apple\n"),
        "{engines}"
    );

    let both = run(vec![
        "--color",
        "no",
        "models",
        "--only-engines",
        "--only-favorites",
    ])
    .await;
    assert!(both.contains("⚑ default\n"), "{both}");
    assert!(!both.contains("plain"), "{both}");
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_trust_blocks_requests_until_approved_and_again_after_changes() {
    let server = MockLlmServer::start_ollama_catalog(vec!["one".into()], Duration::ZERO)
        .await
        .unwrap();
    let fixture = Fixture::new();
    fixture.write_profile(true, "config.toml", &connection("alpha", "${LISTING_URL}"));
    fixture.write_profile(
        true,
        ".env",
        &format!(
            "LISTING_URL={}\nOLLAMA_API_KEY=secret-credential",
            server.base_url()
        ),
    );
    assert_code(&fixture.command(&["models"]).output().unwrap(), 2);
    assert_eq!(server.cumulative_connections(), 0);
    assert_code(&fixture.command(&["trust"]).output().unwrap(), 0);
    let mut command = fixture.command(&["models"]);
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    assert_code(&output, 0);
    assert!(String::from_utf8_lossy(&output.stdout).contains("  one\n"));
    let before_change = server.cumulative_connections();
    fixture.write_profile(
        true,
        ".env",
        &format!(
            "LISTING_URL={}\nOLLAMA_API_KEY=changed-secret",
            server.base_url()
        ),
    );
    let output = fixture.command(&["models"]).output().unwrap();
    assert_code(&output, 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("changed since it was trusted"));
    assert_eq!(server.cumulative_connections(), before_change);
    fixture.assert_read_only();
}

#[tokio::test(flavor = "multi_thread")]
async fn home_dotenv_interpolation_process_precedence_and_credential_supplement_match_startup() {
    let server = MockLlmServer::start_ollama_catalog(vec!["one".into()], Duration::ZERO)
        .await
        .unwrap();
    let fixture = Fixture::new();
    fixture.write_profile(false, "config.toml", &connection("alpha", "${LISTING_URL}"));
    fixture.write_profile(
        false,
        ".env",
        "LISTING_URL=http://127.0.0.1:1\nOLLAMA_API_KEY=dotenv-secret",
    );
    let mut command = fixture.command(&["models"]);
    command
        .env("LISTING_URL", server.base_url())
        .env("OLLAMA_API_KEY", "process-secret");
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    assert_code(&output, 0);
    assert!(server.request_headers().iter().any(|headers| headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("authorization")
            && value == "Bearer process-secret")));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
    fixture.assert_read_only();
}

#[tokio::test(flavor = "multi_thread")]
async fn discovers_environment_only_provider_without_profile_or_session() {
    let server = MockLlmServer::start_ollama_catalog(vec!["one".into()], Duration::ZERO)
        .await
        .unwrap();
    let fixture = Fixture::new();
    let mut command = fixture.command(&["models"]);
    command.env("OLLAMA_BASE_URL", server.base_url());
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    assert_code(&output, 0);
    assert!(String::from_utf8_lossy(&output.stdout).contains("ollama:\n    one\n"));
    assert!(!fixture.home.path().join("profiles").exists());
    fixture.assert_read_only();
}

#[test]
fn strict_configuration_and_missing_explicit_profile_are_usage_errors() {
    let fixture = Fixture::new();
    fixture.write_profile(
        false,
        "config.toml",
        "unknown_option=true\n[provider_connections.alpha]\nprovider='mock'",
    );
    assert_code(&fixture.command(&["models"]).output().unwrap(), 0);
    assert_code(
        &fixture
            .command(&["--strict-config", "models"])
            .output()
            .unwrap(),
        2,
    );
    fixture.write_profile(
        false,
        "config.toml",
        "[provider_connections.alpha]\nprovider='mock'",
    );
    fs::write(fixture.home.path().join("model-scores.toml"), "broken = [").unwrap();
    assert_code(&fixture.command(&["models"]).output().unwrap(), 0);
    assert_code(
        &fixture
            .command(&["--strict-config", "models"])
            .output()
            .unwrap(),
        2,
    );
    assert_code(
        &fixture
            .command(&["--profile", "missing", "models"])
            .output()
            .unwrap(),
        2,
    );
    fixture.assert_read_only();
}

#[test]
fn total_outage_is_exit_one_partial_outage_and_empty_catalog_are_successful() {
    let fixture = Fixture::new();
    let bad = connection(
        "bad",
        "http://secret:credential@127.0.0.1:1/private?token=hidden",
    );
    fixture.write_profile(false, "config.toml", &bad);
    let output = fixture.command(&["models"]).output().unwrap();
    assert_code(&output, 1);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    for private in ["secret", "credential", "private", "token", "hidden"] {
        assert!(!diagnostics.contains(private), "{diagnostics}");
    }
    fixture.write_profile(
        false,
        "config.toml",
        &format!("{bad}\n[provider_connections.empty]\nprovider='mock'"),
    );
    assert_code(&fixture.command(&["models"]).output().unwrap(), 0);
    fixture.assert_read_only();
}

#[test]
fn models_rejects_engine_and_provider_flags_in_either_position() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["--engine", "main", "models"],
        vec!["models", "--engine", "main"],
        vec!["--provider", "alpha", "models"],
        vec!["models", "--provider", "alpha"],
    ] {
        assert_code(&fixture.command(&arguments).output().unwrap(), 2);
    }
    fixture.assert_read_only();
}

#[tokio::test(flavor = "multi_thread")]
async fn redirected_color_policy_and_catalog_control_characters_are_safe() {
    let server = MockLlmServer::start_ollama_catalog(
        vec!["vendor/model\n\r\x1b[31m\u{202e}".into()],
        Duration::ZERO,
    )
    .await
    .unwrap();
    let fixture = Fixture::new();
    fixture.write_profile(
        false,
        "config.toml",
        &connection("alpha", &server.base_url()),
    );
    for (mode, terminal, no_color, expected_color) in [
        ("auto", "xterm", false, false),
        ("auto", "dumb", true, false),
        ("no", "xterm", false, false),
        ("yes", "dumb", true, true),
    ] {
        let mut command = fixture.command(&["--color", mode, "models"]);
        command.env("TERM", terminal);
        if no_color {
            command.env("NO_COLOR", "1");
        }
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        assert_code(&output, 0);
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.contains('\x1b'), expected_color, "{text}");
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(!text.contains('\r'));
        assert!(
            text.contains("vendor/model\\n\\r\\u{1b}[31m\\u{202e}"),
            "{text}"
        );
        if expected_color {
            assert_eq!(text.matches('\x1b').count(), 2);
        }
    }
    fixture.assert_read_only();
}
