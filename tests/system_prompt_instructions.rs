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

//! Wire-level coverage of the initial system-prompt contract: the resolved
//! global SOUL, the safety preamble, and the trusted instruction sections in
//! the documented order (`docs/configuration.md` → *Global persona
//! (`SOUL.md`)*).

use std::fs;
use std::path::Path;

use tempfile::TempDir;

mod common;

use common::mock_llm_server::{content_frame, MockLlmConfig, MockLlmServer};
use common::{leading_system_messages, run_ylva_with_provider};
use ylva::INSTRUCTION_SAFETY_PREAMBLE;

async fn start_mock_server() -> MockLlmServer {
    MockLlmServer::start(MockLlmConfig {
        frames: vec![content_frame("answer")],
        end_after_frames: true,
        ..MockLlmConfig::default()
    })
    .await
    .expect("mock server starts")
}

/// Run the real binary headlessly; `Command::output()` blocks, so the spawn
/// must happen off the accept loop's worker (see `docs/development-workflow.md`
/// → *Spawning the compiled binary in an integration test*).
async fn run_headless(
    server: &MockLlmServer,
    ylva_home: &Path,
    working_directory: &Path,
) -> std::process::Output {
    let provider_base_url = server.base_url();
    let ylva_home = ylva_home.to_path_buf();
    let working_directory = working_directory.to_path_buf();
    tokio::task::spawn_blocking(move || {
        run_ylva_with_provider(&ylva_home, &working_directory, &provider_base_url)
    })
    .await
    .expect("ylva run completes")
}

/// The leading system message of the chat request that carries `marker` —
/// selected by content so a utility-engine request (e.g. title generation)
/// with a different prompt cannot be mistaken for the composed one.
fn composed_system_prompt(server: &MockLlmServer, marker: &str) -> String {
    leading_system_messages(server)
        .into_iter()
        .find(|content| content.contains(marker))
        .unwrap_or_else(|| {
            panic!(
                "a chat request must carry the system prompt containing {marker:?}; requests: {:?}",
                server.request_bodies()
            )
        })
}

/// Assert that every anchor occurs in `haystack`, in the given order.
fn assert_in_order(haystack: &str, anchors: &[&str]) {
    let mut previous: Option<(usize, &str)> = None;
    for anchor in anchors {
        let position = haystack
            .find(anchor)
            .unwrap_or_else(|| panic!("{anchor:?} is missing from the system prompt:\n{haystack}"));
        if let Some((previous_position, previous_anchor)) = previous {
            assert!(
                previous_position < position,
                "{anchor:?} must appear after {previous_anchor:?} in the system prompt"
            );
        }
        previous = Some((position, anchor));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn instruction_sections_follow_the_soul_in_composition_order() {
    let server = start_mock_server().await;
    let ylva_home = TempDir::new().expect("home tempdir");
    let workspace_root = TempDir::new().expect("workspace tempdir");
    fs::create_dir_all(workspace_root.path().join(".ylva")).expect("workspace config directory");
    fs::write(
        workspace_root.path().join(".ylva/SOUL.md"),
        "workspace persona",
    )
    .expect("soul");
    fs::write(ylva_home.path().join("AGENTS.md"), "home agent rules").expect("home instructions");
    fs::write(workspace_root.path().join("CLAUDE.md"), "root claude rules")
        .expect("root CLAUDE instructions");
    fs::write(workspace_root.path().join("AGENTS.md"), "root agent rules")
        .expect("root AGENTS instructions");

    let output = run_headless(&server, ylva_home.path(), workspace_root.path()).await;
    assert!(
        output.status.success(),
        "run succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let prompt = composed_system_prompt(&server, "workspace persona");
    assert!(
        prompt.starts_with("workspace persona"),
        "the resolved SOUL opens the composed prompt:\n{prompt}"
    );
    assert_in_order(
        &prompt,
        &[
            "workspace persona",
            INSTRUCTION_SAFETY_PREAMBLE,
            "[instructions from $YLVA_HOME/AGENTS.md]",
            "home agent rules",
            "[end of instructions from $YLVA_HOME/AGENTS.md]",
            "[instructions from CLAUDE.md]",
            "root claude rules",
            "[end of instructions from CLAUDE.md]",
            "[instructions from AGENTS.md]",
            "root agent rules",
            "[end of instructions from AGENTS.md]",
        ],
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_subdirectory_section_follows_the_workspace_root_section() {
    let server = start_mock_server().await;
    let ylva_home = TempDir::new().expect("home tempdir");
    let workspace_root = TempDir::new().expect("workspace tempdir");
    let subdirectory = workspace_root.path().join("sub");
    fs::create_dir_all(workspace_root.path().join(".ylva")).expect("workspace config directory");
    fs::create_dir_all(&subdirectory).expect("subdirectory");
    fs::write(
        workspace_root.path().join(".ylva/SOUL.md"),
        "workspace persona",
    )
    .expect("soul");
    fs::write(workspace_root.path().join("AGENTS.md"), "root agent rules")
        .expect("root AGENTS instructions");
    fs::write(subdirectory.join("AGENTS.md"), "sub agent rules").expect("sub AGENTS instructions");

    let output = run_headless(&server, ylva_home.path(), &subdirectory).await;
    assert!(
        output.status.success(),
        "run succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let prompt = composed_system_prompt(&server, "workspace persona");
    assert_in_order(
        &prompt,
        &[
            "workspace persona",
            INSTRUCTION_SAFETY_PREAMBLE,
            "[instructions from AGENTS.md]",
            "root agent rules",
            "[end of instructions from AGENTS.md]",
            "[instructions from sub/AGENTS.md]",
            "sub agent rules",
            "[end of instructions from sub/AGENTS.md]",
        ],
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_byte_identical_duplicate_is_composed_exactly_once() {
    let server = start_mock_server().await;
    let ylva_home = TempDir::new().expect("home tempdir");
    let workspace_root = TempDir::new().expect("workspace tempdir");
    fs::create_dir_all(workspace_root.path().join(".ylva")).expect("workspace config directory");
    fs::write(
        workspace_root.path().join(".ylva/SOUL.md"),
        "workspace persona",
    )
    .expect("soul");
    fs::write(
        workspace_root.path().join("CLAUDE.md"),
        "duplicate agent rules",
    )
    .expect("CLAUDE instructions");
    fs::write(
        workspace_root.path().join("AGENTS.md"),
        "duplicate agent rules",
    )
    .expect("AGENTS instructions");

    let output = run_headless(&server, ylva_home.path(), workspace_root.path()).await;
    assert!(
        output.status.success(),
        "run succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let prompt = composed_system_prompt(&server, "workspace persona");
    assert_eq!(
        prompt.matches("duplicate agent rules").count(),
        1,
        "byte-identical content must appear exactly once:\n{prompt}"
    );
    assert!(
        prompt.contains("[instructions from CLAUDE.md]"),
        "the first occurrence in composition order keeps its section:\n{prompt}"
    );
    assert!(
        !prompt.contains("[instructions from AGENTS.md]"),
        "the duplicate must not emit its own section:\n{prompt}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn home_layer_instruction_files_are_composed_with_their_sentinels() {
    let server = start_mock_server().await;
    let ylva_home = TempDir::new().expect("home tempdir");
    let workspace_root = TempDir::new().expect("workspace tempdir");
    fs::create_dir_all(workspace_root.path().join(".ylva")).expect("workspace config directory");
    fs::write(ylva_home.path().join("AGENTS.md"), "home agent rules").expect("home instructions");

    let output = run_headless(&server, ylva_home.path(), workspace_root.path()).await;
    assert!(
        output.status.success(),
        "run succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let prompt = composed_system_prompt(&server, "You are Ylva");
    assert!(
        prompt.starts_with("You are Ylva"),
        "the built-in SOUL opens the composed prompt:\n{prompt}"
    );
    assert_in_order(
        &prompt,
        &[
            INSTRUCTION_SAFETY_PREAMBLE,
            "[instructions from $YLVA_HOME/AGENTS.md]",
            "home agent rules",
            "[end of instructions from $YLVA_HOME/AGENTS.md]",
        ],
    );
    assert!(
        !prompt.contains("[instructions from AGENTS.md]"),
        "no workspace instruction file exists in this run:\n{prompt}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_soul_without_instruction_files_is_composed_unchanged() {
    let server = start_mock_server().await;
    let ylva_home = TempDir::new().expect("home tempdir");
    let workspace_root = TempDir::new().expect("workspace tempdir");
    fs::create_dir_all(workspace_root.path().join(".ylva")).expect("workspace config directory");
    fs::write(
        workspace_root.path().join(".ylva/SOUL.md"),
        "workspace persona",
    )
    .expect("soul");

    let output = run_headless(&server, ylva_home.path(), workspace_root.path()).await;
    assert!(
        output.status.success(),
        "run succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let prompt = composed_system_prompt(&server, "workspace persona");
    assert!(
        prompt.starts_with("workspace persona"),
        "the resolved SOUL opens the composed prompt:\n{prompt}"
    );
    assert!(
        !prompt.contains(INSTRUCTION_SAFETY_PREAMBLE),
        "no safety preamble without instruction files:\n{prompt}"
    );
    assert!(
        !prompt.contains("[instructions from"),
        "no sentinel markers without instruction files:\n{prompt}"
    );
    server.abort();
}
