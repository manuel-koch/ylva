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

use std::sync::Arc;
use std::time::Duration;

use ylva::{
    AgentCommand, AgentLoop, AssistantStatus, Message, MockProvider, ToolRegistry, UiUpdate,
    DEFAULT_SYSTEM_PROMPT, TOOL_PROTOCOL_SYSTEM_PROMPT,
};

mod common;

use common::collect_until;

#[tokio::test(start_paused = true)]
async fn stop_aborts_stream_flushes_partial_and_clears_pending_steers() {
    let provider = MockProvider::new(
        vec!["par-".into(), "tial".into(), "!".into()],
        Duration::from_millis(10),
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider), Arc::new(ToolRegistry::new()));

    agent
        .send_command(AgentCommand::Prompt("first".into()))
        .await;
    let handle = agent.run();

    let started = collect_until(&mut ui_rx, |update| {
        matches!(update, UiUpdate::StreamStarted)
    })
    .await;
    assert!(started.last() == Some(&UiUpdate::StreamStarted));

    // A partial token must already be streaming before the interrupt.
    let partial = collect_until(&mut ui_rx, |update| {
        matches!(update, UiUpdate::AppendText(_))
    })
    .await;
    let emitted = match partial.last() {
        Some(UiUpdate::AppendText(text)) => text.as_str(),
        _ => "par-",
    };
    assert!(!emitted.is_empty());

    // Queue a steer and immediately interrupt: the steer must be dropped.
    agent
        .send_command(AgentCommand::Prompt("dropped steer".into()))
        .await;
    agent.send_command(AgentCommand::Stop).await;

    let cancelled = collect_until(&mut ui_rx, |update| {
        matches!(update, UiUpdate::StreamCancelled)
    })
    .await;
    assert!(cancelled
        .iter()
        .any(|update| matches!(update, UiUpdate::StreamCancelled)));

    drop(agent);
    let context = handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop returned an error");

    // Interrupt path: system message, user message, and a partial assistant
    // marked Interrupted — with no leftover pending steer message.
    assert_eq!(
        context.messages(),
        &[
            Message::System {
                content: DEFAULT_SYSTEM_PROMPT.to_string(),
            },
            Message::System {
                content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
            },
            Message::User {
                content: "first".to_string(),
            },
            Message::Assistant {
                content: emitted.to_string(),
                status: AssistantStatus::Interrupted,
                tool_calls: Vec::new(),
            },
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn stop_of_two_tokened_stream_commits_only_first_token() {
    let provider = MockProvider::new(
        vec!["only-".into(), "rest".into()],
        Duration::from_millis(10),
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider), Arc::new(ToolRegistry::new()));

    agent.send_command(AgentCommand::Prompt("go".into())).await;
    let handle = agent.run();

    let _ = collect_until(&mut ui_rx, is_append_text).await;

    agent.send_command(AgentCommand::Stop).await;

    let _ = collect_until(&mut ui_rx, |update| {
        matches!(update, UiUpdate::StreamCancelled)
    })
    .await;

    drop(agent);
    let context = handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop returned an error");

    assert_eq!(
        context.messages(),
        &[
            Message::System {
                content: DEFAULT_SYSTEM_PROMPT.to_string(),
            },
            Message::System {
                content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
            },
            Message::User {
                content: "go".to_string(),
            },
            Message::Assistant {
                content: "only-".to_string(),
                status: AssistantStatus::Interrupted,
                tool_calls: Vec::new(),
            },
        ]
    );
}

fn is_append_text(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::AppendText(_))
}
