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

//! End-to-end persistence tests: they spawn the **real compiled binary**
//! (`env!("CARGO_BIN_EXE_ylva")`) rather than driving an `AgentLoop`
//! in-process, and then assert what landed under `~/.ylva/chats` through the
//! public [`ylva::Store`] API.
//!
//! Isolation comes from one `tempfile::TempDir` used as both the subagent
//! process's `YLVA_HOME` (so `<tempdir>/chats` resolves inside it, since
//! `YLVA_HOME` names the Ylva home directly — no further `.ylva`
//! nesting is applied when it is set) and its working directory (so the
//! derived `project_key` is deterministic). The dedicated override is set
//! **only** on the spawned subagent (`Command::env`), never on the test
//! runner's own process, and deliberately in place of the real `HOME`:
//! overriding `HOME` would also mislead every executable the subagent might
//! invoke through a tool call (shell rc files, git and ssh config lookups,
//! credential helpers, …), whereas `YLVA_HOME` redirects Ylva's own state and
//! nothing else.
//!
//! The always-run test talks to an in-process
//! [`common::mock_llm_server::MockLlmServer`] — no live network and no API key.
//! The `#[ignore]`d variants forward whichever `OLLAMA_*`/`OPENROUTER_*`
//! variables the test runner already has (the same names
//! `tests/e2e/{ollama-local,ollama-cloud,openrouter}/.envrc` export) to the
//! same helper, so a developer can run the identical assertions against a real
//! backend with `cargo test --test store_persistence_e2e -- --ignored`.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use uuid::Uuid;
use ylva::{project_key, session_path, SessionSummary, Store, StoreEvent};

mod common;

use common::mock_llm_server::{content_frame, MockLlmConfig, MockLlmServer};

/// The user turn sent to the spawned binary. Asserted verbatim in the
/// persisted transcript; the assistant's wording is never asserted.
const TEST_PROMPT: &str =
    "In one short sentence, explain why the sky appears blue. Do not use tools.";

/// One spawned-binary run plus the temporary `YLVA_HOME` it persisted into.
/// The `TempDir` is kept alive so the session files stay readable for
/// assertions.
struct PersistenceRun {
    ylva_home: TempDir,
    project_key: String,
    output: std::process::Output,
}

impl PersistenceRun {
    fn chats_root(&self) -> std::path::PathBuf {
        self.ylva_home.path().join("chats")
    }

    fn sessions(&self) -> Vec<SessionSummary> {
        Store::list_sessions(&self.chats_root(), &self.project_key)
            .expect("the chats root is listable")
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).to_string()
    }
}

/// Spawn the compiled `ylva` binary headlessly inside `ylva_home`, with that
/// directory as both `YLVA_HOME` and the working directory, and
/// `provider_env` selecting the backend.
///
/// `ylva_home` is passed in and handed back so a caller can run the binary
/// twice against the same persisted state (the `--conversation-id` resume
/// test).
///
/// The subagent's environment is an **allowlist**, not a blocklist: `env_clear`
/// drops everything the developer's shell or CI happens to export, and only
/// `PATH`, `YLVA_HOME`, and the caller's `provider_env` are put back. A
/// blocklist of today's provider variables would silently stop isolating this
/// test the day a provider or variable name is added, which is exactly what
/// the shared assertion helper must not depend on.
fn spawn_ylva(
    ylva_home: TempDir,
    provider_env: &[(String, String)],
    extra_args: &[&str],
) -> PersistenceRun {
    spawn_ylva_with_prompt(ylva_home, provider_env, extra_args, TEST_PROMPT)
}

fn spawn_ylva_with_prompt(
    ylva_home: TempDir,
    provider_env: &[(String, String)],
    extra_args: &[&str],
    prompt: &str,
) -> PersistenceRun {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ylva"));
    command
        .env_clear()
        .current_dir(ylva_home.path())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .args(["--no-tui", "--prompt", prompt])
        .args(extra_args);
    // Only variable the binary needs beyond its own: resolving executables it
    // may spawn. Every other startup path reads either `YLVA_HOME` or a
    // provider variable supplied below.
    if let Ok(search_path) = std::env::var("PATH") {
        command.env("PATH", search_path);
    }
    for (name, value) in provider_env {
        command.env(name, value);
    }
    let output = command.output().expect("spawn the ylva binary");
    PersistenceRun {
        project_key: project_key(ylva_home.path()),
        ylva_home,
        output,
    }
}

/// Run the binary once in a fresh temporary `YLVA_HOME`. Shared by the
/// mock-server test and every live-provider variant, so the assertions below
/// are written exactly once.
fn run_persistence_e2e(provider_env: &[(String, String)]) -> PersistenceRun {
    spawn_ylva(TempDir::new().expect("tempdir"), provider_env, &[])
}

/// Every event of one session file, in written order.
fn read_events(path: &Path) -> Vec<StoreEvent> {
    let contents = std::fs::read_to_string(path).expect("session file readable");
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<StoreEvent>(line).expect("every line is a StoreEvent"))
        .collect()
}

/// Structural assertions shared by the deterministic and the live-provider
/// runs: a `meta` header naming this project, the user's prompt, and a
/// completed assistant message with non-empty text. Never an exact-string
/// match on the assistant's wording.
fn assert_session_holds_one_completed_turn(run: &PersistenceRun) {
    assert!(
        run.output.status.success(),
        "the binary must exit zero; stderr:\n{}",
        run.stderr()
    );
    let sessions = run.sessions();
    assert_eq!(
        sessions.len(),
        1,
        "exactly one session is persisted per run: {sessions:?}"
    );
    let events = read_events(&sessions[0].path);

    let StoreEvent::Meta(meta) = &events[0] else {
        panic!(
            "the first line must be the meta header, got {:?}",
            events[0]
        );
    };
    assert_eq!(meta.project_key, run.project_key);
    assert_eq!(meta.session_id, sessions[0].session_id);
    assert_eq!(
        meta.title, "mock answer",
        "the title comes from the utility-engine response, not the user's prompt"
    );

    assert!(
        events.iter().any(|event| matches!(
            event,
            StoreEvent::Message {
                message: ylva::Message::User { content, .. },
                ..
            } if content.contains(TEST_PROMPT)
        )),
        "the sent prompt is persisted as a user message: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            StoreEvent::Message {
                message: ylva::Message::Assistant { content, status, .. },
                ..
            } if !content.trim().is_empty() && *status == ylva::AssistantStatus::Complete
        )),
        "a completed, non-empty assistant message is persisted: {events:?}"
    );

    // On exit, the binary prints the session id and a copy-pasteable resume
    // command to stderr, so a user can pick the conversation back up.
    let session_id = sessions[0].session_id;
    let stderr = run.stderr();
    assert!(
        stderr.contains(&format!("Session: {session_id}")),
        "stderr must print the session id: {stderr}"
    );
    assert!(
        stderr.contains(&format!("ylva --conversation-id {session_id}")),
        "stderr must print a resume command naming the session id: {stderr}"
    );
}

/// Provider environment pointing the spawned binary at an in-process mock LLM
/// server: `select_provider_from_env`'s Ollama-first precedence picks it up from
/// `OLLAMA_BASE_URL` alone, exactly like `tests/e2e/ollama-local/.envrc` does.
async fn start_mock_llm_server() -> (MockLlmServer, Vec<(String, String)>) {
    let server = MockLlmServer::start(MockLlmConfig {
        // Plain text keeps the deterministic variant's transcript fixed; the
        // assertions stay structural so the live variants can differ.
        frames: vec![content_frame("mock answer")],
        end_after_frames: true,
        ..MockLlmConfig::default()
    })
    .await
    .expect("mock llm server starts");
    let provider_env = vec![("OLLAMA_BASE_URL".to_string(), server.base_url())];
    (server, provider_env)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archived_session_clones_read_only_and_resumes_with_the_original_transcript() {
    let (server, provider_environment) = start_mock_llm_server().await;
    let first = tokio::task::spawn_blocking({
        let provider_environment = provider_environment.clone();
        move || run_persistence_e2e(&provider_environment)
    })
    .await
    .unwrap();
    assert_session_holds_one_completed_turn(&first);
    let original = first.sessions().remove(0);
    let original_events = Store::read_events(&original.path).unwrap();
    let report = Store::maintain_sessions(
        &first.chats_root(),
        &first.project_key,
        &ylva::store::SessionSelection::Ids(vec![original.session_id]),
        ylva::store::MaintenanceAction::Compress,
        false,
    )
    .unwrap();
    assert!(report.failures.is_empty());
    let archive_path = original.path.with_extension("jsonl.gz");
    let archive_bytes = std::fs::read(&archive_path).unwrap();
    let cloned = tokio::task::spawn_blocking({
        let provider_environment = provider_environment.clone();
        move || {
            spawn_ylva(
                first.ylva_home,
                &provider_environment,
                &[
                    "--conversation-id",
                    &original.session_id.to_string(),
                    "--clone",
                ],
            )
        }
    })
    .await
    .unwrap();
    assert!(cloned.output.status.success(), "{}", cloned.stderr());
    assert!(!original.path.exists());
    assert_eq!(std::fs::read(&archive_path).unwrap(), archive_bytes);
    let clone = cloned
        .sessions()
        .into_iter()
        .find(|session| session.session_id != original.session_id)
        .unwrap();
    assert_eq!(
        Store::read_session_meta(&clone.path)
            .unwrap()
            .unwrap()
            .cloned_from,
        Some(original.session_id)
    );
    let resumed = tokio::task::spawn_blocking(move || {
        spawn_ylva(
            cloned.ylva_home,
            &provider_environment,
            &["--conversation-id", &original.session_id.to_string()],
        )
    })
    .await
    .unwrap();
    assert!(resumed.output.status.success(), "{}", resumed.stderr());
    assert!(!archive_path.exists());
    assert!(!original.path.with_extension("meta.json").exists());
    assert!(Store::read_events(&original.path)
        .unwrap()
        .starts_with(&original_events));
    assert!(resumed
        .stderr()
        .contains(&format!("Session: {}", original.session_id)));
    assert!(resumed
        .stderr()
        .contains(&format!("Title:   {}", original.title)));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_headless_run_persists_its_turn_to_a_session_file() {
    let (server, provider_env) = start_mock_llm_server().await;
    let run = tokio::task::spawn_blocking(move || run_persistence_e2e(&provider_env))
        .await
        .expect("the spawned binary run completes");
    assert_session_holds_one_completed_turn(&run);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_id_appends_to_the_existing_session_instead_of_overwriting_it() {
    let (server, provider_env) = start_mock_llm_server().await;
    let first = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || run_persistence_e2e(&provider_env)
    })
    .await
    .expect("the first run completes");
    assert_session_holds_one_completed_turn(&first);

    let session_id = first.sessions()[0].session_id;
    let events_after_first_run = read_events(&first.sessions()[0].path).len();

    let resumed = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || {
            spawn_ylva(
                first.ylva_home,
                &provider_env,
                &["--conversation-id", &session_id.to_string()],
            )
        }
    })
    .await
    .expect("the resumed run completes");
    assert!(
        resumed.output.status.success(),
        "resuming must succeed; stderr:\n{}",
        resumed.stderr()
    );

    let sessions = resumed.sessions();
    assert_eq!(
        sessions.len(),
        1,
        "resuming appends to the same file rather than starting a new session: {sessions:?}"
    );
    let events = read_events(&sessions[0].path);
    assert!(
        events.len() > events_after_first_run,
        "the resumed run appended events ({} then {})",
        events_after_first_run,
        events.len()
    );
    assert!(
        matches!(&events[0], StoreEvent::Meta(meta) if meta.session_id == session_id),
        "the original meta header survived the resume: {:?}",
        events[0]
    );
    let user_turns = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                StoreEvent::Message {
                    message: ylva::Message::User { content, .. },
                    ..
                } if content.contains(TEST_PROMPT)
            )
        })
        .count();
    assert_eq!(
        user_turns, 2,
        "both runs' user turns are present: {events:?}"
    );
    server.abort();
}

/// Give the implicit default profile a second root agent type, so `--agent`
/// has something other than `default` to select.
fn write_agent_type_profile(ylva_home: &TempDir) {
    let profile_directory = ylva_home
        .path()
        .join(ylva::PROFILES_DIRECTORY_NAME)
        .join(ylva::DEFAULT_PROFILE_NAME);
    std::fs::create_dir_all(&profile_directory).expect("create profile directory");
    std::fs::write(
        profile_directory.join("config.toml"),
        "[agents.researcher]\ndescription = \"Research things.\"\n",
    )
    .expect("write profile config");
}

/// The log line the binary writes when it resolves the root agent type, e.g.
/// `agent_type=researcher explicit=true resumed=false`.
fn logged_agent_type(log_path: &Path) -> Option<String> {
    let log = std::fs::read_to_string(log_path).expect("read log");
    log.lines()
        .find(|line| line.contains("root agent type resolved"))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|token| token.strip_prefix("agent_type="))
                .map(str::to_string)
        })
}

/// The `agent_type_changed` records of a session, in write order.
fn agent_type_changes(events: &[StoreEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            StoreEvent::AgentTypeChanged { agent_type, .. } => Some(agent_type.clone()),
            _ => None,
        })
        .collect()
}

/// Remove the `agent_type` key from a session's `meta` header, producing the
/// header shape written before the field existed.
fn strip_agent_type_from_header(path: &Path) {
    let contents = std::fs::read_to_string(path).expect("read session file");
    let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
    let mut header: serde_json::Value = serde_json::from_str(&lines[0]).expect("meta line is JSON");
    assert!(
        header.get("agent_type").is_some(),
        "the header must carry one"
    );
    header
        .as_object_mut()
        .expect("meta is a JSON object")
        .remove("agent_type");
    lines[0] = serde_json::to_string(&header).expect("serialize meta line");
    std::fs::write(path, format!("{}\n", lines.join("\n"))).expect("write session file");
}

/// The selected root agent type lands in the session metadata, is restored
/// when a resume omits `--agent`, and an explicit override is appended as the
/// latest selection without rewriting the creation header.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_agent_type_is_persisted_restored_and_overridable() {
    let (server, provider_env) = start_mock_llm_server().await;

    let ylva_home = TempDir::new().expect("tempdir");
    write_agent_type_profile(&ylva_home);
    let first_log_path = ylva_home.path().join("first.log");
    let first_log = first_log_path
        .to_str()
        .expect("log path is UTF-8")
        .to_string();

    let first = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || {
            spawn_ylva(
                ylva_home,
                &provider_env,
                &["--agent", "researcher", "--log-file", &first_log],
            )
        }
    })
    .await
    .expect("the first run completes");
    assert!(
        first.output.status.success(),
        "the first run must succeed; stderr:\n{}",
        first.stderr()
    );
    let session_id = first.sessions()[0].session_id;
    let events = read_events(&first.sessions()[0].path);
    let StoreEvent::Meta(meta) = &events[0] else {
        panic!("first line must be the meta header, got {:?}", events[0])
    };
    assert_eq!(
        meta.agent_type.as_deref(),
        Some("researcher"),
        "the selected type is persisted in the session metadata: {events:?}"
    );
    assert_eq!(
        agent_type_changes(&events),
        Vec::<String>::new(),
        "a fresh session records its type only in `meta`"
    );
    assert_eq!(
        logged_agent_type(&first_log_path).as_deref(),
        Some("researcher"),
        "the run's own log names the resolved type"
    );

    // Resume without the flag: the stored type is restored.
    let restore_log_path = first.ylva_home.path().join("restore.log");
    let restore_log = restore_log_path
        .to_str()
        .expect("log path is UTF-8")
        .to_string();
    let restored = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || {
            spawn_ylva(
                first.ylva_home,
                &provider_env,
                &[
                    "--conversation-id",
                    session_id.to_string().as_str(),
                    "--log-file",
                    &restore_log,
                ],
            )
        }
    })
    .await
    .expect("the resumed run completes");
    assert!(
        restored.output.status.success(),
        "the resuming run must succeed; stderr:\n{}",
        restored.stderr()
    );
    assert_eq!(
        logged_agent_type(&restore_log_path).as_deref(),
        Some("researcher"),
        "a resume without --agent restores the stored type"
    );
    let events = read_events(&restored.sessions()[0].path);
    assert_eq!(
        agent_type_changes(&events),
        Vec::<String>::new(),
        "restoring never appends a change record: {events:?}"
    );

    // Resume with an explicit override: appended as the latest selection.
    let override_log_path = restored.ylva_home.path().join("override.log");
    let override_log = override_log_path
        .to_str()
        .expect("log path is UTF-8")
        .to_string();
    let overridden = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || {
            spawn_ylva(
                restored.ylva_home,
                &provider_env,
                &[
                    "--conversation-id",
                    session_id.to_string().as_str(),
                    "--agent",
                    "default",
                    "--log-file",
                    &override_log,
                ],
            )
        }
    })
    .await
    .expect("the overriding run completes");
    assert!(
        overridden.output.status.success(),
        "the overriding run must succeed; stderr:\n{}",
        overridden.stderr()
    );
    assert_eq!(
        logged_agent_type(&override_log_path).as_deref(),
        Some("default"),
        "the override is the type this run executed as"
    );
    let events = read_events(&overridden.sessions()[0].path);
    assert_eq!(
        agent_type_changes(&events),
        vec!["default".to_string()],
        "the override is persisted as the latest selection: {events:?}"
    );
    let StoreEvent::Meta(meta) = &events[0] else {
        panic!("first line must be the meta header, got {:?}", events[0])
    };
    assert_eq!(
        meta.agent_type.as_deref(),
        Some("researcher"),
        "the creation header keeps the type resolved when the session started"
    );
    server.abort();
}

/// A session file written before `agent_type` existed resumes successfully
/// as the `default` type instead of failing to parse or falling back
/// silently to some other type.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_legacy_session_without_agent_type_metadata_defaults_on_resume() {
    let (server, provider_env) = start_mock_llm_server().await;

    let ylva_home = TempDir::new().expect("tempdir");
    write_agent_type_profile(&ylva_home);
    let first = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || spawn_ylva(ylva_home, &provider_env, &[])
    })
    .await
    .expect("the first run completes");
    assert!(
        first.output.status.success(),
        "the first run must succeed; stderr:\n{}",
        first.stderr()
    );
    let session_id = first.sessions()[0].session_id;
    let session_path = first.sessions()[0].path.clone();

    strip_agent_type_from_header(&session_path);
    assert_eq!(
        Store::read_latest_agent_type(&session_path).expect("legacy file is readable"),
        None,
        "a header without the field reads back as unset"
    );

    let log_path = first.ylva_home.path().join("legacy.log");
    let log = log_path.to_str().expect("log path is UTF-8").to_string();
    let resumed = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || {
            spawn_ylva(
                first.ylva_home,
                &provider_env,
                &[
                    "--conversation-id",
                    session_id.to_string().as_str(),
                    "--log-file",
                    &log,
                ],
            )
        }
    })
    .await
    .expect("the resumed run completes");
    assert!(
        resumed.output.status.success(),
        "a legacy session must resume successfully; stderr:\n{}",
        resumed.stderr()
    );
    assert_eq!(
        logged_agent_type(&log_path).as_deref(),
        Some("default"),
        "a session without a stored type resumes as `default`"
    );
    let events = read_events(&session_path);
    assert_eq!(
        agent_type_changes(&events),
        Vec::<String>::new(),
        "defaulting a legacy session must not append a change record: {events:?}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_headless_request_replays_the_prior_transcript_once() {
    let server = MockLlmServer::start_with_connection_frames(
        MockLlmConfig {
            frames: vec![content_frame("fallback answer")],
            end_after_frames: true,
            ..MockLlmConfig::default()
        },
        vec![
            vec![common::mock_llm_server::tool_call_frame(
                0,
                "glob",
                r#"{"pattern":"Cargo.toml","only_files":true}"#,
            )],
            vec![content_frame("first answer")],
            vec![content_frame("second answer")],
        ],
    )
    .await
    .expect("mock server starts");
    let provider_env = vec![("OLLAMA_BASE_URL".to_string(), server.base_url())];
    let first = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || {
            spawn_ylva_with_prompt(
                TempDir::new().expect("tempdir"),
                &provider_env,
                &[],
                "first prompt",
            )
        }
    })
    .await
    .expect("the first run completes");
    assert!(
        first.output.status.success(),
        "the first run succeeds; stderr:\n{}",
        first.stderr()
    );

    let session_id = first.sessions()[0].session_id;
    let first_events = read_events(&first.sessions()[0].path);
    assert!(first_events.iter().any(|event| {
        matches!(
            event,
            StoreEvent::Message {
                message: ylva::Message::ToolResult {
                    value: ylva::ToolResult { name, .. },
                },
                ..
            } if name == "glob"
        )
    }));

    let resumed = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || {
            spawn_ylva_with_prompt(
                first.ylva_home,
                &provider_env,
                &["--conversation-id", &session_id.to_string()],
                "second prompt",
            )
        }
    })
    .await
    .expect("the resumed run completes");
    assert!(
        resumed.output.status.success(),
        "the resumed run succeeds; stderr:\n{}",
        resumed.stderr()
    );

    let request_bodies = server.request_bodies();
    let second_request = request_bodies
        .iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .filter(|body| body["messages"].is_array())
        .find(|body| {
            body["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["role"] == "user" && message["content"] == "second prompt"
                })
            })
        })
        .expect("the resumed chat request was captured");
    let messages = second_request["messages"]
        .as_array()
        .expect("chat request contains messages");
    assert!(messages
        .iter()
        .any(|message| { message["role"] == "user" && message["content"] == "first prompt" }));
    assert!(messages
        .iter()
        .any(|message| { message["role"] == "assistant" && message["content"] == "first answer" }));
    assert!(messages.iter().any(|message| {
        message["role"] == "tool"
            && message["tool_call_id"].is_string()
            && message["content"].as_str().is_some_and(|content| {
                serde_json::from_str::<serde_json::Value>(content)
                    .ok()
                    .is_some_and(|envelope| envelope["ok"] == true)
            })
    }));
    assert!(messages
        .iter()
        .any(|message| { message["role"] == "user" && message["content"] == "second prompt" }));

    let resumed_events = read_events(&resumed.sessions()[0].path);
    let first_prompt_count = resumed_events
        .iter()
        .filter(|event| {
            matches!(
                event,
                StoreEvent::Message {
                    message: ylva::Message::User { content, .. },
                    ..
                } if content == "first prompt"
            )
        })
        .count();
    let first_answer_count = resumed_events
        .iter()
        .filter(|event| {
            matches!(
                event,
                StoreEvent::Message {
                    message: ylva::Message::Assistant { content, .. },
                    ..
                } if content == "first answer"
            )
        })
        .count();
    assert_eq!(first_prompt_count, 1);
    assert_eq!(first_answer_count, 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compiled_binary_persists_and_resumes_subagent_run_transcripts() {
    let server = MockLlmServer::start_with_connection_frame_rules(
        MockLlmConfig {
            frames: vec![content_frame("fallback answer")],
            end_after_frames: true,
            ..MockLlmConfig::default()
        },
        vec![
            vec![common::mock_llm_server::tool_call_frame(
                0,
                "spawn_subagent",
                r#"{"label":"Inspect repository","task":"Inspect the repository and report a subagent result."}"#,
            )],
            vec![content_frame("parent answer")],
            vec![content_frame("resumed parent answer")],
        ],
        vec![(
            "agent_finish".to_string(),
            vec![common::mock_llm_server::tool_call_frame(
                0,
                "agent_finish",
                r#"{"status":"success","result":"subagent completed","sources":["src/lib.rs"]}"#,
            )],
        )],
    )
    .await
    .expect("mock server starts");
    let provider_env = vec![("OLLAMA_BASE_URL".to_string(), server.base_url())];
    let first = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || {
            spawn_ylva_with_prompt(
                TempDir::new().expect("tempdir"),
                &provider_env,
                &["--auto-approve-spawn"],
                "Delegate a repository inspection to a subagent.",
            )
        }
    })
    .await
    .expect("the subagent run completes");
    assert!(
        first.output.status.success(),
        "the subagent run succeeds; stderr:\n{}",
        first.stderr()
    );

    let root_session = first
        .sessions()
        .into_iter()
        .find(|session| session.parent_session.is_none())
        .expect("the parent session is listed");
    let root_events = read_events(&root_session.path);
    let subagent_run_id = root_events
        .iter()
        .find_map(|event| match event {
            StoreEvent::SubagentStart { id, .. } => Some(*id),
            _ => None,
        })
        .expect("the parent records the subagent start");
    let subagent_runs =
        Store::read_subagent_runs(&root_session.path).expect("subagent runs are readable");
    let subagent_run = subagent_runs
        .iter()
        .find(|run| run.run_id == subagent_run_id)
        .expect("the subagent lifecycle is paired");
    assert_eq!(subagent_run.status.as_deref(), Some("success"));
    assert_eq!(subagent_run.result.as_deref(), Some("subagent completed"));
    assert!(subagent_run.duration.is_some());

    let subagent_transcript_path =
        session_path(&first.chats_root(), &first.project_key, subagent_run_id);
    assert!(
        subagent_transcript_path.is_file(),
        "the subagent transcript uses the stable run-id path"
    );
    let subagent_messages =
        Store::read_messages(&subagent_transcript_path).expect("subagent transcript is readable");
    assert!(subagent_messages.iter().any(|message| {
        matches!(
            message,
            ylva::Message::User { content, .. }
                if content == "Inspect the repository and report a subagent result."
        )
    }));
    assert!(subagent_messages.iter().any(|message| {
        matches!(
            message,
            ylva::Message::ToolResult {
                value: ylva::ToolResult { name, result, .. },
            } if name == "agent_finish" && result.contains("Reported success")
        )
    }));

    let resumed = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        let session_id = root_session.session_id;
        move || {
            spawn_ylva_with_prompt(
                first.ylva_home,
                &provider_env,
                &[
                    "--auto-approve-spawn",
                    "--conversation-id",
                    &session_id.to_string(),
                ],
                "Resume the parent conversation.",
            )
        }
    })
    .await
    .expect("the resumed parent run completes");
    assert!(
        resumed.output.status.success(),
        "the resumed parent run succeeds; stderr:\n{}",
        resumed.stderr()
    );
    let resumed_root = resumed
        .sessions()
        .into_iter()
        .find(|session| session.session_id == root_session.session_id)
        .expect("the resumed parent session remains addressable");
    let resumed_subagent_run = Store::read_subagent_runs(&resumed_root.path)
        .expect("resumed subagent lifecycle is readable")
        .into_iter()
        .find(|run| run.run_id == subagent_run_id)
        .expect("the resumed parent still exposes the subagent bubble metadata");
    assert_eq!(resumed_subagent_run.status.as_deref(), Some("success"));
    assert_eq!(
        resumed_subagent_run.result.as_deref(),
        Some("subagent completed")
    );
    assert!(
        session_path(&resumed.chats_root(), &resumed.project_key, subagent_run_id).is_file(),
        "the resumed parent can still locate the subagent drill-in transcript"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_conversation_id_fails_instead_of_starting_a_new_session() {
    let (server, provider_env) = start_mock_llm_server().await;
    let unknown_session_id = Uuid::new_v4();
    let run = tokio::task::spawn_blocking(move || {
        spawn_ylva(
            TempDir::new().expect("tempdir"),
            &provider_env,
            &["--conversation-id", &unknown_session_id.to_string()],
        )
    })
    .await
    .expect("the run completes");

    assert!(
        !run.output.status.success(),
        "an unknown session id must be a hard error, not a silent fresh session"
    );
    let stderr = run.stderr();
    assert!(
        stderr.contains(&unknown_session_id.to_string()) && stderr.contains("not found"),
        "the error names the missing session: {stderr}"
    );
    assert!(
        run.sessions().is_empty(),
        "no session file is created when the requested one does not exist"
    );
    server.abort();
}

#[tokio::test]
async fn clone_flag_copies_a_session_into_a_new_one_and_leaves_the_source_untouched() {
    let (server, provider_env) = start_mock_llm_server().await;
    let first = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || run_persistence_e2e(&provider_env)
    })
    .await
    .expect("the first run completes");
    assert_session_holds_one_completed_turn(&first);

    let source_summary = first
        .sessions()
        .into_iter()
        .next()
        .expect("the first run persisted a session");
    let source_id = source_summary.session_id;
    let source_path = source_summary.path.clone();
    let source_bytes = std::fs::read(&source_path).expect("source readable");

    let cloned = tokio::task::spawn_blocking({
        let provider_env = provider_env.clone();
        move || {
            spawn_ylva_with_prompt(
                first.ylva_home,
                &provider_env,
                &["--conversation-id", &source_id.to_string(), "--clone"],
                "a fresh turn for the clone",
            )
        }
    })
    .await
    .expect("the clone run completes");
    assert!(
        cloned.output.status.success(),
        "cloning must succeed; stderr:\n{}",
        cloned.stderr()
    );

    assert_eq!(
        std::fs::read(&source_path).expect("source readable"),
        source_bytes,
        "the source file is byte-for-byte unchanged by the clone run"
    );

    let sessions = cloned.sessions();
    assert_eq!(
        sessions.len(),
        2,
        "the clone is a second session next to the source: {sessions:?}"
    );
    assert!(
        sessions
            .iter()
            .any(|session| session.session_id == source_id),
        "the source session still exists: {sessions:?}"
    );
    let clone_summary = sessions
        .iter()
        .find(|session| session.session_id != source_id)
        .expect("a distinct clone session");
    assert_ne!(clone_summary.session_id, source_id);

    let clone_events = read_events(&clone_summary.path);
    let StoreEvent::Meta(clone_meta) = &clone_events[0] else {
        panic!(
            "the clone starts with its own meta header: {:?}",
            clone_events[0]
        );
    };
    assert_eq!(clone_meta.session_id, clone_summary.session_id);
    assert_eq!(
        clone_meta.cloned_from,
        Some(source_id),
        "the clone names its source: {clone_meta:?}"
    );
    assert_eq!(
        clone_meta.parent_session, None,
        "a clone is a top-level session: {clone_meta:?}"
    );
    assert!(
        clone_events.iter().any(|event| matches!(
            event,
            StoreEvent::Message {
                message: ylva::Message::User { content, .. },
                ..
            } if content.contains(TEST_PROMPT)
        )),
        "the clone replays the source's turns: {clone_events:?}"
    );
    assert!(
        clone_events.iter().any(|event| matches!(
            event,
            StoreEvent::Message {
                message: ylva::Message::User { content, .. },
                ..
            } if content.contains("a fresh turn for the clone")
        )),
        "the new turn is persisted on the clone: {clone_events:?}"
    );

    let source_events = read_events(&source_path);
    assert!(
        !source_events.iter().any(|event| matches!(
            event,
            StoreEvent::Message {
                message: ylva::Message::User { content, .. },
                ..
            } if content.contains("a fresh turn for the clone")
        )),
        "the source transcript gains nothing from the clone run: {source_events:?}"
    );
    server.abort();
}

/// Forward the named variables from the test runner's own environment, or
/// return `None` when a required variable is unset.
fn provider_env_from_runner(required: &[&str], optional: &[&str]) -> Option<Vec<(String, String)>> {
    let mut provider_env = Vec::with_capacity(required.len() + optional.len());
    for name in required {
        let value = std::env::var(name).ok().filter(|value| !value.is_empty())?;
        provider_env.push(((*name).to_string(), value));
    }
    for name in optional {
        if let Ok(value) = std::env::var(name) {
            if !value.is_empty() {
                provider_env.push((name.to_string(), value));
            }
        }
    }
    Some(provider_env)
}

/// Body of every `#[ignore]`d live-provider variant: fail when the backend is
/// not configured, otherwise run the very same assertions the mock-server
/// test runs.
fn run_live_provider_variant(label: &str, required: &[&str], optional: &[&str]) {
    let Some(provider_env) = provider_env_from_runner(required, optional) else {
        let required_variables = required.join(", ");
        panic!(
            "{label} requires {required_variables} to be set and non-empty in the test runner \
             environment"
        );
    };
    let run = run_persistence_e2e(&provider_env);
    assert_session_holds_one_completed_turn(&run);
}

#[test]
#[ignore = "requires a locally running Ollama (see tests/e2e/ollama-local/.envrc)"]
fn persistence_e2e_ollama_local() {
    run_live_provider_variant(
        "persistence_e2e_ollama_local",
        &["OLLAMA_BASE_URL"],
        &["OLLAMA_MODEL", "OLLAMA_API_KEY"],
    );
}

#[test]
#[ignore = "requires Ollama Cloud credentials (see tests/e2e/ollama-cloud/.envrc)"]
fn persistence_e2e_ollama_cloud() {
    run_live_provider_variant(
        "persistence_e2e_ollama_cloud",
        &["OLLAMA_BASE_URL", "OLLAMA_API_KEY"],
        &["OLLAMA_MODEL"],
    );
}

#[test]
#[ignore = "requires OpenRouter credentials (see tests/e2e/openrouter/.envrc)"]
fn persistence_e2e_openrouter() {
    run_live_provider_variant(
        "persistence_e2e_openrouter",
        &["OPENROUTER_API_KEY"],
        &["OPENROUTER_BASE_URL", "OPENROUTER_MODEL"],
    );
}
