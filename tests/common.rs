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

// Test-support module: compiled into every integration-test crate via
// `mod common;`, but only some of its items are used by any given test.
// Allow dead code so a test binary that pulls in `mod common;` without
// using every helper does not warn about the ones it never references.
#![allow(dead_code)]

// Cargo compiles this file as a top-level test crate named 'common'; the attribute forces the submodule resolution to match what the other tests expect via `mod common;`. See AGENTS.md "Module layout" for context.
#[path = "common/mock_llm_server.rs"]
pub mod mock_llm_server;

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tokio::sync::mpsc;
use ylva::{EngineConfig, UiUpdate};

/// Spawn the real `ylva` binary in headless mode against a mock provider.
///
/// The child's environment is isolated with an allowlist — `env_clear()`
/// first, then only `PATH`, `YLVA_HOME`, and `OLLAMA_BASE_URL` — so nothing
/// from the developer's or CI's ambient environment leaks into the run. Use
/// this instead of copying the spawn wiring into a test file.
pub fn run_ylva_with_provider(
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

/// The `content` of the leading `system` message of every captured chat
/// request, in arrival order.
///
/// Requests without a leading system message (model catalog calls, plain
/// user-only turns) are skipped, so a caller can pick the message it cares
/// about by content instead of by request position.
pub fn leading_system_messages(server: &mock_llm_server::MockLlmServer) -> Vec<String> {
    server
        .request_bodies()
        .into_iter()
        .filter_map(|body| {
            let value: Value = serde_json::from_str(&body).ok()?;
            let messages = value.get("messages")?.as_array()?;
            let first = messages.first()?;
            if first.get("role")?.as_str()? != "system" {
                return None;
            }
            Some(first.get("content")?.as_str()?.to_string())
        })
        .collect()
}

/// Assert that some chat request's leading system message contains `expected`.
pub fn assert_request_uses_system_prompt(server: &mock_llm_server::MockLlmServer, expected: &str) {
    let messages = leading_system_messages(server);
    assert!(
        messages.iter().any(|content| content.contains(expected)),
        "a chat request must contain system prompt {expected:?}; requests: {:?}",
        server.request_bodies()
    );
}

/// Whether `value` matches `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$`
/// (the trailing `Z` is literal — a `+00:00` style must fail).
pub fn matches_utc_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20 {
        return false;
    }
    let digit_groups = [(0, 4), (5, 2), (8, 2), (11, 2), (14, 2), (17, 2)];
    for (offset, count) in digit_groups {
        let slice = &bytes[offset..offset + count];
        if !slice.iter().all(u8::is_ascii_digit) {
            return false;
        }
    }
    bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
}

/// Consumes `ui_rx` until `done` matches an update, returning every consumed
/// update in order. Panics if more than `budget` updates pass without a match
/// so a runaway loop fails the test instead of hanging.
pub async fn collect_until<F>(ui_rx: &mut mpsc::Receiver<UiUpdate>, done: F) -> Vec<UiUpdate>
where
    F: Fn(&UiUpdate) -> bool,
{
    let mut collected = Vec::new();
    loop {
        let Some(update) = ui_rx.recv().await else {
            return collected;
        };
        let is_done = done(&update);
        collected.push(update.clone());
        if is_done {
            return collected;
        }
        if collected.len() > 500 {
            panic!("collect_until budget exceeded");
        }
    }
}

/// Returns a default [`EngineConfig`] suitable for use in tests.
///
/// Use this helper instead of inline struct literals to keep test code DRY.
pub fn test_engine_config() -> EngineConfig {
    EngineConfig {
        provider_connection: "mock".to_string(),
        model: "test-model".to_string(),
        temperature: None,
        context_length: None,
        api_base_url: None,
        api_key: None,
        reasoning: None,
    }
}
